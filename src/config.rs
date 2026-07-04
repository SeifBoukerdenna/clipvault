//! User settings, stored at ~/.clipvault/config.json.
//!
//! Everything here was a hardcoded constant until the preferences window
//! existed. Defaults are what those constants were, so an absent or partial
//! config file behaves exactly like the previous build.

use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::history::clipvault_dir;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global shortcut, in the form `muda`/`global-hotkey` parse, e.g.
    /// "cmd+shift+KeyV".
    pub hotkey: String,
    /// Milliseconds between clipboard polls.
    pub poll_interval_ms: u64,
    /// How many recent entries the menu lists.
    pub menu_entries: usize,
    /// Entries kept on disk; older ones are pruned. 0 disables pruning.
    pub history_limit: usize,
    /// What the global shortcut opens: "menu" or "search".
    pub hotkey_opens: String,
}

/// Values accepted by [`Config::hotkey_opens`].
pub const OPENS_MENU: &str = "menu";
pub const OPENS_SEARCH: &str = "search";

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "cmd+shift+KeyV".to_string(),
            poll_interval_ms: 750,
            menu_entries: 15,
            history_limit: 1000,
            hotkey_opens: OPENS_MENU.to_string(),
        }
    }
}

impl Config {
    /// Clamps values that would make the app unusable if hand-edited to
    /// something silly — a 1ms poll pegs a core, 0 menu entries hides history.
    pub fn sanitized(mut self) -> Self {
        let defaults = Config::default();

        if self.hotkey.trim().is_empty() {
            self.hotkey = defaults.hotkey;
        }
        self.poll_interval_ms = self.poll_interval_ms.clamp(100, 10_000);
        self.menu_entries = self.menu_entries.clamp(1, 50);

        // Anything unrecognised falls back to the menu rather than silently
        // doing nothing when the shortcut is pressed.
        let opens = self.hotkey_opens.trim().to_lowercase();
        self.hotkey_opens = if opens == OPENS_SEARCH {
            OPENS_SEARCH.to_string()
        } else {
            OPENS_MENU.to_string()
        };
        if self.history_limit != 0 {
            self.history_limit = self.history_limit.max(self.menu_entries);
        }

        self
    }
}

fn config_path() -> Result<std::path::PathBuf> {
    let mut path = clipvault_dir()?;
    path.push("config.json");
    Ok(path)
}

/// Loads the config, falling back to defaults for anything missing or corrupt.
/// Settings are never important enough to stop the app from starting.
pub fn load() -> Config {
    let Ok(path) = config_path() else {
        return Config::default();
    };

    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str::<Config>(&raw)
            .unwrap_or_default()
            .sanitized(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(_) => Config::default(),
    }
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    fs::write(&path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_fields_fall_back_to_defaults() {
        // A config written by an older build won't have every key.
        let partial: Config = serde_json::from_str(r#"{"menu_entries": 5}"#).unwrap();
        assert_eq!(partial.menu_entries, 5);
        assert_eq!(partial.hotkey, Config::default().hotkey);
        assert_eq!(partial.poll_interval_ms, Config::default().poll_interval_ms);
    }

    #[test]
    fn hand_edited_nonsense_is_clamped() {
        let wild = Config {
            hotkey: "   ".into(),
            poll_interval_ms: 1,
            menu_entries: 0,
            history_limit: 3,
            ..Config::default()
        }
        .sanitized();

        assert_eq!(wild.hotkey, Config::default().hotkey);
        assert_eq!(wild.poll_interval_ms, 100);
        assert_eq!(wild.menu_entries, 1);
        // The cap can never sit below what the menu wants to show.
        assert!(wild.history_limit >= wild.menu_entries);
    }

    #[test]
    fn hotkey_target_falls_back_to_the_menu_when_unrecognised() {
        let wild = Config {
            hotkey_opens: "banana".into(),
            ..Config::default()
        }
        .sanitized();
        assert_eq!(wild.hotkey_opens, OPENS_MENU);
    }

    #[test]
    fn hotkey_target_accepts_search_in_any_casing() {
        let config = Config {
            hotkey_opens: "  SEARCH ".into(),
            ..Config::default()
        }
        .sanitized();
        assert_eq!(config.hotkey_opens, OPENS_SEARCH);
    }

    #[test]
    fn zero_history_limit_means_unlimited_and_survives_sanitizing() {
        let config = Config {
            history_limit: 0,
            ..Config::default()
        }
        .sanitized();
        assert_eq!(config.history_limit, 0);
    }
}
