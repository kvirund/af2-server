//! The server side of fishing. The client asks for a bite every five seconds
//! while the bait is in the water (1120), fights the fish itself, and reports a
//! landed fish by rod index (1130).
//!
//! `config/fish.json` is rebuilt from archived player guides. Bite chance, the
//! mix of size classes and the fight parameters are recorded nowhere and are set
//! in `config/settings.json`.

use anyhow::{Context, Result};
use chrono::{Local, Timelike};
use rand::random;

use super::{Bite, Outcome, Session, active, character, keepnet, inventory, loot, vitals, perks};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{FishTable, FishingSettings, ProgressionSettings, Species};
use crate::messages as msg;
use crate::storage::{CaughtFish, UNKNOWN_BAIT};

pub const DEFAULT_LOCATION: i32 = 4;

const TROPHY_CLASS: i32 = 2;
const LARGE_CLASS: i32 = 1;

/// Rod, two three-double vectors.
const DEPTH_OFFSET: usize = 4 + 24 + 24;
/// Smallest fish, in grams, for each bait size class: the second number of the
/// item's `basePar`. The client dips the float once for anything lighter, with
/// no bite to set the hook on. Other classes have no minimum.
const BAIT_MIN_GRAMS: [f64; 5] = [0.0, 500.0, 3000.0, 7000.0, 20000.0];
const NIGHT_START_HOUR: f64 = 21.5;
const NIGHT_END_HOUR: f64 = 4.5;

pub(super) fn is_night(settings: &FishingSettings) -> bool {
    let now = Local::now().time();
    let hours = f64::from(now.num_seconds_from_midnight()) / 3600.0;
    let game_hour = (hours * settings.game_hours_per_real_hour) % 24.0;
    game_hour >= NIGHT_START_HOUR || game_hour < NIGHT_END_HOUR
}

struct Conditions {
    bait: Option<i32>,
    min_grams: f64,
    depth: f64,
    bottom: f64,
    night: bool,
}

fn candidates<'a>(
    table: &'a FishTable,
    settings: &FishingSettings,
    location: i32,
    conditions: &Conditions,
) -> Vec<(&'a Species, f64)> {
    let ids = table.by_location.get(&location).map_or(&[][..], Vec::as_slice);
    ids.iter()
        .filter_map(|id| table.get(*id))
        .filter_map(|species| {
            if let Some(bait) = conditions.bait
                && !species.baits.is_empty()
                && !species.baits.contains(&bait)
            {
                return None;
            }
            if f64::from(species.large_grams) < conditions.min_grams {
                return None;
            }
            if conditions.depth < f64::from(species.min_depth_cm) {
                return None;
            }
            if species.max_depth_cm > 0 && conditions.depth > f64::from(species.max_depth_cm) {
                return None;
            }

            let mut weight = species.share;
            if species.near_bottom {
                let close = conditions.bottom - conditions.depth <= settings.near_bottom_cm;
                weight *= if close { settings.near_bottom_bonus } else { settings.away_from_bottom_factor };
            }
            if species.likes_depth {
                let deep = conditions.bottom >= settings.trophy_depth_cm;
                weight *= if deep { settings.deep_water_bonus } else { settings.shallow_water_factor };
            }
            if conditions.night {
                weight *= species.night_factor;
            }
            (weight > 0.0).then_some((species, weight))
        })
        .collect()
}

fn pick<'a>(candidates: &[(&'a Species, f64)], total: f64, mut roll: f64) -> &'a Species {
    roll *= total;
    for (species, weight) in candidates {
        roll -= weight;
        if roll < 0.0 {
            return species;
        }
    }
    candidates[candidates.len() - 1].0
}

fn lerp(from: f64, to: f64, t: f64) -> i32 {
    (from + (to - from) * t).round() as i32
}

fn bait_min_grams(xml: &str) -> f64 {
    let class = xml
        .split_once("<basePar>")
        .and_then(|(_, rest)| rest.split('/').nth(1))
        .and_then(|class| class.parse::<usize>().ok());
    class.and_then(|class| BAIT_MIN_GRAMS.get(class).copied()).unwrap_or(0.0)
}

