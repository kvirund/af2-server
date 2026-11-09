//! Files read once at startup: `config/` is edited by hand, `data/items.json`
//! is extracted from the player's own copy of the game.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use json_comments::StripComments;
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    pub bind: String,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSettings {
    pub name: String,
    pub level: i32,
    pub starting_money: i64,
    pub premium_money: i64,
    pub start_base: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FishingSettings {
    pub bite_chance: f64,
    pub full_bite_weight: f64,
    pub large_share: f64,
    pub trophy_share: f64,
    pub trophy_depth_cm: f64,
    pub deep_trophy_multiplier: f64,
    pub top_weight_factor: f64,
    pub near_bottom_cm: f64,
    pub near_bottom_bonus: f64,
    pub away_from_bottom_factor: f64,
    pub deep_water_bonus: f64,
    pub shallow_water_factor: f64,
    pub game_hours_per_real_hour: f64,
    pub grams_per_speed: f64,
    pub min_speed: f64,
    pub max_speed: f64,
    pub grams_per_stamina: f64,
    pub min_stamina: f64,
    pub max_stamina: f64,
    pub hardy_stamina_factor: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionSettings {
    pub max_level: i32,
    pub level_base: f64,
    pub level_exponent: f64,
    pub experience_per_fish: f64,
    pub experience_per_kg: f64,
    pub large_experience_factor: f64,
    pub trophy_experience_factor: f64,
    pub record_experience_factor: i32,
    pub base_vital: i32,
    /// Gained per level: life, stamina, radiation, food, water.
    pub vitals_per_level: Vec<i32>,
}

impl ProgressionSettings {
    pub fn experience_to_next(&self, level: i32) -> i64 {
        (self.level_base * f64::from(level).powf(self.level_exponent)).round() as i64
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FishMarketSettings {
    pub base_price_per_kg: f64,
    pub large_price_factor: f64,
    pub trophy_price_factor: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TravelSettings {
    pub enforce_levels: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseSettings {
    pub min_level: i32,
    pub travel_cost: i64,
    pub locations: Vec<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarterItem {
    pub item: i32,
    pub bag: i32,
    pub cell: i32,
    pub count: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub server: ServerSettings,
    pub character: CharacterSettings,
    pub fishing: FishingSettings,
    pub progression: ProgressionSettings,
    pub fish_market: FishMarketSettings,
    pub travel: TravelSettings,
    pub bases: BTreeMap<i32, BaseSettings>,
    pub starter_kit: Vec<StarterItem>,
}

/// A species as players recorded it. The weights are the thresholds of the
/// game's three size classes: small, large and trophy.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Species {
    pub id: i32,
    pub name: String,
    pub small_grams: i32,
    pub large_grams: i32,
    pub trophy_grams: i32,
    pub min_depth_cm: i32,
    pub max_depth_cm: i32,
    pub near_bottom: bool,
    pub likes_depth: bool,
    pub night_factor: f64,
    pub hardy: bool,
    pub share: f64,
    pub baits: Vec<i32>,
}

#[derive(Debug, Deserialize)]
struct FishFile {
    species: Vec<Species>,
    locations: HashMap<i32, Vec<i32>>,
}

#[derive(Debug)]
pub struct FishTable {
    species: BTreeMap<i32, Species>,
    pub by_location: HashMap<i32, Vec<i32>>,
}

impl FishTable {
    pub fn all(&self) -> impl Iterator<Item = &Species> {
        self.species.values()
    }

    pub fn get(&self, id: i32) -> Option<&Species> {
        self.species.get(&id)
    }

    pub fn largest_share(&self) -> f64 {
        self.all().map(|s| s.share).fold(0.0, f64::max)
    }

    #[cfg(test)]
    pub fn for_tests(species: Vec<Species>, location: i32) -> Self {
        let ids = species.iter().map(|s| s.id).collect();
        Self {
            species: species.into_iter().map(|s| (s.id, s)).collect(),
            by_location: HashMap::from([(location, ids)]),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CatalogItem {
    pub id: i32,
    pub kind: i32,
    pub cost: i64,
    pub durability: f32,
    pub stacks: bool,
    pub xml: String,
}

#[derive(Debug)]
pub struct Catalog {
    /// In file order, which is the order shops list items.
    items: Vec<CatalogItem>,
    index: HashMap<i32, usize>,
}

impl Catalog {
    pub fn get(&self, id: i32) -> Option<&CatalogItem> {
        self.index.get(&id).map(|&position| &self.items[position])
    }

    pub fn for_sale(&self) -> impl Iterator<Item = &CatalogItem> {
        self.items.iter().filter(|item| item.cost > 0)
    }
}

pub struct GameData {
    pub settings: Settings,
    pub fish: FishTable,
    pub catalog: Catalog,
}

pub fn load(config_dir: &Path, root: &Path) -> Result<GameData> {
    let settings: Settings = read_commented(&config_dir.join("settings.json"))?;

    let fish_path = config_dir.join("fish.json");
    let fish: FishFile = read_commented(&fish_path)?;

    let items_path = root.join("data").join("items.json");
    let text = fs::read_to_string(&items_path).with_context(|| {
        format!("{} is missing; run tools/extract_items.py first", items_path.display())
    })?;
    let items: Vec<CatalogItem> =
        serde_json::from_str(&text).with_context(|| items_path.display().to_string())?;

    let index = items.iter().enumerate().map(|(position, item)| (item.id, position)).collect();
    let catalog = Catalog { items, index };

    for kit in &settings.starter_kit {
        if catalog.get(kit.item).is_none() {
            bail!("settings.json: starterKit has unknown item {}", kit.item);
        }
    }

    Ok(GameData {
        settings,
        fish: FishTable {
            species: fish.species.into_iter().map(|s| (s.id, s)).collect(),
            by_location: fish.locations,
        },
        catalog,
    })
}

fn read_commented<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path).with_context(|| path.display().to_string())?;
    serde_json::from_reader(StripComments::new(file)).with_context(|| path.display().to_string())
}

/// The build sits in `target/...`, so look upward for the project's config folder.
pub fn find_config_dir() -> Result<PathBuf> {
    let start = std::env::current_exe()?;
    for dir in start.ancestors() {
        let candidate = dir.join("config");
        if candidate.join("settings.json").is_file() {
            return Ok(candidate);
        }
    }
    bail!("config/settings.json not found above {}", start.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_curve_grows() {
        let settings = ProgressionSettings {
            max_level: 40,
            level_base: 50.0,
            level_exponent: 1.3,
            experience_per_fish: 8.0,
            experience_per_kg: 12.0,
            large_experience_factor: 1.5,
            trophy_experience_factor: 3.0,
            record_experience_factor: 3,
            base_vital: 100,
            vitals_per_level: vec![5, 2, 1, 1, 1],
        };
        assert_eq!(settings.experience_to_next(1), 50);
        assert!(settings.experience_to_next(10) > settings.experience_to_next(9));
    }
}
