//! Timed effects that items switch on (`gu:addActiveEffectLUA` in their use
//! scripts). The client draws them from its `ActiveIcons.xml`: 20 is VIP,
//! 21 «Обучение +X%» (more experience for a fish). The character keeps the
//! effect's end and strength among its quest values. A second use does not add
//! up: the effect lasts until the later of the two ends, at the stronger force
//! (the record token's description: «эффекты не суммируются»).

use anyhow::Result;
use chrono::{DateTime, Local, Utc};

use super::{Conn, Player, vip};
use crate::afc::AfcWriter;
use crate::messages as msg;
use crate::storage::CharacterData;

/// «Обучение +[X]%»: X percent more experience for a fish.
pub const EDUCATION: i32 = 21;
/// The VIP icon; as an effect it is a short patent.
pub const VIP: i32 = 20;
/// Effects the server keeps; any other the scripts name is only logged.
const KEPT: [i32; 1] = [EDUCATION];

fn until_key(effect: i32) -> String {
    format!("effect_{effect}_until")
}

fn force_key(effect: i32) -> String {
    format!("effect_{effect}_force")
}

fn now_seconds() -> i64 {
    Utc::now().timestamp()
}

/// The force of an effect that has not run out, or `None`.
fn force_at(character: &CharacterData, effect: i32, now: i64) -> Option<i32> {
    let until = i64::from(character.quest_values.get(&until_key(effect)).copied().unwrap_or(0));
    (until > now).then(|| character.quest_values.get(&force_key(effect)).copied().unwrap_or(0))
}

/// 1 plus the percent of «Обучение» while it lasts.
pub fn education(character: &CharacterData) -> f64 {
    force_at(character, EDUCATION, now_seconds()).map_or(1.0, |percent| 1.0 + f64::from(percent) / 100.0)
}

fn start_at(character: &mut CharacterData, effect: i32, force: i32, seconds: i64, now: i64) {
    let force = force_at(character, effect, now).map_or(force, |old| old.max(force));
    let until = i64::from(character.quest_values.get(&until_key(effect)).copied().unwrap_or(0)).max(now + seconds);
    character.quest_values.insert(until_key(effect), until as i32);
    character.quest_values.insert(force_key(effect), force);
}

/// Case 260 for one kept effect: int icon, 3 x double for the tooltip's [X],
/// [Y], [Z], end date, bool never ends.
async fn send_icon(conn: &Conn, effect: i32, force: i32, until: i64) -> Result<()> {
    let ends = DateTime::from_timestamp(until, 0).map_or_else(|| Local::now().naive_local(), |at| at.with_timezone(&Local).naive_local());
    let mut w = AfcWriter::new();
    w.int32(effect).double(f64::from(force)).double(0.0).double(0.0);
    w.date_time(ends);
    w.boolean(false);
    conn.send(msg::ACTIVE_ICONS, w.finish()).await
}

/// The icons of the effects still running, when the character enters the game.
pub async fn send_icons(conn: &Conn, character: &CharacterData) -> Result<()> {
    let now = now_seconds();
    for effect in KEPT {
        if let Some(force) = force_at(character, effect, now) {
            let until = i64::from(character.quest_values.get(&until_key(effect)).copied().unwrap_or(0));
            send_icon(conn, effect, force, until).await?;
        }
    }
    Ok(())
}

/// Switches an effect on for `seconds`. Returns what to tell the player, or
/// `None` for an effect the server does not know.
pub async fn start(conn: &Conn, player: &mut Player, effect: i32, force: i32, seconds: i64) -> Result<Option<String>> {
    let minutes = seconds / 60;
    match effect {
        VIP => {
            let now = now_seconds();
            player.data.vip_until = player.data.vip_until.max(now + seconds);
            conn.save(player).await?;
            vip::send_icon(conn, &player.data).await?;
            Ok(Some(format!("VIP на {minutes} мин.")))
        }
        EDUCATION => {
            let now = now_seconds();
            start_at(&mut player.data, effect, force, seconds, now);
            conn.save(player).await?;
            let until = i64::from(player.data.quest_values[&until_key(effect)]);
            let force = player.data.quest_values[&force_key(effect)];
            send_icon(conn, effect, force, until).await?;
            Ok(Some(format!("«Обучение +{force}%» на {minutes} мин.")))
        }
        _ => {
            conn.log(&format!("   effect {effect} (force {force}, {seconds} s) is not kept by the server"));
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn education_runs_out() {
        let mut character = CharacterData::default();
        start_at(&mut character, EDUCATION, 50, 7200, 1000);
        assert_eq!(force_at(&character, EDUCATION, 1000), Some(50));
        assert_eq!(force_at(&character, EDUCATION, 8200), None);
    }

    #[test]
    fn a_second_use_does_not_add_up() {
        let mut character = CharacterData::default();
        start_at(&mut character, EDUCATION, 50, 7200, 1000);
        start_at(&mut character, EDUCATION, 20, 300, 2000);
        assert_eq!(force_at(&character, EDUCATION, 8000), Some(50));
        assert_eq!(force_at(&character, EDUCATION, 8200), None);
    }
}
