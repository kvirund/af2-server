//! Where accounts and characters live. Game code only sees [`Storage`], so
//! another database needs one more implementation and nothing else.

mod sqlite;

use std::collections::HashMap;

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
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaughtFish {
    pub id: i64,
    pub species: i32,
    pub grams: i32,
    pub size_class: i32,
    /// Which keepnet holds it: 0 is the one in use, 1 the spare.
    pub list: i32,
    /// Experience before the personal-record multiplier.
    pub experience: i32,
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
    /// Heaviest fish kept, in grams, by species id.
    pub records: HashMap<i32, i32>,
}

#[derive(Debug, Clone)]
pub struct CharacterSummary {
    pub id: i64,
    pub name: String,
    pub level: i32,
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
}
