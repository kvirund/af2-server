//! Титулы: the character card's titles tab (client `Titul.xml`).
//!
//! Client 101 (int 0) asks for the tab; reply 101 is the titles owned, ints
//! until the end. Client 102 (int title) picks the one shown under the name;
//! reply 108 (int title) sets it, and the field after the location in the
//! character details carries it from then on. Case 109 (int title) says a new
//! one was earned. The client counts down to the next change itself from the
//! user value `titul_change` (case 15, `DateTime.ToBinary`).
//!
//! Owned titles are quest values `title_<id>`, the shown one `title`, the time
//! of the last change `title_changed`; the rules count quest values
//! `count_<name>` and mark bases as `visited_<base>`.

use std::time::Instant;

use anyhow::Result;
use chrono::{Local, TimeDelta, Utc};

use super::{Conn, Outcome, Player, Session, active, chat, doctor, reputation, travel, vip};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{ReputationSettings, TitleRule};
use crate::messages as msg;
use crate::storage::CharacterData;

/// "Нет титула".
const NO_TITLE: i32 = 0;
const SHOWN: &str = "title";
const CHANGED: &str = "title_changed";
const CHANGE_PARAM: &str = "titul_change";
/// How often the rules are looked at while the character is in play.
const CHECK_SECONDS: u64 = 30;
/// The client's VIP window: "Время на смену титула -50%".
const VIP_CHANGE_SHARE: f64 = 0.5;

fn owned_key(title: i32) -> String {
    format!("title_{title}")
}

fn count_key(name: &str) -> String {
    format!("count_{name}")
}

fn visited_key(base: i32) -> String {
    format!("visited_{base}")
}

fn value(character: &CharacterData, key: &str) -> i32 {
    character.quest_values.get(key).copied().unwrap_or(0)
}

/// The title shown under the name.
pub fn shown(character: &CharacterData) -> i32 {
    value(character, SHOWN)
}

fn owns(character: &CharacterData, title: i32) -> bool {
    title == NO_TITLE || value(character, &owned_key(title)) > 0
}

fn owned(character: &CharacterData) -> Vec<i32> {
    let mut titles: Vec<i32> = character
        .quest_values
        .iter()
        .filter(|(_, value)| **value > 0)
        .filter_map(|(key, _)| key.strip_prefix("title_")?.parse().ok())
        .collect();
    titles.sort_unstable();
    titles
}

/// Whether everything the rule names holds; a rule naming nothing never does.
fn earned(factions: &ReputationSettings, character: &CharacterData, rule: &TitleRule, vip_on: bool) -> bool {
    let mut any = false;
    let mut holds = true;
    if !rule.counters.is_empty() {
        any = true;
        let sum: i64 = rule.counters.iter().map(|name| i64::from(value(character, &count_key(name)))).sum();
        holds &= sum >= i64::from(rule.at);
    }
    let landed = |species: &i32| character.catches.get(species).map_or(0, |catches| catches.count);
    if !rule.species.is_empty() {
        any = true;
        holds &= rule.species.iter().map(landed).sum::<i64>() >= i64::from(rule.at);
    }
    if rule.fish > 0 {
        any = true;
        holds &= character.catches.keys().map(landed).sum::<i64>() >= i64::from(rule.fish);
    }
    if rule.faction > 0 {
        any = true;
        let top = factions.factions.get(&rule.faction).map(|faction| faction.levels - 1);
        holds &= top.is_some_and(|top| reputation::level_with(factions, character, rule.faction) >= top);
    }
    if rule.level > 0 {
        any = true;
        holds &= character.level >= rule.level;
    }
    if rule.records > 0 {
        any = true;
        holds &= character.records.len() >= rule.records as usize;
    }
    if !rule.bases.is_empty() {
        any = true;
        holds &= rule.bases.iter().all(|base| value(character, &visited_key(*base)) > 0);
    }
    if rule.vip {
        any = true;
        holds &= vip_on;
    }
    any && holds
}

