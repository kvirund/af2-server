//! The web advisor: what a cast would bring, worked out with the bite's own
//! functions (the odds, the weight roll, the tackle, prices and experience)
//! rather than a copy of them.

use std::collections::BTreeMap;

use rand::random;
use serde::Serialize;

use super::fishing::{self, Cast, Odds, experience_for, fit_to_tackle, roll_weight};
pub use super::fishing::{BITE_REQUEST_SECONDS, NIGHT_END_HOUR, NIGHT_START_HOUR};
use super::{effects, keepnet, perks, reputation, shop, vip};
use crate::config::CatalogItem;
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
    pub lost: f64,
    pub species: Vec<SpeciesForecast>,
}

#[derive(Debug, Serialize)]
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
    /// Of its bites, too heavy for the tackle.
    pub lost: f64,
}

impl Forecast {
    pub fn share_of(&self, species: i32) -> f64 {
        self.species.iter().find(|one| one.id == species).map_or(0.0, |one| one.share)
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
    trophies: f64,
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
/// A bite request lets small fish at the bait `smallFishNibbleShare` of the
/// time, so the forecast mixes both kinds of request the same way.
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

    let nibble_share = settings.fishing.small_fish_nibble_share;
    let mut sums: Vec<(i32, Sum)> = Vec::new();
    let mut chance = 0.0;
    for (nibble, share) in [(false, 1.0 - nibble_share), (true, nibble_share)] {
        let Odds { chance: this_chance, options, total, min_grams, luck } = fishing::odds(world, character, &attempt, nibble);
        if options.is_empty() || total <= 0.0 {
            continue;
        }
        chance += share * this_chance;
        for (species, weight) in &options {
            let part = share * this_chance * weight / total;
            let mut sum = Sum::default();
            for _ in 0..SAMPLES {
                let rolled = roll_weight(&settings.fishing, species, depth, min_grams, luck, random(), random());
                let (grams, size_class) = fit_to_tackle(species, rolled, min_grams, max_grams, random());
                sum.all_grams += f64::from(grams);
                if f64::from(grams) > tackle.landed {
                    continue;
                }
                sum.landed += 1.0;
                sum.grams += f64::from(grams);
                sum.money += keepnet::value_of(&world.fish, &settings.fish_market, species, grams, size_class) as f64 * price_factor / 100.0;
                sum.experience += f64::from(experience_for(&settings.progression, grams, size_class, education)) * experience_factor;
                sum.trophies += f64::from(u8::from(size_class == fishing::TROPHY_CLASS));
            }
            let samples = SAMPLES as f64;
            let index = match sums.iter().position(|(id, _)| *id == species.id) {
                Some(index) => index,
                None => {
                    sums.push((species.id, Sum::default()));
                    sums.len() - 1
                }
            };
            let found = &mut sums[index].1;
            found.share += part;
            found.landed += part * sum.landed / samples;
            found.grams += part * sum.grams / samples;
            found.all_grams += part * sum.all_grams / samples;
            found.money += part * sum.money / samples;
            found.experience += part * sum.experience / samples;
            found.trophies += part * sum.trophies / samples;
        }
    }

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
            trophies: sum.trophies / sum.share,
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
        lost: mean(|one| one.lost),
        species,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lures_go_on_spinning_rods_and_baits_on_the_others() {
        assert!(takes(SPINNING_ROD, LURES));
        assert!(!takes(SPINNING_ROD, 10));
        assert!(takes(1, 10));
        assert!(takes(2, 11));
        assert!(!takes(1, LURES));
    }
}
