//! Health, stamina, radiation resistance, food and water.

use std::time::Instant;

use anyhow::Result;

use std::collections::BTreeMap;

use super::{Conn, Outcome, Player, Session, active, death, fishing, fridge, inventory, perks, scripts, titles, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{DoctorSettings, ProgressionSettings, Settings, VitalsSettings};
use crate::messages as msg;
use crate::storage::{CharacterData, Vitals};
use crate::world::World;

const COUNT: usize = 5;
const LIFE: usize = 0;
const STAMINA: usize = 1;
const RADIATION: usize = 2;
const FOOD: usize = 3;
const WATER: usize = 4;
/// The client's order of the five: health, radiation resistance, stamina, food,
/// water (labels gi_uinf_5..9 of its character info, and the order case 1000 and
/// the character card are read in).
const CLIENT_ORDER: [usize; COUNT] = [LIFE, RADIATION, STAMINA, FOOD, WATER];

/// The five values in the order the client reads them.
pub fn for_client<T: Copy>(values: [T; COUNT]) -> [T; COUNT] {
    CLIENT_ORDER.map(|index| values[index])
}

const CLOTHING_BAG: i32 = 60;
const SUIT_KIND: i32 = 33;

const FOOD_KIND: i32 = 21;
const DRINK_KIND: i32 = 22;
const MEDICINE_KIND: i32 = 23;

pub fn maxima(progression: &ProgressionSettings, level: i32) -> [f32; COUNT] {
    let mut maxima = [progression.base_vital as f32; COUNT];
    for (max, per_level) in maxima.iter_mut().zip(&progression.vitals_per_level) {
        *max += ((level - 1) * per_level) as f32;
    }
    maxima
}

/// Skills that raise scales: Атлетика food, life and stamina, Сталкер radiation
/// resistance and water, each by its `val` per level.
const SCALE_SKILLS: [(i32, &[usize]); 2] = [(110, &[FOOD, LIFE, STAMINA]), (140, &[RADIATION, WATER])];

fn skill_bonus(settings: &Settings, levels: &BTreeMap<i32, i32>) -> [f32; COUNT] {
    let mut bonus = [0.0; COUNT];
    for (perk, scales) in SCALE_SKILLS {
        let per_level = settings.skill_tree.get(perk).map_or(0.0, |node| node.effect[0]);
        let gain = per_level * f64::from(levels.get(&perk).copied().unwrap_or(0));
        for scale in scales {
            bonus[*scale] += gain as f32;
        }
    }
    bonus
}

/// The character's maxima with what skills and gear add.
pub fn limits(settings: &Settings, catalog: &crate::config::Catalog, character: &CharacterData) -> [f32; COUNT] {
    let bonus = skill_bonus(settings, &perks::levels(settings, catalog, character));
    let mut limits = maxima(&settings.progression, character.level);
    for (max, extra) in limits.iter_mut().zip(bonus) {
        *max += extra;
    }
    limits
}

pub fn full(progression: &ProgressionSettings, level: i32) -> Vitals {
    Vitals { values: maxima(progression, level), work_points: 0.0 }
}

struct Passing {
    seconds: f32,
    /// Radiation and aggressive environment that get through the suit.
    radiation: f32,
    aggression: f32,
    resting: bool,
}

fn advance(settings: &VitalsSettings, maxima: &[f32; COUNT], vitals: &mut Vitals, passing: &Passing) {
    let seconds = passing.seconds;
    let minutes = seconds / 60.0;
    let values = &mut vitals.values;

    let hunger = 1.0 + (passing.radiation + passing.aggression) * settings.environment_hunger_percent / 100.0;
    let thirst = if values[FOOD] <= 0.0 { settings.no_food_water_factor } else { 1.0 };
    values[WATER] -= settings.water_per_minute * thirst * hunger * minutes;
    values[FOOD] -= settings.food_per_minute * hunger * minutes;
    values[RADIATION] -= passing.radiation * settings.radiation_per_minute_per_point * minutes;
    if values[WATER] <= 0.0 {
        values[LIFE] -= settings.no_water_life_per_second * seconds;
    }
    if values[RADIATION] <= 0.0 {
        values[LIFE] -= settings.no_resistance_life_per_second * seconds;
    }
    if passing.resting && values[FOOD] >= settings.rest_needs_food {
        values[STAMINA] += settings.rest_stamina_per_minute * minutes;
    }
    vitals.work_points = (vitals.work_points + settings.work_points_per_minute * minutes).min(settings.max_work_points);
    keep_in_range(maxima, vitals);
}

fn keep_in_range(maxima: &[f32; COUNT], vitals: &mut Vitals) {
    for (value, max) in vitals.values.iter_mut().zip(maxima) {
        *value = value.clamp(0.0, *max);
    }
}

/// Radiation and aggressive environment where the character stands: the top
/// of the base's range at night, the bottom by day, nothing inside a base.
fn background(conn: &Conn, location: i32) -> [f32; 2] {
    let settings = &conn.world.settings;
    let Some(base) = travel::base_of(&settings.bases, location) else {
        return [0.0; 2];
    };
    let night = fishing::is_night(&settings.fishing);
    let at = |table: &BTreeMap<i32, [f32; 2]>| table.get(&base).map_or(0.0, |[low, high]| if night { *high } else { *low });
    [at(&settings.vitals.radiation), at(&settings.vitals.aggression)]
}

/// The worn suit's protection from radiation and aggressive environment: the
/// first two numbers of its `basePar` (the client's «wpn_dress1», «wpn_dress2»).
pub fn protection(world: &World, character: &CharacterData) -> [f32; 2] {
    character
        .items
        .iter()
        .filter(|item| item.bag == CLOTHING_BAG && world.catalog.get(item.template).is_some_and(|template| template.kind == SUIT_KIND))
        .map(|item| scripts::base_par(&inventory::item_xml(world, item)))
        .fold([0.0; 2], |best, [radiation, aggression, _]| [best[0].max(radiation as f32), best[1].max(aggression as f32)])
}

/// What gets through a suit (guide 1102257736): all above its protection and
/// a third of the rest, «Р-Зщ=ПР, Зщ/3=ОР, ПР+ОР=ОУ; если Зщ больше Р, ОУ=Р/3».
pub fn felt(level: f32, protection: f32) -> f32 {
    (level - protection).max(0.0) + level.min(protection) / 3.0
}

/// Case 1001 when the location's numbers change. The weather in it stays
/// clear: the server has none of its own.
async fn show_environment(conn: &Conn, player: &mut Player, levels: [f32; 2]) -> Result<()> {
    let shown = levels.map(|level| level.round() as i32);
    if player.environment == Some(shown) {
        return Ok(());
    }
    player.environment = Some(shown);
    let mut w = AfcWriter::new();
    for _ in 0..5 {
        w.single(0.0);
    }
    w.int32(shown[0]).int32(shown[1]);
    conn.send(msg::WEATHER, w.finish()).await?;
    conn.log(&format!("   -> 1001 radiation {}, aggressive environment {}", shown[0], shown[1]));
    Ok(())
}

/// Runs off the client's regular traffic, so it stops when the client does.
pub async fn tick(session: &mut Session) -> Result<()> {
    let Some(player) = session.player.as_mut() else {
        return Ok(());
    };
    if player.dead {
        return Ok(());
    }
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let seconds = player.last_tick.elapsed().as_secs_f32();
    if seconds < settings.vitals.tick_seconds {
        return Ok(());
    }
    player.last_tick = Instant::now();

    let levels = background(conn, player.location);
    let [radiation, aggression] = protection(&conn.world, &player.data);
    let passing = Passing {
        seconds,
        radiation: felt(levels[0], radiation),
        aggression: felt(levels[1], aggression),
        resting: player.last_active.elapsed().as_secs_f32() >= settings.vitals.rest_after_seconds,
    };
    show_environment(conn, player, levels).await?;
    let limits = limits(settings, &conn.world.catalog, &player.data);
    if let Some(vitals) = player.data.vitals.as_mut() {
        advance(&settings.vitals, &limits, vitals, &passing);
    }
    send(conn, player).await?;
    if death::is_dead(&player.data) {
        return death::die(conn, player).await;
    }

    if player.last_save.elapsed().as_secs_f32() >= settings.vitals.save_every_seconds {
        player.last_save = Instant::now();
        conn.save(player).await?;
    }
    Ok(())
}

/// Case 1000: five floats, an unused vector, work points, an unused float.
pub async fn send(conn: &Conn, player: &Player) -> Result<()> {
    let Some(vitals) = &player.data.vitals else {
        return Ok(());
    };
    let mut w = AfcWriter::new();
    for value in for_client(vitals.values) {
        w.single(value);
    }
    w.double(0.0).double(0.0).double(0.0).single(vitals.work_points).single(0.0);
    conn.send(msg::VITALS, w.finish()).await
}

/// The client asks about bites over and over while the bait is in the water,
/// so this interrupts resting but costs nothing.
pub fn keep_busy(player: &mut Player) {
    player.last_active = Instant::now();
}

pub fn tire(player: &mut Player, amount: f32) {
    keep_busy(player);
    if let Some(vitals) = player.data.vitals.as_mut() {
        vitals.values[STAMINA] = (vitals.values[STAMINA] - amount).max(0.0);
    }
}

/// The doctor's treatment, within the character's limits.
pub fn heal(settings: &Settings, catalog: &crate::config::Catalog, character: &mut CharacterData, doctor: &DoctorSettings) {
    let limits = limits(settings, catalog, character);
    if let Some(vitals) = character.vitals.as_mut() {
        vitals.values[LIFE] += doctor.life;
        vitals.values[RADIATION] += doctor.radiation;
        vitals.values[FOOD] += doctor.food;
        vitals.values[WATER] += doctor.water;
        keep_in_range(&limits, vitals);
    }
}

/// Work points for a landed fish, up to the most a character holds.
pub fn reward_catch(settings: &VitalsSettings, player: &mut Player, grams: i32) {
    if let Some(vitals) = player.data.vitals.as_mut() {
        let gained = settings.work_points_per_kg * (grams / 1000) as f32;
        vitals.work_points = (vitals.work_points + gained).min(settings.max_work_points.max(vitals.work_points));
    }
}

/// What each of the three `basePar` numbers changes, as the item's tooltip says.
fn targets(kind: i32) -> Option<[usize; 3]> {
    match kind {
        FOOD_KIND => Some([FOOD, WATER, RADIATION]),
        DRINK_KIND => Some([FOOD, WATER, STAMINA]),
        MEDICINE_KIND => Some([LIFE, RADIATION, STAMINA]),
        _ => None,
    }
}

fn effect(kind: i32, xml: &str) -> Option<[(usize, f32); 3]> {
    let targets = targets(kind)?;
    let numbers = xml.split_once("<basePar>")?.1.split('<').next()?;
    let mut numbers = numbers.split('/').map(|number| number.parse::<f32>().unwrap_or(0.0));
    Some(targets.map(|target| (target, numbers.next().unwrap_or(0.0))))
}

/// Client: long item. It takes the piece away itself and expects no reply, only
/// the new values.
pub async fn use_item(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;

    let Some(template) = player.data.items.iter().find(|item| item.id == id).map(|item| item.template) else {
        conn.log(&format!("   use: character has no item {id}"));
        return Ok(Outcome::Unanswered);
    };
    if template == settings.fridge.card {
        let base = fridge::use_card(settings, player);
        inventory::remove_one(&mut player.data, id);
        conn.save(player).await?;
        conn.log(&format!("   fridge card used at base {base}"));
        return Ok(Outcome::Answered);
    }
    let kind = conn.world.catalog.get(template).map_or(0, |item| item.kind);
    let found = conn.world.catalog.get(template).and_then(|item| effect(item.kind, &item.xml));
    let Some(changes) = found else {
        conn.log(&format!("   use: item {template} is not food or medicine"));
        return Ok(Outcome::Unanswered);
    };

    let limits = limits(settings, &conn.world.catalog, &player.data);
    if let Some(vitals) = player.data.vitals.as_mut() {
        for (target, change) in changes {
            vitals.values[target] += change;
        }
        keep_in_range(&limits, vitals);
    }
    inventory::remove_one(&mut player.data, id);
    match kind {
        FOOD_KIND => titles::count(player, "eat", 1),
        DRINK_KIND => titles::count(player, "drink", 1),
        _ => {}
    }
    conn.save(player).await?;
    send(conn, player).await?;
    conn.log(&format!("   -> 1000 used item {template}"));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> VitalsSettings {
        VitalsSettings {
            tick_seconds: 5.0,
            save_every_seconds: 30.0,
            food_per_minute: 1.0,
            water_per_minute: 1.0,
            no_food_water_factor: 3.0,
            no_water_life_per_second: 0.2,
            no_resistance_life_per_second: 0.1,
            rest_stamina_per_minute: 6.0,
            rest_after_seconds: 30.0,
            rest_needs_food: 30.0,
            stamina_per_fish: 1.0,
            stamina_per_kg: 0.5,
            radiation_per_minute_per_point: 0.1,
            death_experience_percent: 10.0,
            respawn_percent: 50.0,
            work_points_per_minute: 10.0,
            work_points_per_kg: 1.0,
            max_work_points: 3000.0,
            radiation: Default::default(),
            aggression: Default::default(),
            environment_hunger_percent: 1.0,
        }
    }


    const LIMITS: [f32; COUNT] = [100.0; COUNT];

    fn vitals(values: [f32; COUNT]) -> Vitals {
        Vitals { values, work_points: 0.0 }
    }

    fn after_a_minute(start: [f32; COUNT], radiation: f32, resting: bool) -> Vitals {
        let mut vitals = vitals(start);
        advance(&settings(), &LIMITS, &mut vitals, &Passing { seconds: 60.0, radiation, aggression: 0.0, resting });
        vitals
    }

    #[test]
    fn food_and_water_run_down_and_work_points_grow() {
        let result = after_a_minute([100.0, 50.0, 100.0, 100.0, 100.0], 0.0, false);
        assert_eq!(result.values, [100.0, 50.0, 100.0, 99.0, 99.0]);
        assert_eq!(result.work_points, 10.0);
    }

    #[test]
    fn without_food_water_goes_three_times_faster() {
        assert_eq!(after_a_minute([100.0, 50.0, 100.0, 0.0, 100.0], 0.0, false).values[WATER], 97.0);
    }

    #[test]
    fn without_water_life_drains() {
        let result = after_a_minute([100.0, 50.0, 100.0, 100.0, 0.0], 0.0, false);
        assert!((result.values[LIFE] - 88.0).abs() < 0.001);
    }

    #[test]
    fn life_runs_out_at_zero() {
        assert_eq!(after_a_minute([2.0, 50.0, 100.0, 100.0, 0.0], 0.0, false).values[LIFE], 0.0);
    }

    #[test]
    fn resting_needs_enough_food() {
        assert_eq!(after_a_minute([100.0, 50.0, 100.0, 80.0, 100.0], 0.0, true).values[STAMINA], 56.0);
        assert_eq!(after_a_minute([100.0, 50.0, 100.0, 20.0, 100.0], 0.0, true).values[STAMINA], 50.0);
        assert_eq!(after_a_minute([100.0, 50.0, 100.0, 80.0, 100.0], 0.0, false).values[STAMINA], 50.0);
    }

    #[test]
    fn a_suit_lets_through_what_is_above_it_and_a_third_of_the_rest() {
        // The guide's example: 60/30 in a 40/40 suit is 33.3 radiation and 10 aggressive.
        assert!((felt(60.0, 40.0) - 33.333).abs() < 0.01);
        assert!((felt(30.0, 40.0) - 10.0).abs() < 0.01);
        assert_eq!(felt(50.0, 0.0), 50.0);
    }

    #[test]
    fn the_environment_makes_one_hungry_and_thirsty() {
        let mut vitals = vitals([100.0; COUNT]);
        advance(&settings(), &LIMITS, &mut vitals, &Passing { seconds: 60.0, radiation: 0.0, aggression: 100.0, resting: false });
        assert_eq!((vitals.values[FOOD], vitals.values[WATER]), (98.0, 98.0));
    }

    #[test]
    fn radiation_wears_down_resistance() {
        assert_eq!(after_a_minute([100.0, 50.0, 100.0, 100.0, 100.0], 20.0, false).values[RADIATION], 98.0);
    }

    #[test]
    fn the_three_numbers_depend_on_the_kind_of_item() {
        let xml = "<base><basePar>10/-15/5</basePar></base>";
        assert_eq!(effect(21, xml), Some([(FOOD, 10.0), (WATER, -15.0), (RADIATION, 5.0)]));
        assert_eq!(effect(22, xml), Some([(FOOD, 10.0), (WATER, -15.0), (STAMINA, 5.0)]));
        assert_eq!(effect(23, xml), Some([(LIFE, 10.0), (RADIATION, -15.0), (STAMINA, 5.0)]));
        assert_eq!(effect(1, xml), None);
    }

    #[test]
    fn the_client_gets_radiation_before_stamina() {
        assert_eq!(for_client([1, 2, 3, 4, 5]), [1, 3, 2, 4, 5]);
    }

    #[test]
    fn every_whole_kilogram_caught_gives_a_work_point() {
        let mut player = Player::new(CharacterData { vitals: Some(vitals([100.0; COUNT])), ..Default::default() });
        reward_catch(&settings(), &mut player, 2900);
        assert_eq!(player.data.vitals.as_ref().unwrap().work_points, 2.0);
        reward_catch(&settings(), &mut player, 900);
        assert_eq!(player.data.vitals.as_ref().unwrap().work_points, 2.0);
    }
}
