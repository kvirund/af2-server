//! Доктор Григорий: heals once an hour for a fish, up to a level.
//!
//! The window is the client's quest dialog. Case 70 loads `quests/{file}.lng`
//! and shows its text `n{node}`, each case 72 adds the answer `a{id}`, and case
//! 74 opens the window. A click sends 65 with the answer and the long that came
//! with it, and locks the buttons until the next 74 or 76.

use anyhow::Result;
use chrono::{Local, NaiveDate, NaiveDateTime, TimeDelta};

use super::{Conn, Outcome, Player, Session, active, keepnet, travel, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::DoctorSettings;
use crate::messages as msg;
use crate::storage::CaughtFish;

const GREETING: i32 = 1;
const TERMS: i32 = 2;

const DECLINE: i32 = 1;
const ASK: i32 = 2;
const GIVE_FISH: i32 = 3;
const UNDERSTOOD: i32 = 4;

/// The fish the doctor gets: the lightest one in the keepnet heavy enough.
fn payment(fish: &[CaughtFish], min_grams: i32) -> Option<&CaughtFish> {
    fish.iter()
        .filter(|fish| fish.list == keepnet::IN_USE_LIST && fish.grams > min_grams)
        .min_by_key(|fish| fish.grams)
}

/// Seconds until the doctor heals again.
fn wait_seconds(healed_at: i64, now: i64, cooldown_minutes: i64) -> i64 {
    if healed_at == 0 {
        return 0;
    }
    (healed_at + cooldown_minutes * 60 - now).max(0)
}

/// What `DateTime.FromBinary` reads for a local time: ticks of 100 ns since
/// the year 1, with the kind bits left at "unspecified".
fn dotnet_ticks(time: NaiveDateTime) -> i64 {
    let origin = NaiveDate::from_ymd_opt(1, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default();
    (time - origin).num_seconds() * 10_000_000
}

async fn option(conn: &Conn, file: i32, node: i32, answer: i32, param: i64) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(file).int32(node).int32(answer).int64(param);
    conn.send(msg::DIALOG_OPTION, w.finish()).await
}

async fn text(conn: &Conn, file: i32, node: i32) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(file).int32(node);
    conn.send(msg::DIALOG_TEXT, w.finish()).await
}

async fn open(conn: &Conn, player: &mut Player, file: i32) -> Result<()> {
    player.dialog = Some(file);
    let mut w = AfcWriter::new();
    w.int32(file);
    conn.send(msg::DIALOG_OPEN, w.finish()).await
}

async fn close(conn: &Conn, player: &mut Player, file: i32) -> Result<()> {
    player.dialog = None;
    let mut w = AfcWriter::new();
    w.int32(file);
    conn.send(msg::DIALOG_CLOSE, w.finish()).await
}

/// Node 2: the terms, when the next treatment is, and the fish to give if the
/// character may be healed now.
async fn terms(conn: &Conn, player: &mut Player, doctor: &DoctorSettings) -> Result<()> {
    let now = Local::now();
    let wait = wait_seconds(player.data.healed_at, now.timestamp(), doctor.cooldown_minutes);
    text(conn, doctor.npc, TERMS).await?;

    let mut w = AfcWriter::new();
    w.string("[STIME]").int32(((wait + 59) / 60) as i32);
    conn.send(msg::DIALOG_SPAN, w.finish()).await?;
    let mut w = AfcWriter::new();
    w.string("[DTIME]").int64(dotnet_ticks((now + TimeDelta::seconds(wait)).naive_local()));
    conn.send(msg::DIALOG_DATE, w.finish()).await?;

    let fish = payment(&player.data.fish, doctor.min_fish_grams);
    if player.data.level > doctor.max_level {
        conn.log(&format!("   doctor: level {} is above {}", player.data.level, doctor.max_level));
    } else if wait > 0 {
        conn.log(&format!("   doctor: next treatment in {wait} s"));
    } else if let Some(fish) = fish {
        option(conn, doctor.npc, TERMS, GIVE_FISH, fish.id).await?;
    } else {
        conn.log(&format!("   doctor: no fish over {} g in the keepnet", doctor.min_fish_grams));
    }
    option(conn, doctor.npc, TERMS, UNDERSTOOD, 0).await?;
    open(conn, player, doctor.npc).await
}

