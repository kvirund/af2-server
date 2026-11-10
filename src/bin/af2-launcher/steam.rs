//! Finding the game through Steam's own files, and starting it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const APP_ID: u32 = 557780;
const GAME_EXE: &str = "AFCGame.exe";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Long enough for Steam itself to start first.
const START_TIMEOUT: Duration = Duration::from_secs(300);

/// The value on a VDF line such as `"path"   "C:\\Steam"`, if the key matches.
fn vdf_value(line: &str, key: &str) -> Option<String> {
    let parts: Vec<&str> = line.trim().split('"').collect();
    (parts.len() >= 4 && parts[1] == key).then(|| parts[3].replace("\\\\", "\\"))
}

fn steam_root() -> Result<PathBuf> {
    let output = Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .output()
        .context("не удалось прочитать реестр")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text
        .lines()
        .find_map(|line| line.split_once("REG_SZ"))
        .map(|(_, value)| value.trim().to_owned())
        .context("Steam не найден; укажи папку игры через --game")?;
    Ok(path.into())
}

fn libraries(root: &Path) -> Vec<PathBuf> {
    let mut found = vec![root.to_owned()];
    if let Ok(text) = fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        found.extend(text.lines().filter_map(|line| vdf_value(line, "path")).map(PathBuf::from));
    }
    found
}

fn game_in_library(library: &Path) -> Option<PathBuf> {
    let apps = library.join("steamapps");
    let manifest = fs::read_to_string(apps.join(format!("appmanifest_{APP_ID}.acf"))).ok()?;
    let folder = manifest.lines().find_map(|line| vdf_value(line, "installdir"))?;
    let game = apps.join("common").join(folder);
    game.join("AFCGame_Data").is_dir().then_some(game)
}

pub fn find_game() -> Result<PathBuf> {
    libraries(&steam_root()?)
        .iter()
        .find_map(|library| game_in_library(library))
        .context("Atom Fishing 2 не найдена в библиотеках Steam; укажи папку игры через --game")
}

pub fn game_is_running() -> Result<bool> {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {GAME_EXE}"), "/NH"])
        .output()
        .context("не удалось получить список процессов")?;
    let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
    Ok(text.contains(&GAME_EXE.to_lowercase()))
}

/// Steam has to start the game: run directly, the client refuses to sign in.
pub fn launch_game() -> Result<()> {
    let status = Command::new("cmd")
        .args(["/C", "start", "", &format!("steam://rungameid/{APP_ID}")])
        .status()
        .context("не удалось запустить игру через Steam")?;
    if !status.success() {
        bail!("Steam не запустил игру");
    }
    Ok(())
}

pub fn wait_until_game_closes() -> Result<()> {
    let asked = Instant::now();
    while !game_is_running()? {
        if asked.elapsed() > START_TIMEOUT {
            bail!("игра не запустилась за {} минут", START_TIMEOUT.as_secs() / 60);
        }
        sleep(POLL_INTERVAL);
    }
    while game_is_running()? {
        sleep(POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vdf_values() {
        let line = "\t\t\"path\"\t\t\"D:\\\\Games\\\\Steam\"";
        assert_eq!(vdf_value(line, "path").unwrap(), r"D:\Games\Steam");
        assert_eq!(vdf_value(line, "label"), None);
        assert_eq!(vdf_value("\t{", "path"), None);
    }
}
