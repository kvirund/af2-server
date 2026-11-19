//! Watches `config/` and reads it again when a file there changes.
//!
//! The files are the server's own, edited by hand while it runs; a reload is
//! meant to save a restart, not to be a feature players can feel. So it is
//! deliberately timid: it only ever replaces the tables the files describe, it
//! leaves everything alive exactly where it was, and a file it cannot read
//! changes nothing at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::config;
use crate::world::Worlds;

/// What the files looked like last time: the newest write among them, and how
/// many there were, so a deleted file counts as a change too.
#[derive(PartialEq)]
struct Stamp {
    newest: Option<SystemTime>,
    files: usize,
}

fn stamp(config_dir: &Path) -> Stamp {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return Stamp { newest: None, files: 0 };
    };
    let times: Vec<SystemTime> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .collect();
    Stamp { newest: times.iter().copied().max(), files: times.len() }
}

/// Re-reads the files whenever one of them is written. Runs until the server
/// stops; `every` of zero never looks at all.
pub async fn watch(worlds: Arc<Worlds>, config_dir: PathBuf, root: PathBuf, every: Duration) {
    if every.is_zero() {
        return;
    }
    let mut seen = stamp(&config_dir);
    loop {
        tokio::time::sleep(every).await;
        let now = stamp(&config_dir);
        if now == seen {
            continue;
        }
        // Count the write as seen either way: a file left broken must not make
        // us read it again every tick and say so every tick.
        seen = now;
        let world = worlds.current();
        let read = {
            let config_dir = config_dir.clone();
            let root = root.clone();
            tokio::task::spawn_blocking(move || config::load(&config_dir, &root)).await
        };
        match read {
            Ok(Ok(data)) => {
                let kept = world.settings.server.clone();
                let mut next = world.with_tables(data.settings, data.fish, data.catalog, data.recipes);
                // Addresses and the admin token are read once, at startup: the
                // sockets are bound and the admin channel is handed out. Keep
                // the ones in force so the files cannot claim otherwise.
                next.settings.server = kept;
                world.log.line("config reloaded");
                worlds.replace(next);
            }
            Ok(Err(error)) => world.log.line(&format!("config left as it was: {error:#}")),
            Err(error) => world.log.line(&format!("config left as it was: {error}")),
        }
    }
}
