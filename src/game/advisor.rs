//! The web advisor: what a cast would bring, worked out with the bite's own
//! functions (the odds, the weight roll, the tackle, prices and experience)
//! rather than a copy of them.

use rand::random;
use serde::Serialize;

use super::fishing::{self, Cast, Odds, experience_for, fit_to_tackle, roll_weight};
use super::{effects, keepnet, perks, reputation, vip};
use crate::storage::CharacterData;
use crate::world::{LastCast, World};

/// Natural baits, imitations and boilies, lures.
const BAIT_KINDS: [i32; 3] = [10, 11, 12];
/// Bags from this one on are not the character's own: deleted, storage...
const FIRST_NON_INVENTORY_BAG: i32 = 50;
/// Weight rolls per species and kind of request.
const SAMPLES: usize = 48;
/// Where the bait is taken to be without a cast to measure the place: on
/// the bottom at 2 m.
const GUESSED_BOTTOM_CM: f64 = 200.0;

#[derive(Debug, Serialize)]
pub struct Forecast {
    pub location: i32,
    pub bait: i32,
    pub depth: f64,
    pub bottom: f64,
    /// Depth and bottom come from the character's last cast there.
    pub measured: bool,
    /// Chance of a bite per bite request.
    pub chance: f64,
    /// Per bite: credits, experience, the share of trophies.
    pub money: f64,
    pub experience: f64,
    pub trophies: f64,
    pub species: Vec<SpeciesForecast>,
}

#[derive(Debug, Serialize)]
pub struct SpeciesForecast {
    pub id: i32,
    pub name: String,
    /// Of the bites.
    pub share: f64,
    pub grams: f64,
    pub money: f64,
    pub experience: f64,
    pub trophies: f64,
}

/// The bait templates the character carries.
pub fn baits_of(world: &World, character: &CharacterData) -> Vec<i32> {
    let mut baits: Vec<i32> = character
        .items
        .iter()
        .filter(|item| item.bag < FIRST_NON_INVENTORY_BAG)
        .filter(|item| world.catalog.get(item.template).is_some_and(|template| BAIT_KINDS.contains(&template.kind)))
        .map(|item| item.template)
        .collect();
    baits.sort_unstable();
    baits.dedup();
    baits
}

pub fn is_night(world: &World) -> bool {
    fishing::is_night(&world.settings.fishing)
}

#[derive(Default)]
struct Sum {
    share: f64,
    grams: f64,
    money: f64,
    experience: f64,
    trophies: f64,
}

/// What casting this bait at the location brings the character. A bite
/// request lets small fish at the bait `smallFishNibbleShare` of the time,
/// so the forecast mixes both kinds of request the same way.
pub fn forecast(world: &World, character: &CharacterData, base: i32, location: i32, bait: i32, cast: Option<&LastCast>, night: bool) -> Forecast {
    let settings = &world.settings;
    let (depth, bottom) = cast.map_or((GUESSED_BOTTOM_CM, GUESSED_BOTTOM_CM), |cast| (cast.depth, cast.bottom));
    let bag = character.items.iter().find(|item| item.template == bait).map(|item| item.bag);
    let max_grams =
        bag.map_or(f64::INFINITY, |bag| fishing::tackle_limit_in(&|item| fishing::test_of(world, item), &character.items, bag, settings.fishing.tackle_weight_factor));
    let attempt = Cast { base, location, bait: Some(bait), max_grams, depth, bottom, night };

    let per_level = &settings.reputation.per_level;
    let education = perks::effects_for(settings, &world.catalog, character).experience
        * effects::education(character)
        * reputation::base_bonus_at(settings, character, base, per_level.experience_percent);
    let experience_factor = vip::multiplier(character, settings.vip.experience_percent);
    let price_factor = vip::multiplier(character, settings.vip.fish_price_percent);

    let nibble_share = settings.fishing.small_fish_nibble_share;
    let mut sums: Vec<(i32, String, Sum)> = Vec::new();
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
                sum.grams += f64::from(grams);
                sum.money += keepnet::value_of(&world.fish, &settings.fish_market, species, grams, size_class) as f64 * price_factor / 100.0;
                sum.experience += f64::from(experience_for(&settings.progression, grams, size_class, education)) * experience_factor;
                sum.trophies += f64::from(u8::from(size_class == fishing::TROPHY_CLASS));
            }
            let samples = SAMPLES as f64;
            let index = match sums.iter().position(|(id, _, _)| *id == species.id) {
                Some(index) => index,
                None => {
                    sums.push((species.id, species.name.clone(), Sum::default()));
                    sums.len() - 1
                }
            };
            let found = &mut sums[index].2;
            found.share += part;
            found.grams += part * sum.grams / samples;
            found.money += part * sum.money / samples;
            found.experience += part * sum.experience / samples;
            found.trophies += part * sum.trophies / samples;
        }
    }

    let mut species: Vec<SpeciesForecast> = sums
        .into_iter()
        .filter(|(_, _, sum)| sum.share > 0.0)
        .map(|(id, name, sum)| SpeciesForecast {
            id,
            name,
            share: sum.share / chance,
            grams: sum.grams / sum.share,
            money: sum.money / sum.share,
            experience: sum.experience / sum.share,
            trophies: sum.trophies / sum.share,
        })
        .collect();
    species.sort_by(|a, b| b.share.total_cmp(&a.share));
    let mean = |value: fn(&SpeciesForecast) -> f64| species.iter().map(|one| one.share * value(one)).sum::<f64>();
    Forecast {
        location,
        bait,
        depth,
        bottom,
        measured: cast.is_some(),
        chance,
        money: mean(|one| one.money),
        experience: mean(|one| one.experience),
        trophies: mean(|one| one.trophies),
        species,
    }
}