/// Adds to a counter the rules look at; saved with whatever the caller saves next.
pub fn count(player: &mut Player, name: &str, amount: i32) {
    *player.data.quest_values.entry(count_key(name)).or_insert(0) += amount;
}

/// Gives a title once, with the client's "Получен новый титул".
pub async fn grant(conn: &Conn, player: &mut Player, title: i32) -> Result<()> {
    if title <= NO_TITLE || owns(&player.data, title) {
        return Ok(());
    }
    player.data.quest_values.insert(owned_key(title), 1);
    conn.save(player).await?;
    let mut w = AfcWriter::new();
    w.int32(title);
    conn.send(msg::TITLE_NEW, w.finish()).await?;
    conn.log(&format!("   -> 109 new title {title}"));
    Ok(())
}

/// Marks the base as visited and gives the titles whose rules now hold.
pub async fn check(conn: &Conn, player: &mut Player) -> Result<()> {
    let world = conn.world.clone();
    let settings = &world.settings;
    let base = travel::current_base(settings, player);
    if base > 0 && value(&player.data, &visited_key(base)) == 0 {
        player.data.quest_values.insert(visited_key(base), 1);
        conn.save(player).await?;
    }
    let vip_on = vip::active(&player.data);
    for rule in &settings.quests.titles.rules {
        if !owns(&player.data, rule.title) && earned(&settings.reputation, &player.data, rule, vip_on) {
            grant(conn, player, rule.title).await?;
        }
    }
    Ok(())
}

/// Looks at the rules every so often while the character is in play.
pub async fn tick(session: &mut Session) -> Result<()> {
    let Some(player) = session.player.as_mut() else {
        return Ok(());
    };
    if player.last_title_check.elapsed().as_secs() < CHECK_SECONDS {
        return Ok(());
    }
    player.last_title_check = Instant::now();
    check(&session.conn, player).await
}

/// Seconds until the shown title may change again.
fn wait_seconds(change_minutes: i64, changed_at: i64, vip_on: bool, now: i64) -> i64 {
    if changed_at == 0 {
        return 0;
    }
    let minutes = if vip_on { (change_minutes as f64 * VIP_CHANGE_SHARE).round() as i64 } else { change_minutes };
    (changed_at + minutes * 60 - now).max(0)
}

fn wait_for(conn: &Conn, character: &CharacterData) -> i64 {
    let minutes = conn.world.settings.quests.titles.change_minutes;
    wait_seconds(minutes, i64::from(value(character, CHANGED)), vip::active(character), Utc::now().timestamp())
}

/// Case 15 `titul_change`: when the button "Выбрать" works again, 0 for now.
pub async fn send_change_time(conn: &Conn, player: &Player) -> Result<()> {
    let wait = wait_for(conn, &player.data);
    let until = if wait > 0 { doctor::dotnet_ticks((Local::now() + TimeDelta::seconds(wait)).naive_local()) } else { 0 };
    let mut w = AfcWriter::new();
    w.string(CHANGE_PARAM).int64(until);
    conn.send(msg::USER_PARAMS, w.finish()).await
}

/// Client 101 for the own card.
pub async fn tab(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let titles = owned(&player.data);
    let mut w = AfcWriter::new();
    w.int32(NO_TITLE);
    for title in &titles {
        w.int32(*title);
    }
    conn.send(msg::TITLES, w.finish()).await?;
    send_change_time(conn, player).await?;
    conn.log(&format!("   -> 101 {} title(s)", titles.len()));
    Ok(Outcome::Answered)
}

