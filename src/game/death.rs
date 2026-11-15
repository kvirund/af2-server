//! Смерть: жизнь дошла до нуля (нет воды, кончилась радстойкость).
//!
//! S→C 905 (пусто): the client says "Игрок умер...", plays the death sound
//! and after three seconds sends 905 (int 0) back to come to life again. By
//! the developer's words in the Steam discussions, a death takes part of the
//! experience gathered (never a level) and all the catch in the keepnet, and
//! no items. How much experience and what the character comes back with are
//! `vitals.deathExperiencePercent` and `vitals.respawnPercent`.

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, character, chat, keepnet, titles, travel, vitals};
use crate::afc::AfcWriter;
use crate::config::{Catalog, Settings};
use crate::messages as msg;
use crate::storage::{CharacterData, Vitals};

/// Seconds in play since the last death (or since this was counted).
pub const ALIVE_KEY: &str = "alive_seconds";
const BASE_LOCATION: i32 = 0;

/// The experience of the current level a death takes; the level stays.
fn experience_lost(experience: i64, percent: f64) -> i64 {
    (experience.max(0) as f64 * percent / 100.0).round() as i64
}

/// What every scale comes back at: a share of its maximum.
fn respawn_values(limits: &[f32; 5], percent: f32) -> [f32; 5] {
    limits.map(|max| max * percent / 100.0)
}

/// Puts a dead character's scales back, with no loss: for one that left the
/// game before coming to life again.
pub fn revive(settings: &Settings, catalog: &Catalog, character: &mut CharacterData) {
    let limits = vitals::limits(settings, catalog, character);
    let values = respawn_values(&limits, settings.vitals.respawn_percent);
    let vitals = character.vitals.get_or_insert(Vitals { values, work_points: 0.0 });
    vitals.values = values;
}

pub fn is_dead(character: &CharacterData) -> bool {
    character.vitals.as_ref().is_some_and(|vitals| vitals.values[0] <= 0.0)
}

/// Life has run out: the losses, then 905.
pub async fn die(conn: &Conn, player: &mut Player) -> Result<()> {
    player.dead = true;
    let percent = conn.world.settings.vitals.death_experience_percent;
    let lost = experience_lost(player.data.experience, percent);
    player.data.experience -= lost;
    let gone: Vec<i64> = player.data.fish.iter().filter(|fish| keepnet::in_keepnet(fish)).map(|fish| fish.id).collect();
    player.data.fish.retain(|fish| !keepnet::in_keepnet(fish));
    player.landed.clear();
    player.rod_kinds.clear();
    player.bites_by_rod.clear();
    titles::count(player, "death", 1);
    player.data.quest_values.insert(ALIVE_KEY.into(), 0);
    conn.save(player).await?;

    for id in &gone {
        let mut w = AfcWriter::new();
        w.int64(*id);
        conn.send(msg::FISH_REMOVE, w.finish()).await?;
    }
    character::send_update(conn, player).await?;
    conn.send(msg::DEATH, Vec::new()).await?;
    chat::system(conn, &format!("Смерть: потеряно {lost} опыта и {} рыб из садка.", gone.len())).await?;
    conn.log(&format!("   -> 905 died: -{lost} exp, {} fish from the keepnet", gone.len()));
    Ok(())
}

/// Client 905 (int 0), three seconds after the death: back at the base.
pub async fn respawn(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if !player.dead {
        conn.log("   905 while alive");
        return Ok(Outcome::Unanswered);
    }
    player.dead = false;
    let world = conn.world.clone();
    revive(&world.settings, &world.catalog, &mut player.data);
    let base = travel::current_base(&world.settings, player);
    conn.save(player).await?;
    travel::teleport(conn, player, base, BASE_LOCATION).await?;
    vitals::send(conn, player).await?;
    conn.log(&format!("   came back to life at base {base}"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_death_takes_a_share_of_the_level_experience() {
        assert_eq!(experience_lost(1000, 10.0), 100);
        assert_eq!(experience_lost(0, 10.0), 0);
        assert_eq!(experience_lost(-5, 10.0), 0);
    }

    #[test]
    fn the_scales_come_back_at_a_share() {
        assert_eq!(respawn_values(&[100.0, 50.0, 120.0, 80.0, 60.0], 50.0), [50.0, 25.0, 60.0, 40.0, 30.0]);
    }

    #[test]
    fn a_character_is_dead_at_zero_life() {
        let mut character = CharacterData::default();
        assert!(!is_dead(&character));
        character.vitals = Some(Vitals { values: [0.0, 10.0, 10.0, 10.0, 0.0], work_points: 0.0 });
        assert!(is_dead(&character));
    }
}
