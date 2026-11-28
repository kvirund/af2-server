//! Finding the installation's folders and reading its hand-edited files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use json_comments::StripComments;
use serde::de::DeserializeOwned;

/// A byte order mark: not part of JSON, and Windows editors offer to write one.
const MARK: char = '\u{feff}';

/// Reads JSON that may contain `//` comments.
pub fn read_commented<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = read_utf8(path)?;
    serde_json::from_reader(StripComments::new(text.as_bytes())).with_context(|| path.display().to_string())
}

/// Every file of ours is UTF-8 without a byte order mark. An editor that puts a
/// mark at the start leaves the text itself right, so drop the mark and read on.
/// Another encoding we cannot read, and the player is the one who has to fix it:
/// `serde_json` would say only «expected value at line 1 column 1», about a line
/// that looks perfectly fine to them, so name the real trouble instead.
fn read_utf8(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let mut text = String::from_utf8(bytes)
        .map_err(|_| anyhow!("{}: файл не в UTF-8 — сохрани его в UTF-8 без BOM", path.display()))?;
    if text.starts_with(MARK) {
        text.remove(0);
    }
    Ok(text)
}

/// Reads `path`, then lays the file beside it named `*.local.json` over it, if
/// there is one: its keys replace the same keys at any depth. The local file
/// stays out of git, for settings meant for one machine only.
pub fn read_commented_with_local<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut value: serde_json::Value = read_commented(path)?;
    let local = path.with_extension("local.json");
    if local.is_file() {
        let over: serde_json::Value = read_commented(&local)?;
        overlay(&mut value, over);
    }
    serde_json::from_value(value).with_context(|| path.display().to_string())
}

fn overlay(base: &mut serde_json::Value, over: serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(base), serde_json::Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => overlay(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// A release keeps `config` beside the program and a build keeps it above
/// `target`, so look upward from the program for the folder.
pub fn find_config_dir() -> Result<PathBuf> {
    let start = std::env::current_exe()?;
    for dir in start.ancestors() {
        let candidate = dir.join("config");
        if candidate.join("settings.json").is_file() {
            return Ok(candidate);
        }
    }
    bail!("config/settings.json not found above {}", start.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `bytes` to a file of its own and hands it to `read_commented`.
    fn read_bytes(name: &str, bytes: &[u8]) -> Result<serde_json::Value> {
        let path = std::env::temp_dir().join(format!("af2-{name}.json"));
        std::fs::write(&path, bytes).unwrap();
        let read = read_commented(&path);
        let _ = std::fs::remove_file(&path);
        read
    }

    #[test]
    fn a_byte_order_mark_is_dropped() {
        let marked = "\u{feff}// настройки\n{\"port\": 9201}";
        let read = read_bytes("mark", marked.as_bytes()).unwrap();
        assert_eq!(read, serde_json::json!({"port": 9201}));
    }

    #[test]
    fn another_encoding_says_what_is_wrong() {
        // `{"base": "Убежище"}` saved as windows-1251, as Notepad's «ANSI» does.
        let mut ansi = b"{\"base\": \"".to_vec();
        ansi.extend_from_slice(&[0xd3, 0xe1, 0xe5, 0xe6, 0xe8, 0xf9, 0xe5]);
        ansi.extend_from_slice(b"\"}");
        let said = read_bytes("ansi", &ansi).unwrap_err().to_string();
        assert!(said.contains("не в UTF-8"), "{said}");
    }

    #[test]
    fn a_local_file_replaces_only_its_own_keys() {
        let mut base = serde_json::json!({"progression": {"experiencePerFish": 2, "experiencePerKg": 3}, "port": 1});
        overlay(&mut base, serde_json::json!({"progression": {"experiencePerFish": 4}}));
        assert_eq!(base, serde_json::json!({"progression": {"experiencePerFish": 4, "experiencePerKg": 3}, "port": 1}));
    }
}
