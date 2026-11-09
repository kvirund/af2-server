//! What every connection shares. A character's own state belongs to its
//! connection's task, so game code needs no locks.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};

use chrono::Local;
use tokio::sync::Notify;

use crate::config::{Catalog, FishTable, Settings};
use crate::storage::Storage;

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

pub struct World {
    pub settings: Settings,
    pub fish: FishTable,
    pub catalog: Catalog,
    pub store: Box<dyn Storage>,
    pub log: Log,
    online: OnlineCharacters,
}

impl World {
    pub fn new(
        settings: Settings,
        fish: FishTable,
        catalog: Catalog,
        store: Box<dyn Storage>,
        log: Log,
    ) -> Self {
        Self { settings, fish, catalog, store, log, online: OnlineCharacters::default() }
    }

    /// Marks a character as played by this connection and disconnects anyone
    /// already playing it: two owners would overwrite each other's saves.
    pub fn claim(&self, character: i64, connection: u64, kick: Arc<Notify>) -> bool {
        self.online.claim(character, connection, kick)
    }

    pub fn release(&self, character: i64, connection: u64) {
        self.online.release(character, connection);
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
