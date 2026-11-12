//! The fridge: fish kept on ice at a base, away from the keepnet.
//!
//! A click on a fridge in the scene sends 166 with the object's own number
//! (0 in the bases, 1 for the doctor's in Убежище) and raises the black
//! overlay. Case 166 back opens the window: int fridge, int places, then fish
//! records until the end. The window shows
//! the keepnet in use on the left and the fridge on the right; its two buttons
//! move the selected fish and send 167 (into the fridge) or 168 (out of it),
//! both int fridge and long fish. The client moves the fish itself and ignores
//! any reply. Here the fridge number is the base, and fish on ice keep their
//! place in the character's fish with list [`FIRST_LIST`] plus the base.

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, keepnet, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{FridgeSettings, Settings};
use crate::messages as msg;
use crate::storage::{CaughtFish, CharacterData};

/// Fish lists from here on are fridges, one per base.
pub const FIRST_LIST: i32 = 100;

/// Set to 1 once the doctor lets the character use his fridge.
pub const DOCTOR_ALLOWS: &str = "doctor_fridge";

pub fn list_of(base: i32) -> i32 {
    FIRST_LIST + base
}

/// The quest value the fridge card's own script raises: `holod_<base>`.
pub fn cards_used(base: i32) -> String {
    format!("holod_{base}")
}

pub fn quest_value(character: &CharacterData, name: &str) -> i32 {
    character.quest_values.get(name).copied().unwrap_or(0)
}

/// Whether the fridge at a base takes fish: the doctor's stays shut until he
/// has had his biomass.
pub fn is_open(fridge: &FridgeSettings, character: &CharacterData, base: i32) -> bool {
    !fridge.doctor_bases.contains(&base) || quest_value(character, DOCTOR_ALLOWS) > 0
}

/// Places in the fridge at a base; none while it is shut.
pub fn places(fridge: &FridgeSettings, character: &CharacterData, base: i32) -> i32 {
    if !is_open(fridge, character, base) {
        return 0;
    }
    fridge.cells + quest_value(character, &cards_used(base))
}

fn on_ice(character: &CharacterData, base: i32) -> impl Iterator<Item = &CaughtFish> {
    character.fish.iter().filter(move |fish| fish.list == list_of(base))
}

/// The fridge the client names, if it is the one at the character's base.
fn here(conn: &Conn, player: &Player, fridge: i32) -> Option<i32> {
    let base = travel::current_base(&conn.world.settings, player);
    if fridge == base {
        Some(base)
    } else {
        conn.log(&format!("   fridge {fridge} is not at base {base}"));
        None
    }
}

/// Client 166: int the object's number. Answers 166 with the base's fridge.
pub async fn open(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let object = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let base = travel::current_base(settings, player);
    let places = places(&settings.fridge, &player.data, base);

    let mut w = AfcWriter::new();
    w.int32(base).int32(places);
    let mut count = 0;
    for fish in on_ice(&player.data, base) {
        // The window puts a fish it takes out into the keepnet in use as it is.
        let shown = CaughtFish { list: keepnet::IN_USE_LIST, ..fish.clone() };
        keepnet::write_fish(&mut w, &shown, &settings.fishing.mutant_species);
        count += 1;
    }
    conn.send(msg::FRIDGE, w.finish()).await?;
    conn.clear_overlay().await?;
    if is_open(&settings.fridge, &player.data, base) {
        conn.log(&format!("   -> 166 fridge {object} at base {base}: {count} of {places} places"));
    } else {
        conn.log(&format!("   -> 166 fridge {object} at base {base} is shut until the doctor opens it"));
    }
    Ok(Outcome::Answered)
}

/// Client 167 (into the fridge) or 168 (out of it): int fridge, long fish.
pub async fn transfer(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let fridge = r.int32()?;
    let id = r.int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(base) = here(conn, player, fridge) else {
        keepnet::send_all(conn, player).await?;
        return Ok(Outcome::Answered);
    };
    let putting = message.id == msg::FRIDGE_PUT;
    let (from, to) = if putting {
        (keepnet::IN_USE_LIST, list_of(base))
    } else {
        (list_of(base), keepnet::IN_USE_LIST)
    };
    let full = putting && on_ice(&player.data, base).count() as i32 >= places(&conn.world.settings.fridge, &player.data, base);
    let fish = player.data.fish.iter_mut().find(|fish| fish.id == id && fish.list == from);
    match fish {
        Some(fish) if !full => {
            fish.list = to;
            conn.save(player).await?;
            let way = if putting { "into" } else { "out of" };
            conn.log(&format!("   fish {id} moved {way} the fridge at base {base}"));
        }
        _ => {
            // The client has moved it already; the keepnet is put back right,
            // the fridge window is right again the next time it opens.
            conn.log(&format!("   fridge at base {base}: fish {id} not moved (full {full})"));
            keepnet::send_all(conn, player).await?;
        }
    }
    Ok(Outcome::Answered)
}

/// The fridge card used up: one more place at the character's base, as the
/// card's script in the client does it.
pub fn use_card(settings: &Settings, player: &mut Player) -> i32 {
    let base = travel::current_base(settings, player);
    let name = cards_used(base);
    let value = quest_value(&player.data, &name) + 1;
    player.data.quest_values.insert(name, value);
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> FridgeSettings {
        FridgeSettings { cells: 5, card: 1266, doctor_bases: vec![1] }
    }

    #[test]
    fn the_doctors_fridge_opens_when_he_allows_it() {
        let settings = settings();
        let mut character = CharacterData::default();
        assert_eq!(places(&settings, &character, 1), 0);
        assert_eq!(places(&settings, &character, 3), 5);
        character.quest_values.insert(DOCTOR_ALLOWS.into(), 1);
        assert_eq!(places(&settings, &character, 1), 5);
    }

    #[test]
    fn a_card_adds_a_place_at_its_own_base() {
        let settings = settings();
        let mut character = CharacterData::default();
        character.quest_values.insert(cards_used(3), 2);
        assert_eq!(places(&settings, &character, 3), 7);
        assert_eq!(places(&settings, &character, 4), 5);
    }

    #[test]
    fn fish_on_ice_belong_to_one_base() {
        let mut character = CharacterData::default();
        for (id, list) in [(1, 0), (2, list_of(3)), (3, list_of(4)), (4, list_of(3))] {
            character.fish.push(CaughtFish { id, species: 1, grams: 100, size_class: 0, bait: 0, list, experience: 0 });
        }
        let ids: Vec<i64> = on_ice(&character, 3).map(|fish| fish.id).collect();
        assert_eq!(ids, vec![2, 4]);
    }
}
