//! Files read once at startup: `config/` is edited by hand, `data/items.json`
//! is extracted from the player's own copy of the game.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use af2_server::files::read_commented;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// How many bite states the client keeps a behaviour for.
const BITE_STATES: i32 = 10;

/// The client keeps bag sizes in an array of 15 and never uses index 0.
const MAX_BAG_PAGES: i32 = 14;

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
    pub bite_behaviour: String,
    pub small_fish_nibble_share: f64,
    pub bait_per_catch: i32,
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
    /// With the character's discount applied, never below one point.
    pub fn experience_needed(&self, level: i32, cost_factor: f64) -> i64 {
        ((self.experience_to_next(level) as f64 * cost_factor).round() as i64).max(1)
    }

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
pub struct BagSettings {
    pub pages: i32,
    pub start_cells: i32,
    pub storage_cells: i32,
    pub cells_per_purchase: i32,
    pub purchase_money: i64,
    pub purchase_premium: i64,
    /// Which item kinds (`wtype`) each page holds.
    pub page_kinds: BTreeMap<i32, Vec<i32>>,
    pub other_page: i32,
    /// Bags whose items all add their skills and attributes.
    pub worn_bags: Vec<i32>,
    /// Assembled tackle: only the best set counts for each skill.
    pub tackle_bags: Vec<i32>,
}

impl BagSettings {
    pub fn page_for(&self, kind: i32) -> i32 {
        self.page_kinds.iter().find(|(_, kinds)| kinds.contains(&kind)).map_or(self.other_page, |(page, _)| *page)
    }

    pub fn holds_gear(&self, bag: i32) -> bool {
        self.worn_bags.contains(&bag) || self.tackle_bags.contains(&bag)
    }

    fn pages_exist(&self) -> bool {
        self.page_kinds.keys().chain([&self.other_page]).all(|page| (1..=self.pages).contains(page))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeSettings {
    pub education_experience_percent: f64,
    pub education_level_cost_percent: f64,
    pub luck_trophy_percent: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VitalsSettings {
    pub tick_seconds: f32,
    pub save_every_seconds: f32,
    pub food_per_minute: f32,
    pub water_per_minute: f32,
    pub no_food_water_factor: f32,
    pub no_water_life_per_second: f32,
    pub no_resistance_life_per_second: f32,
    pub rest_stamina_per_minute: f32,
    pub rest_after_seconds: f32,
    pub rest_needs_food: f32,
    pub stamina_per_fish: f32,
    pub stamina_per_kg: f32,
    pub radiation_per_minute_per_point: f32,
    pub lowest_life: f32,
    pub work_points_per_minute: f32,
    pub max_work_points: f32,
    /// Base to its radiation by day and at night.
    pub radiation: BTreeMap<i32, [f32; 2]>,
}

#[derive(Default, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShopSettings {
    pub enforce_levels: bool,
    /// Each base sells what `config/shops.json` lists; otherwise everything with a price.
    #[serde(default)]
    pub guide_stock: bool,
    #[serde(skip)]
    pub stock: ShopStock,
    /// Item kinds nobody can wear, so the shop leaves them out.
    pub hidden_kinds: Vec<i32>,
    pub hidden_items: Vec<i32>,
    /// Item id to price, replacing the price in the client's data.
    pub prices: BTreeMap<i32, i64>,
}

/// Things that come up on the hook together with a fish.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LootSettings {
    pub chance_percent: f64,
    pub items: Vec<LootItem>,
    pub base_card_chance_percent: f64,
    /// Base id to the card of that base.
    pub base_cards: BTreeMap<i32, i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LootItem {
    pub item: i32,
    pub weight: f64,
    #[serde(default = "one")]
    pub count: i32,
}

fn one() -> i32 {
    1
}

/// `config/shops.json`: item ids each base's shops sell.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShopStock {
    /// Bases without a list of their own sell this base's.
    pub fallback_base: i32,
    pub bases: BTreeMap<i32, Vec<i32>>,
}

impl ShopSettings {
    /// What a base sells, or `None` when every priced item is for sale.
    pub fn stocked(&self, base: i32) -> Option<&[i32]> {
        if !self.guide_stock {
            return None;
        }
        let stock = &self.stock;
        Some(stock.bases.get(&base).or_else(|| stock.bases.get(&stock.fallback_base)).map_or(&[], Vec::as_slice))
    }

