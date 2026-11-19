//! The web advisor: what a cast would bring, worked out with the bite's own
//! functions (the odds, the weight roll, the tackle, prices and experience)
//! rather than a copy of them.

use std::collections::BTreeMap;

use rand::random;
use serde::Serialize;

use super::fishing::{self, Cast, Odds, experience_for, fit_to_tackle, roll_weight};
pub use super::fishing::{BITE_REQUEST_SECONDS, NIGHT_END_HOUR, NIGHT_START_HOUR};
use super::{effects, keepnet, perks, reputation, shop, vip};
use crate::config::{CatalogItem, Species};
use crate::storage::CharacterData;
use crate::world::World;

/// Natural baits, imitations and boilies, lures.
pub const BAIT_KINDS: [i32; 3] = [10, 11, 12];
const LURES: i32 = 12;
/// Bags whose baits the character does not carry: deleted things, storage.
const NOT_CARRIED_BAGS: [i32; 2] = [50, 52];
/// Assembled rods sit one to a bag from this bag on: the rod in cell 1, the
/// bait in cell 20.
const FIRST_ROD_BAG: i32 = 100;
const ROD_CELL: i32 = 1;
const BAIT_CELL: i32 = 20;
/// The spinning rod's kind; float and bottom rods take the other baits.
const SPINNING_ROD: i32 = 3;
/// The client lets a fish go («Слишком большая рыба») when it is heavier than
/// the weaker of the rod's and the line's tests times this (plus a little per
/// skill level, left out here).
const CLIENT_ESCAPE_FACTOR: f64 = 2.5;
const LINE_CELL: i32 = 3;
/// Weight rolls per species and kind of request.
const SAMPLES: usize = 48;

