//! Files read once at startup: `config/` is edited by hand, `data/items.json`
//! is extracted from the player's own copy of the game.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use af2_server::files::{read_commented, read_commented_with_local};
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
    pub level: i32,
    pub starting_money: i64,
    pub premium_money: i64,
    pub start_base: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FishingSettings {
    /// Species the client draws as mutants (`isMutant` in its fishModelData).
    #[serde(default)]
    pub mutant_species: Vec<i32>,
    pub bite_chance: f64,
    pub bite_behaviour: String,
    pub small_fish_nibble_share: f64,
    pub bait_per_catch: i32,
    pub full_bite_weight: f64,
    /// How much of the bait's attractiveness reaches the bite: 1 in full, 0 not at all.
    #[serde(default)]
    pub bait_attraction: f64,
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
    /// The heaviest fish that bites, as a multiple of the weakest test of the
    /// rod, reel and line; 0 for no limit.
    pub tackle_weight_factor: f64,
    /// Individual traits a landed fish may have.
    #[serde(default)]
    pub traits: Vec<TraitSettings>,
}

/// One individual trait: which species may have it, from what weight, how often.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraitSettings {
    #[serde(rename = "trait")]
    pub trait_id: i32,
    pub species: Vec<i32>,
    pub min_grams: i32,
    pub percent: f64,
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
    /// Base to the NPC numbers there that buy fish; NPC numbers repeat between bases.
    #[serde(default)]
    pub buyers: BTreeMap<i32, Vec<i32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BagSettings {
    pub pages: i32,
    pub start_cells: i32,
    pub storage_cells: i32,
    pub cells_per_purchase: i32,
    /// The first way to pay: pieces of an item, `token_item` for the bags and
    /// `storage_token_item` for the warehouse.
    pub token_item: i32,
    pub purchase_tokens: i32,
    pub storage_token_item: i32,
    pub storage_purchase_tokens: i32,
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
    /// The share of the current level's experience a death takes.
    pub death_experience_percent: f64,
    /// The share of each maximum the scales come back at after a death.
    pub respawn_percent: f32,
    pub work_points_per_minute: f32,
    /// Work points every kilogram of a landed fish gives.
    pub work_points_per_kg: f32,
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
    /// Items sold without the client's price in other items (`costWpn`).
    #[serde(default)]
    pub no_item_price: Vec<i32>,
}

/// `config/gathering.json`: what grows and lies at the gather points of each base.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatheringSettings {
    pub respawn_minutes: [f64; 2],
    pub start_filled_percent: f64,
    pub resources: BTreeMap<i32, GatherResource>,
    pub bases: BTreeMap<i32, GatherBase>,
}

/// One kind of thing on a point, numbered as the client's `restype_N` model.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatherResource {
    pub name: String,
    pub work_points: f32,
    pub seconds: f32,
    /// The skill (130 Ремесло, 145 Грибник) and its level a player guide names.
    #[serde(default)]
    pub skill: i32,
    #[serde(default)]
    pub skill_level: i32,
    #[serde(default = "one")]
    pub rolls: i32,
    pub drops: Vec<GatherDrop>,
}

#[derive(Debug, Deserialize)]
pub struct GatherDrop {
    pub item: i32,
    pub weight: f64,
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Default, Deserialize)]
pub struct GatherBase {
    /// Point type in the scene to the point ids of that type.
    pub points: BTreeMap<i32, Vec<i32>>,
    /// Point type to the resources that grow there, with weights.
    pub spawns: BTreeMap<i32, BTreeMap<i32, f64>>,
}

/// Things that come up on the hook together with a fish.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LootSettings {
    pub chance_percent: f64,
    pub items: Vec<LootItem>,
    pub base_card_chance_percent: f64,
    /// The card of the species landed, for the species collection recipes ask for.
    #[serde(default)]
    pub fish_card_chance_percent: f64,
    /// Base id to the card of that base.
    pub base_cards: BTreeMap<i32, i32>,
    /// Premium currency on the hook: chance per fish and how much, least and most.
    #[serde(default)]
    pub premium_chance_percent: f64,
    #[serde(default = "one_each")]
    pub premium: [i64; 2],
    /// Cards that come up with any fish of some species, like «Форель» with trout;
    /// no species at all means with any fish.
    #[serde(default)]
    pub species_cards: Vec<SpeciesCard>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeciesCard {
    pub item: i32,
    pub species: Vec<i32>,
    pub chance_percent: f64,
}

