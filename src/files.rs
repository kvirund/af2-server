//! Finding the installation's folders and reading its hand-edited files.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use json_comments::StripComments;
use serde::de::DeserializeOwned;

/// Reads JSON that may contain `//` comments.
pub fn read_commented<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = File::open(path).with_context(|| path.display().to_string())?;
    serde_json::from_reader(StripComments::new(file)).with_context(|| path.display().to_string())
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

    #[test]
    fn a_local_file_replaces_only_its_own_keys() {
        let mut base = serde_json::json!({"progression": {"experiencePerFish": 2, "experiencePerKg": 3}, "port": 1});
        overlay(&mut base, serde_json::json!({"progression": {"experiencePerFish": 4}}));
        assert_eq!(base, serde_json::json!({"progression": {"experiencePerFish": 4, "experiencePerKg": 3}, "port": 1}));
    }
}
