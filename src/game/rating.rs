//! The rating board on the bases (`RatingBoard` in the client): C→S 1440 (int
//! table, 0 to 4) when the board comes into the scene and on its buttons.
//! S→C 1440: int table, then rows until the end: long, long, string. The
//! client draws only the string, after the row's place; the longs here are the
//! character and the value. Titles from the client's `rait_1`..`rait_5`.
//!
//! The board has a fixed number of lines and the client writes past the last
//! one, so no more than [`MOST_ROWS`] rows are sent.

use anyhow::Result;

use super::{Outcome, Session, active, character, fishing::TROPHY_CLASS};
use crate::afc::{AfcMessage, AfcWriter};
use crate::messages as msg;
use crate::storage::{CaughtFish, CharacterData};

/// Rows a board shows at most (an assumption: the line count is in the scene).
const MOST_ROWS: usize = 10;
/// Trophies landed, counted from when this counter was added.
pub const TROPHIES_LANDED: &str = "trophies_landed";

/// «Самые богатые», «Выловили больше всего по количеству», «... по весу»,
/// «Почетные трофейщики», «Почетные меценаты».
#[derive(Debug, Clone, Copy, PartialEq)]
enum Table {
    Money,
    Count,
    Weight,
    Trophies,
    Patrons,
}

fn table(index: i32) -> Option<Table> {
    Some(match index {
        0 => Table::Money,
        1 => Table::Count,
        2 => Table::Weight,
        3 => Table::Trophies,
        4 => Table::Patrons,
        _ => return None,
    })
}

fn value(table: Table, character: &CharacterData) -> i64 {
    let counted = |key: &str| i64::from(character.quest_values.get(key).copied().unwrap_or(0));
    match table {
        Table::Money => character.money,
        Table::Count => character.catches.values().map(|catches| catches.count).sum(),
        Table::Weight => character.catches.values().map(|catches| catches.grams).sum(),
        Table::Trophies => counted(TROPHIES_LANDED),
        // Invented: the original's patrons paid real money, here it is the ergs spent.
        Table::Patrons => counted(character::ERGS_SPENT),
    }
}

/// 1234567 → "1 234 567".
fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(digit);
    }
    if number < 0 { format!("-{out}") } else { out }
}

fn shown(table: Table, value: i64) -> String {
    match table {
        Table::Money => format!("{} кр.", grouped(value / 100)),
        Table::Count => format!("{} рыб", grouped(value)),
        Table::Weight => format!("{},{} кг", grouped(value / 1000), value % 1000 / 100),
        Table::Trophies => format!("{} трофеев", grouped(value)),
        Table::Patrons => format!("{} эргов", grouped(value)),
    }
}

/// The best first; nobody with nothing to show.
fn rows(table: Table, characters: &[CharacterData]) -> Vec<(i64, i64, String)> {
    let mut rows: Vec<_> = characters
        .iter()
        .map(|character| (character.id, value(table, character), character.name.clone()))
        .filter(|(_, value, _)| *value > 0)
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.truncate(MOST_ROWS);
    rows.into_iter().map(|(id, value, name)| (id, value, format!("{name} — {}", shown(table, value)))).collect()
}

/// A trophy landed: the trophy table counts it.
pub fn landed(character: &mut CharacterData, fish: &CaughtFish) {
    if fish.size_class == TROPHY_CLASS {
        *character.quest_values.entry(TROPHIES_LANDED.into()).or_insert(0) += 1;
    }
}

pub async fn board(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let index = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(table) = table(index) else {
        conn.log(&format!("   rating table {index} is unknown"));
        return Ok(Outcome::Unanswered);
    };
    let world = &conn.world;
    let mut characters = Vec::new();
    for summary in world.store.all_characters().await? {
        let data = if summary.id == player.data.id {
            Some(player.data.clone())
        } else if let Some(live) = world.live_one(summary.id) {
            Some(live.data)
        } else {
            world.store.load_character(summary.id).await?
        };
        characters.extend(data);
    }
    let rows = rows(table, &characters);
    let mut w = AfcWriter::new();
    w.int32(index);
    for (id, value, text) in &rows {
        w.int64(*id).int64(*value).string(text);
    }
    conn.send(msg::RATING, w.finish()).await?;
    conn.log(&format!("   -> 1440 rating {index}: {} row(s)", rows.len()));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Catches;

    fn character(id: i64, name: &str, money: i64, catches: &[(i32, i64, i64)]) -> CharacterData {
        CharacterData {
            id,
            name: name.into(),
            money,
            catches: catches.iter().map(|(species, count, grams)| (*species, Catches { count: *count, grams: *grams })).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn tables_rank_the_best_first() {
        let all = [character(1, "Аня", 123_456_789, &[(1, 3, 2500)]), character(2, "Боря", 50_000, &[(1, 5, 900), (2, 1, 300)]), character(3, "Вася", 0, &[])];
        assert_eq!(
            rows(Table::Money, &all),
            vec![(1, 123_456_789, "Аня — 1 234 567 кр.".to_string()), (2, 50_000, "Боря — 500 кр.".to_string())]
        );
        assert_eq!(rows(Table::Count, &all).iter().map(|row| row.0).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(rows(Table::Weight, &all)[0].2, "Аня — 2,5 кг");
        assert!(rows(Table::Trophies, &all).is_empty());
    }

    #[test]
    fn trophies_are_counted_when_landed() {
        let fish = |size_class| CaughtFish { id: 1, species: 7, grams: 900, size_class, bait: 1010, list: 0, experience: 1, traits: Vec::new() };
        let mut one = CharacterData::default();
        landed(&mut one, &fish(TROPHY_CLASS));
        landed(&mut one, &fish(0));
        assert_eq!(value(Table::Trophies, &one), 1);
    }

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1 000");
        assert_eq!(grouped(-1_234_567), "-1 234 567");
    }
}