fn one_each() -> [i64; 2] {
    [1, 1]
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
    /// Items sold from a reputation level on: item to [faction, level].
    #[serde(default)]
    pub reputation: BTreeMap<i32, [i32; 2]>,
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
    /// The client wants every part of a price at once, so an item costs either
    /// credits or, with none, the ergs of its `pricePt`. A price set here is in
    /// credits.
    fn adjust(&self, item: &mut CatalogItem) {
        if let Some(price) = self.prices.get(&item.id) {
            item.cost = *price;
        } else if item.cost <= 0 {
            item.premium_cost = tag_number(&item.xml, "pricePt").unwrap_or(0).max(0);
        }
        if self.hidden_kinds.contains(&item.kind) || self.hidden_items.contains(&item.id) {
            item.cost = 0;
            item.premium_cost = 0;
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
    /// From `config/gathering.json`; without that file nothing can be gathered.
    #[serde(skip)]
    pub gathering: GatheringSettings,
    pub skills: SkillSettings,
    pub doctor: DoctorSettings,
    pub fridge: FridgeSettings,
    #[serde(default)]
    pub mastery: MasterySettings,
    #[serde(default)]
    pub reputation: ReputationSettings,
    #[serde(default)]
    pub vip: VipSettings,
    #[serde(default)]
    pub board: BoardSettings,
    #[serde(default)]
    pub spas: SpasSettings,
    /// From `config/quests.json`: the NPCs' quest chains.
    #[serde(skip)]
    pub quests: QuestSettings,
    /// From `data/skills.json`, which af2-launcher makes from the game.
    #[serde(skip)]
    pub skill_tree: SkillTree,
    pub starter_kit: Vec<StarterItem>,
}

/// The С.П.А.С. terminal.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpasSettings {
    /// Base to the terminal's NPC number in that base's scene, and its dialog file.
    pub npcs: BTreeMap<i32, i32>,
    pub dialog: i32,
    /// What a trophy fish is traded for: `trophy_tokens` pieces plus one for
    /// every `grams_per_token`.
    pub trophy_token: i32,
    pub trophy_tokens: i32,
    pub grams_per_token: i32,
    /// Trophies listed one by one; the rest go with "all".
    pub max_listed: usize,
    pub patents: Vec<PatentSettings>,
    /// A record fish (the heaviest of its species the character has) for
    /// Record Holder's tokens.
    #[serde(default)]
    pub record_token: i32,
    #[serde(default)]
    pub record_tokens: i32,
    /// A fish with traits for Explorer's tokens, once the quest value
    /// `unique_flag` is set (Лиза's quest).
    #[serde(default)]
    pub unique_token: i32,
    #[serde(default)]
    pub unique_tokens: i32,
    #[serde(default)]
    pub unique_flag: String,
    /// The free «Сброс параметров» course, once, from this level.
    #[serde(default)]
    pub reset_course: i32,
    #[serde(default)]
    pub reset_level: i32,
}

/// `config/quests.json`: which NPC talks with which dialog file, and the quest
/// chains told in those files.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestSettings {
    #[serde(default)]
    pub npcs: Vec<QuestNpc>,
    #[serde(default)]
    pub chains: Vec<QuestChain>,
    #[serde(default)]
    pub boat: Option<BoatSettings>,
    #[serde(default)]
    pub deeds: Vec<Deed>,
    #[serde(default)]
    pub tournaments: Vec<Tournament>,
    #[serde(default)]
    pub titles: TitleSettings,
    /// The quest values the NPCs of the scenes watch for their «!»: key →
    /// the chain and the value that lights it.
    #[serde(default)]
    pub icons: BTreeMap<String, IconRule>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IconRule {
    /// Empty: an NPC with nothing to give here, never lit.
    #[serde(default)]
    pub chain: String,
    pub show: i64,
}

/// Titles (the card's titles tab, client `Titul.xml`): which are earned how,
/// and how often the shown one may change.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleSettings {
    #[serde(default)]
    pub change_minutes: i64,
    #[serde(default)]
    pub rules: Vec<TitleRule>,
}

