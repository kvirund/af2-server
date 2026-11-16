//! Skills and attributes as the client sees them: levels studied (see
//! `skills.rs`) plus what items give through `<addPerk>id/level/0</addPerk>`.
//! The client adds nothing up itself, it shows what the server sends.

use std::collections::BTreeMap;

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, skills};
use crate::afc::AfcWriter;
use crate::config::{AttributeSettings, BagSettings, Catalog, Settings};
use crate::messages as msg;
use crate::storage::CharacterData;

/// Raises every attribute.
const ALL_ATTRIBUTES: i32 = 500;
/// Strength, accuracy, luck and education, in the order of the details message.
const ATTRIBUTE_PERKS: [i32; 4] = [510, 520, 540, 530];
const BASE_ATTRIBUTE: i32 = 1;
const LUCK_TOTAL: usize = 6;
const EDUCATION_TOTAL: usize = 7;
/// Named by some items but missing from the client's table of perks.
const UNDEFINED_PERKS: [i32; 1] = [4];

fn item_perks(xml: &str) -> impl Iterator<Item = (i32, i32)> + '_ {
    xml.split("<addPerk>").skip(1).filter_map(|rest| {
        let mut fields = rest.split('<').next()?.split('/');
        Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
    })
}

/// Bonuses of different worn items add up. Assembled tackle is several sets of
/// which one is in hand, so each skill takes only its best set.
fn combine(bags: &BagSettings, gear: impl Iterator<Item = (i32, i32, i32)>) -> BTreeMap<i32, i32> {
    let mut bonuses = BTreeMap::new();
    let mut in_sets = BTreeMap::new();
    for (bag, perk, level) in gear {
        if bags.worn_bags.contains(&bag) {
            *bonuses.entry(perk).or_insert(0) += level;
        } else if bags.tackle_bags.contains(&bag) {
            *in_sets.entry((bag, perk)).or_insert(0) += level;
        }
    }
    let mut best = BTreeMap::new();
    for ((_, perk), level) in in_sets {
        let top = best.entry(perk).or_insert(0);
        *top = level.max(*top);
    }
    for (perk, level) in best {
        *bonuses.entry(perk).or_insert(0) += level;
    }
    bonuses
}

pub fn worn_bonuses(bags: &BagSettings, catalog: &Catalog, character: &CharacterData) -> BTreeMap<i32, i32> {
    let gear = character.items.iter().filter_map(|item| Some((item.bag, catalog.get(item.template)?))).flat_map(
        |(bag, template)| {
            item_perks(&template.xml)
                .filter(|(perk, _)| !UNDEFINED_PERKS.contains(perk))
                .map(move |(perk, level)| (bag, perk, level))
        },
    );
    combine(bags, gear)
}

/// Studied levels plus what the gear gives: what every rule of the game reads.
pub fn levels(settings: &Settings, catalog: &Catalog, character: &CharacterData) -> BTreeMap<i32, i32> {
    let mut levels = worn_bonuses(&settings.bags, catalog, character);
    for (perk, skill) in &character.skills {
        *levels.entry(*perk).or_insert(0) += skill.level;
    }
    levels
}

/// Four base attributes, then the same four with the bonuses.
pub fn attributes(bonuses: &BTreeMap<i32, i32>) -> [i32; 8] {
    let bonus = |perk: &i32| bonuses.get(perk).copied().unwrap_or(0);
    let mut values = [BASE_ATTRIBUTE; 8];
    for (total, perk) in values[4..].iter_mut().zip(&ATTRIBUTE_PERKS) {
        *total += bonus(perk) + bonus(&ALL_ATTRIBUTES);
    }
    values
}

/// What the client leaves to the server: nothing in it reads these two.
#[derive(Debug, Clone, Copy)]
pub struct Effects {
    pub experience: f64,
    pub level_cost: f64,
    pub trophy: f64,
}

