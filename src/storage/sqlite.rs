use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{Sqlite, Transaction};

use super::{CaughtFish, CharacterData, CharacterSummary, ItemEntry, Storage};

const MAX_CONNECTIONS: u32 = 4;

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
            sqlx::query("INSERT INTO fish (id, character_id, species, grams, size_class, list, experience) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(fish.id)
                .bind(id)
                .bind(fish.species)
                .bind(fish.grams)
                .bind(fish.size_class)
                .bind(fish.list)
                .bind(fish.experience)
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
        let mut transaction = self.pool.begin().await?;
        let id = sqlx::query(
            "INSERT INTO characters (account_id, name, level, experience, money, premium) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(character.account)
        .bind(&character.name)
        .bind(character.level)
        .bind(character.experience)
        .bind(character.money)
        .bind(character.premium)
        .execute(&mut *transaction)
        .await?
        .last_insert_rowid();
        Self::insert_children(&mut transaction, id, character).await?;
        transaction.commit().await?;
        Ok(id)
    }

    async fn load_character(&self, id: i64) -> Result<Option<CharacterData>> {
        let row: Option<(i64, String, i32, i64, i64, i64)> = sqlx::query_as(
            "SELECT account_id, name, level, experience, money, premium FROM characters WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((account, name, level, experience, money, premium)) = row else {
            return Ok(None);
        };

        let items: Vec<(i64, i32, i32, i32, f64, i32)> = sqlx::query_as(
            "SELECT id, bag, cell, count, durability, template FROM items WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let fish: Vec<(i64, i32, i32, i32, i32, i32)> = sqlx::query_as(
            "SELECT id, species, grams, size_class, list, experience FROM fish WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let records: Vec<(i32, i32)> =
            sqlx::query_as("SELECT species, grams FROM records WHERE character_id = ?")
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
                .map(|(id, species, grams, size_class, list, experience)| CaughtFish {
                    id,
                    species,
                    grams,
                    size_class,
                    list,
                    experience,
                })
                .collect(),
            records: records.into_iter().collect(),
        }))
    }

    async fn save_character(&self, character: &CharacterData) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE characters SET level = ?, experience = ?, money = ?, premium = ? WHERE id = ?")
            .bind(character.level)
            .bind(character.experience)
            .bind(character.money)
            .bind(character.premium)
            .bind(character.id)
            .execute(&mut *transaction)
            .await?;
        for table in ["items", "fish", "records"] {
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
            items: vec![ItemEntry { id: 1, bag: 0, cell: 2, count: 10, durability: 1.5, template: 1010 }],
            fish: vec![CaughtFish { id: 2, species: 7, grams: 300, size_class: 1, list: 0, experience: 12 }],
            ..Default::default()
        };
        character.records.insert(7, 300);
        character.id = store.create_character(&character).await.unwrap();

        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);

        character.fish.clear();
        character.money = 900;
        store.save_character(&character).await.unwrap();
        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);
    }

    #[tokio::test]
    async fn ids_do_not_repeat() {
        let store = memory_store().await;
        let first = store.next_id().await.unwrap();
        assert_eq!(store.next_id().await.unwrap(), first + 1);
    }
}
