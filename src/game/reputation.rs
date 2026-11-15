//! Reputation with the factions of `RepFractions`: the card's tab, the points
//! the character earns and what a base's level adds there.
//!
//! Case 30 is the tab, one row per faction until the end: int faction, int
//! level, int points into that level. The client draws them against the points
//! the level needs, which it works out from its own copy of the faction table.
//! Case 305 (int faction) says a faction is new, 306 (int faction, int level)
//! that a level is reached and 307 (int faction, int, int, int points) how much
//! was earned; the two middle numbers are read and dropped.

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, perks, travel};
use crate::afc::AfcWriter;
use crate::config::{Catalog, FactionSettings, ReputationSettings, Settings};
use crate::messages as msg;
use crate::storage::{CharacterData, ItemEntry};

/// The cell of a rod's bag that holds the rod itself.
const ROD_CELL: i32 = 1;

/// Points it takes to go from `level` to the next one, as the client counts it.
pub fn needed(faction: &FactionSettings, level: i32) -> i64 {
    (faction.start_a as f64 + faction.start_b as f64 * faction.kf_b * f64::from(level)) as i64
}

/// The level the points reach and the points past it.
pub fn level_of(faction: &FactionSettings, points: i64) -> (i32, i64) {
    let top = (faction.levels - 1).max(0);
    let (mut level, mut left) = (0, points.max(0));
    while level < top && left >= needed(faction, level) {
        left -= needed(faction, level);
        level += 1;
    }
    (level, left)
}

/// The character's level with a faction; 0 for one it has no points with.
pub fn level_with(settings: &ReputationSettings, character: &CharacterData, faction: i32) -> i32 {
    let points = character.reputation.get(&faction).copied().unwrap_or(0);
    settings.factions.get(&faction).map_or(0, |found| level_of(found, points).0)
}

pub fn base_faction(settings: &ReputationSettings, base: i32) -> Option<i32> {
    settings.base_factions.get(&base).copied()
}

/// A share of a chance or an amount, raised by `percent` for each level of the
/// reputation with the base the character is at.
pub fn base_bonus(settings: &Settings, player: &Player, percent: f64) -> f64 {
    let reputation = &settings.reputation;
    let base = travel::current_base(settings, player);
    let level = base_faction(reputation, base).map_or(0, |faction| level_with(reputation, &player.data, faction));
    1.0 + percent / 100.0 * f64::from(level)
}

/// Points for letting a fish go.
pub fn for_release(settings: &ReputationSettings, grams: i32) -> i64 {
    let release = &settings.release;
    if release.points <= 0 && release.grams_per_point <= 0 {
        return 0;
    }
    release.points + i64::from(grams) / release.grams_per_point.max(1)
}

/// The kind of the rod in the same bag as the bait.
pub fn rod_kind(catalog: &Catalog, items: &[ItemEntry], bait_item: i64) -> Option<i32> {
    let bag = items.iter().find(|item| item.id == bait_item)?.bag;
    let rod = items.iter().find(|item| item.bag == bag && item.cell == ROD_CELL)?;
    catalog.get(rod.template).map(|template| template.kind)
}

/// The guild and points a «Вклад» skill gives for a fish landed on a rod of
/// that kind, at the skill's level.
pub fn for_contribution(settings: &ReputationSettings, rod_kind: i32, level: i32, grams: i32) -> Option<(i32, i64)> {
    let skill = settings.contribution.iter().find(|skill| skill.rod_kind == rod_kind)?;
    let index = usize::try_from(level).ok()?.checked_sub(1)?;
    let [points, grams_per_point] = *settings.contribution_levels.get(index.min(settings.contribution_levels.len().checked_sub(1)?))?;
    Some((skill.faction, points + i64::from(grams) / grams_per_point.max(1)))
}

