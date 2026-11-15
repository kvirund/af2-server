//! Доктор Григорий: heals once an hour for a fish, up to a level, and lets the
//! character use his fridge for biomass, then sells fridge cards for mutomass.
//!
//! The window is the client's quest dialog. Case 70 loads `quests/{file}.lng`
//! and shows its text `n{node}`, each case 72 adds the answer `a{id}`, and case
//! 74 opens the window. A click sends 65 with the answer and the long that came
//! with it, and locks the buttons until the next 74 or 76.

use anyhow::Result;
use chrono::{Local, NaiveDate, NaiveDateTime, TimeDelta};

use super::dialog::{COUNT_MARK, close, open, option, replace, text};
use super::{Conn, Outcome, Player, Session, active, fridge, inventory, keepnet, travel, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{DoctorSettings, Price};
use crate::messages as msg;
use crate::storage::CaughtFish;

const GREETING: i32 = 1;
const TERMS: i32 = 2;
const WANTS_BIOMASS: i32 = 3;
const CARD: i32 = 4;
const WANTS_MUTOMASS: i32 = 5;

const DECLINE: i32 = 1;
const ASK: i32 = 2;
const GIVE_FISH: i32 = 3;
const UNDERSTOOD: i32 = 4;
const ASK_STORE: i32 = 5;
const ACCEPTED: i32 = 6;
const GIVE_BIOMASS: i32 = 7;
const THANKS: i32 = 8;
const ASK_MORE: i32 = 9;
const GIVE_MUTOMASS: i32 = 10;

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
pub(super) fn dotnet_ticks(time: NaiveDateTime) -> i64 {
    let origin = NaiveDate::from_ymd_opt(1, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default();
    (time - origin).num_seconds() * 10_000_000
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

/// Nodes 3 and 5: what the doctor wants, how much the character carries, and
/// the answer that gives it when there is enough.
async fn wants(conn: &Conn, player: &mut Player, doctor: &DoctorSettings, node: i32, price: &Price, give: i32) -> Result<()> {
    let carried = inventory::pieces_carried(&player.data, price.item);
    text(conn, doctor.npc, node).await?;
    replace(conn, COUNT_MARK, &carried.to_string()).await?;
    if carried >= price.count {
        option(conn, doctor.npc, node, give, 0).await?;
    }
    option(conn, doctor.npc, node, UNDERSTOOD, 0).await?;
    open(conn, player, doctor.npc).await
}

/// Takes the price, hands over a fridge card and shows node 4 with `reply`.
/// `false` if the character no longer has enough or has no room for the card.
async fn sell_card(conn: &Conn, player: &mut Player, doctor: &DoctorSettings, price: &Price, reply: i32) -> Result<bool> {
    let settings = &conn.world.settings;
    let Some(card) = conn.world.catalog.get(settings.fridge.card) else {
        conn.log(&format!("   fridge card {} is not in the catalog", settings.fridge.card));
        return Ok(false);
    };
    if let Err(error) = inventory::plan_pieces(&settings.bags, &player.data, card, 1) {
        conn.log(&format!("   no room for the fridge card: {}", error.status()));
        return Ok(false);
    }
    let Some((smaller, gone)) = inventory::take_carried(&mut player.data, price.item, price.count) else {
        return Ok(false);
    };
    let (mut changed, gained) = give_gifts(conn, player, &[conn.world.settings.fridge.card]).await?;
    changed.extend(smaller);
    conn.save(player).await?;
    for id in gone {
        inventory::send_removed(conn, id).await?;
    }
    inventory::send_updates(conn, player.data.id, &changed).await?;
    if !gained.is_empty() {
        conn.send(msg::ITEMS_GAINED, gained).await?;
    }
    text(conn, doctor.npc, CARD).await?;
    option(conn, doctor.npc, CARD, reply, 0).await?;
    open(conn, player, doctor.npc).await?;
    Ok(true)
}

/// One piece of each gift: the changed cells and the case 125 that announces them.
async fn give_gifts(conn: &Conn, player: &mut Player, gifts: &[i32]) -> Result<(Vec<crate::storage::ItemEntry>, Vec<u8>)> {
    let mut changed = Vec::new();
    let mut gained = AfcWriter::new();
    let mut any = false;
    for template in gifts {
        let Some(item) = conn.world.catalog.get(*template) else {
            conn.log(&format!("   doctor's gift {template} is not in the catalog"));
            continue;
        };
        match inventory::give(conn, player, item, 1).await? {
            Ok(entries) => {
                changed.extend(entries);
                gained.int32(*template).int32(1).int32(0);
                any = true;
            }
            Err(error) => conn.log(&format!("   doctor's gift {template} lost: {}", error.status())),
        }
    }
    Ok((changed, if any { gained.finish() } else { Vec::new() }))
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
    let fridge = if fridge::quest_value(&player.data, fridge::DOCTOR_ALLOWS) > 0 { ASK_MORE } else { ASK_STORE };
    option(conn, doctor.npc, GREETING, fridge, 0).await?;
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
                    let gifts = give_gifts(conn, player, &doctor.gifts).await?;
                    conn.save(player).await?;
                    inventory::send_updates(conn, player.data.id, &gifts.0).await?;
                    if !gifts.1.is_empty() {
                        conn.send(msg::ITEMS_GAINED, gifts.1).await?;
                    }
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
        ASK_STORE => wants(conn, player, doctor, WANTS_BIOMASS, &doctor.fridge_unlock, GIVE_BIOMASS).await?,
        ASK_MORE => wants(conn, player, doctor, WANTS_MUTOMASS, &doctor.fridge_card, GIVE_MUTOMASS).await?,
        GIVE_BIOMASS if fridge::quest_value(&player.data, fridge::DOCTOR_ALLOWS) == 0 => {
            player.data.quest_values.insert(fridge::DOCTOR_ALLOWS.into(), 1);
            if sell_card(conn, player, doctor, &doctor.fridge_unlock, ACCEPTED).await? {
                conn.log("   doctor opened his fridge for biomass");
            } else {
                player.data.quest_values.remove(fridge::DOCTOR_ALLOWS);
                conn.log("   doctor: biomass not taken");
                wants(conn, player, doctor, WANTS_BIOMASS, &doctor.fridge_unlock, GIVE_BIOMASS).await?;
            }
        }
        GIVE_MUTOMASS => {
            if sell_card(conn, player, doctor, &doctor.fridge_card, THANKS).await? {
                conn.log("   doctor gave a fridge card for mutomass");
            } else {
                conn.log("   doctor: mutomass not taken");
                wants(conn, player, doctor, WANTS_MUTOMASS, &doctor.fridge_card, GIVE_MUTOMASS).await?;
            }
        }
        _ => close(conn, player, file).await?,
    }
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fish(id: i64, grams: i32, list: i32) -> CaughtFish {
        CaughtFish { id, species: 1, grams, size_class: 0, bait: 0, list, experience: 0, traits: Vec::new() }
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
