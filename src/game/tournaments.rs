//! Tournaments (`config/quests.json`, `tournaments`): periodic, no sign-up;
//! whoever lands the fish they want at their base while one runs takes part
//! (Steam announcements). There are no other players here, so places are
//! score bars from the config, shown in the table as rows of their own.
//!
//! The client asks for nothing itself: the board's tab 1110 gets 705, a list
//! of tournament records; 808 says one has started for the player, 810 is the
//! table (the record, then rows until the end), 812 says it is over, 818 is
//! the result window with its rows. A record: long id, int template (QTask.xml
//! `tnames`), int state (0 preparing, 2 running, 4 closed), int base, 3
//! doubles (levels from, to, and an unknown 0), int players, dates sign-up,
//! start and end, string result, four prize strings (1st, 2nd, 3rd, anyone),
//! then the task list's record with category 3000. A row: long character,
//! int place, int score, string name, string prize.
//!
//! The player's run is in quest values `turn_<template>_*`; a run that has
//! ended is paid on the next frame.

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, Utc};

use super::board::write_task;
use super::{Conn, Outcome, Player, Session, chat, deeds, quests, travel};
use crate::afc::AfcWriter;
use crate::config::Tournament;
use crate::messages as msg;
use crate::storage::{BoardTask, CaughtFish, CharacterData};

const CATEGORY: i32 = 3000;
const FISH: i32 = 1;
const PREPARING: i32 = 0;
const RUNNING: i32 = 2;
const CLOSED: i32 = 4;
const PLACES: [&str; 3] = ["Порог 1 места", "Порог 2 места", "Порог 3 места"];

/// What the task's score is (QTask.xml category 3000).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Measure {
    Count,
    Total,
    ThreeLargest,
    Largest,
    Spread,
}

fn measure(task: i32) -> Measure {
    match task {
        4 | 5 => Measure::Total,
        6 => Measure::ThreeLargest,
        8 | 9 => Measure::Largest,
        10 | 11 => Measure::Spread,
        _ => Measure::Count,
    }
}

/// A run of a tournament: its number and its minutes since the epoch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Run {
    index: i64,
    start: i64,
    end: i64,
}

fn run_at(tournament: &Tournament, minute: i64) -> Run {
    let every = tournament.every.max(1);
    let index = (minute - tournament.offset).div_euclid(every);
    let start = index * every + tournament.offset;
    Run { index, start, end: start + tournament.minutes.min(every) }
}

fn now_minute() -> i64 {
    Utc::now().timestamp().div_euclid(60)
}

fn local(minute: i64) -> NaiveDateTime {
    DateTime::from_timestamp(minute * 60, 0).map_or_else(|| Local::now().naive_local(), |at| at.with_timezone(&Local).naive_local())
}

fn key(tournament: &Tournament, what: &str) -> String {
    format!("turn_{}_{what}", tournament.template)
}

fn get(character: &CharacterData, tournament: &Tournament, what: &str) -> i32 {
    character.quest_values.get(&key(tournament, what)).copied().unwrap_or(0)
}

fn set(character: &mut CharacterData, tournament: &Tournament, what: &str, value: i32) {
    character.quest_values.insert(key(tournament, what), value);
}

fn fits(tournament: &Tournament, fish: &CaughtFish) -> bool {
    (tournament.species == 0 || tournament.species == fish.species)
        && (tournament.bait == 0 || tournament.bait == fish.bait)
        && fish.grams >= tournament.min_grams
        && (tournament.max_grams == 0 || fish.grams <= tournament.max_grams)
}

/// Adds a fish to the player's run.
fn count(character: &mut CharacterData, tournament: &Tournament, grams: i32) {
    let n = get(character, tournament, "n");
    set(character, tournament, "n", n + 1);
    set(character, tournament, "total", get(character, tournament, "total").saturating_add(grams));
    set(character, tournament, "max", get(character, tournament, "max").max(grams));
    let least = get(character, tournament, "min");
    set(character, tournament, "min", if n == 0 { grams } else { least.min(grams) });
    let mut top = [get(character, tournament, "a"), get(character, tournament, "b"), get(character, tournament, "c"), grams];
    top.sort_unstable_by(|a, b| b.cmp(a));
    for (what, value) in ["a", "b", "c"].iter().zip(top) {
        set(character, tournament, what, value);
    }
}