/// Returns grams and size class for two rolls in `0..1`.
fn roll_weight(settings: &FishingSettings, species: &Species, depth: f64, min_grams: f64, luck: f64, roll: f64, point: f64) -> (i32, i32) {
    let trophy_share = luck
        * if depth > settings.trophy_depth_cm {
            settings.trophy_share * settings.deep_trophy_multiplier
        } else {
            settings.trophy_share
        };
    let trophy = f64::from(species.trophy_grams);
    let large = f64::from(species.large_grams);
    let small = f64::from(species.small_grams).max(min_grams);

    if roll < trophy_share {
        (lerp(trophy, trophy * settings.top_weight_factor, point * point), TROPHY_CLASS)
    } else if roll < trophy_share + settings.large_share {
        (lerp(large, trophy - 1.0, point), LARGE_CLASS)
    } else {
        (lerp(small, large - 1.0, point), 0)
    }
}

/// Client: int rod, 2 x vec3 (doubles), float depth, float bottom, 3 x long bait
/// item id, 7 x bool, float distance.
///
/// Reply case 1120: bool bite. false is followed by float chance; true by the
/// fields of `Fisherman.klevAction`.
pub async fn bite(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let rod = r.int32()?;
    r.skip(DEPTH_OFFSET - 4)?;
    let depth = f64::from(r.single()?);
    let bottom = f64::from(r.single()?);
    let bait_id = r.int64()?;

    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = &conn.world;
    let settings = &world.settings.fishing;
    vitals::keep_busy(player);

    let bait = player.data.items.iter().find(|item| item.id == bait_id).map(|item| item.template);
    let min_grams = if random::<f64>() < settings.small_fish_nibble_share {
        0.0
    } else {
        bait.and_then(|template| world.catalog.get(template)).map_or(0.0, |item| bait_min_grams(&item.xml))
    };
    let conditions = Conditions { bait, min_grams, depth, bottom, night: is_night(settings) };
    let options = candidates(&world.fish, settings, player.location, &conditions);
    let total: f64 = options.iter().map(|(_, weight)| weight).sum();
    let chance = settings.bite_chance * (total / settings.full_bite_weight).min(1.0);

    let mut w = AfcWriter::new();
    if options.is_empty() || random::<f64>() >= chance {
        w.boolean(false).single((chance * 100.0) as f32);
        conn.send(msg::BITE, w.finish()).await?;
        return Ok(Outcome::Answered);
    }

    let species = pick(&options, total, random());
    let luck = perks::effects(conn, player).trophy;
    let (grams, size_class) = roll_weight(settings, species, depth, min_grams, luck, random(), random());
    player.bites_by_rod.insert(rod, Bite { species: species.id, grams, size_class, bait: bait.unwrap_or(UNKNOWN_BAIT), bait_item: bait_id });

    let small = f64::from(species.small_grams);
    let top = f64::from(species.trophy_grams) * settings.top_weight_factor;
    let size = ((f64::from(grams) - small) / (top - small)).clamp(0.0, 1.0);
    let weight = f64::from(grams);
    let hardiness = if species.hardy { settings.hardy_stamina_factor } else { 1.0 };
    let speed = (weight / settings.grams_per_speed).clamp(settings.min_speed, settings.max_speed);
    let stamina = (weight / settings.grams_per_stamina).clamp(settings.min_stamina, settings.max_stamina);

    w.boolean(true);
    w.int32(0); // read and discarded
    w.int32(grams);
    w.string(&settings.bite_behaviour);
    w.single(0.0); // shy-bite modifier
    w.single(speed as f32);
    w.single((stamina * hardiness) as f32);
    w.single(1.0); // no client use found
    w.single(1.0); // drain rate during the fight
    w.int32(species.id);
    w.single(size as f32); // between the model's smallest and largest
    w.int64(bait_id);
    conn.send(msg::BITE, w.finish()).await?;

    conn.log(&format!(
        "   -> 1120 BITE rod {rod} loc {} depth {depth:.0}/{bottom:.0} cm: {} {grams} g, class {size_class}",
        player.location, species.name
    ));
    Ok(Outcome::Answered)
}

