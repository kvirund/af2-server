//! What every connection shares. A character's own state belongs to its
//! connection's task, so game code needs no locks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Local;
use tokio::sync::Notify;

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

struct OnlineEntry {
    connection: u64,
    kick: Arc<Notify>,
}

/// Which connection is playing each character.
#[derive(Default)]
struct OnlineCharacters {
    entries: Mutex<HashMap<i64, OnlineEntry>>,
}

impl OnlineCharacters {
    fn claim(&self, character: i64, connection: u64, kick: Arc<Notify>) -> bool {
        let entry = OnlineEntry { connection, kick };
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

pub struct World {
    pub settings: Settings,
    /// Base to its points, filled the first time anyone is at that base.
    pub gather_points: Mutex<HashMap<i32, BTreeMap<i32, GatherPoint>>>,
    /// Base and tab to its offers, made the first time anyone looks.
    pub boards: Mutex<HashMap<(i32, i32), BoardOffers>>,
    next_offer: AtomicI64,
    pub fish: FishTable,
    pub catalog: Catalog,
    pub recipes: Vec<Recipe>,
    pub store: Box<dyn Storage>,
    pub log: Log,
    online: OnlineCharacters,
    live: Mutex<HashMap<i64, LiveCharacter>>,
    /// Characters with a letter they have not been told of.
    mail_notices: Mutex<HashSet<i64>>,
}

impl World {
    pub fn new(
        settings: Settings,
        fish: FishTable,
        catalog: Catalog,
        recipes: Vec<Recipe>,
        store: Box<dyn Storage>,
        log: Log,
    ) -> Self {
        Self {
            settings,
            gather_points: Mutex::default(),
            boards: Mutex::default(),
            next_offer: AtomicI64::new(1),
            fish,
            catalog,
            recipes,
            store,
            log,
            online: OnlineCharacters::default(),
            live: Mutex::default(),
            mail_notices: Mutex::default(),
        }
    }

    /// Marks a character as played by this connection and disconnects anyone
    /// already playing it: two owners would overwrite each other's saves.
    pub fn claim(&self, character: i64, connection: u64, kick: Arc<Notify>) -> bool {
        self.online.claim(character, connection, kick)
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

    #[tokio::test]
    async fn a_second_connection_kicks_the_first() {
        let online = OnlineCharacters::default();
        let first = Arc::new(Notify::new());
        assert!(!online.claim(1, 10, first.clone()));
        assert!(online.claim(1, 20, Arc::new(Notify::new())));
        first.notified().await;
    }

    #[test]
    fn a_kicked_connection_cannot_release_its_successor() {
        let online = OnlineCharacters::default();
        online.claim(1, 10, Arc::new(Notify::new()));
        online.claim(1, 20, Arc::new(Notify::new()));
        online.release(1, 10);

        let kick = Arc::new(Notify::new());
        assert!(online.claim(1, 30, kick), "connection 20 should still hold the character");
    }
}