/// Adds points with a faction, telling the client what changed, and saves.
pub async fn add(conn: &Conn, player: &mut Player, faction: i32, points: i64) -> Result<()> {
    let settings = &conn.world.settings.reputation;
    let Some(table) = settings.factions.get(&faction) else {
        conn.log(&format!("   reputation: faction {faction} is not in the settings"));
        return Ok(());
    };
    if points <= 0 {
        return Ok(());
    }
    let before = player.data.reputation.get(&faction).copied().unwrap_or(0);
    let after = before + points;
    player.data.reputation.insert(faction, after);
    conn.save(player).await?;

    if before == 0 {
        let mut w = AfcWriter::new();
        w.int32(faction);
        conn.send(msg::FACTION_OPENED, w.finish()).await?;
    }
    let (was, _) = level_of(table, before);
    let (level, into) = level_of(table, after);
    for reached in was + 1..=level {
        let mut w = AfcWriter::new();
        w.int32(faction).int32(reached);
        conn.send(msg::REPUTATION_LEVEL, w.finish()).await?;
    }
    if level > was {
        super::board::send_reputation_levels(conn, player).await?;
    }
    let mut w = AfcWriter::new();
    w.int32(faction).int32(level).int32(into as i32).int32(points as i32);
    conn.send(msg::REPUTATION_GAINED, w.finish()).await?;
    conn.log(&format!("   -> 307 reputation {faction} +{points}, now {after} (level {level}, {into} into it)"));
    Ok(())
}

/// The reputation of the base the character is at, for a fish let go.
pub async fn fish_released(conn: &Conn, player: &mut Player, grams: i32) -> Result<()> {
    let settings = &conn.world.settings;
    let base = travel::current_base(settings, player);
    let Some(faction) = base_faction(&settings.reputation, base) else {
        return Ok(());
    };
    add(conn, player, faction, for_release(&settings.reputation, grams)).await
}

/// A guild's reputation for a fish landed, if a «Вклад» skill covers the rod.
pub async fn fish_landed(conn: &Conn, player: &mut Player, bait_item: i64, grams: i32) -> Result<()> {
    let settings = &conn.world.settings.reputation;
    let Some(kind) = rod_kind(&conn.world.catalog, &player.data.items, bait_item) else {
        return Ok(());
    };
    let Some(skill) = settings.contribution.iter().find(|skill| skill.rod_kind == kind) else {
        return Ok(());
    };
    let level = perks::level_of(conn, player, skill.perk);
    if let Some((faction, points)) = for_contribution(settings, kind, level, grams) {
        add(conn, player, faction, points).await?;
    }
    Ok(())
}

/// Client 30: the card's reputation tab.
pub async fn tab(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings.reputation;
    let mut w = AfcWriter::new();
    let mut rows = 0;
    for (faction, points) in &player.data.reputation {
        let Some(table) = settings.factions.get(faction) else {
            continue;
        };
        let (level, into) = level_of(table, *points);
        w.int32(*faction).int32(level).int32(into as i32);
        rows += 1;
    }
    conn.send(msg::REPUTATION, w.finish()).await?;
    conn.log(&format!("   -> 30 reputation, {rows} factions"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ContributionSettings, ReleaseSettings};

    fn refuge() -> FactionSettings {
        FactionSettings { start_a: 300, start_b: 500, kf_b: 2.0, levels: 4, name: String::new() }
    }

    fn settings() -> ReputationSettings {
        ReputationSettings {
            release: ReleaseSettings { points: 1, grams_per_point: 1000 },
            contribution: vec![ContributionSettings { perk: 180, rod_kind: 1, faction: 1003 }],
            contribution_levels: vec![[2, 12000], [3, 6000], [4, 4000], [5, 3000], [6, 2400]],
            ..Default::default()
        }
    }

    #[test]
    fn levels_take_more_points_each_time() {
        let faction = refuge();
        assert_eq!((needed(&faction, 0), needed(&faction, 1), needed(&faction, 2)), (300, 1300, 2300));
        assert_eq!(level_of(&faction, 0), (0, 0));
        assert_eq!(level_of(&faction, 299), (0, 299));
        assert_eq!(level_of(&faction, 300), (1, 0));
        assert_eq!(level_of(&faction, 1700), (2, 100));
        assert_eq!(level_of(&faction, 10_000), (3, 10_000 - 3900));
    }

    #[test]
    fn heavier_fish_let_go_count_for_more() {
        let settings = settings();
        assert_eq!(for_release(&settings, 400), 1);
        assert_eq!(for_release(&settings, 2500), 3);
    }

    #[test]
    fn contribution_follows_the_skill_level() {
        let settings = settings();
        assert_eq!(for_contribution(&settings, 1, 0, 5000), None);
        assert_eq!(for_contribution(&settings, 1, 1, 25_000), Some((1003, 4)));
        assert_eq!(for_contribution(&settings, 1, 5, 4800), Some((1003, 8)));
        assert_eq!(for_contribution(&settings, 3, 5, 4800), None);
    }
}