/// Where the bait is and when.
#[derive(Debug, Clone, Copy)]
pub struct Spot {
    pub base: i32,
    pub location: i32,
    pub depth: f64,
    pub bottom: f64,
    pub night: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Forecast {
    pub base: i32,
    pub location: i32,
    pub bait: i32,
    pub night: bool,
    pub depth: f64,
    pub bottom: f64,
    /// Chance of a bite per bite request.
    pub chance: f64,
    /// Per bite: credits, experience, the share of trophies, the share of fish
    /// too heavy for the tackle.
    pub money: f64,
    pub experience: f64,
    pub trophies: f64,
    /// Per bite, the fish landed of each size class: ordinary, large, trophy.
    /// The last one is `trophies`; together they are one minus what gets away.
    pub by_class: [f64; 3],
    pub lost: f64,
    pub species: Vec<SpeciesForecast>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeciesForecast {
    pub id: i32,
    /// Of the bites.
    pub share: f64,
    /// Of the fish landed; of all when none is.
    pub grams: f64,
    /// Per bite of the species, the lost ones bringing nothing.
    pub money: f64,
    pub experience: f64,
    pub trophies: f64,
    /// Per bite of the species, the fish landed of each size class.
    pub by_class: [f64; 3],
    /// Of its bites, too heavy for the tackle.
    pub lost: f64,
}

impl Forecast {
    pub fn share_of(&self, species: i32) -> f64 {
        self.species.iter().find(|one| one.id == species).map_or(0.0, |one| one.share)
    }

    /// Per bite, the fish of that size class landed: of one species, or of all
    /// of them when none is named.
    pub fn class_share(&self, species: Option<i32>, class: usize) -> f64 {
        match species {
            Some(species) => self.species.iter().find(|one| one.id == species).map_or(0.0, |one| one.share * one.by_class[class]),
            None => self.by_class[class],
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RodPart {
    pub cell: i32,
    pub template: i32,
    pub name: String,
    /// Rod, reel and line: what the piece holds.
    pub test_kg: Option<f64>,
}

/// An assembled rod of the character's.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rod {
    pub bag: i32,
    pub kind: i32,
    pub name: String,
    /// The heaviest fish the tackle lands: what the client lets through, and
    /// the server's own limit when it has one; none without a limit.
    pub limit_grams: Option<f64>,
    /// The server's limit alone (`tackleWeightFactor`); none without one.
    pub server_limit_grams: Option<f64>,
    /// The bait on it, 0 for none.
    pub bait: i32,
    pub parts: Vec<RodPart>,
}

/// The reel's cell and the item kinds of the three tackle pieces: a rod is a
/// float, feeder or spinning one, then the reel and the line.
const REEL_CELL: i32 = 2;
const REEL_KIND: i32 = 4;
const LINE_KIND: i32 = 5;
const ROD_KINDS: [i32; 3] = [1, 2, SPINNING_ROD];

/// The heaviest fish of a size class: ordinary run up to the large weight,
/// large up to the trophy weight, and trophies up to the species' own top.
pub fn class_top_grams(top_factor: f64, species: &Species, class: usize) -> f64 {
    match class {
        0 => f64::from(species.large_grams),
        1 => f64::from(species.trophy_grams),
        _ => f64::from(species.trophy_grams) * top_factor,
    }
}

/// What a piece in this cell has to hold, in kg, for a fish of `grams` to stay
/// on: the client lets go of anything over its test times [`CLIENT_ESCAPE_FACTOR`],
/// and the server's own limit, when it has one, counts the reel in too.
fn needed_test(factor: f64, cell: i32, grams: f64) -> f64 {
    let server = if factor > 0.0 { grams / (factor * 1000.0) } else { 0.0 };
    if cell == REEL_CELL { server } else { server.max(grams / (CLIENT_ESCAPE_FACTOR * 1000.0)) }
}

/// The heaviest fish this one piece would allow, the rest of the tackle aside.
fn cell_limit(factor: f64, cell: i32, test: f64) -> f64 {
    let server = if factor > 0.0 { test * factor * 1000.0 } else { f64::INFINITY };
    if cell == REEL_CELL { server } else { server.min(test * CLIENT_ESCAPE_FACTOR * 1000.0) }
}

/// One of the character's rods against a target weight.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RodFit {
    pub bag: i32,
    pub name: String,
    pub kind: i32,
    pub limit_grams: Option<f64>,
    /// Whether it lands the whole class without the fish getting away.
    pub enough: bool,
    /// The piece that gives out first: its cell, name and test.
    pub weak_cell: Option<i32>,
    pub weak_name: Option<String>,
    pub weak_test_kg: Option<f64>,
}

/// A piece the shops here sell that would do.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartPick {
    pub template: i32,
    pub name: String,
    pub kind: i32,
    pub test_kg: f64,
    /// In hundredths of a credit, as everywhere; ergs when there is no price.
    pub cost: i64,
    pub premium_cost: i64,
    pub level: i32,
}

/// What to put in one cell to reach the target.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellPick {
    pub cell: i32,
    pub need_test_kg: f64,
    /// The cheapest piece on this base that holds it; none when none does.
    pub item: Option<PartPick>,
}

/// Which tackle lands a given size of a given species.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TackleAdvice {
    pub species: i32,
    pub class: usize,
    /// The heaviest fish of that class, which the tackle has to hold.
    pub need_grams: f64,
    pub mine: Vec<RodFit>,
    pub buy: Vec<CellPick>,
}

/// Rod kinds that take this species' baits: lures only go on spinning rods.
fn rod_kinds_for(world: &World, species: &Species) -> Vec<i32> {
    let kinds: Vec<i32> = species.baits.iter().filter_map(|bait| world.catalog.get(*bait)).map(|item| item.kind).collect();
    match (kinds.iter().any(|kind| *kind == LURES), kinds.iter().any(|kind| *kind != LURES)) {
        (true, false) => vec![SPINNING_ROD],
        (false, true) => ROD_KINDS.iter().copied().filter(|kind| *kind != SPINNING_ROD).collect(),
        _ => ROD_KINDS.to_vec(),
    }
}

