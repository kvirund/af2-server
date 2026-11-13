//! Where accounts and characters live. Game code only sees [`Storage`], so
//! another database needs one more implementation and nothing else.

mod sqlite;

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use async_trait::async_trait;

pub use sqlite::SqliteStorage;

#[derive(Debug, Clone, PartialEq)]
pub struct ItemEntry {
    pub id: i64,
    pub bag: i32,
    pub cell: i32,
    pub count: i32,
    pub durability: f32,
    pub template: i32,
    /// What this one piece is of, where the template leaves it open: the
    /// species of a fish card. 0 for everything else.
    pub param: i32,
}

/// Worms: shown for catches saved before baits were recorded.
pub const UNKNOWN_BAIT: i32 = 1010;

#[derive(Debug, Clone, PartialEq)]
pub struct CaughtFish {
    pub id: i64,
    pub species: i32,
    pub grams: i32,
    pub size_class: i32,
    /// Item template of the bait; the catch window needs it to draw anything.
    pub bait: i32,
    /// Which keepnet holds it: 0 is the one in use, 1 the spare.
    pub list: i32,
    /// Experience before the personal-record multiplier.
    pub experience: i32,
}

/// The heaviest fish of a species kept, and where and on what it was caught.
/// Records from before the place was kept have location 0 and the unknown bait.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Record {
    pub grams: i32,
    pub location: i32,
    pub bait: i32,
}

/// How many fish of a species were landed and their weight together.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Catches {
    pub count: i64,
    pub grams: i64,
}

/// Life, stamina, radiation resistance, food and water, plus work points.
#[derive(Debug, Clone, PartialEq)]
pub struct Vitals {
    pub values: [f32; 5],
    pub work_points: f32,
}

/// A skill the character has studied: its level without gear bonuses and the
/// points gathered towards the next one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Skill {
    pub level: i32,
    pub experience: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CharacterData {
    pub id: i64,
    pub account: i64,
    pub name: String,
    pub level: i32,
    pub experience: i64,
    pub money: i64,
    pub premium: i64,
    pub items: Vec<ItemEntry>,
    pub fish: Vec<CaughtFish>,
    /// Heaviest fish kept, by species id.
    pub records: HashMap<i32, Record>,
    /// Fish landed, by species id.
    pub catches: BTreeMap<i32, Catches>,
    /// Cells bought on top of the starting size, by bag.
    pub bag_extra: HashMap<i32, i32>,
    /// Uses left of each learned recipe, negative meaning no limit.
    pub known_recipes: HashMap<i32, i32>,
    /// Skills opened by a course, by perk id; sub-skills start at level 0.
    pub skills: BTreeMap<i32, Skill>,
    /// Named numbers the original's item and quest scripts read and set with
    /// `getQValue` and `setQValue`, such as `holod_1`, the fridge cards used at
    /// base 1. A missing name reads as 0.
    pub quest_values: BTreeMap<String, i32>,
    /// Reputation points earned with each faction, all levels together.
    pub reputation: BTreeMap<i32, i64>,
    /// The skill experience goes to, 0 for none.
    pub studying: i32,
    /// When the doctor last healed the character, Unix seconds; 0 for never.
    pub healed_at: i64,
    /// VIP for good, set by hand in the database: the bought patent is `vip_until`.
    pub vip: bool,
    /// When the bought VIP patent ends, Unix seconds; 0 for never bought.
    pub vip_until: i64,
    /// `None` until the character first enters the game: it starts at full.
    pub vitals: Option<Vitals>,
}

#[derive(Debug, Clone)]
pub struct CharacterSummary {
    pub id: i64,
    pub name: String,
    pub level: i32,
}

/// A gather point of a base scene, shared by every player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StoredGatherPoint {
    pub base: i32,
    pub point: i32,
    /// The point's type in the scene, which says what can grow on it.
    pub kind: i32,
    /// What lies there now, 0 for nothing.
    pub resource: i32,
    /// When an empty point grows again, Unix seconds.
    pub respawn_at: i64,
}

#[async_trait]
pub trait Storage: Send + Sync {
    /// Finds the account for a login key, creating it the first time.
    async fn account_for(&self, key: &str) -> Result<i64>;

    async fn list_characters(&self, account: i64) -> Result<Vec<CharacterSummary>>;

    /// Stores a new character and returns its id; `character.id` is ignored.
    async fn create_character(&self, character: &CharacterData) -> Result<i64>;

    async fn load_character(&self, id: i64) -> Result<Option<CharacterData>>;

    async fn save_character(&self, character: &CharacterData) -> Result<()>;

    /// An id for an item or fish, unique across all characters.
    async fn next_id(&self) -> Result<i64>;

    async fn load_gather_points(&self) -> Result<Vec<StoredGatherPoint>>;

    /// Replaces what is kept for this base.
    async fn save_gather_points(&self, base: i32, points: &[StoredGatherPoint]) -> Result<()>;
}
