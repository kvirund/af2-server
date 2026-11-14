//! The VIP patent. What it costs and gives is the client's own VIP window
//! (`viptxt`, `vipbuycost`): 350 ergs for 30 days; fish sell for 50% more,
//! loot is 10% more likely, a fish gives 50% more experience, and the skill on
//! study gets a point every two minutes online. The other lines of the window
//! (quest wait, title change, mail tax) are about things the server lacks.

use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Local, TimeDelta, Utc};

use super::{Conn, Outcome, Player, Session, active as in_play, character, skills};
use crate::afc::AfcWriter;
use crate::messages as msg;
use crate::storage::CharacterData;

/// The client's icon for VIP; it alone enables the market's "prices" and
/// "sell all" buttons.
const ICON: i32 = 20;
const BOUGHT: i32 = 1;
const NO_MONEY: i32 = -1;
const DAY_SECONDS: i64 = 24 * 60 * 60;
/// How far ahead the icon of a VIP set by hand says it ends; it never does.
const FOREVER_DAYS: i64 = 365;

fn now_seconds() -> i64 {
    Utc::now().timestamp()
}

fn active_at(character: &CharacterData, now: i64) -> bool {
    character.vip || character.vip_until > now
}

pub fn active(character: &CharacterData) -> bool {
    active_at(character, now_seconds())
}

/// 1 plus the percent for a VIP, 1 for everyone else.
pub fn multiplier(character: &CharacterData, percent: f64) -> f64 {
    if active(character) { 1.0 + percent / 100.0 } else { 1.0 }
}

/// What a VIP gets on top of `amount`.
pub fn bonus(character: &CharacterData, percent: f64, amount: f64) -> f64 {
    amount * (multiplier(character, percent) - 1.0)
}

/// A new patent starts now, a renewed one where the old one ends.
fn extended(until: i64, now: i64, days: i64) -> i64 {
    until.max(now) + days * DAY_SECONDS
}

/// Case 260: until the end int icon, 3 x double for its tooltip, end date, bool
/// never expires. The client takes the icon away by itself once the date passes.
pub async fn send_icon(conn: &Conn, character: &CharacterData) -> Result<()> {
    let ends = if character.vip {
        Local::now().naive_local() + TimeDelta::days(FOREVER_DAYS)
    } else {
        DateTime::from_timestamp(character.vip_until, 0).map_or_else(|| Local::now().naive_local(), |at| at.with_timezone(&Local).naive_local())
    };
    let mut w = AfcWriter::new();
    w.int32(ICON).double(0.0).double(0.0).double(0.0);
    w.date_time(ends);
    w.boolean(character.vip);
    conn.send(msg::ACTIVE_ICONS, w.finish()).await
}

/// Takes the ergs and lengthens the patent by `days`; `false` if the ergs are
/// short. Sends the new balance and the icon.
pub async fn grant(conn: &Conn, player: &mut Player, days: i64, premium: i64) -> Result<bool> {
    if days <= 0 || player.data.premium < premium {
        return Ok(false);
    }
    player.data.premium -= premium;
    player.data.vip_until = extended(player.data.vip_until, now_seconds(), days);
    conn.save(player).await?;
    character::send_money(conn, player, 0).await?;
    send_icon(conn, &player.data).await?;
    Ok(true)
}

/// Client 220 from the VIP window. Reply case 220: int 1 bought, -1 short of
/// ergs; the new balance comes as 21.
pub async fn buy(session: &mut Session) -> Result<Outcome> {
    let player = in_play(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings.vip;
    let mut w = AfcWriter::new();
    if !grant(conn, player, settings.days, settings.premium_cost).await? {
        w.int32(NO_MONEY);
        conn.send(msg::VIP_BUY, w.finish()).await?;
        conn.log(&format!("   -> 220 VIP: {} ergs, {} needed", player.data.premium, settings.premium_cost));
        return Ok(Outcome::Answered);
    }
    w.int32(BOUGHT);
    conn.send(msg::VIP_BUY, w.finish()).await?;
    conn.log(&format!("   -> 21, 260, 220 VIP bought until {}", player.data.vip_until));
    Ok(Outcome::Answered)
}

/// Skill experience for the time online, a whole point at a time.
pub async fn tick(session: &mut Session) -> Result<()> {
    let Some(player) = session.player.as_mut() else {
        return Ok(());
    };
    let minutes = player.last_vip_tick.elapsed().as_secs_f64() / 60.0;
    player.last_vip_tick = Instant::now();
    let settings = &session.conn.world.settings.vip;
    if !active(&player.data) || player.data.studying == 0 {
        player.vip_skill_points = 0.0;
        return Ok(());
    }
    player.vip_skill_points += minutes * settings.skill_points_per_minute;
    let whole = player.vip_skill_points.floor();
    if whole >= 1.0 {
        player.vip_skill_points -= whole;
        skills::give(&session.conn, player, whole).await?;
        session.conn.log(&format!("   VIP: +{whole} skill experience for time online"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patent_runs_out() {
        let mut character = CharacterData { vip_until: 1000, ..Default::default() };
        assert!(active_at(&character, 999));
        assert!(!active_at(&character, 1000));
        character.vip = true;
        assert!(active_at(&character, 5000));
    }

    #[test]
    fn a_renewal_adds_to_what_is_left() {
        assert_eq!(extended(0, 100, 30), 100 + 30 * DAY_SECONDS);
        assert_eq!(extended(500, 100, 1), 500 + DAY_SECONDS);
    }
}
