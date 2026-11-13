//! The character card's "Статистика" and "Рекорды" tabs.
//!
//! Case 1150 is the statistics, one row per species until the end: int
//! species, long fish landed, long their grams together, long the personal
//! record, float mastery towards the next wreath (100 shows as 100%), int
//! wreaths 0 to 3. Case 40 is the records, one row per species: int species,
//! long holder, string holder's name, int grams, int base, int location, int
//! bait. The client draws the bait's icon, so it must be a real item. With
//! nobody else on this server the records are the character's own.

use std::collections::BTreeMap;

use anyhow::Result;

use super::{Outcome, Session, active, travel};
use crate::afc::AfcWriter;
use crate::config::MasterySettings;
use crate::messages as msg;
use crate::storage::{Catches, CharacterData};

/// Wreaths won and the share of the way to the next one, in percent.
pub fn mastery(settings: &MasterySettings, landed: i64) -> (i32, f32) {
    let mut from = 0;
    for (won, &to) in settings.catches.iter().enumerate() {
        if landed < to {
            let share = (landed - from) as f32 / (to - from).max(1) as f32;
            return (won as i32, share * 100.0);
        }
        from = to;
    }
    (settings.catches.len() as i32, 100.0)
}

/// Each wreath makes its species bite a little more often.
pub fn bite_factor(settings: &MasterySettings, catches: &BTreeMap<i32, Catches>, species: i32) -> f64 {
    let landed = catches.get(&species).map_or(0, |catches| catches.count);
    let (wreaths, _) = mastery(settings, landed);
    1.0 + settings.bite_percent / 100.0 * f64::from(wreaths)
}

/// Records kept before fish were counted say that at least one was landed.
pub fn count_old_records(character: &mut CharacterData) -> usize {
    let mut added = 0;
    for (species, record) in &character.records {
        if !character.catches.contains_key(species) {
            character.catches.insert(*species, Catches { count: 1, grams: i64::from(record.grams) });
            added += 1;
        }
    }
    added
}

pub async fn statistics(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let mut w = AfcWriter::new();
    for (species, catches) in &player.data.catches {
        let record = player.data.records.get(species).map_or(0, |record| record.grams);
        let (wreaths, share) = mastery(&conn.world.settings.mastery, catches.count);
        w.int32(*species).int64(catches.count).int64(catches.grams).int64(i64::from(record));
        w.single(share).int32(wreaths);
    }
    conn.send(msg::STATISTICS, w.finish()).await?;
    conn.log(&format!("   -> 1150 statistics, {} species", player.data.catches.len()));
    Ok(Outcome::Answered)
}

pub async fn records(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let mut records: Vec<_> = player.data.records.iter().collect();
    records.sort_by_key(|(species, _)| **species);
    let mut w = AfcWriter::new();
    for (species, record) in &records {
        let base = travel::base_of(&settings.bases, record.location).unwrap_or(settings.character.start_base);
        w.int32(**species).int64(player.data.id).string(&player.data.name).int32(record.grams);
        w.int32(base).int32(record.location).int32(record.bait);
    }
    conn.send(msg::RECORDS, w.finish()).await?;
    conn.log(&format!("   -> 40 records, {} species", records.len()));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Record;

    #[test]
    fn wreaths_follow_the_fish_landed() {
        let settings = MasterySettings { catches: vec![50, 200, 500], bite_percent: 3.0, card_percent: 5.0 };
        assert_eq!(mastery(&settings, 0), (0, 0.0));
        assert_eq!(mastery(&settings, 25), (0, 50.0));
        assert_eq!(mastery(&settings, 50), (1, 0.0));
        assert_eq!(mastery(&settings, 125), (1, 50.0));
        assert_eq!(mastery(&settings, 900), (3, 100.0));
    }

    #[test]
    fn each_wreath_adds_to_the_bite() {
        let settings = MasterySettings { catches: vec![50, 200, 500], bite_percent: 3.0, card_percent: 5.0 };
        let mut catches = BTreeMap::new();
        catches.insert(5, Catches { count: 250, grams: 0 });
        assert!((bite_factor(&settings, &catches, 5) - 1.06).abs() < 1e-9);
        assert_eq!(bite_factor(&settings, &catches, 7), 1.0);
    }

    #[test]
    fn an_old_record_counts_as_one_fish() {
        let mut character = CharacterData::default();
        character.records.insert(5, Record { grams: 1500, location: 0, bait: 1010 });
        character.records.insert(7, Record { grams: 300, location: 4, bait: 1016 });
        character.catches.insert(7, Catches { count: 9, grams: 2000 });
        assert_eq!(count_old_records(&mut character), 1);
        assert_eq!(character.catches[&5], Catches { count: 1, grams: 1500 });
        assert_eq!(character.catches[&7].count, 9);
    }
}