/// The cheapest piece of these kinds the base sells that holds the test. A
/// price in credits beats one in ergs, which only the premium counter asks.
fn cheapest<'a>(world: &'a World, base: i32, level: i32, kinds: &[i32], test: f64) -> Option<&'a CatalogItem> {
    let key = |item: &CatalogItem| if item.cost > 0 { (0, item.cost) } else { (1, item.premium_cost) };
    world
        .catalog
        .for_sale()
        .chain(world.catalog.premium_only())
        .filter(|item| kinds.contains(&item.kind))
        .filter(|item| buyable(world, base, level, item.id))
        .filter(|item| fishing::test_of_template(item).is_some_and(|own| own >= test))
        .min_by_key(|item| key(item))
}

fn pick_of(item: &CatalogItem) -> PartPick {
    PartPick {
        template: item.id,
        name: item.name.clone(),
        kind: item.kind,
        test_kg: fishing::test_of_template(item).unwrap_or(0.0),
        cost: item.cost,
        premium_cost: item.premium_cost,
        level: shop::level_of(item),
    }
}

fn rod_fit(factor: f64, rod: &Rod, need_grams: f64) -> RodFit {
    let weak = rod
        .parts
        .iter()
        .filter_map(|part| part.test_kg.filter(|test| *test > 0.0).map(|test| (part, test)))
        .min_by(|a, b| cell_limit(factor, a.0.cell, a.1).total_cmp(&cell_limit(factor, b.0.cell, b.1)));
    RodFit {
        bag: rod.bag,
        name: rod.name.clone(),
        kind: rod.kind,
        limit_grams: rod.limit_grams,
        enough: rod.limit_grams.is_none_or(|limit| limit >= need_grams),
        weak_cell: weak.map(|(part, _)| part.cell),
        weak_name: weak.map(|(part, _)| part.name.clone()),
        weak_test_kg: weak.map(|(_, test)| test),
    }
}

/// What the character already has and what the base sells for landing fish of
/// that size class, whole: the rod and the line carry the client's limit, the
/// reel only counts when the server has a limit of its own.
pub fn tackle_advice(world: &World, character: &CharacterData, species: &Species, class: usize, base: i32) -> TackleAdvice {
    let factor = world.settings.fishing.tackle_weight_factor;
    let need_grams = class_top_grams(world.settings.fishing.top_weight_factor, species, class);
    let kinds = rod_kinds_for(world, species);
    let mine = rods(world, character)
        .iter()
        .filter(|rod| kinds.contains(&rod.kind))
        .map(|rod| rod_fit(factor, rod, need_grams))
        .collect();
    let buy = [(ROD_CELL, kinds.clone()), (REEL_CELL, vec![REEL_KIND]), (LINE_CELL, vec![LINE_KIND])]
        .into_iter()
        .map(|(cell, kinds)| (cell, kinds, needed_test(factor, cell, need_grams)))
        .filter(|(_, _, test)| *test > 0.0)
        .map(|(cell, kinds, test)| CellPick {
            cell,
            need_test_kg: test,
            item: cheapest(world, base, character.level, &kinds, test).map(pick_of),
        })
        .collect();
    TackleAdvice { species: species.id, class, need_grams, mine, buy }
}

fn name_of(world: &World, template: i32) -> String {
    world.catalog.get(template).map(|item| item.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| format!("предмет {template}"))
}