fn experience_for(progression: &ProgressionSettings, grams: i32, size_class: i32, education: f64) -> i32 {
    let size = match size_class {
        TROPHY_CLASS => progression.trophy_experience_factor,
        LARGE_CLASS => progression.large_experience_factor,
        _ => 1.0,
    };
    let base = progression.experience_per_fish + progression.experience_per_kg * f64::from(grams) / 1000.0;
    ((base * size * education).round() as i32).max(1)
}

/// Client: int rod. Reply case 1130: int rod, the fish record, int shown
/// experience, int, float. The client puts the fish in its keepnet at once.
pub async fn landed(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let rod = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let progression = &conn.world.settings.progression;

    let Some(bite) = player.bites_by_rod.remove(&rod) else {
        conn.log(&format!("   fish landed on rod {rod} with no bite on record"));
        return Ok(Outcome::Unanswered);
    };

    let education = perks::effects(conn, player).experience;
    let fish = CaughtFish {
        id: conn.world.store.next_id().await?,
        species: bite.species,
        grams: bite.grams,
        size_class: bite.size_class,
        bait: bite.bait,
        list: keepnet::IN_USE_LIST,
        experience: experience_for(progression, bite.grams, bite.size_class, education),
    };
    let is_record = bite.grams > player.data.records.get(&bite.species).copied().unwrap_or(0);
    let shown = if is_record { fish.experience * progression.record_experience_factor } else { fish.experience };

    let mut w = AfcWriter::new();
    w.int32(rod);
    keepnet::write_fish(&mut w, &fish, &conn.world.settings.fishing.mutant_species);
    w.int32(shown).int32(0).single(0.0);
    conn.send(msg::FISH_LANDED, w.finish()).await?;

    conn.log(&format!("   -> 1130 landed species {} {} g as fish {}, worth {shown} exp", fish.species, fish.grams, fish.id));
    let effort = &conn.world.settings.vitals;
    vitals::tire(player, effort.stamina_per_fish + effort.stamina_per_kg * fish.grams as f32 / 1000.0);
    player.landed.insert(fish.id, fish);
    inventory::use_up(conn, player, bite.bait_item, conn.world.settings.fishing.bait_per_catch).await?;
    loot::after_catch(conn, player).await?;
    Ok(Outcome::Answered)
}