/// A title earned when everything the rule names holds. Titles from deeds,
/// quests and tournaments come with their reward instead.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRule {
    pub title: i32,
    /// Summed quest values `count_<name>`, at least `at`.
    #[serde(default)]
    pub counters: Vec<String>,
    #[serde(default)]
    pub at: i32,
    /// Fish of these species ever landed, at least `at`.
    #[serde(default)]
    pub species: Vec<i32>,
    /// Fish of any kind ever landed.
    #[serde(default)]
    pub fish: i32,
    /// The top level with this faction.
    #[serde(default)]
    pub faction: i32,
    /// At least this character level.
    #[serde(default)]
    pub level: i32,
    /// Minutes in play without a death.
    #[serde(default)]
    pub alive_minutes: i32,
    /// Personal records in at least this many species.
    #[serde(default)]
    pub records: i32,
    /// Every one of these bases visited.
    #[serde(default)]
    pub bases: Vec<i32>,
    /// A VIP patent on.
    #[serde(default)]
    pub vip: bool,
}

/// A periodic tournament at a base: `template` is its name in QTask.xml
/// `tnames`, `task` its line in category 3000.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tournament {
    pub template: i32,
    pub base: i32,
    pub task: i32,
    /// 0 for any fish.
    #[serde(default)]
    pub species: i32,
    #[serde(default)]
    pub bait: i32,
    #[serde(default)]
    pub min_grams: i32,
    #[serde(default)]
    pub max_grams: i32,
    /// Minutes from one start to the next, and the first start's minute.
    pub every: i64,
    #[serde(default)]
    pub offset: i64,
    /// How long one runs.
    pub minutes: i64,
    /// Character levels that take part; [0, 0] for any.
    #[serde(default)]
    pub levels: [i32; 2],
    /// Scores for the 1st, 2nd and 3rd place.
    pub places: [i32; 3],
    /// For the 1st, 2nd, 3rd place and for taking part.
    #[serde(default)]
    pub prizes: Vec<QuestReward>,
}

/// A deed (Деяния, the card's achievements tab): so many fish of a kind over
/// the whole game. `id` is its name in QTask.xml `achivments_name`, `task` its
/// line in category 2000.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deed {
    pub id: i32,
    pub task: i32,
    pub count: i32,
    /// Any of these species; empty for any fish.
    #[serde(default)]
    pub species: Vec<i32>,
    #[serde(default)]
    pub min_grams: i32,
    #[serde(default)]
    pub max_grams: i32,
    #[serde(default)]
    pub trophy: bool,
    /// 1 float, 2 feeder, 3 spinning.
    #[serde(default)]
    pub rod_kind: i32,
    /// Only fish with traits.
    #[serde(default)]
    pub unique: bool,
    /// Deeds to finish first.
    #[serde(default)]
    pub requires: Vec<i32>,
    /// Shown once one of these species has been caught.
    #[serde(default)]
    pub opens_on: Vec<i32>,
    #[serde(default)]
    pub reward: QuestReward,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestNpc {
    pub base: i32,
    pub npc: i32,
    pub dialog: i32,
}

