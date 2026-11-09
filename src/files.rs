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