/// Client 60 for the doctor. `None` leaves the NPC to the others.
pub async fn talk(session: &mut Session, message: &AfcMessage) -> Result<Option<Outcome>> {
    let npc = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let doctor = &conn.world.settings.doctor;
    if npc != i64::from(doctor.npc) || travel::current_base(&conn.world.settings, player) != doctor.base {
        return Ok(None);
    }
    text(conn, doctor.npc, GREETING).await?;
    option(conn, doctor.npc, GREETING, ASK, 0).await?;
    option(conn, doctor.npc, GREETING, DECLINE, 0).await?;
    open(conn, player, doctor.npc).await?;
    conn.log("   -> 70/72/74 doctor's greeting");
    Ok(Some(Outcome::Answered))
}

/// Client 65: int answer, long what came with it (the fish for "Отдать").
pub async fn answer(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let answer = r.int32()?;
    let param = r.int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let doctor = &settings.doctor;

    let Some(file) = player.dialog else {
        conn.log(&format!("   dialog answer {answer} with no dialog open"));
        return Ok(Outcome::Unanswered);
    };
    if file != doctor.npc {
        conn.log(&format!("   dialog {file} answer {answer} is not handled; closing"));
        close(conn, player, file).await?;
        return Ok(Outcome::Unanswered);
    }

    match answer {
        ASK => {
            terms(conn, player, doctor).await?;
            conn.log("   -> doctor's terms");
        }
        GIVE_FISH => {
            let wait = wait_seconds(player.data.healed_at, Local::now().timestamp(), doctor.cooldown_minutes);
            let paid = player
                .data
                .fish
                .iter()
                .position(|fish| fish.id == param && fish.list == keepnet::IN_USE_LIST && fish.grams > doctor.min_fish_grams);
            match paid {
                Some(index) if wait == 0 && player.data.level <= doctor.max_level => {
                    let fish = player.data.fish.remove(index);
                    player.data.healed_at = Local::now().timestamp();
                    vitals::heal(settings, &conn.world.catalog, &mut player.data, doctor);
                    conn.save(player).await?;
                    let mut w = AfcWriter::new();
                    w.int64(fish.id);
                    conn.send(msg::FISH_REMOVE, w.finish()).await?;
                    vitals::send(conn, player).await?;
                    conn.log(&format!("   doctor healed for fish {} ({} g)", fish.id, fish.grams));
                }
                _ => conn.log(&format!("   doctor: fish {param} not accepted")),
            }
            terms(conn, player, doctor).await?;
        }
        _ => close(conn, player, file).await?,
    }
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fish(id: i64, grams: i32, list: i32) -> CaughtFish {
        CaughtFish { id, species: 1, grams, size_class: 0, bait: 0, list, experience: 0 }
    }

    #[test]
    fn the_doctor_takes_the_lightest_fish_over_the_weight() {
        let keepnet = [fish(1, 800, 0), fish(2, 2000, 0), fish(3, 900, 0), fish(4, 850, 1)];
        assert_eq!(payment(&keepnet, 800).map(|fish| fish.id), Some(3));
        assert_eq!(payment(&keepnet[..1], 800), None);
    }

    #[test]
    fn the_doctor_heals_once_an_hour() {
        assert_eq!(wait_seconds(0, 1_000_000, 60), 0);
        assert_eq!(wait_seconds(1_000_000, 1_000_600, 60), 3000);
        assert_eq!(wait_seconds(1_000_000, 1_004_000, 60), 0);
    }

    #[test]
    fn dates_go_to_the_client_as_dotnet_ticks() {
        let time = NaiveDate::from_ymd_opt(2001, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap();
        assert_eq!(dotnet_ticks(time), 631_139_040_000_000_000);
    }
}