/// A text of the dialog file with the answer under it, and maybe a second
/// answer that closes the window.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestPage {
    pub node: i32,
    pub answer: i32,
    #[serde(default)]
    pub decline: i32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestChain {
    pub name: String,
    pub dialog: i32,
    /// The client's task category (`<cat>` in QTask.xml) the tasks show under.
    pub category: i32,
    /// Shown when nothing is due.
    pub idle: QuestPage,
    /// Shown when a reward does not fit.
    #[serde(default)]
    pub full: Option<QuestPage>,
    /// A menu first (node, the answer that leads to the quests, the one that leaves).
    #[serde(default)]
    pub menu: Option<QuestMenu>,
    pub steps: Vec<QuestStep>,
    /// Texts for pages (`n3`) and answers (`a5`) the dialog file leaves empty:
    /// the client shows those as "- none -", which case 78 then replaces.
    #[serde(default)]
    pub texts: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestMenu {
    pub node: i32,
    pub quests: i32,
    pub leave: i32,
    #[serde(default)]
    pub tokens: Option<TokenExchange>,
}

/// Tokens for reputation at an NPC: one or all of them.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenExchange {
    pub answer: i32,
    pub node: i32,
    pub one: i32,
    pub all: i32,
    pub item: i32,
    pub faction: i32,
    pub points: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestStep {
    /// What the NPC asks; its answer takes the tasks. Without it the tasks
    /// come as soon as the step before is rewarded.
    #[serde(default)]
    pub offer: Option<QuestPage>,
    #[serde(default)]
    pub tasks: Vec<QuestTask>,
    #[serde(default)]
    pub waiting: Option<QuestPage>,
    /// When the tasks are done: its answer gives the reward. Without it the
    /// reward comes at once.
    #[serde(default)]
    pub done: Option<QuestPage>,
    /// The done page lists fish with this trait; the answer takes one.
    #[serde(default)]
    pub deliver_trait: i32,
    /// A line «Поговорить с ...» of the chain's category (QTask.xml) that joins
    /// the list once the tasks are done and goes with the reward: the NPC's
    /// check mark watches for it.
    #[serde(default)]
    pub report: i32,
    /// Another chain that has to be finished first.
    #[serde(default)]
    pub requires: Option<String>,
    #[serde(default)]
    pub reward: QuestReward,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    #[default]
    Catch,
    Make,
    Have,
    Talk,
    Pour,
}

/// One line in the client's task list. Fish tasks count fish put in the
/// keepnet; `weighed` ones count grams.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestTask {
    pub task: i32,
    /// When the line belongs to another NPC's category ("Поговорить с ...").
    #[serde(default)]
    pub category: i32,
    #[serde(default)]
    pub kind: TaskKind,
    #[serde(default)]
    pub count: i32,
    #[serde(default)]
    pub species: i32,
    #[serde(default)]
    pub bait: i32,
    #[serde(default)]
    pub min_grams: i32,
    #[serde(default)]
    pub max_grams: i32,
    #[serde(default)]
    pub base: i32,
    #[serde(default)]
    pub location: i32,
    #[serde(default)]
    pub trophy: bool,
    #[serde(default)]
    pub weighed: bool,
    /// 1 float, 2 feeder, 3 spinning.
    #[serde(default)]
    pub rod_kind: i32,
    /// For make and have.
    #[serde(default)]
    pub item: i32,
    /// For talk: the dialog of the NPC to talk to.
    #[serde(default)]
    pub dialog: i32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestReward {
    #[serde(default)]
    pub items: Vec<[i32; 2]>,
    /// Recipes learned for good.
    #[serde(default)]
    pub recipes: Vec<i32>,
    #[serde(default)]
    pub reputation: Vec<[i64; 2]>,
    #[serde(default)]
    pub experience: i32,
    /// Kopecks.
    #[serde(default)]
    pub money: i64,
    /// Ergs.
    #[serde(default)]
    pub premium: i64,
    /// Quest values set to 1 (what other places check).
    #[serde(default)]
    pub flags: Vec<String>,
    /// A title (`Titul.xml`).
    #[serde(default)]
    pub title: i32,
}

/// The boat at the lake: fuel it with biofuel, then go to its location for a
/// token.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoatSettings {
    pub base: i32,
    pub npc: i32,
    pub dialog: i32,
    pub location: i32,
    pub fuel_item: i32,
    pub fuel_percent: i32,
    pub token: i32,
    pub trip_percent: i32,
}

/// A VIP patent in the terminal: the dialog answer, days and ergs.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatentSettings {
    pub answer: i32,
    pub days: i64,
    pub premium: i64,
}

