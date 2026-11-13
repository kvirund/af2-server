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

use super::{Bite, Outcome, Session, active, cards, character, keepnet, inventory, loot, reputation, vitals, perks};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{Catalog, FishTable, FishingSettings, ProgressionSettings, Species};
use crate::messages as msg;
use crate::storage::{CaughtFish, ItemEntry, Record, UNKNOWN_BAIT};

pub const DEFAULT_LOCATION: i32 = 4;

const TROPHY_CLASS: i32 = 2;
const LARGE_CLASS: i32 = 1;

/// Rod, two three-double vectors.
const DEPTH_OFFSET: usize = 4 + 24 + 24;
/// Smallest fish, in grams, for each bait size class: the second number of the
/// item's `basePar`. The client dips the float once for anything lighter, with
/// no bite to set the hook on. Other classes have no minimum.
const BAIT_MIN_GRAMS: [f64; 5] = [0.0, 500.0, 3000.0, 7000.0, 20000.0];
/// Cells of a rod's bag holding the rod, reel and line.
const TACKLE_CELLS: [i32; 3] = [1, 2, 3];
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
    /// What the tackle holds; infinite without a limit.
    max_grams: f64,
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
            if f64::from(species.small_grams) > conditions.max_grams {
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

/// The first number of an item's `basePar`: the test of a rod, reel or line, in kg.
fn test_kg(xml: &str) -> Option<f64> {
    xml.split_once("<basePar>")?.1.split(['/', '<']).next()?.trim().parse().ok()
}

/// The heaviest fish the rod's tackle holds: the weakest of its rod, reel and
/// line tests times the factor. The bait tells which rod's bag to look in.
fn tackle_limit(catalog: &Catalog, items: &[ItemEntry], bait_item: i64, factor: f64) -> f64 {
    let Some(bag) = items.iter().find(|item| item.id == bait_item).map(|item| item.bag) else {
        return f64::INFINITY;
    };
    if factor <= 0.0 {
        return f64::INFINITY;
    }
    let mut weakest = f64::INFINITY;
    for cell in TACKLE_CELLS {
        let strongest = items
            .iter()
            .filter(|item| item.bag == bag && item.cell == cell)
            .filter_map(|item| catalog.get(item.template).and_then(|found| test_kg(&found.xml)))
            .filter(|test| *test > 0.0)
            .fold(None, |best: Option<f64>, test| Some(best.map_or(test, |best| best.max(test))));
        if let Some(test) = strongest {
            weakest = weakest.min(test);
        }
    }
    weakest * factor * 1000.0
}

fn size_class(species: &Species, grams: i32) -> i32 {
    if grams >= species.trophy_grams {
        TROPHY_CLASS
    } else if grams >= species.large_grams {
        LARGE_CLASS
    } else {
        0
    }
}

/// A fish too heavy for the tackle comes up again within what the tackle holds.
fn fit_to_tackle(species: &Species, rolled: (i32, i32), min_grams: f64, max_grams: f64, point: f64) -> (i32, i32) {
    if f64::from(rolled.0) <= max_grams {
        return rolled;
    }
    let lightest = f64::from(species.small_grams).max(min_grams).min(max_grams);
    let grams = lerp(lightest, max_grams, point);
    (grams, size_class(species, grams))
}

fn bait_min_grams(xml: &str) -> f64 {
    let class = xml
        .split_once("<basePar>")
        .and_then(|(_, rest)| rest.split('/').nth(1))
        .and_then(|class| class.parse::<usize>().ok());
    class.and_then(|class| BAIT_MIN_GRAMS.get(class).copied()).unwrap_or(0.0)
}

/// The bait's attractiveness, the first number of its `basePar`, as a share of
/// the normal 100.
fn attractiveness(xml: &str) -> f64 {
    xml.split_once("<basePar>")
        .and_then(|(_, rest)| rest.split(['/', '<']).next())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .map_or(1.0, |value| value / 100.0)
}

/// The bite chance with the bait's attractiveness applied.
fn attracted(settings: &FishingSettings, chance: f64, attractiveness: f64) -> f64 {
    (chance * (1.0 + settings.bait_attraction * (attractiveness - 1.0))).clamp(0.0, 1.0)
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
    if bait.is_none() {
        // The client keeps the last piece on the rod after the server used it up.
        conn.log(&format!("   no bait: item {bait_id} is gone"));
        let mut w = AfcWriter::new();
        w.boolean(false).single(0.0);
        conn.send(msg::BITE, w.finish()).await?;
        return Ok(Outcome::Answered);
    }
    let min_grams = if random::<f64>() < settings.small_fish_nibble_share {
        0.0
    } else {
        bait.and_then(|template| world.catalog.get(template)).map_or(0.0, |item| bait_min_grams(&item.xml))
    };
    let max_grams = tackle_limit(&world.catalog, &player.data.items, bait_id, settings.tackle_weight_factor);
    let conditions = Conditions { bait, min_grams, max_grams, depth, bottom, night: is_night(settings) };
    let mut options = candidates(&world.fish, settings, player.location, &conditions);
    for (species, weight) in &mut options {
        *weight *= cards::bite_factor(&world.settings.mastery, &player.data.catches, species.id);
    }
    let total: f64 = options.iter().map(|(_, weight)| weight).sum();
    let bait_item = bait.and_then(|template| world.catalog.get(template));
    let chance = settings.bite_chance * (total / settings.full_bite_weight).min(1.0);
    let chance = attracted(settings, chance, bait_item.map_or(1.0, |item| attractiveness(&item.xml)));

    let mut w = AfcWriter::new();
    if options.is_empty() || random::<f64>() >= chance {
        w.boolean(false).single((chance * 100.0) as f32);
        conn.send(msg::BITE, w.finish()).await?;
        return Ok(Outcome::Answered);
    }

    let species = pick(&options, total, random());
    let luck_percent = world.settings.reputation.per_level.luck_percent;
    let luck = perks::effects(conn, player).trophy * reputation::base_bonus(&world.settings, player, luck_percent);
    let rolled = roll_weight(settings, species, depth, min_grams, luck, random(), random());
    let (grams, size_class) = fit_to_tackle(species, rolled, min_grams, max_grams, random());
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

    let per_level = &conn.world.settings.reputation.per_level;
    let education = perks::effects(conn, player).experience * reputation::base_bonus(&conn.world.settings, player, per_level.experience_percent);
    let fish = CaughtFish {
        id: conn.world.store.next_id().await?,
        species: bite.species,
        grams: bite.grams,
        size_class: bite.size_class,
        bait: bite.bait,
        list: keepnet::IN_USE_LIST,
        experience: experience_for(progression, bite.grams, bite.size_class, education),
    };
    let is_record = bite.grams > player.data.records.get(&bite.species).map_or(0, |record| record.grams);
    let shown = if is_record { fish.experience * progression.record_experience_factor } else { fish.experience };

    let mut w = AfcWriter::new();
    w.int32(rod);
    keepnet::write_fish(&mut w, &fish, &conn.world.settings.fishing.mutant_species);
    w.int32(shown).int32(0).single(0.0);
    conn.send(msg::FISH_LANDED, w.finish()).await?;

    conn.log(&format!("   -> 1130 landed species {} {} g as fish {}, worth {shown} exp", fish.species, fish.grams, fish.id));
    let effort = &conn.world.settings.vitals;
    vitals::tire(player, effort.stamina_per_fish + effort.stamina_per_kg * fish.grams as f32 / 1000.0);
    vitals::reward_catch(effort, player, fish.grams);
    let catches = player.data.catches.entry(fish.species).or_default();
    catches.count += 1;
    catches.grams += i64::from(fish.grams);
    let grams = fish.grams;
    player.landed.insert(fish.id, fish);
    reputation::fish_landed(conn, player, bite.bait_item, grams).await?;
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

    let is_record = fish.grams > player.data.records.get(&fish.species).map_or(0, |record| record.grams);
    if is_record {
        let record = Record { grams: fish.grams, location: player.location, bait: fish.bait };
        player.data.records.insert(fish.species, record);
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
            bait_attraction: 1.0,
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
            tackle_weight_factor: 1.5,
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
        let conditions = |min_grams| Conditions { bait: Some(1010), min_grams, max_grams: f64::INFINITY, depth: 50.0, bottom: 100.0, night: false };
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
        let conditions = Conditions { bait: Some(7), min_grams: 0.0, max_grams: f64::INFINITY, depth: 50.0, bottom: 100.0, night: false };
        assert!(candidates(&table, &settings(), 4, &conditions).is_empty());
        let conditions = Conditions { bait: Some(1010), min_grams: 0.0, max_grams: f64::INFINITY, depth: 50.0, bottom: 100.0, night: false };
        assert_eq!(candidates(&table, &settings(), 4, &conditions).len(), 1);
    }

    fn gear(id: i32, test: &str) -> crate::config::CatalogItem {
        crate::config::CatalogItem {
            id,
            kind: 1,
            cost: 0,
            durability: 1.0,
            stacks: false,
            shop_count: 1,
            max_count: 1,
            xml: format!("<basePar>{test}</basePar>"),
        }
    }

    fn held(id: i64, bag: i32, cell: i32, template: i32) -> ItemEntry {
        ItemEntry { id, bag, cell, count: 1, durability: 1.0, template }
    }

    #[test]
    fn the_weakest_part_of_the_tackle_limits_the_fish() {
        let catalog = Catalog::new(vec![gear(1, "1.5/2.2/150"), gear(2, "1/0/1"), gear(3, "2/0.15/30"), gear(4, "100/1/0"), gear(5, "6/0/5")]);
        let mut items = vec![held(10, 100, 1, 1), held(11, 100, 2, 2), held(12, 100, 3, 3), held(13, 100, 20, 4)];
        assert_eq!(tackle_limit(&catalog, &items, 13, 1.5), 1500.0);
        assert_eq!(tackle_limit(&catalog, &items, 13, 0.0), f64::INFINITY);
        assert_eq!(tackle_limit(&catalog, &items, 99, 1.5), f64::INFINITY);
        items[1].template = 5;
        assert_eq!(tackle_limit(&catalog, &items, 13, 1.5), 2250.0);
    }

    #[test]
    fn a_fish_too_heavy_for_the_tackle_comes_up_lighter() {
        let species = species();
        assert_eq!(fit_to_tackle(&species, (800, 0), 0.0, 1500.0, 0.5), (800, 0));
        assert_eq!(fit_to_tackle(&species, (4000, LARGE_CLASS), 0.0, 1500.0, 1.0), (1500, LARGE_CLASS));
        assert_eq!(fit_to_tackle(&species, (4000, LARGE_CLASS), 0.0, 1500.0, 0.0), (100, 0));
    }

    #[test]
    fn species_too_heavy_for_the_tackle_do_not_bite() {
        let table = FishTable::for_tests(vec![species()], 4);
        let conditions = Conditions { bait: Some(1010), min_grams: 0.0, max_grams: 50.0, depth: 50.0, bottom: 100.0, night: false };
        assert!(candidates(&table, &settings(), 4, &conditions).is_empty());
    }

    #[test]
    fn attractive_bait_bites_more_often() {
        assert_eq!(attractiveness("<basePar>120/1/0</basePar>"), 1.2);
        assert_eq!(attractiveness("<basePar>90</basePar>"), 0.9);
        assert_eq!(attractiveness("<weight>1</weight>"), 1.0);
        let mut settings = settings();
        assert!((attracted(&settings, 0.3, 1.2) - 0.36).abs() < 1e-9);
        assert!((attracted(&settings, 0.3, 0.9) - 0.27).abs() < 1e-9);
        assert_eq!(attracted(&settings, 0.9, 1.5), 1.0);
        settings.bait_attraction = 0.0;
        assert_eq!(attracted(&settings, 0.3, 1.5), 0.3);
    }
}