/// Client 102: int title, from the tab's "Выбрать".
pub async fn choose(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let title = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if !owns(&player.data, title) {
        conn.log(&format!("   title {title} is not owned"));
        return Ok(Outcome::Unanswered);
    }
    let wait = wait_for(conn, &player.data);
    if wait > 0 && title != shown(&player.data) {
        chat::system(conn, &format!("Сменить титул можно через {} мин.", (wait + 59) / 60)).await?;
        conn.log(&format!("   title change in {wait} s"));
        return Ok(Outcome::Answered);
    }
    player.data.quest_values.insert(SHOWN.into(), title);
    player.data.quest_values.insert(CHANGED.into(), Utc::now().timestamp() as i32);
    conn.save(player).await?;
    let mut w = AfcWriter::new();
    w.int32(title);
    conn.send(msg::TITLE_SET, w.finish()).await?;
    send_change_time(conn, player).await?;
    conn.log(&format!("   -> 108 title {title}"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FactionSettings;
    use crate::storage::{Catches, Record};

    #[test]
    fn owned_titles_are_read_from_quest_values() {
        let mut character = CharacterData::default();
        character.quest_values.insert("title_128".into(), 1);
        character.quest_values.insert("title_112".into(), 1);
        character.quest_values.insert(CHANGED.into(), 1_700_000_000);
        character.quest_values.insert(SHOWN.into(), 128);
        assert_eq!(owned(&character), vec![112, 128]);
        assert!(owns(&character, NO_TITLE));
        assert!(!owns(&character, 113));
    }

    #[test]
    fn a_rule_holds_when_all_it_names_does() {
        let mut settings = ReputationSettings::default();
        settings.factions.insert(3, FactionSettings { start_a: 100, start_b: 0, kf_b: 0.0, levels: 4, name: String::new() });
        let mut character = CharacterData::default();
        let nothing = TitleRule { title: 1, ..Default::default() };
        assert!(!earned(&settings, &character, &nothing, true));

        let drinker = TitleRule { title: 115, counters: vec!["drink".into()], at: 3, ..Default::default() };
        character.quest_values.insert("count_drink".into(), 2);
        assert!(!earned(&settings, &character, &drinker, false));
        character.quest_values.insert("count_drink".into(), 3);
        assert!(earned(&settings, &character, &drinker, false));

        let bar = TitleRule { title: 133, faction: 3, ..Default::default() };
        character.reputation.insert(3, 299);
        assert!(!earned(&settings, &character, &bar, false));
        character.reputation.insert(3, 300);
        assert!(earned(&settings, &character, &bar, false));

        let pioneer = TitleRule { title: 121, bases: vec![1, 3], ..Default::default() };
        character.quest_values.insert("visited_1".into(), 1);
        assert!(!earned(&settings, &character, &pioneer, false));
        character.quest_values.insert("visited_3".into(), 1);
        assert!(earned(&settings, &character, &pioneer, false));

        let records = TitleRule { title: 111, records: 2, ..Default::default() };
        character.records.insert(1, Record { grams: 1, location: 0, bait: 0 });
        assert!(!earned(&settings, &character, &records, false));
        character.records.insert(2, Record { grams: 1, location: 0, bait: 0 });
        assert!(earned(&settings, &character, &records, false));

        let top = TitleRule { title: 148, species: vec![19, 112], at: 3, ..Default::default() };
        character.catches.insert(19, Catches { count: 2, grams: 1000 });
        assert!(!earned(&settings, &character, &top, false));
        character.catches.insert(112, Catches { count: 1, grams: 10 });
        assert!(earned(&settings, &character, &top, false));
        let fisher = TitleRule { title: 135, fish: 4, ..Default::default() };
        assert!(!earned(&settings, &character, &fisher, false));
        character.catches.insert(5, Catches { count: 1, grams: 10 });
        assert!(earned(&settings, &character, &fisher, false));

        let donor = TitleRule { title: 127, vip: true, ..Default::default() };
        assert!(earned(&settings, &character, &donor, true));
        assert!(!earned(&settings, &character, &donor, false));
    }

    #[test]
    fn vip_halves_the_wait_for_a_change() {
        assert_eq!(wait_seconds(60, 0, false, 1000), 0);
        assert_eq!(wait_seconds(60, 1000, false, 1000), 3600);
        assert_eq!(wait_seconds(60, 1000, true, 1000), 1800);
        assert_eq!(wait_seconds(60, 1000, false, 1000 + 4000), 0);
    }
}
