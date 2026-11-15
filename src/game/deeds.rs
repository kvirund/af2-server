//! Деяния: the character card's achievements tab (`config/quests.json`,
//! `deeds`). A deed counts fish as they are landed, kept or not, and pays once.
//!
//! Client 130 (int 0, the own card) asks for the tab; reply 130 is the deeds
//! until the end, each: int deed (its name in QTask.xml `achivments_name`),
//! int state (0 going, 2 done), int progress, then the task list's record (long
//! id, int 1 fish, int category 2000, int task text, ... string reward). Case
//! 131 is one such record for a deed that has just opened, 132 for one just
//! done (with fanfare). Progress is the quest value `deed_<id>`, done
//! `deed_<id>_done`, announced `deed_<id>_open`.

use anyhow::Result;

use super::board::write_task;
use super::fishing::TROPHY_CLASS;
use super::{Conn, Outcome, Player, Session, active, chat, quests};
use crate::afc::AfcWriter;
use crate::config::{Deed, QuestReward};
use crate::messages as msg;
use crate::storage::{BoardTask, CaughtFish, CharacterData};

const CATEGORY: i32 = 2000;
const FISH: i32 = 1;
const GOING: i32 = 0;
const DONE: i32 = 2;

fn progress_key(deed: &Deed) -> String {
    format!("deed_{}", deed.id)
}

fn done_key(deed: &Deed) -> String {
    format!("deed_{}_done", deed.id)
}

fn open_key(deed: &Deed) -> String {
    format!("deed_{}_open", deed.id)
}

fn value(character: &CharacterData, key: &str) -> i32 {
    character.quest_values.get(key).copied().unwrap_or(0)
}

fn is_done(character: &CharacterData, deed: &Deed) -> bool {
    value(character, &done_key(deed)) > 0
}

/// Whether the deed shows: the deeds before it are done, and its species, if
/// it waits for one, has been caught.
fn is_open(deeds: &[Deed], character: &CharacterData, deed: &Deed) -> bool {
    let before = deed.requires.iter().all(|id| deeds.iter().find(|one| one.id == *id).is_some_and(|one| is_done(character, one)));
    let caught = deed.opens_on.is_empty() || deed.opens_on.iter().any(|species| character.catches.get(species).is_some_and(|catches| catches.count > 0));
    before && caught
}

/// Whether a landed fish counts for the deed.
fn counts(deed: &Deed, fish: &CaughtFish, rod_kind: Option<i32>) -> bool {
    (deed.species.is_empty() || deed.species.contains(&fish.species))
        && fish.grams >= deed.min_grams
        && (deed.max_grams == 0 || fish.grams <= deed.max_grams)
        && (!deed.trophy || fish.size_class == TROPHY_CLASS)
        && (deed.rod_kind == 0 || rod_kind == Some(deed.rod_kind))
        && (!deed.unique || !fish.traits.is_empty())
}

/// The reward in the client's words for the info window.
pub(super) fn prize(reward: &QuestReward) -> String {
    let mut parts: Vec<String> = reward.items.iter().map(|[item, count]| format!("wpn={item}/{}", (*count).max(1))).collect();
    if reward.experience > 0 {
        parts.push(format!("exp={}", reward.experience));
    }
    if reward.money > 0 {
        parts.push(format!("money={}", reward.money));
    }
    parts.extend(reward.reputation.iter().map(|[faction, points]| format!("repfrac={faction}/{points}")));
    parts.join(";")
}

fn write(w: &mut AfcWriter, character: &CharacterData, deed: &Deed) {
    let done = is_done(character, deed);
    let progress = if done { deed.count } else { value(character, &progress_key(deed)).min(deed.count) };
    w.int32(deed.id).int32(if done { DONE } else { GOING }).int32(progress);
    let task = BoardTask {
        id: i64::from(deed.id),
        kind: FISH,
        category: CATEGORY,
        task: deed.task,
        species: if deed.species.len() == 1 { deed.species[0] } else { 0 },
        target: deed.count,
        min_grams: deed.min_grams,
        max_grams: deed.max_grams,
        progress,
        reward: prize(&deed.reward),
        ..Default::default()
    };
    write_task(w, &task);
}

async fn send_one(conn: &Conn, id: i32, character: &CharacterData, deed: &Deed) -> Result<()> {
    let mut w = AfcWriter::new();
    write(&mut w, character, deed);
    conn.send(id, w.finish()).await
}

/// Client 130 for the own card: the deeds that show.
pub async fn tab(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let deeds = &conn.world.settings.quests.deeds;
    let mut w = AfcWriter::new();
    let mut shown = 0;
    for deed in deeds.iter().filter(|deed| is_open(deeds, &player.data, deed)) {
        write(&mut w, &player.data, deed);
        shown += 1;
    }
    conn.send(msg::ACHIEVEMENTS, w.finish()).await?;
    conn.log(&format!("   -> 130 {shown} deed(s)"));
    Ok(Outcome::Answered)
}

