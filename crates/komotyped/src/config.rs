//! User configuration: `%USERPROFILE%\.config\komotype\config.toml`.

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::devices::DeviceMatcher;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Keystrokes per day on the main keyboard that keep the streak alive (default 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streak_threshold: Option<u32>,
    /// The keyboard whose typing counts. Set with `komotyped pick`.
    pub main: Option<DeviceMatcher>,
}

impl Config {
    pub fn streak_threshold(&self) -> u32 {
        self.streak_threshold.unwrap_or(2000)
    }
}

pub fn path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config")
        .join("komotype")
        .join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_default().join("komotype")
}

pub fn modified() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

pub fn load() -> Result<Config> {
    let path = path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(config: &Config) -> Result<()> {
    let path = path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let text = format!(
        "# komotype configuration\n# Main keyboard: run `komotyped pick` to choose it by pressing a key.\n\n{}",
        toml::to_string_pretty(config)?
    );
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}
