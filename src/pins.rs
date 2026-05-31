//! Pinned snippets, kept at the top of the menu and never pruned.
//!
//! Stored separately from the history rather than as a flag on an entry: the
//! history is append-only, so flipping a field in place would mean rewriting
//! the whole file. Pins are keyed by content, so pinning survives the original
//! entry aging out.

use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::history::clipvault_dir;

/// A pinned snippet. The source app rides along so the menu can show its icon
/// without scanning the history for a matching entry on every rebuild.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bundle_id: Option<String>,
}

/// Accepts both the current object form and the bare-string form written by the
/// first version of pinning, so existing pins survive the upgrade.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredPin {
    Rich(Pin),
    Legacy(String),
}

impl From<StoredPin> for Pin {
    fn from(stored: StoredPin) -> Self {
        match stored {
            StoredPin::Rich(pin) => pin,
            StoredPin::Legacy(content) => Pin {
                content,
                source: None,
                source_bundle_id: None,
            },
        }
    }
}

/// Upper bound, so pins can't crowd the recent list out of the menu.
pub const MAX_PINS: usize = 9;

fn pins_path() -> Result<std::path::PathBuf> {
    let mut path = clipvault_dir()?;
    path.push("pins.json");
    crate::history::restrict(&path, 0o600);
    Ok(path)
}

/// Pinned entries, most recently pinned first. A missing file means no pins.
pub fn read_pins() -> Result<Vec<Pin>> {
    let path = pins_path()?;

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    // A corrupt pins file shouldn't stop the app from starting; the worst case
    // is you re-pin a few things.
    let stored: Vec<StoredPin> = serde_json::from_str(&raw).unwrap_or_default();
    Ok(stored.into_iter().map(Pin::from).collect())
}

fn write_pins(pins: &[Pin]) -> Result<()> {
    let path = pins_path()?;
    fs::write(&path, serde_json::to_string_pretty(pins)?)?;
    // Pinned entries are clipboard content too.
    crate::history::restrict(&path, 0o600);
    Ok(())
}

pub fn is_pinned(content: &str) -> Result<bool> {
    Ok(read_pins()?.iter().any(|p| p.content == content))
}

/// Pins `content` if it isn't pinned, unpins it if it is.
/// Returns the state it ended up in.
pub fn toggle_pin(
    content: &str,
    source: Option<&str>,
    source_bundle_id: Option<&str>,
) -> Result<bool> {
    let mut pins = read_pins()?;

    if let Some(position) = pins.iter().position(|p| p.content == content) {
        pins.remove(position);
        write_pins(&pins)?;
        return Ok(false);
    }

    pins.insert(
        0,
        Pin {
            content: content.to_string(),
            source: source.map(str::to_string),
            source_bundle_id: source_bundle_id.map(str::to_string),
        },
    );
    pins.truncate(MAX_PINS);
    write_pins(&pins)?;
    Ok(true)
}

/// Drops a pin if it exists. Used when its entry is deleted outright, so the
/// menu can't keep offering something the history no longer has.
pub fn remove_pin(content: &str) -> Result<()> {
    let mut pins = read_pins()?;
    let before = pins.len();
    pins.retain(|p| p.content != content);

    if pins.len() != before {
        write_pins(&pins)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises the ordering and cap rules directly; the file I/O around them is
    // a thin read/write pair.
    fn toggle(pins: &mut Vec<Pin>, content: &str) -> bool {
        if let Some(position) = pins.iter().position(|p| p.content == content) {
            pins.remove(position);
            return false;
        }
        pins.insert(
            0,
            Pin {
                content: content.to_string(),
                source: None,
                source_bundle_id: None,
            },
        );
        pins.truncate(MAX_PINS);
        true
    }

    fn contents(pins: &[Pin]) -> Vec<&str> {
        pins.iter().map(|p| p.content.as_str()).collect()
    }

    #[test]
    fn legacy_string_pins_still_load() {
        // pins.json written by the first version of this feature.
        let stored: Vec<StoredPin> = serde_json::from_str(r#"["alpha","beta"]"#).unwrap();
        let pins: Vec<Pin> = stored.into_iter().map(Pin::from).collect();
        assert_eq!(contents(&pins), ["alpha", "beta"]);
        assert!(pins[0].source_bundle_id.is_none());
    }

    #[test]
    fn rich_pins_round_trip_with_their_source() {
        let pins = vec![Pin {
            content: "x".into(),
            source: Some("Safari".into()),
            source_bundle_id: Some("com.apple.Safari".into()),
        }];
        let json = serde_json::to_string(&pins).unwrap();
        let stored: Vec<StoredPin> = serde_json::from_str(&json).unwrap();
        let back: Vec<Pin> = stored.into_iter().map(Pin::from).collect();
        assert_eq!(back, pins);
    }

    #[test]
    fn pinning_puts_the_newest_first() {
        let mut pins = Vec::new();
        toggle(&mut pins, "a");
        toggle(&mut pins, "b");
        assert_eq!(contents(&pins), ["b", "a"]);
    }

    #[test]
    fn toggling_a_pinned_item_removes_it() {
        let mut pins = Vec::new();
        assert!(toggle(&mut pins, "a"));
        assert!(!toggle(&mut pins, "a"));
        assert!(pins.is_empty());
    }

    #[test]
    fn the_cap_drops_the_oldest_pin() {
        let mut pins = Vec::new();
        for i in 0..MAX_PINS + 3 {
            toggle(&mut pins, &format!("pin {i}"));
        }
        assert_eq!(pins.len(), MAX_PINS);
        assert_eq!(pins[0].content, format!("pin {}", MAX_PINS + 2));
        assert!(
            !pins.iter().any(|p| p.content == "pin 0"),
            "oldest should be gone"
        );
    }
}