/// The boards of tasks at the bases.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardSettings {
    /// Offers on each tab but the daily one.
    pub offers: usize,
    pub daily_offers: usize,
    /// How long a tab's offers stay before new ones come.
    pub refresh_minutes: i64,
    pub daily_refresh_minutes: i64,
    /// Tasks of one tab a character holds at once.
    pub max_taken: usize,
    /// Between two daily tasks taken.
    pub daily_cooldown_minutes: i64,
    /// How many players can take one offer, from and to.
    pub copies: [i32; 2],
    /// Chances of the colours, black to gold.
    pub quality_weights: Vec<f64>,
    /// The base reputation level each colour asks for.
    pub quality_reputation: Vec<i32>,
    /// Each colour step adds this to the task's size and reward.
    pub quality_percent: f64,
    /// Fish to catch, from and to, before the colour.
    pub count: [i32; 2],
    /// Fish whose typical weight makes the target of a weighed task.
    pub weight_count: [i32; 2],
    pub trophies: [i32; 2],
    /// Income tasks pay this share of what the fish would sell for.
    pub income_percent: f64,
    /// Experience tasks give this share of the fish's own experience.
    pub experience_percent: f64,
    /// Base reputation per colour step plus one.
    pub reputation_points: i64,
    pub daily_income_percent: f64,
    pub daily_experience_percent: f64,
    /// Resource tasks give colour plus one pieces of one of these.
    pub resource_items: Vec<i32>,
    /// Base id to more resource rewards found only on that base's board.
    #[serde(default)]
    pub base_resource_items: BTreeMap<i32, Vec<i32>>,
}

/// The VIP patent: its price and what it gives.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VipSettings {
    /// Price in ergs, the premium money.
    pub premium_cost: i64,
    pub days: i64,
    /// Added to what the fish market pays.
    pub fish_price_percent: f64,
    /// Of the chance itself.
    pub loot_chance_percent: f64,
    /// Added to the experience for a fish.
    pub experience_percent: f64,
    /// Experience points for the skill on study while online.
    pub skill_points_per_minute: f64,
}

/// The doctor in a base's scene, who heals for a fish.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorSettings {
    /// The NPC's id in the base scene, which is also its dialog file.
    pub npc: i32,
    pub base: i32,
    /// The highest character level the doctor still heals.
    pub max_level: i32,
    /// A fish lighter than this is not taken.
    pub min_fish_grams: i32,
    pub cooldown_minutes: i64,
    pub life: f32,
    pub radiation: f32,
    pub food: f32,
    pub water: f32,
    /// Items handed over with each treatment, one piece each.
    #[serde(default)]
    pub gifts: Vec<i32>,
    /// What the doctor wants for opening the fridge at his base.
    pub fridge_unlock: Price,
    /// What he wants for each fridge card after that.
    pub fridge_card: Price,
}

/// Pieces of one item.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub item: i32,
    pub count: i32,
}

/// Wreaths for landing many fish of one species, shown in the statistics.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MasterySettings {
    /// Fish of the species landed for each wreath, in order.
    pub catches: Vec<i64>,
    /// How much more often a species bites per wreath, in percent.
    #[serde(default)]
    pub bite_percent: f64,
    /// How much likelier the species' card is per wreath, in percent of its chance.
    #[serde(default)]
    pub card_percent: f64,
}

/// One faction, as the client's RepFractions resource has it. Going from level
/// `n` to `n + 1` takes `start_a + start_b * kf_b * n` points, as the client
/// computes it for the bar in the reputation tab.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactionSettings {
    pub start_a: i64,
    pub start_b: i64,
    pub kf_b: f64,
    /// Level names the client has for the faction, the first being level 0.
    pub levels: i32,
    /// Its name in the client's RepFractions.xml.
    #[serde(default)]
    pub name: String,
}

/// Biofuel handed in at the generator.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorSettings {
    /// The NPC's id in the base scene, which is also its dialog file.
    pub npc: i32,
    pub base: i32,
    pub faction: i32,
    pub item: i32,
    pub points: i64,
}

/// Points for a fish let go: `points` plus one for every `grams_per_point`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSettings {
    pub points: i64,
    pub grams_per_point: i64,
}

/// A «Вклад» skill: points with a guild for fish landed on its kind of rod.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContributionSettings {
    pub perk: i32,
    pub rod_kind: i32,
    pub faction: i32,
}