fn effects_of(settings: &AttributeSettings, attributes: &[i32; 8]) -> Effects {
    let per_point = |points: i32, percent: f64| f64::from(points) * percent / 100.0;
    let (luck, education) = (attributes[LUCK_TOTAL], attributes[EDUCATION_TOTAL]);
    Effects {
        experience: 1.0 + per_point(education, settings.education_experience_percent),
        level_cost: (1.0 - per_point(education, settings.education_level_cost_percent)).max(0.0),
        trophy: 1.0 + per_point(luck, settings.luck_trophy_percent),
    }
}

pub fn effects(conn: &Conn, player: &Player) -> Effects {
    effects_for(&conn.world.settings, &conn.world.catalog, &player.data)
}

pub fn effects_for(settings: &Settings, catalog: &Catalog, character: &CharacterData) -> Effects {
    effects_of(&settings.attributes, &attributes(&levels(settings, catalog, character)))
}

/// A skill's level with everything that adds to it.
pub fn level_of(conn: &Conn, player: &Player, perk: i32) -> i32 {
    levels_of(conn, player).get(&perk).copied().unwrap_or(0)
}

/// One line of the skills window: int perk, bool studied, int level, int level
/// with bonuses, float points, int points to the next level. Without "studied"
/// the client will not put the skill on study and draws no progress bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Entry {
    pub perk: i32,
    pub studied: bool,
    pub level: i32,
    pub total: i32,
    pub points: f32,
    pub to_next: i32,
}

fn write_entry(w: &mut AfcWriter, entry: &Entry) {
    w.int32(entry.perk).boolean(entry.studied).int32(entry.level).int32(entry.total);
    w.single(entry.points).int32(entry.to_next);
}

fn entry(settings: &Settings, bonuses: &BTreeMap<i32, i32>, character: &CharacterData, perk: i32) -> Entry {
    let bonus = bonuses.get(&perk).copied().unwrap_or(0);
    match character.skills.get(&perk) {
        Some(skill) => Entry {
            perk,
            studied: true,
            level: skill.level,
            total: skill.level + bonus,
            points: skill.experience as f32,
            to_next: skills::points_to_next(&settings.skill_tree, &settings.skills, perk, skill.level).round() as i32,
        },
        None => Entry { perk, studied: false, level: 0, total: bonus, points: 0.0, to_next: 0 },
    }
}

/// Every studied skill and every skill some worn item gives.
fn entries(settings: &Settings, catalog: &Catalog, character: &CharacterData) -> Vec<Entry> {
    let bonuses = worn_bonuses(&settings.bags, catalog, character);
    let mut perks: Vec<i32> = character.skills.keys().chain(bonuses.keys()).copied().collect();
    perks.sort_unstable();
    perks.dedup();
    perks.into_iter().map(|perk| entry(settings, &bonuses, character, perk)).collect()
}

/// Cases 1400 and 1401: int levels used, int limit, then entries to the end.
fn perk_list(used: i32, limit: i32, entries: &[Entry]) -> Vec<u8> {
    let mut w = AfcWriter::new();
    w.int32(used).int32(limit);
    for entry in entries {
        write_entry(&mut w, entry);
    }
    w.finish()
}

fn levels_of(conn: &Conn, player: &Player) -> BTreeMap<i32, i32> {
    levels(&conn.world.settings, &conn.world.catalog, &player.data)
}

fn list_for(conn: &Conn, player: &Player) -> (Vec<u8>, String) {
    let settings = &conn.world.settings;
    let character = &player.data;
    let (used, limit) = (skills::used(character), skills::limit(&settings.skills, character));
    let entries = entries(settings, &conn.world.catalog, character);
    let shown: Vec<String> = entries.iter().map(|entry| format!("{}:{}", entry.perk, entry.total)).collect();
    (perk_list(used, limit, &entries), format!("limit {used}/{limit}, {}", shown.join(" ")))
}

/// Tells the client without opening its window; its fishing formulas read these.
pub async fn send(conn: &Conn, player: &Player) -> Result<()> {
    let (list, shown) = list_for(conn, player);
    conn.send(msg::PERKS_UPDATE, list).await?;
    conn.log(&format!("   -> 1400 perks {shown}"));
    Ok(())
}

