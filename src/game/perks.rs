//! Skills and attributes. Items give them through `<addPerk>id/level/0</addPerk>`;
//! the client adds nothing up itself, it shows what the server sends.

use std::collections::BTreeMap;

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active};
use crate::afc::AfcWriter;
use crate::config::{BagSettings, Catalog};
use crate::messages as msg;
use crate::storage::CharacterData;

/// Raises every attribute.
const ALL_ATTRIBUTES: i32 = 500;
/// Strength, accuracy, luck and education, in the order of the details message.
const ATTRIBUTE_PERKS: [i32; 4] = [510, 520, 540, 530];
const BASE_ATTRIBUTE: i32 = 1;
/// Named by some items but missing from the client's table of perks.
const UNDEFINED_PERKS: [i32; 1] = [4];

fn item_perks(xml: &str) -> impl Iterator<Item = (i32, i32)> + '_ {
    xml.split("<addPerk>").skip(1).filter_map(|rest| {
        let mut fields = rest.split('<').next()?.split('/');
        Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
    })
}

/// Perk levels the worn gear gives. Two items with one perk do not add up: the
/// higher level counts, as the client does for the parts of one item.
pub fn worn_bonuses(bags: &BagSettings, catalog: &Catalog, character: &CharacterData) -> BTreeMap<i32, i32> {
    let mut bonuses = BTreeMap::new();
    let worn = character.items.iter().filter(|item| bags.gear_bags.contains(&item.bag));
    for template in worn.filter_map(|item| catalog.get(item.template)) {
        for (perk, level) in item_perks(&template.xml).filter(|(perk, _)| !UNDEFINED_PERKS.contains(perk)) {
            let best = bonuses.entry(perk).or_insert(level);
            *best = level.max(*best);
        }
    }
    bonuses
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

/// Cases 1400 and 1401: int used, int limit, then until the end of the message
/// int perk, bool learned, int level, int level with bonuses, float exp, int exp
/// to the next level. Nothing can be learned yet, so every entry is a bonus.
fn perk_list(bonuses: &BTreeMap<i32, i32>) -> Vec<u8> {
    let mut w = AfcWriter::new();
    w.int32(0).int32(0);
    for (perk, level) in bonuses {
        w.int32(*perk).boolean(false).int32(0).int32(*level).single(0.0).int32(0);
    }
    w.finish()
}

fn bonuses_of(conn: &Conn, player: &Player) -> BTreeMap<i32, i32> {
    worn_bonuses(&conn.world.settings.bags, &conn.world.catalog, &player.data)
}

/// Tells the client without opening its window; its fishing formulas read these.
pub async fn send(conn: &Conn, player: &Player) -> Result<()> {
    let bonuses = bonuses_of(conn, player);
    conn.send(msg::PERKS_UPDATE, perk_list(&bonuses)).await?;
    conn.log(&format!("   -> 1400 perks from gear {bonuses:?}"));
    Ok(())
}

pub async fn window(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let bonuses = bonuses_of(&session.conn, player);
    session.conn.send(msg::PERKS, perk_list(&bonuses)).await?;
    session.conn.log(&format!("   -> 1401 perks from gear {bonuses:?}"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let bonuses = [(540, 5)].into();
        // Two header ints, then int, byte, int, int, float, int.
        assert_eq!(perk_list(&bonuses).len(), 8 + 21);
    }
}
