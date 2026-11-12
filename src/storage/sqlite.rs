use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{Sqlite, Transaction};

use super::{CaughtFish, CharacterData, CharacterSummary, ItemEntry, Skill, Storage, UNKNOWN_BAIT, Vitals};

const MAX_CONNECTIONS: u32 = 4;

/// `schema.sql` only creates missing tables, so a column added later needs this.
async fn add_column(pool: &SqlitePool, table: &str, column: &str, declaration: &str) -> Result<()> {
    let info = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = '{column}'");
    let found: Option<(i64,)> = sqlx::query_as(&info).fetch_optional(pool).await?;
    if found.is_none() {
        sqlx::raw_sql(&format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}")).execute(pool).await?;
    }
    Ok(())
}

async fn add_missing_columns(pool: &SqlitePool) -> Result<()> {
    add_column(pool, "fish", "bait", &format!("INTEGER NOT NULL DEFAULT {UNKNOWN_BAIT}")).await?;
    add_column(pool, "characters", "vip", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(pool, "characters", "life", "REAL").await?;
    for column in ["stamina", "radiation", "food", "water", "work_points"] {
        add_column(pool, "characters", column, "REAL NOT NULL DEFAULT 0").await?;
    }
    add_column(pool, "characters", "studying", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(pool, "characters", "healed_at", "INTEGER NOT NULL DEFAULT 0").await?;
    Ok(())
}

fn vitals_columns(character: &CharacterData) -> (Option<f32>, f32, f32, f32, f32, f32) {
    match &character.vitals {
        Some(Vitals { values: [life, stamina, radiation, food, water], work_points }) => {
            (Some(*life), *stamina, *radiation, *food, *water, *work_points)
        }
        None => (None, 0.0, 0.0, 0.0, 0.0, 0.0),
    }
}

pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(include_str!("schema.sql")).execute(&pool).await?;
        add_missing_columns(&pool).await?;
        Ok(Self { pool })
    }

    async fn insert_children(
        transaction: &mut Transaction<'_, Sqlite>,
        id: i64,
        character: &CharacterData,
    ) -> Result<()> {
        for item in &character.items {
            sqlx::query("INSERT INTO items (id, character_id, bag, cell, count, durability, template) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(item.id)
                .bind(id)
                .bind(item.bag)
                .bind(item.cell)
                .bind(item.count)
                .bind(f64::from(item.durability))
                .bind(item.template)
                .execute(&mut **transaction)
                .await?;
        }
        for fish in &character.fish {
            sqlx::query("INSERT INTO fish (id, character_id, species, grams, size_class, list, experience, bait) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(fish.id)
                .bind(id)
                .bind(fish.species)
                .bind(fish.grams)
                .bind(fish.size_class)
                .bind(fish.list)
                .bind(fish.experience)
                .bind(fish.bait)
                .execute(&mut **transaction)
                .await?;
        }
        for (species, grams) in &character.records {
            sqlx::query("INSERT INTO records (character_id, species, grams) VALUES (?, ?, ?)")
                .bind(id)
                .bind(species)
                .bind(grams)
                .execute(&mut **transaction)
                .await?;
        }
        for (bag, cells) in &character.bag_extra {
            sqlx::query("INSERT INTO bag_extra (character_id, bag, cells) VALUES (?, ?, ?)")
                .bind(id)
                .bind(bag)
                .bind(cells)
                .execute(&mut **transaction)
                .await?;
        }
        for (recipe, uses) in &character.known_recipes {
            sqlx::query("INSERT INTO known_recipes (character_id, recipe, uses) VALUES (?, ?, ?)")
                .bind(id)
                .bind(recipe)
                .bind(uses)
                .execute(&mut **transaction)
                .await?;
        }
        for (name, value) in &character.quest_values {
            sqlx::query("INSERT INTO quest_values (character_id, name, value) VALUES (?, ?, ?)")
                .bind(id)
                .bind(name)
                .bind(value)
                .execute(&mut **transaction)
                .await?;
        }
        for (perk, skill) in &character.skills {
            sqlx::query("INSERT INTO skills (character_id, perk, level, experience) VALUES (?, ?, ?, ?)")
                .bind(id)
                .bind(perk)
                .bind(skill.level)
                .bind(skill.experience)
                .execute(&mut **transaction)
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn account_for(&self, key: &str) -> Result<i64> {
        sqlx::query("INSERT OR IGNORE INTO accounts (login_key) VALUES (?)")
            .bind(key)
            .execute(&self.pool)
            .await?;
        let (id,): (i64,) = sqlx::query_as("SELECT id FROM accounts WHERE login_key = ?")
            .bind(key)
            .fetch_one(&self.pool)
            .await?;
        Ok(id)
    }

    async fn list_characters(&self, account: i64) -> Result<Vec<CharacterSummary>> {
        let rows: Vec<(i64, String, i32)> =
            sqlx::query_as("SELECT id, name, level FROM characters WHERE account_id = ? ORDER BY id")
                .bind(account)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id, name, level)| CharacterSummary { id, name, level }).collect())
    }

    async fn create_character(&self, character: &CharacterData) -> Result<i64> {
        let (life, stamina, radiation, food, water, work_points) = vitals_columns(character);
        let mut transaction = self.pool.begin().await?;
        let id = sqlx::query(
            "INSERT INTO characters (account_id, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(character.account)
        .bind(&character.name)
        .bind(character.level)
        .bind(character.experience)
        .bind(character.money)
        .bind(character.premium)
        .bind(character.vip)
        .bind(life)
        .bind(stamina)
        .bind(radiation)
        .bind(food)
        .bind(water)
        .bind(work_points)
        .bind(character.studying)
        .bind(character.healed_at)
        .execute(&mut *transaction)
        .await?
        .last_insert_rowid();
        Self::insert_children(&mut transaction, id, character).await?;
        transaction.commit().await?;
        Ok(id)
    }

    async fn load_character(&self, id: i64) -> Result<Option<CharacterData>> {
        let row: Option<(i64, String, i32, i64, i64, i64, bool, Option<f32>, f32, f32, f32, f32, f32, i32, i64)> = sqlx::query_as(
            "SELECT account_id, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at FROM characters WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((account, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at)) = row else {
            return Ok(None);
        };

        let items: Vec<(i64, i32, i32, i32, f64, i32)> = sqlx::query_as(
            "SELECT id, bag, cell, count, durability, template FROM items WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let fish: Vec<(i64, i32, i32, i32, i32, i32, i32)> = sqlx::query_as(
            "SELECT id, species, grams, size_class, list, experience, bait FROM fish WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let records: Vec<(i32, i32)> =
            sqlx::query_as("SELECT species, grams FROM records WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let bag_extra: Vec<(i32, i32)> =
            sqlx::query_as("SELECT bag, cells FROM bag_extra WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let known_recipes: Vec<(i32, i32)> =
            sqlx::query_as("SELECT recipe, uses FROM known_recipes WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let skills: Vec<(i32, i32, f64)> =
            sqlx::query_as("SELECT perk, level, experience FROM skills WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let quest_values: Vec<(String, i32)> =
            sqlx::query_as("SELECT name, value FROM quest_values WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        Ok(Some(CharacterData {
            id,
            account,
            name,
            level,
            experience,
            money,
            premium,
            vip,
            vitals: life.map(|life| Vitals { values: [life, stamina, radiation, food, water], work_points }),
            items: items
                .into_iter()
                .map(|(id, bag, cell, count, durability, template)| ItemEntry {
                    id,
                    bag,
                    cell,
                    count,
                    durability: durability as f32,
                    template,
                })
                .collect(),
            fish: fish
                .into_iter()
                .map(|(id, species, grams, size_class, list, experience, bait)| CaughtFish {
                    id,
                    species,
                    grams,
                    size_class,
                    bait,
                    list,
                    experience,
                })
                .collect(),
            records: records.into_iter().collect(),
            bag_extra: bag_extra.into_iter().collect(),
            known_recipes: known_recipes.into_iter().collect(),
            skills: skills.into_iter().map(|(perk, level, experience)| (perk, Skill { level, experience })).collect(),
            quest_values: quest_values.into_iter().collect(),
            studying,
            healed_at,
        }))
    }

    async fn save_character(&self, character: &CharacterData) -> Result<()> {
        let (life, stamina, radiation, food, water, work_points) = vitals_columns(character);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE characters SET level = ?, experience = ?, money = ?, premium = ?, vip = ?, life = ?, stamina = ?, radiation = ?, food = ?, water = ?, work_points = ?, studying = ?, healed_at = ? WHERE id = ?")
            .bind(character.level)
            .bind(character.experience)
            .bind(character.money)
            .bind(character.premium)
            .bind(character.vip)
            .bind(life)
            .bind(stamina)
            .bind(radiation)
            .bind(food)
            .bind(water)
            .bind(work_points)
            .bind(character.studying)
            .bind(character.healed_at)
            .bind(character.id)
            .execute(&mut *transaction)
            .await?;
        for table in ["items", "fish", "records", "bag_extra", "known_recipes", "skills", "quest_values"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE character_id = ?"))
                .bind(character.id)
                .execute(&mut *transaction)
                .await?;
        }
        Self::insert_children(&mut transaction, character.id, character).await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn next_id(&self) -> Result<i64> {
        let (id,): (i64,) =
            sqlx::query_as("UPDATE id_sequence SET next_id = next_id + 1 RETURNING next_id - 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_store() -> SqliteStorage {
        let options = SqliteConnectOptions::new().in_memory(true);
        let pool =
            SqlitePoolOptions::new().max_connections(1).connect_with(options).await.unwrap();
        sqlx::raw_sql(include_str!("schema.sql")).execute(&pool).await.unwrap();
        SqliteStorage { pool }
    }

    #[tokio::test]
    async fn same_key_is_the_same_account() {
        let store = memory_store().await;
        let first = store.account_for("a").await.unwrap();
        assert_eq!(store.account_for("a").await.unwrap(), first);
        assert_ne!(store.account_for("b").await.unwrap(), first);
    }

    #[tokio::test]
    async fn character_round_trips() {
        let store = memory_store().await;
        let account = store.account_for("a").await.unwrap();
        let mut character = CharacterData {
            account,
            name: "Рыбак".into(),
            level: 3,
            experience: 40,
            money: 500,
            premium: 5,
            vip: true,
            vitals: Some(Vitals { values: [90.0, 80.5, 70.0, 60.0, 50.0], work_points: 12.5 }),
            items: vec![ItemEntry { id: 1, bag: 0, cell: 2, count: 10, durability: 1.5, template: 1010 }],
            fish: vec![CaughtFish { id: 2, species: 7, grams: 300, size_class: 1, bait: 1016, list: 0, experience: 12 }],
            ..Default::default()
        };
        character.records.insert(7, 300);
        character.bag_extra.insert(52, 5);
        character.known_recipes.insert(142, 10);
        character.skills.insert(100, Skill { level: 2, experience: 150.5 });
        character.studying = 100;
        character.healed_at = 1_700_000_000;
        character.quest_values.insert("holod_1".into(), 3);
        character.id = store.create_character(&character).await.unwrap();

        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);

        character.fish.clear();
        character.money = 900;
        store.save_character(&character).await.unwrap();
        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);
    }

    #[tokio::test]
    async fn old_catches_get_a_bait() {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql("CREATE TABLE fish (id INTEGER PRIMARY KEY); INSERT INTO fish (id) VALUES (1); CREATE TABLE characters (id INTEGER PRIMARY KEY); INSERT INTO characters (id) VALUES (1)")
            .execute(&pool)
            .await
            .unwrap();

        add_missing_columns(&pool).await.unwrap();
        add_missing_columns(&pool).await.unwrap();

        let (bait,): (i32,) = sqlx::query_as("SELECT bait FROM fish").fetch_one(&pool).await.unwrap();
        assert_eq!(bait, UNKNOWN_BAIT);
        let (vip,): (bool,) = sqlx::query_as("SELECT vip FROM characters").fetch_one(&pool).await.unwrap();
        assert!(!vip);
    }

    #[tokio::test]
    async fn ids_do_not_repeat() {
        let store = memory_store().await;
        let first = store.next_id().await.unwrap();
        assert_eq!(store.next_id().await.unwrap(), first + 1);
    }
}