/// Client: long fish id, keeping it. Reply case 1135: int experience.
pub async fn kept(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    let Some(fish) = player.landed.remove(&id) else {
        conn.log(&format!("   kept unknown fish {id}"));
        return Ok(Outcome::Unanswered);
    };

    let is_record = fish.grams > player.data.records.get(&fish.species).copied().unwrap_or(0);
    if is_record {
        player.data.records.insert(fish.species, fish.grams);
        let mut w = AfcWriter::new();
        w.int32(fish.species).int32(fish.grams).int32(0);
        conn.send(msg::PERSONAL_RECORD, w.finish()).await?;
    }

    let progression = &conn.world.settings.progression;
    let multiplier = if is_record { progression.record_experience_factor } else { 1 };
    let experience = fish.experience;
    player.data.fish.push(fish);

    let mut w = AfcWriter::new();
    w.int32(experience.checked_mul(multiplier).context("experience overflow")?);
    conn.send(msg::FISH_KEPT, w.finish()).await?;

    character::award_experience(conn, player, experience, multiplier).await?;
    conn.log(&format!("   -> 1135 kept fish {id}, record: {is_record}"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> FishingSettings {
        FishingSettings {
            mutant_species: vec![],
            bite_chance: 1.0,
            bite_behaviour: String::new(),
            small_fish_nibble_share: 0.0,
            bait_per_catch: 1,
            full_bite_weight: 8.0,
            large_share: 0.3,
            trophy_share: 0.1,
            trophy_depth_cm: 300.0,
            deep_trophy_multiplier: 2.0,
            top_weight_factor: 1.5,
            near_bottom_cm: 40.0,
            near_bottom_bonus: 2.5,
            away_from_bottom_factor: 0.35,
            deep_water_bonus: 2.0,
            shallow_water_factor: 0.5,
            game_hours_per_real_hour: 12.0,
            grams_per_speed: 500.0,
            min_speed: 1.0,
            max_speed: 4.0,
            grams_per_stamina: 40.0,
            min_stamina: 8.0,
            max_stamina: 60.0,
            hardy_stamina_factor: 1.4,
        }
    }

    fn species() -> Species {
        Species {
            id: 1,
            name: "Карп".into(),
            small_grams: 100,
            large_grams: 1000,
            trophy_grams: 5000,
            min_depth_cm: 0,
            max_depth_cm: 300,
            near_bottom: false,
            likes_depth: false,
            night_factor: 1.0,
            hardy: false,
            share: 2.0,
            baits: vec![1010],
        }
    }

    #[test]
    fn luck_widens_the_trophy_share() {
        let (settings, species) = (settings(), species());
        assert_eq!(roll_weight(&settings, &species, 50.0, 0.0, 1.0, 0.15, 0.0).1, LARGE_CLASS);
        assert_eq!(roll_weight(&settings, &species, 50.0, 0.0, 2.0, 0.15, 0.0).1, TROPHY_CLASS);
    }

    #[test]
    fn rolls_land_in_their_size_class() {
        let (settings, species) = (settings(), species());
        assert_eq!(roll_weight(&settings, &species, 50.0, 0.0, 1.0, 0.05, 0.0), (5000, TROPHY_CLASS));
        assert_eq!(roll_weight(&settings, &species, 50.0, 0.0, 1.0, 0.2, 0.0), (1000, LARGE_CLASS));
        assert_eq!(roll_weight(&settings, &species, 50.0, 0.0, 1.0, 0.9, 0.0), (100, 0));
    }

    #[test]
    fn deep_water_doubles_trophies() {
        let (settings, species) = (settings(), species());
        assert_eq!(roll_weight(&settings, &species, 400.0, 0.0, 1.0, 0.15, 0.0).1, TROPHY_CLASS);
    }

    #[test]
    fn pick_follows_the_weights() {
        let (a, mut b) = (species(), species());
        b.id = 2;
        let options = vec![(&a, 1.0), (&b, 3.0)];
        assert_eq!(pick(&options, 4.0, 0.1).id, 1);
        assert_eq!(pick(&options, 4.0, 0.9).id, 2);
    }

    #[test]
    fn bait_size_class_comes_from_the_item() {
        assert_eq!(bait_min_grams("<base><basePar>150/1/0</basePar></base>"), 500.0);
        assert_eq!(bait_min_grams("<base><basePar>110/0/0</basePar></base>"), 0.0);
        assert_eq!(bait_min_grams("<base><basePar>105/1010/0</basePar></base>"), 0.0);
        assert_eq!(bait_min_grams("<base></base>"), 0.0);
    }

    #[test]
    fn a_big_bait_skips_fish_too_small_for_it() {
        let table = FishTable::for_tests(vec![species()], 4);
        let conditions = |min_grams| Conditions { bait: Some(1010), min_grams, depth: 50.0, bottom: 100.0, night: false };
        assert_eq!(candidates(&table, &settings(), 4, &conditions(1000.0)).len(), 1);
        assert!(candidates(&table, &settings(), 4, &conditions(1001.0)).is_empty());
    }

    #[test]
    fn a_rolled_weight_reaches_the_bait_minimum() {
        let (settings, species) = (settings(), species());
        assert_eq!(roll_weight(&settings, &species, 50.0, 500.0, 1.0, 0.9, 0.0), (500, 0));
    }

    #[test]
    fn wrong_bait_gets_no_bite() {
        let table = FishTable::for_tests(vec![species()], 4);
        let conditions = Conditions { bait: Some(7), min_grams: 0.0, depth: 50.0, bottom: 100.0, night: false };
        assert!(candidates(&table, &settings(), 4, &conditions).is_empty());
        let conditions = Conditions { bait: Some(1010), min_grams: 0.0, depth: 50.0, bottom: 100.0, night: false };
        assert_eq!(candidates(&table, &settings(), 4, &conditions).len(), 1);
    }
}