/// A fish has been landed: counts it, pays the deeds it finishes and tells
/// about the ones that open.
pub async fn landed(conn: &Conn, player: &mut Player, fish: &CaughtFish, rod_kind: Option<i32>) -> Result<()> {
    let world = conn.world.clone();
    let deeds = &world.settings.quests.deeds;
    if deeds.is_empty() {
        return Ok(());
    }
    let mut finished = Vec::new();
    for deed in deeds {
        if is_done(&player.data, deed) || !is_open(deeds, &player.data, deed) || !counts(deed, fish, rod_kind) {
            continue;
        }
        let progress = value(&player.data, &progress_key(deed)) + 1;
        player.data.quest_values.insert(progress_key(deed), progress);
        if progress >= deed.count {
            player.data.quest_values.insert(done_key(deed), 1);
            finished.push(deed);
        }
    }
    for deed in finished {
        send_one(conn, msg::DEED_DONE, &player.data, deed).await?;
        if !quests::reward_fits(conn, player, &deed.reward) {
            chat::system(conn, "Награда за деяние не поместилась в рюкзак.").await?;
        }
        let said = quests::give(conn, player, &deed.reward).await?;
        if !said.is_empty() {
            chat::system(conn, &format!("Деяние выполнено: {}.", said.join(", "))).await?;
        }
        conn.log(&format!("   -> 132 deed {} done: {}", deed.id, said.join(", ")));
    }
    // Deeds that wait for something are told about once they open.
    for deed in deeds {
        let waits = !deed.requires.is_empty() || !deed.opens_on.is_empty();
        if waits && value(&player.data, &open_key(deed)) == 0 && is_open(deeds, &player.data, deed) {
            player.data.quest_values.insert(open_key(deed), 1);
            send_one(conn, msg::DEED_OPENED, &player.data, deed).await?;
            conn.log(&format!("   -> 131 deed {} opened", deed.id));
        }
    }
    conn.save(player).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Catches;

    fn fish(species: i32, grams: i32) -> CaughtFish {
        CaughtFish { id: 1, species, grams, size_class: 1, bait: 1010, list: 0, experience: 1, traits: Vec::new() }
    }

    #[test]
    fn a_deed_counts_its_own_fish() {
        let small = Deed { id: 2, task: 4, count: 1000, max_grams: 100, ..Default::default() };
        assert!(counts(&small, &fish(1, 90), None));
        assert!(!counts(&small, &fish(1, 120), None));
        let leeches = Deed { id: 8, task: 9, count: 600, species: vec![72, 132, 53], ..Default::default() };
        assert!(counts(&leeches, &fish(132, 8), None));
        assert!(!counts(&leeches, &fish(1, 8), None));
        let spinning = Deed { id: 16, task: 16, count: 2000, trophy: true, rod_kind: 3, ..Default::default() };
        let trophy = CaughtFish { size_class: TROPHY_CLASS, ..fish(95, 5000) };
        assert!(counts(&spinning, &trophy, Some(3)));
        assert!(!counts(&spinning, &trophy, Some(1)));
        let rare = Deed { id: 14, task: 14, count: 1000, unique: true, max_grams: 20000, ..Default::default() };
        assert!(counts(&rare, &CaughtFish { traits: vec![108], ..fish(95, 5000) }, None));
        assert!(!counts(&rare, &fish(95, 5000), None));
    }

    #[test]
    fn a_deed_opens_after_its_deeds_and_its_species() {
        let deeds = vec![
            Deed { id: 2, task: 4, count: 1, ..Default::default() },
            Deed { id: 19, task: 1, count: 1, requires: vec![2], ..Default::default() },
            Deed { id: 3, task: 1, count: 1, opens_on: vec![38], ..Default::default() },
        ];
        let mut character = CharacterData::default();
        assert!(is_open(&deeds, &character, &deeds[0]));
        assert!(!is_open(&deeds, &character, &deeds[1]));
        assert!(!is_open(&deeds, &character, &deeds[2]));
        character.quest_values.insert("deed_2_done".into(), 1);
        character.catches.insert(38, Catches { count: 1, grams: 300 });
        assert!(is_open(&deeds, &character, &deeds[1]));
        assert!(is_open(&deeds, &character, &deeds[2]));
    }

    #[test]
    fn the_reward_reads_like_a_board_reward() {
        let reward = QuestReward { items: vec![[1466, 3]], experience: 300, reputation: vec![[7, 1000]], ..Default::default() };
        assert_eq!(prize(&reward), "wpn=1466/3;exp=300;repfrac=7/1000");
    }
}