/// The character's assembled rods, by bag.
pub fn rods(world: &World, character: &CharacterData) -> Vec<Rod> {
    let factor = world.settings.fishing.tackle_weight_factor;
    let mut rods: Vec<Rod> = character
        .items
        .iter()
        .filter(|item| item.bag >= FIRST_ROD_BAG && item.cell == ROD_CELL)
        .map(|rod| {
            let mut parts: Vec<RodPart> = character
                .items
                .iter()
                .filter(|item| item.bag == rod.bag)
                .map(|item| RodPart {
                    cell: item.cell,
                    template: item.template,
                    name: name_of(world, item.template),
                    test_kg: fishing::test_of(world, item).filter(|_| (1..=3).contains(&item.cell)),
                })
                .collect();
            parts.sort_by_key(|part| part.cell);
            let server = fishing::tackle_limit_in(&|item| fishing::test_of(world, item), &character.items, rod.bag, factor);
            let weaker = parts
                .iter()
                .filter(|part| part.cell == ROD_CELL || part.cell == LINE_CELL)
                .filter_map(|part| part.test_kg)
                .filter(|test| *test > 0.0)
                .reduce(f64::min);
            let client = weaker.map_or(f64::INFINITY, |test| test * CLIENT_ESCAPE_FACTOR * 1000.0);
            let limit = client.min(server);
            Rod {
                bag: rod.bag,
                kind: world.catalog.get(rod.template).map_or(0, |item| item.kind),
                name: name_of(world, rod.template),
                limit_grams: limit.is_finite().then_some(limit),
                server_limit_grams: server.is_finite().then_some(server),
                bait: character.items.iter().find(|item| item.bag == rod.bag && item.cell == BAIT_CELL).map_or(0, |item| item.template),
                parts,
            }
        })
        .collect();
    rods.sort_by_key(|rod| rod.bag);
    rods
}

/// Whether a rod of this kind takes a bait of this kind: lures go on spinning rods.
pub fn takes(rod_kind: i32, bait_kind: i32) -> bool {
    (bait_kind == LURES) == (rod_kind == SPINNING_ROD)
}

/// The bait templates the character carries, in the backpack or on a rod, with their pieces.
pub fn baits_of(world: &World, character: &CharacterData) -> BTreeMap<i32, i32> {
    let mut baits = BTreeMap::new();
    for item in character.items.iter().filter(|item| !NOT_CARRIED_BAGS.contains(&item.bag)) {
        if world.catalog.get(item.template).is_some_and(|template| BAIT_KINDS.contains(&template.kind)) {
            *baits.entry(item.template).or_insert(0) += item.count.max(1);
        }
    }
    baits
}

