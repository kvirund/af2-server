use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::{Sqlite, Transaction};

use super::{
    BoardTask, Catches, CaughtFish, CharacterData, CharacterSummary, ItemEntry, Record, Skill, Storage, StoredGatherPoint, UNKNOWN_BAIT, Vitals,
    parse_traits, traits_text,
};

const MAX_CONNECTIONS: u32 = 4;

/// `schema.sql` only creates missing tables, so a column added later needs this.
/// A table that is not there is left alone.
async fn add_column(pool: &SqlitePool, table: &str, column: &str, declaration: &str) -> Result<()> {
    let exists = format!("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '{table}'");
    if sqlx::query_as::<_, (i64,)>(&exists).fetch_optional(pool).await?.is_none() {
        return Ok(());
    }
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
    add_column(pool, "records", "location", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(pool, "records", "bait", &format!("INTEGER NOT NULL DEFAULT {UNKNOWN_BAIT}")).await?;
    add_column(pool, "items", "param", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(pool, "board_tasks", "kind", "INTEGER NOT NULL DEFAULT 1").await?;
    add_column(pool, "characters", "vip_until", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(pool, "fish", "traits", "TEXT NOT NULL DEFAULT ''").await?;
    add_column(pool, "characters", "appearance", "BLOB NOT NULL DEFAULT x''").await?;
    add_column(pool, "items", "parts", "TEXT NOT NULL DEFAULT ''").await?;
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
            sqlx::query("INSERT INTO items (id, character_id, bag, cell, count, durability, template, param, parts) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(item.id)
                .bind(id)
                .bind(item.bag)
                .bind(item.cell)
                .bind(item.count)
                .bind(f64::from(item.durability))
                .bind(item.template)
                .bind(item.param)
                .bind(super::parts_text(&item.parts))
                .execute(&mut **transaction)
                .await?;
        }
        for fish in &character.fish {
            sqlx::query("INSERT INTO fish (id, character_id, species, grams, size_class, list, experience, bait, traits) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(fish.id)
                .bind(id)
                .bind(fish.species)
                .bind(fish.grams)
                .bind(fish.size_class)
                .bind(fish.list)
                .bind(fish.experience)
                .bind(fish.bait)
                .bind(traits_text(&fish.traits))
                .execute(&mut **transaction)
                .await?;
        }
        for (species, record) in &character.records {
            sqlx::query("INSERT INTO records (character_id, species, grams, location, bait) VALUES (?, ?, ?, ?, ?)")
                .bind(id)
                .bind(species)
                .bind(record.grams)
                .bind(record.location)
                .bind(record.bait)
                .execute(&mut **transaction)
                .await?;
        }
        for (species, catches) in &character.catches {
            sqlx::query("INSERT INTO catches (character_id, species, count, grams) VALUES (?, ?, ?, ?)")
                .bind(id)
                .bind(species)
                .bind(catches.count)
                .bind(catches.grams)
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
        for task in &character.board_tasks {
            sqlx::query("INSERT INTO board_tasks (id, character_id, base, category, task, quality, title, species, bait, target, min_grams, max_grams, progress, reward, ends_at, kind) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(task.id)
                .bind(id)
                .bind(task.base)
                .bind(task.category)
                .bind(task.task)
                .bind(task.quality)
                .bind(task.title)
                .bind(task.species)
                .bind(task.bait)
                .bind(task.target)
                .bind(task.min_grams)
                .bind(task.max_grams)
                .bind(task.progress)
                .bind(&task.reward)
                .bind(task.ends_at)
                .bind(task.kind)
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
        for (faction, points) in &character.reputation {
            sqlx::query("INSERT INTO reputation (character_id, faction, points) VALUES (?, ?, ?)")
                .bind(id)
                .bind(faction)
                .bind(points)
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
        let rows: Vec<(i64, String, i32, Vec<u8>)> =
            sqlx::query_as("SELECT id, name, level, appearance FROM characters WHERE account_id = ? ORDER BY id")
                .bind(account)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(id, name, level, appearance)| CharacterSummary { id, name, level, appearance }).collect())
    }

    async fn name_taken(&self, name: &str) -> Result<bool> {
        let names: Vec<(String,)> = sqlx::query_as("SELECT name FROM characters").fetch_all(&self.pool).await?;
        let wanted = name.to_lowercase();
        Ok(names.iter().any(|(name,)| name.to_lowercase() == wanted))
    }

    async fn create_character(&self, character: &CharacterData) -> Result<i64> {
        let (life, stamina, radiation, food, water, work_points) = vitals_columns(character);
        let mut transaction = self.pool.begin().await?;
        let id = sqlx::query(
            "INSERT INTO characters (account_id, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at, vip_until, appearance) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(character.vip_until)
        .bind(&character.appearance)
        .execute(&mut *transaction)
        .await?
        .last_insert_rowid();
        Self::insert_children(&mut transaction, id, character).await?;
        transaction.commit().await?;
        Ok(id)
    }

    async fn load_character(&self, id: i64) -> Result<Option<CharacterData>> {
        let row: Option<(i64, String, i32, i64, i64, i64, bool, Option<f32>, f32, f32, f32, f32, f32, i32, i64, i64)> = sqlx::query_as(
            "SELECT account_id, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at, vip_until FROM characters WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((account, name, level, experience, money, premium, vip, life, stamina, radiation, food, water, work_points, studying, healed_at, vip_until)) = row else {
            return Ok(None);
        };

        let items: Vec<(i64, i32, i32, i32, f64, i32, i32, String)> = sqlx::query_as(
            "SELECT id, bag, cell, count, durability, template, param, parts FROM items WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let fish: Vec<(i64, i32, i32, i32, i32, i32, i32, String)> = sqlx::query_as(
            "SELECT id, species, grams, size_class, list, experience, bait, traits FROM fish WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let (appearance,): (Vec<u8>,) = sqlx::query_as("SELECT appearance FROM characters WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;

        let records: Vec<(i32, i32, i32, i32)> =
            sqlx::query_as("SELECT species, grams, location, bait FROM records WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let catches: Vec<(i32, i64, i64)> =
            sqlx::query_as("SELECT species, count, grams FROM catches WHERE character_id = ?")
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

        #[allow(clippy::type_complexity)]
        let board_tasks: Vec<(i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, String, i64, i32)> = sqlx::query_as(
            "SELECT id, base, category, task, quality, title, species, bait, target, min_grams, max_grams, progress, reward, ends_at, kind FROM board_tasks WHERE character_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let quest_values: Vec<(String, i32)> =
            sqlx::query_as("SELECT name, value FROM quest_values WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        let reputation: Vec<(i32, i64)> =
            sqlx::query_as("SELECT faction, points FROM reputation WHERE character_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;

        Ok(Some(CharacterData {
            id,
            account,
            appearance,
            name,
            level,
            experience,
            money,
            premium,
            vip,
            vip_until,
            vitals: life.map(|life| Vitals { values: [life, stamina, radiation, food, water], work_points }),
            items: items
                .into_iter()
                .map(|(id, bag, cell, count, durability, template, param, parts)| ItemEntry {
                    id,
                    bag,
                    cell,
                    count,
                    durability: durability as f32,
                    template,
                    param,
                    parts: super::parse_parts(&parts),
                })
                .collect(),
            fish: fish
                .into_iter()
                .map(|(id, species, grams, size_class, list, experience, bait, traits)| CaughtFish {
                    id,
                    species,
                    grams,
                    size_class,
                    bait,
                    list,
                    experience,
                    traits: parse_traits(&traits),
                })
                .collect(),
            records: records
                .into_iter()
                .map(|(species, grams, location, bait)| (species, Record { grams, location, bait }))
                .collect(),
            catches: catches.into_iter().map(|(species, count, grams)| (species, Catches { count, grams })).collect(),
            bag_extra: bag_extra.into_iter().collect(),
            known_recipes: known_recipes.into_iter().collect(),
            skills: skills.into_iter().map(|(perk, level, experience)| (perk, Skill { level, experience })).collect(),
            quest_values: quest_values.into_iter().collect(),
            reputation: reputation.into_iter().collect(),
            studying,
            healed_at,
            board_tasks: board_tasks
                .into_iter()
                .map(|(id, base, category, task, quality, title, species, bait, target, min_grams, max_grams, progress, reward, ends_at, kind)| BoardTask {
                    id,
                    kind,
                    base,
                    category,
                    task,
                    quality,
                    title,
                    species,
                    bait,
                    target,
                    min_grams,
                    max_grams,
                    progress,
                    reward,
                    ends_at,
                })
                .collect(),
        }))
    }

    async fn save_character(&self, character: &CharacterData) -> Result<()> {
        let (life, stamina, radiation, food, water, work_points) = vitals_columns(character);
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE characters SET level = ?, experience = ?, money = ?, premium = ?, vip = ?, life = ?, stamina = ?, radiation = ?, food = ?, water = ?, work_points = ?, studying = ?, healed_at = ?, vip_until = ? WHERE id = ?")
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
            .bind(character.vip_until)
            .bind(character.id)
            .execute(&mut *transaction)
            .await?;
        for table in ["items", "fish", "records", "catches", "bag_extra", "known_recipes", "skills", "quest_values", "reputation", "board_tasks"] {
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

    async fn load_gather_points(&self) -> Result<Vec<StoredGatherPoint>> {
        let rows: Vec<(i32, i32, i32, i32, i64)> =
            sqlx::query_as("SELECT base, point, kind, resource, respawn_at FROM gather_points ORDER BY base, point")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(base, point, kind, resource, respawn_at)| StoredGatherPoint { base, point, kind, resource, respawn_at })
            .collect())
    }

    async fn save_gather_points(&self, base: i32, points: &[StoredGatherPoint]) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM gather_points WHERE base = ?").bind(base).execute(&mut *transaction).await?;
        for point in points {
            sqlx::query("INSERT INTO gather_points (base, point, kind, resource, respawn_at) VALUES (?, ?, ?, ?, ?)")
                .bind(base)
                .bind(point.point)
                .bind(point.kind)
                .bind(point.resource)
                .bind(point.respawn_at)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
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
    async fn gather_points_of_a_base_are_replaced_together() {
        let store = memory_store().await;
        let point = |base, point, resource| StoredGatherPoint { base, point, kind: 1, resource, respawn_at: 1_700_000_000 };
        store.save_gather_points(1, &[point(1, 5, 101), point(1, 6, 0)]).await.unwrap();
        store.save_gather_points(2, &[point(2, 5, 102)]).await.unwrap();
        store.save_gather_points(1, &[point(1, 5, 0)]).await.unwrap();
        assert_eq!(store.load_gather_points().await.unwrap(), vec![point(1, 5, 0), point(2, 5, 102)]);
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
            vip_until: 1_800_000_000,
            vitals: Some(Vitals { values: [90.0, 80.5, 70.0, 60.0, 50.0], work_points: 12.5 }),
            items: vec![ItemEntry { id: 1, bag: 0, cell: 2, count: 10, durability: 1.5, template: 1010, param: 0, parts: [0; 4] }, ItemEntry { id: 3, bag: 1, cell: 4, count: 2, durability: 1.0, template: 1309, param: 77, parts: [2, 0, 5, 3] }],
            fish: vec![CaughtFish { id: 2, species: 7, grams: 300, size_class: 1, bait: 1016, list: 0, experience: 12, traits: vec![108] }],
            appearance: vec![2, 3, 1, 4, 0, 1, 9, 8, 7, 6, 5, 4, 3, 2, 0, 0, 0, 1, 0],
            ..Default::default()
        };
        character.records.insert(7, Record { grams: 300, location: 4, bait: 1016 });
        character.catches.insert(7, Catches { count: 3, grams: 700 });
        character.bag_extra.insert(52, 5);
        character.known_recipes.insert(142, 10);
        character.skills.insert(100, Skill { level: 2, experience: 150.5 });
        character.studying = 100;
        character.healed_at = 1_700_000_000;
        character.quest_values.insert("holod_1".into(), 3);
        character.board_tasks.push(BoardTask { id: 900, base: 1, category: 1000, task: 2, target: 5, reward: "money=100".into(), ..Default::default() });
        character.reputation.insert(1, 450);
        character.id = store.create_character(&character).await.unwrap();

        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);

        character.fish.clear();
        character.money = 900;
        store.save_character(&character).await.unwrap();
        assert_eq!(store.load_character(character.id).await.unwrap().unwrap(), character);

        let listed = store.list_characters(account).await.unwrap();
        assert_eq!(listed[0].appearance, character.appearance);
        assert!(store.name_taken("рыбак").await.unwrap());
        assert!(!store.name_taken("Рыба").await.unwrap());
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