/// What each level of a base's reputation adds there, in percent.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReputationBonuses {
    pub experience_percent: f64,
    pub loot_percent: f64,
    pub luck_percent: f64,
    pub base_card_percent: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReputationSettings {
    pub factions: BTreeMap<i32, FactionSettings>,
    /// The faction of each base.
    pub base_factions: BTreeMap<i32, i32>,
    pub generator: GeneratorSettings,
    pub release: ReleaseSettings,
    pub contribution: Vec<ContributionSettings>,
    /// By skill level from 1: points for each fish and grams for one point more.
    pub contribution_levels: Vec<[i64; 2]>,
    pub per_level: ReputationBonuses,
}

/// Fish kept on ice, one fridge per base.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FridgeSettings {
    /// Places in a fridge before any card is used.
    pub cells: i32,
    /// The card that adds a place to the fridge of the base it is used at.
    pub card: i32,
    /// Bases whose fridge stays shut until the doctor opens it.
    #[serde(default)]
    pub doctor_bases: Vec<i32>,
}

/// Studying skills. The cost of a level comes from the game's own tree.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSettings {
    /// Share of every experience gain that goes to the skill being studied.
    pub experience_percent: f64,
    /// Points a second copy of a course gives to its skill while it is studied.
    pub course_experience: f64,
    /// Points it gives instead to a studied sub-skill once the skill is at the top.
    pub course_sub_experience: f64,
    pub max_level: i32,
    /// Levels of all skills together a character may have: base + perLevel * level.
    pub limit_base: i32,
    pub limit_per_level: i32,
    /// A lure's `subType` to the skill that helps with it: «Джиги», «Воблеры»,
    /// «Блёсны».
    #[serde(default)]
    pub lure_skills: BTreeMap<i32, i32>,
    /// Bite chance added per point of that skill, in percent of the chance.
    #[serde(default)]
    pub lure_bite_percent: f64,
    /// The station that cuts fish, where «Полевая переработка рыбы» adds its
    /// points to the critical chance.
    #[serde(default)]
    pub fish_cutting_station: i32,
}

/// One skill of the game's tree.
#[derive(Debug, Clone, Deserialize)]
pub struct SkillNode {
    pub id: i32,
    /// 0 for a main skill.
    pub parent: i32,
    /// What one level gives, as the client's description uses it.
    pub effect: [f64; 3],
    /// Points to the next level: a + b * (c + level).
    pub cost: [f64; 3],
}

#[derive(Debug, Default)]
pub struct SkillTree {
    nodes: BTreeMap<i32, SkillNode>,
}

impl SkillTree {
    pub fn new(nodes: Vec<SkillNode>) -> Self {
        Self { nodes: nodes.into_iter().map(|node| (node.id, node)).collect() }
    }

    pub fn get(&self, id: i32) -> Option<&SkillNode> {
        self.nodes.get(&id)
    }

    pub fn children(&self, id: i32) -> impl Iterator<Item = &SkillNode> {
        self.nodes.values().filter(move |node| node.parent == id)
    }
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
    /// The `refine=` line: what «Переработать» turns the item into.
    #[serde(default)]
    pub refine: String,
    /// The `iname=` line, the item's name; `{0}` in a fish card's.
    #[serde(default)]
    pub name: String,
    /// Pieces of another item asked on top of the price (`costWpn=`): [item,
    /// pieces], zeros for none.
    #[serde(default)]
    pub cost_item: [i32; 2],
    /// Price in ergs, the premium money, for an item with no price in credits:
    /// the `pricePt` of its description, set when the settings are applied.
    #[serde(skip)]
    pub premium_cost: i64,
}

/// A whole number in an item's description, such as `<pricePt>20</pricePt>`.
fn tag_number(xml: &str, tag: &str) -> Option<i64> {
    let open = format!("<{tag}>");
    xml.split_once(open.as_str())?.1.split('<').next()?.trim().parse().ok()
}

#[derive(Debug)]
pub struct Catalog {
    /// In file order, which is the order shops list items.
    items: Vec<CatalogItem>,
    index: HashMap<i32, usize>,
}

impl Catalog {
    pub fn new(items: Vec<CatalogItem>) -> Self {
        let index = items.iter().enumerate().map(|(position, item)| (item.id, position)).collect();
        Self { items, index }
    }