    /// Applies the settings to one catalog item; a price of 0 means "not for sale".
    fn adjust(&self, item: &mut CatalogItem) {
        if let Some(price) = self.prices.get(&item.id) {
            item.cost = *price;
        }
        if self.hidden_kinds.contains(&item.kind) || self.hidden_items.contains(&item.id) {
            item.cost = 0;
        }
    }
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
    pub bags: BagSettings,
    pub shop: ShopSettings,
    pub vitals: VitalsSettings,
    pub attributes: AttributeSettings,
    #[serde(default)]
    pub loot: LootSettings,
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
    /// Pieces in one purchase.
    #[serde(default = "one")]
    pub shop_count: i32,
    /// Pieces that fit in one cell.
    #[serde(default = "one")]
    pub max_count: i32,
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

/// One line of `data/recipes.json`, which the launcher extracts from the game.
#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    pub id: i32,
    pub station: i32,
    /// Which list of the shared station shows it.
    pub show: i32,
    pub category: i32,
    pub kind: i32,
    /// Known without learning.
    pub default: bool,
    pub result: [f64; 3],
    pub cost: [f64; 3],
    pub time: i32,
    #[serde(default)]
    pub crit_chance: f64,
    /// Made instead of `result` on a critical success; item 0 means none.
    #[serde(default)]
    pub crit_result: [f64; 3],
    pub profession: i32,
    pub profession_level: i32,
    pub ingredients: Vec<[f64; 6]>,
}

pub struct GameData {
    pub settings: Settings,
    pub fish: FishTable,
    pub catalog: Catalog,
    pub recipes: Vec<Recipe>,
}

pub fn load(config_dir: &Path, root: &Path) -> Result<GameData> {
    let mut settings: Settings = read_commented(&config_dir.join("settings.json"))?;
    if settings.shop.guide_stock {
        settings.shop.stock = read_commented(&config_dir.join("shops.json"))?;
    }
    check_bite_behaviour(&settings.fishing.bite_behaviour)?;
    if !(1..=MAX_BAG_PAGES).contains(&settings.bags.pages) || settings.bags.cells_per_purchase < 1 {
        bail!("bags: pages must be 1..={MAX_BAG_PAGES} and cellsPerPurchase at least 1");
    }
    if !settings.bags.pages_exist() {
        bail!("bags: pageKinds and otherPage must name pages that exist");
    }

    let fish_path = config_dir.join("fish.json");
    let fish: FishFile = read_commented(&fish_path)?;

    let items_path = root.join("data").join("items.json");
    let text = fs::read_to_string(&items_path).with_context(|| {
        format!("{} is missing; run af2-launcher first", items_path.display())
    })?;
    let mut items: Vec<CatalogItem> =
        serde_json::from_str(&text).with_context(|| items_path.display().to_string())?;
    items.iter_mut().for_each(|item| settings.shop.adjust(item));

    let index = items.iter().enumerate().map(|(position, item)| (item.id, position)).collect();
    let catalog = Catalog { items, index };

    let recipes_path = root.join("data").join("recipes.json");
    let text = fs::read_to_string(&recipes_path).with_context(|| {
        format!("{} is missing; run af2-launcher first", recipes_path.display())
    })?;
    let recipes: Vec<Recipe> = serde_json::from_str(&text).with_context(|| recipes_path.display().to_string())?;

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
        recipes,
    })
}

/// The client parses this string with no safety net, so a typo here would break
/// fishing for every player instead of failing once at startup.
fn check_bite_behaviour(text: &str) -> Result<()> {
    for record in text.split('/') {
        let valid = record.split_once(':').is_some_and(|(state, choices)| {
            state.parse::<i32>().is_ok_and(|state| (0..BITE_STATES).contains(&state))
                && choices.split(',').all(|choice| {
                    choice.split_once('+').is_some_and(|(next, weight)| {
                        next.parse::<i32>().is_ok() && weight.parse::<f64>().is_ok()
                    })
                })
        });
        if !valid {
            bail!("settings.json: fishing.biteBehaviour has a bad record \"{record}\"");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shop_settings_reprice_and_hide() {
        let shop = ShopSettings {
            enforce_levels: true,
            hidden_kinds: vec![29],
            hidden_items: vec![7],
            prices: [(5, 1000)].into(),
            ..Default::default()
        };
        let item = |id, kind| CatalogItem { id, kind, cost: 100, durability: 1.0, stacks: false, shop_count: 1, max_count: 1, xml: String::new() };
        let cost = |mut item: CatalogItem| {
            shop.adjust(&mut item);
            item.cost
        };
        assert_eq!(cost(item(5, 14)), 1000);
        assert_eq!(cost(item(6, 29)), 0);
        assert_eq!(cost(item(7, 14)), 0);
        assert_eq!(cost(item(8, 14)), 100);
    }

    #[test]
    fn bite_behaviour_is_checked() {
        assert!(check_bite_behaviour("0:1+100/1:1+45,2+55").is_ok());
        assert!(check_bite_behaviour("0:1+100/").is_err());
        assert!(check_bite_behaviour("0:1").is_err());
        assert!(check_bite_behaviour("10:1+100").is_err());
        assert!(check_bite_behaviour("a:1+100").is_err());
    }

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