fn score(character: &CharacterData, tournament: &Tournament) -> i32 {
    let n = get(character, tournament, "n");
    match measure(tournament.task) {
        Measure::Count => n,
        Measure::Total => get(character, tournament, "total"),
        Measure::ThreeLargest => ["a", "b", "c"].iter().map(|what| get(character, tournament, what)).sum(),
        Measure::Largest => get(character, tournament, "max"),
        Measure::Spread if n >= 2 => get(character, tournament, "max") - get(character, tournament, "min"),
        Measure::Spread => 0,
    }
}

/// 1 to 3 for the bars reached, 4 for taking part, 0 for nothing.
fn place(tournament: &Tournament, score: i32) -> usize {
    if score <= 0 {
        return 0;
    }
    tournament.places.iter().position(|bar| score >= *bar).map_or(4, |index| index + 1)
}

fn clear(character: &mut CharacterData, tournament: &Tournament, run: i64) {
    for what in ["n", "total", "max", "min", "a", "b", "c"] {
        character.quest_values.remove(&key(tournament, what));
    }
    set(character, tournament, "run", run as i32);
}

fn prize_text(tournament: &Tournament, place: usize) -> String {
    place.checked_sub(1).and_then(|index| tournament.prizes.get(index)).map(deeds::prize).unwrap_or_default()
}

fn write(w: &mut AfcWriter, tournament: &Tournament, run: Run, state: i32, character: &CharacterData) {
    let mine = get(character, tournament, "run") == run.index as i32 && get(character, tournament, "n") > 0;
    let id = i64::from(tournament.template) * 10_000_000 + run.index;
    w.int64(id).int32(tournament.template).int32(state).int32(tournament.base);
    w.double(f64::from(tournament.levels[0])).double(f64::from(tournament.levels[1])).double(0.0);
    w.int32(i32::from(mine));
    w.date_time(local(run.start)).date_time(local(run.start)).date_time(local(run.end));
    w.string("");
    for index in 0..4 {
        w.string(&prize_text(tournament, index + 1));
    }
    let task = BoardTask {
        id,
        kind: FISH,
        base: tournament.base,
        category: CATEGORY,
        task: tournament.task,
        species: tournament.species,
        bait: tournament.bait,
        target: tournament.places[0],
        min_grams: tournament.min_grams,
        max_grams: tournament.max_grams,
        progress: if mine { score(character, tournament) } else { 0 },
        ends_at: run.end * 60,
        ..Default::default()
    };
    write_task(w, &task);
}

/// The bars and the player, best first.
fn write_rows(w: &mut AfcWriter, tournament: &Tournament, character: &CharacterData) {
    let mine = score(character, tournament);
    let mut rows: Vec<(i64, i32, String)> = PLACES.iter().zip(tournament.places).map(|(name, bar)| (0, bar, (*name).to_string())).collect();
    rows.push((character.id, mine, character.name.clone()));
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    for (row, (id, value, name)) in rows.iter().enumerate() {
        let at = if *id == 0 { row as i32 + 1 } else { place(tournament, mine) as i32 };
        let prize = if *id == 0 { String::new() } else { prize_text(tournament, place(tournament, mine)) };
        w.int64(*id).int32(at).int32(*value).string(name).string(&prize);
    }
}

/// Pays the runs that have ended.
async fn settle(conn: &Conn, player: &mut Player) -> Result<()> {
    let world = conn.world.clone();
    let minute = now_minute();
    for tournament in &world.settings.quests.tournaments {
        let index = i64::from(get(&player.data, tournament, "run"));
        if get(&player.data, tournament, "n") == 0 || index == run_at(tournament, minute).index && minute < run_at(tournament, minute).end {
            continue;
        }
        let every = tournament.every.max(1);
        let start = index * every + tournament.offset;
        let run = Run { index, start, end: start + tournament.minutes.min(every) };
        let mine = score(&player.data, tournament);
        let at = place(tournament, mine);

        let mut w = AfcWriter::new();
        write(&mut w, tournament, run, CLOSED, &player.data);
        conn.send(msg::TOURNAMENT_END, w.finish()).await?;
        let mut w = AfcWriter::new();
        write(&mut w, tournament, run, CLOSED, &player.data);
        write_rows(&mut w, tournament, &player.data);
        conn.send(msg::TOURNAMENT_RESULT, w.finish()).await?;

        clear(&mut player.data, tournament, index);
        let said = match at.checked_sub(1).and_then(|index| tournament.prizes.get(index)) {
            Some(prize) => quests::give(conn, player, prize).await?,
            None => Vec::new(),
        };
        conn.save(player).await?;
        let place_text = if at <= 3 { format!("{at} место") } else { "участие".to_string() };
        chat::system(conn, &format!("Турнир окончен, счёт {mine}: {place_text}. {}", said.join(", "))).await?;
        conn.log(&format!("   -> 812, 818 tournament {} run {index}: score {mine}, place {at}", tournament.template));
    }
    Ok(())
}