    pub fn get(&self, id: i32) -> Option<&CatalogItem> {
        self.index.get(&id).map(|&position| &self.items[position])
    }

    pub fn for_sale(&self) -> impl Iterator<Item = &CatalogItem> {
        self.items.iter().filter(|item| item.cost > 0 || item.cost_item[0] > 0)
    }
}

/// One line of `data/recipes.json`, which the launcher extracts from the game.
#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    pub id: i32,
    pub station: i32,
    /// `showid` of the recipe; for collections it looks like the base. Not used yet.
    #[allow(dead_code)]
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
    /// «Конструирование» (kind 1): extra parts all components take together.
    #[serde(default)]
    pub max_parts: i32,
    /// `componentA`–`D`: part, pieces at least, what each piece more adds to
    /// the 1st, 2nd and 3rd numbers of the result's `basePar` (D: unused).
    #[serde(default)]
    pub components: [[f64; 3]; 4],
    /// The thing the constructor builds on, used up: item, pieces.
    #[serde(default)]
    pub passive: [f64; 3],
    /// `kgcount`: fish weight at least, then grams of fish per piece made.
    #[serde(default)]
    pub kg_count: [f64; 3],
}

pub struct GameData {
    pub settings: Settings,
    pub fish: FishTable,
    pub catalog: Catalog,
    pub recipes: Vec<Recipe>,
}

pub fn load(config_dir: &Path, root: &Path) -> Result<GameData> {
    let mut settings: Settings = read_commented_with_local(&config_dir.join("settings.json"))?;
    if settings.shop.guide_stock {
        settings.shop.stock = read_commented(&config_dir.join("shops.json"))?;
    }
    let quests = config_dir.join("quests.json");
    if quests.exists() {
        settings.quests = read_commented(&quests)?;
    }
    let gathering = config_dir.join("gathering.json");
    if gathering.exists() {
        settings.gathering = read_commented(&gathering)?;
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

    let catalog = Catalog::new(items);

    let recipes_path = root.join("data").join("recipes.json");
    let text = fs::read_to_string(&recipes_path).with_context(|| {
        format!("{} is missing; run af2-launcher first", recipes_path.display())
    })?;
    let recipes: Vec<Recipe> = serde_json::from_str(&text).with_context(|| recipes_path.display().to_string())?;

    let skills_path = root.join("data").join("skills.json");
    let text = fs::read_to_string(&skills_path).with_context(|| {
        format!("{} is missing; run af2-launcher first", skills_path.display())
    })?;
    let nodes: Vec<SkillNode> = serde_json::from_str(&text).with_context(|| skills_path.display().to_string())?;
    settings.skill_tree = SkillTree::new(nodes);

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
    fn an_item_without_credits_costs_its_ergs() {
        let shop = ShopSettings { hidden_items: vec![7], prices: [(5, 1000)].into(), ..Default::default() };
        let item = |id, cost| CatalogItem {
            id,
            kind: 34,
            cost,
            durability: 1.0,
            stacks: false,
            shop_count: 1,
            max_count: 1,
            premium_cost: 0,
            xml: "<priceCr>0</priceCr><pricePt>20</pricePt>".into(),
            refine: String::new(),
            name: String::new(),
            cost_item: [0, 0],
        };
        let prices = |mut item: CatalogItem| {
            shop.adjust(&mut item);
            (item.cost, item.premium_cost)
        };
        assert_eq!(prices(item(4, 0)), (0, 20));
        assert_eq!(prices(item(4, 300)), (300, 0));
        assert_eq!(prices(item(5, 0)), (1000, 0));
        assert_eq!(prices(item(7, 0)), (0, 0));
    }

    #[test]
    fn shop_settings_reprice_and_hide() {
        let shop = ShopSettings {
            enforce_levels: true,
            hidden_kinds: vec![29],
            hidden_items: vec![7],
            prices: [(5, 1000)].into(),
            ..Default::default()
        };
        let item = |id, kind| CatalogItem { id, kind, cost: 100, durability: 1.0, stacks: false, shop_count: 1, max_count: 1, premium_cost: 0, xml: String::new(), refine: String::new(), name: String::new(), cost_item: [0, 0] };
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
