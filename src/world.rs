//! What every connection shares. A character's own state belongs to its
//! connection's task, so game code needs no locks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Local;
use serde::Deserialize;
use tokio::sync::{Notify, mpsc, oneshot};

use crate::config::{Catalog, FishTable, Recipe, Settings};
use crate::storage::{BoardTask, CharacterData, Storage, StoredGatherPoint};

/// Where the bait went last time at a location.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct LastCast {
    pub depth: f64,
    pub bottom: f64,
    /// The bait's template, 0 for none.
    pub bait: i32,
    /// Unix seconds.
    pub at: i64,
}

/// A character in play as the web pages see it: a copy its connection
/// refreshes every few seconds, so the pages never wait on the game.
#[derive(Debug, Clone)]
pub struct LiveCharacter {
    pub data: CharacterData,
    pub connection: u64,
    pub base: i32,
    pub location: i32,
    /// When the character entered the game, Unix seconds.
    pub entered_at: i64,
    pub dead: bool,
    /// The last cast at each location this session.
    pub casts: BTreeMap<i32, LastCast>,
}

pub struct Log {
    trace: Option<Mutex<File>>,
}

impl Log {
    pub fn new(trace: Option<File>) -> Self {
        Self { trace: trace.map(Mutex::new) }
    }

    pub fn line(&self, text: &str) {
        let stamped = format!("{} {text}", Local::now().format("%H:%M:%S%.3f"));
        println!("{stamped}");
        if let Some(trace) = &self.trace {
            let mut file = trace.lock().expect("a logging thread panicked");
            // A full disk must not take the game down with it.
            let _ = writeln!(file, "{stamped}");
        }
    }
}

/// One change asked for from outside the game, carried to the connection that
/// plays the character so the game's own code applies it and the client hears
/// about it. `answer` says what happened, or why nothing did.
#[derive(Debug)]
pub struct AdminJob {
    pub action: AdminAction,
    pub answer: oneshot::Sender<Result<String, String>>,
}

/// What the admin API can ask for. Each one is served by the function the game
/// itself uses, so nothing here is a second copy of a rule.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum AdminAction {
    /// Puts items in the character's bags, as a shop or a reward would.
    Give { template: i32, #[serde(default = "one")] count: i32 },
    /// Experience as a caught fish gives it, level-ups and all.
    Experience { amount: i32 },
    Reputation { faction: i32, points: i64 },
    Money { amount: i64 },
    Premium { amount: i64 },
    /// Pays a board task's reward and takes the task off the list.
    FinishTask { task: i64 },
    /// Takes a board task off the list and pays nothing.
    DropTask { task: i64 },
}

fn one() -> i32 {
    1
}

struct OnlineEntry {
    connection: u64,
    kick: Arc<Notify>,
    admin: mpsc::Sender<AdminJob>,
}

/// Which connection is playing each character.
#[derive(Default)]
struct OnlineCharacters {
    entries: Mutex<HashMap<i64, OnlineEntry>>,
}

impl OnlineCharacters {
    fn claim(&self, character: i64, connection: u64, kick: Arc<Notify>, admin: mpsc::Sender<AdminJob>) -> bool {
        let entry = OnlineEntry { connection, kick, admin };
        let previous = self.entries.lock().expect("a game thread panicked").insert(character, entry);
        match previous {
            Some(old) if old.connection != connection => {
                old.kick.notify_one();
                true
            }
            _ => false,
        }
    }

    fn release(&self, character: i64, connection: u64) {
        let mut entries = self.entries.lock().expect("a game thread panicked");
        if entries.get(&character).is_some_and(|entry| entry.connection == connection) {
            entries.remove(&character);
        }
    }
}

/// The offers of one tab of a base's board. Every player sees the same ones;
/// they live only while the server runs.
#[derive(Debug, Default)]
pub struct BoardOffers {
    /// When new offers replace these, Unix seconds.
    pub refresh_at: i64,
    pub offers: Vec<BoardOffer>,
}

#[derive(Debug, Clone)]
pub struct BoardOffer {
    pub id: i64,
    /// What the character gets on taking it, but for its own id.
    pub task: BoardTask,
    /// The base reputation level it asks for.
    pub reputation: i32,
    pub copies: i32,
    pub takers: HashSet<i64>,
}

/// A gather point of a base scene. Every player sees the same ones.
pub struct GatherPoint {
    pub kind: i32,
    /// The client's `restype_N`, or 0 while the point is empty.
    pub resource: i32,
    /// When an empty point grows again, Unix seconds: it keeps counting while
    /// the server is off.
    pub respawn_at: i64,
}

/// Everything the server reads from `config/` and `data/`, and nothing else.
/// Re-reading those files builds a new `World` around the same live state, so
/// each of these is replaced whole or not at all.
///
/// Everything below them is alive — who is playing, the boards already drawn,
/// the points already picked — and is held behind an `Arc` for that reason: a
/// reload hands the new `World` the very same state, never a copy of it.
pub struct World {
    pub settings: Settings,
    /// Base to its points, filled the first time anyone is at that base.
    pub gather_points: Arc<Mutex<HashMap<i32, BTreeMap<i32, GatherPoint>>>>,
    /// Base and tab to its offers, made the first time anyone looks.
    pub boards: Arc<Mutex<HashMap<(i32, i32), BoardOffers>>>,
    next_offer: Arc<AtomicI64>,
    pub fish: FishTable,
    pub catalog: Catalog,
    pub recipes: Vec<Recipe>,
    pub store: Arc<dyn Storage>,
    pub log: Arc<Log>,
    online: Arc<OnlineCharacters>,
    live: Arc<Mutex<HashMap<i64, LiveCharacter>>>,
    /// Characters with a letter they have not been told of.
    mail_notices: Arc<Mutex<HashSet<i64>>>,
}