/// Every frame: pays the runs that have ended.
pub async fn tick(session: &mut Session) -> Result<()> {
    let Some(player) = session.player.as_mut() else {
        return Ok(());
    };
    settle(&session.conn, player).await
}

/// The board's tab 1110: case 705, the tournaments now or next.
pub async fn tab(conn: &Conn, player: &mut Player) -> Result<Outcome> {
    settle(conn, player).await?;
    let minute = now_minute();
    let mut w = AfcWriter::new();
    let tournaments = &conn.world.settings.quests.tournaments;
    for tournament in tournaments {
        let now = run_at(tournament, minute);
        let (run, state) = if minute < now.end { (now, RUNNING) } else { (run_at(tournament, now.start + tournament.every.max(1)), PREPARING) };
        write(&mut w, tournament, run, state, &player.data);
    }
    conn.send(msg::TOURNAMENTS, w.finish()).await?;
    conn.log(&format!("   -> 705 {} tournament(s)", tournaments.len()));
    Ok(Outcome::Answered)
}

/// A fish has been landed: counts it in the tournament running at the base.
pub async fn landed(conn: &Conn, player: &mut Player, fish: &CaughtFish) -> Result<()> {
    let world = conn.world.clone();
    let minute = now_minute();
    let base = travel::current_base(&world.settings, player);
    for tournament in &world.settings.quests.tournaments {
        let run = run_at(tournament, minute);
        let level = player.data.level;
        let allowed = tournament.levels[1] == 0 || (tournament.levels[0]..=tournament.levels[1]).contains(&level);
        if tournament.base != base || minute >= run.end || !allowed || !fits(tournament, fish) {
            continue;
        }
        settle(conn, player).await?;
        if get(&player.data, tournament, "run") != run.index as i32 {
            clear(&mut player.data, tournament, run.index);
        }
        let first = get(&player.data, tournament, "n") == 0;
        count(&mut player.data, tournament, fish.grams);
        conn.save(player).await?;
        if first {
            let mut w = AfcWriter::new();
            write(&mut w, tournament, run, RUNNING, &player.data);
            conn.send(msg::TOURNAMENT_STARTED, w.finish()).await?;
        }
        let mut w = AfcWriter::new();
        write(&mut w, tournament, run, RUNNING, &player.data);
        write_rows(&mut w, tournament, &player.data);
        conn.send(msg::TOURNAMENT_TABLE, w.finish()).await?;
        conn.log(&format!("   -> 810 tournament {}: score {}", tournament.template, score(&player.data, tournament)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tournament(task: i32) -> Tournament {
        Tournament { template: task, base: 1, task, every: 60, minutes: 30, places: [30, 20, 10], ..Default::default() }
    }

    #[test]
    fn runs_follow_the_clock() {
        let hourly = Tournament { offset: 10, ..tournament(1) };
        assert_eq!(run_at(&hourly, 125), Run { index: 1, start: 70, end: 100 });
        assert_eq!(run_at(&hourly, 130), Run { index: 2, start: 130, end: 160 });
    }

    #[test]
    fn the_score_is_what_the_task_measures() {
        let mut character = CharacterData::default();
        for task in [1, 4, 6, 8, 10] {
            let one = tournament(task);
            for grams in [500, 900, 200, 700] {
                count(&mut character, &one, grams);
            }
        }
        assert_eq!(score(&character, &tournament(1)), 4);
        assert_eq!(score(&character, &tournament(4)), 2300);
        assert_eq!(score(&character, &tournament(6)), 2100);
        assert_eq!(score(&character, &tournament(8)), 900);
        assert_eq!(score(&character, &tournament(10)), 700);
    }

    #[test]
    fn places_are_bars() {
        let one = tournament(1);
        assert_eq!(place(&one, 0), 0);
        assert_eq!(place(&one, 5), 4);
        assert_eq!(place(&one, 20), 2);
        assert_eq!(place(&one, 45), 1);
    }
}
