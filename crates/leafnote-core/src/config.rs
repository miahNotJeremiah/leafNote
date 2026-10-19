//! Application configuration and default locations.
//!
//! leafNote is *local-first*: notebooks live in an ordinary folder chosen by
//! the user (default: `~/Documents/leafNote`). The app itself keeps only a
//! small TOML config in the platform config directory.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Root folder holding all notebooks.
    pub data_dir: PathBuf,
    /// Open the last session's notebooks on startup.
    pub restore_session: bool,
    /// Recently opened notebook paths (most recent first).
    pub recent: Vec<PathBuf>,
    /// Default author name stamped onto new pages.
    pub author: String,
    /// UI theme: "light" | "dark".
    pub theme: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            restore_session: true,
            recent: Vec::new(),
            author: whoami(),
            theme: "light".into(),
        }
    }
}

/// Where notebooks go if the user never picks a folder: `~/Documents/leafNote`
/// (falls back to the home dir, then cwd).
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = dirs::document_dir() {
        return d.join("leafNote");
    }
    if let Some(h) = dirs::home_dir() {
        return h.join("leafNote");
    }
    PathBuf::from("leafNote")
}

/// `~/.config/leafnote` (platform standard).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("leafnote")
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// Load config, or return defaults if missing/corrupt (never fatal —
    /// local-first means the app must always start).
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(raw) => toml::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> crate::Result<()> {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn push_recent(&mut self, path: PathBuf) {
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(12);
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "me".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_serializes_toml() {
        let c = Config::default();
        let raw = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&raw).unwrap();
        assert_eq!(back.data_dir, c.data_dir);
        assert_eq!(back.theme, "light");
    }
}