pub async fn window(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let (list, shown) = list_for(&session.conn, player);
    session.conn.send(msg::PERKS, list).await?;
    session.conn.log(&format!("   -> 1401 perks {shown}"));
    Ok(Outcome::Answered)
}

/// Case 1408 shows "Изучен навык ... N уровня" (or "Получен навык" at level 0)
/// for each entry; case 1410 changes the entries without a word.
pub async fn announce(conn: &Conn, player: &Player, perks: &[i32], loud: bool) -> Result<()> {
    if perks.is_empty() {
        return Ok(());
    }
    let settings = &conn.world.settings;
    let bonuses = worn_bonuses(&settings.bags, &conn.world.catalog, &player.data);
    let mut w = AfcWriter::new();
    for perk in perks {
        write_entry(&mut w, &entry(settings, &bonuses, &player.data, *perk));
    }
    let id = if loud { msg::PERK_LEARNED } else { msg::PERKS_QUIET };
    conn.send(id, w.finish()).await?;
    conn.log(&format!("   -> {id} perks {perks:?}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bags() -> BagSettings {
        BagSettings {
            pages: 1,
            start_cells: 1,
            storage_cells: 1,
            cells_per_purchase: 1,
            token_item: 1,
            purchase_tokens: 1,
            storage_token_item: 1,
            storage_purchase_tokens: 1,
            purchase_premium: 1,
            page_kinds: Default::default(),
            other_page: 1,
            worn_bags: vec![60],
            tackle_bags: vec![100, 101],
        }
    }

    #[test]
    fn different_worn_items_add_up() {
        let gear = vec![(60, 540, 5), (60, 540, 1)];
        assert_eq!(combine(&bags(), gear.into_iter()), [(540, 6)].into());
    }

    #[test]
    fn tackle_sets_do_not_add_up_but_join_what_is_worn() {
        let gear = vec![(100, 403, 1), (100, 403, 2), (101, 403, 2), (60, 403, 1)];
        assert_eq!(combine(&bags(), gear.into_iter()), [(403, 4)].into());
    }

    #[test]
    fn items_in_other_bags_give_nothing() {
        assert!(combine(&bags(), vec![(1, 540, 5), (52, 530, 5)].into_iter()).is_empty());
    }

    #[test]
    fn attributes_change_experience_cost_and_trophies() {
        let settings = AttributeSettings {
            education_experience_percent: 2.0,
            education_level_cost_percent: 1.0,
            luck_trophy_percent: 10.0,
        };
        let effects = effects_of(&settings, &[1, 1, 1, 1, 1, 1, 5, 10]);
        assert!((effects.experience - 1.2).abs() < 1e-9);
        assert!((effects.level_cost - 0.9).abs() < 1e-9);
        assert!((effects.trophy - 1.5).abs() < 1e-9);
    }

    #[test]
    fn perks_parse_from_the_item() {
        let xml = "<addperks><addPerk>540/5/0</addPerk><addPerk>530/2/0</addPerk></addperks>";
        assert_eq!(item_perks(xml).collect::<Vec<_>>(), vec![(540, 5), (530, 2)]);
        assert_eq!(item_perks("<addperks />").count(), 0);
    }

    #[test]
    fn attributes_show_the_base_and_the_total() {
        let bonuses = [(540, 5), (530, 2)].into();
        assert_eq!(attributes(&bonuses), [1, 1, 1, 1, 1, 1, 6, 3]);
    }

    #[test]
    fn a_bonus_to_all_raises_every_attribute() {
        let bonuses = [(500, 2), (510, 1)].into();
        assert_eq!(attributes(&bonuses), [1, 1, 1, 1, 4, 3, 3, 3]);
    }

    #[test]
    fn the_list_has_one_entry_per_perk() {
        let entry = Entry { perk: 540, studied: false, level: 0, total: 5, points: 0.0, to_next: 0 };
        // Two header ints, then int, byte, int, int, float, int.
        assert_eq!(perk_list(0, 0, &[entry]).len(), 8 + 21);
    }
}
