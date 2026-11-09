//! Bases, locations and the global map.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::Local;

use super::{Conn, Outcome, Player, Session, active, character};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::BaseSettings;
use crate::messages as msg;

const ANY_LEVEL: i32 = 1000;
const OPTION_RECORD_LEN: usize = 12;
/// Every base lists this location: the base itself.
const BASE_LOCATION: i32 = 0;

/// Case 145: the catalogue of bases, read until the payload ends.
pub async fn send_base_list(conn: &Conn) -> Result<()> {
    let mut w = AfcWriter::new();
    for id in conn.world.settings.bases.keys() {
        w.int32(*id).int32(0).int32(0).int32(0).int32(0).boolean(true).int32(0).int32(0);
    }
    conn.send(msg::BASE_LIST, w.finish()).await
}

/// Case 150: one base and its locations.
pub async fn send_base_details(conn: &Conn, base: i32) -> Result<()> {
    let Some(info) = conn.world.settings.bases.get(&base) else {
        anyhow::bail!("settings.json has no base {base}");
    };
    let mut w = AfcWriter::new();
    w.int32(base).boolean(true).int32(0).int32(0).int32(0).boolean(true);
    w.date_time(Local::now().naive_local());
    w.int32(info.locations.len() as i32);
    for location in &info.locations {
        w.int32(base).int32(*location);
        w.int32(0).int32(0).int32(0); // unknown, map x, map y
        w.boolean(true).boolean(true).int32(0).int32(0);
    }
    conn.send(msg::BASE_DETAILS, w.finish()).await
}

fn destination<'a>(conn: &'a Conn, base: i32, location: i32) -> Option<&'a BaseSettings> {
    conn.world.settings.bases.get(&base).filter(|info| info.locations.contains(&location))
}

fn level_allows(conn: &Conn, info: &BaseSettings, player: &Player) -> bool {
    !conn.world.settings.travel.enforce_levels || player.data.level >= info.min_level
}

/// Client: int base, int location, int point. 138 walks there; 137 arrives from
/// the global map and pays the fare.
pub async fn go(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (base, location, point) = (r.int32()?, r.int32()?, r.int32()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    let Some(info) = destination(conn, base, location) else {
        conn.log(&format!("   travel to base {base} location {location}: unknown destination"));
        return Ok(Outcome::Unanswered);
    };
    if !level_allows(conn, info, player) {
        conn.log(&format!("   travel to base {base} needs level {}, have {}", info.min_level, player.data.level));
        return Ok(Outcome::Unanswered);
    }

    let cost = info.travel_cost;
    if message.id == msg::TRAVEL_ARRIVE && cost > 0 && player.data.money >= cost {
        player.data.money -= cost;
        conn.save(player).await?;
        character::send_money(conn, player, -cost).await?;
        conn.log(&format!("   -> 21 travel to base {base} cost {cost}"));
    }

    // The client only knows the starting base's locations until told about others.
    if base != conn.world.settings.character.start_base {
        send_base_details(conn, base).await?;
    }

    player.location = location;

    let mut w = AfcWriter::new();
    w.int32(point);
    conn.send(msg::SPAWN_POINT, w.finish()).await?;

    let mut w = AfcWriter::new();
    w.int32(base).int32(location).int32(0).int32(0);
    conn.send(msg::GO_TO_LOCATION, w.finish()).await?;
    conn.log(&format!("   -> 158, 160 travel to base {base} location {location} point {point}"));
    Ok(Outcome::Answered)
}

/// Which base a fishing location belongs to.
pub(super) fn base_of(bases: &BTreeMap<i32, BaseSettings>, location: i32) -> Option<i32> {
    if location == BASE_LOCATION {
        return None;
    }
    bases.iter().find(|(_, info)| info.locations.contains(&location)).map(|(id, _)| *id)
}

/// Client: int 0, from the map's button for the player's own base. Reply: case
/// 150, which also resets the client's current location, then 151 to load it.
pub async fn return_to_base(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;

    let base = base_of(&settings.bases, player.location).unwrap_or(settings.character.start_base);
    player.location = BASE_LOCATION;
    send_base_details(conn, base).await?;
    conn.send(msg::LOAD_BASE, Vec::new()).await?;
    conn.log(&format!("   -> 150, 151 back to base {base}"));
    Ok(Outcome::Answered)
}

/// Client: triples of base, location, point. Reply case 134: until the end,
/// int base, int location, int min level, int max level, 3 x double cost (item,
/// count, money). The client then loads the global map.
pub async fn options(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let conn = &session.conn;
    let enforce = conn.world.settings.travel.enforce_levels;
    let mut r = message.reader();
    let mut w = AfcWriter::new();
    let mut count = 0;

    while r.remaining() >= OPTION_RECORD_LEN {
        let (base, location) = (r.int32()?, r.int32()?);
        r.int32()?;
        if let Some(info) = destination(conn, base, location) {
            w.int32(base).int32(location);
            w.int32(if enforce { info.min_level } else { 0 }).int32(ANY_LEVEL);
            w.double(0.0).double(0.0).double(info.travel_cost as f64);
            count += 1;
        }
    }

    if count == 0 {
        conn.log("   134: no known destination in the request");
        return Ok(Outcome::Unanswered);
    }
    conn.send(msg::TRAVEL_OPTIONS, w.finish()).await?;
    conn.log(&format!("   -> 134 map with {count} destination(s)"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(locations: Vec<i32>) -> BaseSettings {
        BaseSettings { min_level: 1, travel_cost: 0, locations }
    }

    #[test]
    fn a_location_leads_back_to_its_own_base() {
        let bases = BTreeMap::from([(1, base(vec![0, 1, 2])), (2, base(vec![0, 21]))]);
        assert_eq!(base_of(&bases, 21), Some(2));
        assert_eq!(base_of(&bases, BASE_LOCATION), None);
        assert_eq!(base_of(&bases, 99), None);
    }
}