/// Every bait some fish takes.
pub fn known_baits(world: &World) -> Vec<&CatalogItem> {
    let mut ids: Vec<i32> = world.fish.all().flat_map(|species| species.baits.iter().copied()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter().filter_map(|id| world.catalog.get(id)).filter(|item| BAIT_KINDS.contains(&item.kind)).collect()
}

/// Whether the base's shops sell the bait to a character of this level.
pub fn buyable(world: &World, base: i32, level: i32, template: i32) -> bool {
    shop::sells(world, base, template) && world.catalog.get(template).is_some_and(|item| shop::level_allows(&world.settings.shop, level, item))
}

pub fn level_of(item: &CatalogItem) -> i32 {
    shop::level_of(item)
}

/// Whether some fish of the location takes the bait.
pub fn bites_at(world: &World, location: i32, bait: i32) -> bool {
    world
        .fish
        .by_location
        .get(&location)
        .is_some_and(|ids| ids.iter().filter_map(|id| world.fish.get(*id)).any(|species| species.baits.is_empty() || species.baits.contains(&bait)))
}

/// How far above `nearBottomCm` the bait is lifted to keep off the bottom fish.
const OFF_BOTTOM_MARGIN_CM: f64 = 10.0;

/// Depths worth trying at a place with this bottom: on the bottom; just above
/// the bottom fish's reach when the place has some, so they bite less and the
/// others take a bigger share; and at the deepest some fish of the place still
/// takes when that is above the bottom.
pub fn depths(world: &World, location: i32, bottom: f64) -> Vec<f64> {
    let mut depths = vec![bottom];
    let here: Vec<_> = world.fish.by_location.get(&location).into_iter().flatten().filter_map(|id| world.fish.get(*id)).collect();
    let off_bottom = bottom - world.settings.fishing.near_bottom_cm - OFF_BOTTOM_MARGIN_CM;
    if off_bottom > 0.0 && here.iter().any(|species| species.near_bottom) {
        depths.push(off_bottom);
    }
    for species in here {
        let deepest = f64::from(species.max_depth_cm);
        if species.max_depth_cm > 0 && deepest < bottom {
            depths.push(deepest);
        }
    }
    depths.sort_by(f64::total_cmp);
    depths.dedup();
    depths
}

pub fn is_night(world: &World) -> bool {
    fishing::is_night(&world.settings.fishing)
}

pub fn game_hour(world: &World) -> f64 {
    fishing::game_hour(&world.settings.fishing)
}

#[derive(Default)]
struct Sum {
    share: f64,
    grams: f64,
    /// Grams of all the fish, landed or not, and the share landed.
    all_grams: f64,
    landed: f64,
    money: f64,
    experience: f64,
    by_class: [f64; 3],
}

/// The tackle a forecast is for, in grams; infinite: no limit.
#[derive(Debug, Clone, Copy)]
pub struct Tackle {
    /// The server's limit: heavier fish come up again lighter.
    pub server: f64,
    /// What the client lands: heavier fish get away.
    pub landed: f64,
}

impl Tackle {
    pub const NONE: Tackle = Tackle { server: f64::INFINITY, landed: f64::INFINITY };

    pub fn of(rod: &Rod) -> Tackle {
        Tackle { server: rod.server_limit_grams.unwrap_or(f64::INFINITY), landed: rod.limit_grams.unwrap_or(f64::INFINITY) }
    }
}

/// What casting this bait at the spot brings the character with the tackle.
/// Fish lighter than the bait's size come off instead of taking the hook
/// (`drop_chance`), so the samples let them go and the chance falls with them.
pub fn forecast(world: &World, character: &CharacterData, spot: Spot, bait: i32, tackle: Tackle) -> Forecast {
    let max_grams = tackle.server;
    let settings = &world.settings;
    let Spot { base, location, depth, bottom, night } = spot;
    let attempt = Cast { base, location, bait: Some(bait), max_grams, depth, bottom, night };

    let per_level = &settings.reputation.per_level;
    let education = perks::effects_for(settings, &world.catalog, character).experience
        * effects::education(character)
        * reputation::base_bonus_at(settings, character, base, per_level.experience_percent);
    let experience_factor = vip::multiplier(character, settings.vip.experience_percent);
    let price_factor = vip::multiplier(character, settings.vip.fish_price_percent);

    let samples = SAMPLES as f64;
    let mut sums: Vec<(i32, Sum)> = Vec::new();
    let Odds { chance: bite_chance, options, total, min_grams, luck } = fishing::odds(world, character, &attempt);
    if total > 0.0 {
        for (species, weight) in &options {
            let mut sum = Sum::default();
            let mut hooked = 0.0;
            for _ in 0..SAMPLES {
                let rolled = roll_weight(&settings.fishing, species, depth, luck, random(), random());
                let (grams, size_class) = fit_to_tackle(species, rolled, max_grams, random());
                if random::<f64>() < fishing::drop_chance(min_grams, f64::from(grams)) {
                    continue;
                }
                hooked += 1.0;
                sum.all_grams += f64::from(grams);
                if f64::from(grams) > tackle.landed {
                    continue;
                }
                sum.landed += 1.0;
                sum.grams += f64::from(grams);
                sum.money += keepnet::value_of(&world.fish, &settings.fish_market, species, grams, size_class) as f64 * price_factor / 100.0;
                sum.experience += f64::from(experience_for(&settings.progression, grams, size_class, education)) * experience_factor;
                sum.by_class[(size_class.max(0) as usize).min(2)] += 1.0;
            }
            if hooked <= 0.0 {
                continue;
            }
            // The fish that came off are not bites: the species' share falls with them.
            let part = bite_chance * weight / total * hooked / samples;
            sums.push((
                species.id,
                Sum {
                    share: part,
                    grams: part * sum.grams / hooked,
                    all_grams: part * sum.all_grams / hooked,
                    landed: part * sum.landed / hooked,
                    money: part * sum.money / hooked,
                    experience: part * sum.experience / hooked,
                    by_class: sum.by_class.map(|count| part * count / hooked),
                },
            ));
        }
    }
    let chance: f64 = sums.iter().map(|(_, sum)| sum.share).sum();

    let mut species: Vec<SpeciesForecast> = sums
        .into_iter()
        .filter(|(_, sum)| sum.share > 0.0)
        .map(|(id, sum)| SpeciesForecast {
            id,
            share: sum.share / chance,
            grams: if sum.landed > 0.0 { sum.grams / sum.landed } else { sum.all_grams / sum.share },
            lost: (1.0 - sum.landed / sum.share).max(0.0),
            money: sum.money / sum.share,
            experience: sum.experience / sum.share,
            trophies: sum.by_class[fishing::TROPHY_CLASS as usize] / sum.share,
            by_class: sum.by_class.map(|count| count / sum.share),
        })
        .collect();
    species.sort_by(|a, b| b.share.total_cmp(&a.share));
    let mean = |value: fn(&SpeciesForecast) -> f64| species.iter().map(|one| one.share * value(one)).sum::<f64>();
    Forecast {
        base,
        location,
        bait,
        night,
        depth,
        bottom,
        chance,
        money: mean(|one| one.money),
        experience: mean(|one| one.experience),
        trophies: mean(|one| one.trophies),
        by_class: [mean(|one| one.by_class[0]), mean(|one| one.by_class[1]), mean(|one| one.by_class[2])],
        lost: mean(|one| one.lost),
        species,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn species() -> Species {
        Species {
            id: 7,
            name: "Карп".into(),
            small_grams: 100,
            large_grams: 1000,
            trophy_grams: 5000,
            min_depth_cm: 0,
            max_depth_cm: 0,
            near_bottom: false,
            likes_depth: false,
            night_factor: 1.0,
            hardy: false,
            share: 2.0,
            baits: vec![1010],
        }
    }

    #[test]
    fn a_size_class_tops_out_where_the_next_one_starts() {
        let fish = species();
        assert_eq!(class_top_grams(1.5, &fish, 0), 1000.0);
        assert_eq!(class_top_grams(1.5, &fish, 1), 5000.0);
        assert_eq!(class_top_grams(1.5, &fish, 2), 7500.0);
    }

    #[test]
    fn the_rod_and_the_line_carry_the_clients_limit_and_the_reel_only_the_servers() {
        // The client lands 2,5 × the test, so a 5 kg fish needs a 2 kg rod and line.
        assert_eq!(needed_test(0.0, ROD_CELL, 5000.0), 2.0);
        assert_eq!(needed_test(0.0, LINE_CELL, 5000.0), 2.0);
        // With no limit of its own the server asks nothing of the reel.
        assert_eq!(needed_test(0.0, REEL_CELL, 5000.0), 0.0);
        // A server limit of 1 kg per kg of test is the stricter rule.
        assert_eq!(needed_test(1.0, ROD_CELL, 5000.0), 5.0);
        assert_eq!(needed_test(1.0, REEL_CELL, 5000.0), 5.0);
        // And the same the other way round.
        assert_eq!(cell_limit(0.0, ROD_CELL, 2.0), 5000.0);
        assert_eq!(cell_limit(0.0, REEL_CELL, 2.0), f64::INFINITY);
        assert_eq!(cell_limit(1.0, ROD_CELL, 2.0), 2000.0);
    }

    #[test]
    fn lures_go_on_spinning_rods_and_baits_on_the_others() {
        assert!(takes(SPINNING_ROD, LURES));
        assert!(!takes(SPINNING_ROD, 10));
        assert!(takes(1, 10));
        assert!(takes(2, 11));
        assert!(!takes(1, LURES));
    }
}