/// The world as it stands. A handler takes a pointer out of this when a message
/// arrives and keeps that one for the whole message, so a reload lands between
/// messages and never underneath one.
pub struct Worlds(std::sync::RwLock<Arc<World>>);

impl Worlds {
    pub fn new(world: World) -> Self {
        Self(std::sync::RwLock::new(Arc::new(world)))
    }

    pub fn current(&self) -> Arc<World> {
        self.0.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub fn replace(&self, world: World) {
        *self.0.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(world);
    }
}

impl World {
    pub fn new(
        settings: Settings,
        fish: FishTable,
        catalog: Catalog,
        recipes: Vec<Recipe>,
        store: Arc<dyn Storage>,
        log: Arc<Log>,
    ) -> Self {
        Self {
            settings,
            gather_points: Arc::default(),
            boards: Arc::default(),
            next_offer: Arc::new(AtomicI64::new(1)),
            fish,
            catalog,
            recipes,
            store,
            log,
            online: Arc::default(),
            live: Arc::default(),
            mail_notices: Arc::default(),
        }
    }

    /// The same world with the files read again. Everything alive carries over
    /// untouched, so a player keeps their board offers and the bait in the
    /// water; only the tables underneath them change.
    pub fn with_tables(&self, settings: Settings, fish: FishTable, catalog: Catalog, recipes: Vec<Recipe>) -> Self {
        Self {
            settings,
            fish,
            catalog,
            recipes,
            gather_points: self.gather_points.clone(),
            boards: self.boards.clone(),
            next_offer: self.next_offer.clone(),
            store: self.store.clone(),
            log: self.log.clone(),
            online: self.online.clone(),
            live: self.live.clone(),
            mail_notices: self.mail_notices.clone(),
        }
    }

    /// Marks a character as played by this connection and disconnects anyone
    /// already playing it: two owners would overwrite each other's saves.
    pub fn claim(&self, character: i64, connection: u64, kick: Arc<Notify>, admin: mpsc::Sender<AdminJob>) -> bool {
        self.online.claim(character, connection, kick, admin)
    }

    /// The way in to a character being played, for the admin API. None means
    /// nobody is playing it, and nothing outside the game may touch a save the
    /// game would write over anyway.
    pub fn admin_channel(&self, character: i64) -> Option<mpsc::Sender<AdminJob>> {
        let entries = self.online.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.get(&character).map(|entry| entry.admin.clone())
    }

    /// Who is playing right now, for the admin API to name them.
    pub fn online_characters(&self) -> Vec<i64> {
        let entries = self.online.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.keys().copied().collect()
    }

    pub fn release(&self, character: i64, connection: u64) {
        self.online.release(character, connection);
        let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if live.get(&character).is_some_and(|found| found.connection == connection) {
            live.remove(&character);
        }
    }

    pub fn publish(&self, character: LiveCharacter) {
        let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        live.insert(character.data.id, character);
    }

    /// Everyone in play, by character id.
    pub fn live(&self) -> Vec<LiveCharacter> {
        let live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut all: Vec<LiveCharacter> = live.values().cloned().collect();
        all.sort_by_key(|character| character.data.id);
        all
    }

    /// A letter came for the character; its connection tells it on its next tick.
    pub fn notify_mail(&self, character: i64) {
        self.mail_notices.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(character);
    }

    pub fn take_mail_notice(&self, character: i64) -> bool {
        self.mail_notices.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&character)
    }

    pub fn live_one(&self, id: i64) -> Option<LiveCharacter> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&id).cloned()
    }

    /// Offers live only while the server runs, so their ids need not last.
    pub fn next_offer_id(&self) -> i64 {
        self.next_offer.fetch_add(1, Ordering::Relaxed)
    }

    /// Puts back the gather points kept from the last run.
    pub fn restore_gather_points(&self, stored: Vec<StoredGatherPoint>) {
        let mut all = self.gather_points.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for point in stored {
            let kept = GatherPoint { kind: point.kind, resource: point.resource, respawn_at: point.respawn_at };
            all.entry(point.base).or_default().insert(point.point, kept);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel nobody reads: these tests only care who holds the character.
    fn jobs() -> mpsc::Sender<AdminJob> {
        mpsc::channel(1).0
    }

    #[tokio::test]
    async fn a_second_connection_kicks_the_first() {
        let online = OnlineCharacters::default();
        let first = Arc::new(Notify::new());
        assert!(!online.claim(1, 10, first.clone(), jobs()));
        assert!(online.claim(1, 20, Arc::new(Notify::new()), jobs()));
        first.notified().await;
    }

    #[test]
    fn a_kicked_connection_cannot_release_its_successor() {
        let online = OnlineCharacters::default();
        online.claim(1, 10, Arc::new(Notify::new()), jobs());
        online.claim(1, 20, Arc::new(Notify::new()), jobs());
        online.release(1, 10);

        let kick = Arc::new(Notify::new());
        assert!(online.claim(1, 30, kick, jobs()), "connection 20 should still hold the character");
    }
}
