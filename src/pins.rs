//! Pinned snippets, kept at the top of the menu and never pruned.
//!
//! Stored separately from the history rather than as a flag on an entry: the
//! history is append-only, so flipping a field in place would mean rewriting
//! the whole file. Pins are keyed by content, so pinning survives the original
//! entry aging out.

use std::fs;
use std::io;

use crate::Result;
use crate::history::clipvault_dir;

/// Upper bound, so pins can't crowd the recent list out of the menu.
pub const MAX_PINS: usize = 9;

fn pins_path() -> Result<std::path::PathBuf> {
    let mut path = clipvault_dir()?;
    path.push("pins.json");
    crate::history::restrict(&path, 0o600);
    Ok(path)
}

/// Pinned entries, most recently pinned first. A missing file means no pins.
pub fn read_pins() -> Result<Vec<String>> {
    let path = pins_path()?;

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    // A corrupt pins file shouldn't stop the app from starting; the worst case
    // is you re-pin a few things.
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn write_pins(pins: &[String]) -> Result<()> {
    let path = pins_path()?;
    fs::write(&path, serde_json::to_string_pretty(pins)?)?;
    // Pinned entries are clipboard content too.
    crate::history::restrict(&path, 0o600);
    Ok(())
}

pub fn is_pinned(content: &str) -> Result<bool> {
    Ok(read_pins()?.iter().any(|p| p == content))
}

/// Pins `content` if it isn't pinned, unpins it if it is.
/// Returns the state it ended up in.
pub fn toggle_pin(content: &str) -> Result<bool> {
    let mut pins = read_pins()?;

    if let Some(position) = pins.iter().position(|p| p == content) {
        pins.remove(position);
        write_pins(&pins)?;
        return Ok(false);
    }

    pins.insert(0, content.to_string());
    pins.truncate(MAX_PINS);
    write_pins(&pins)?;
    Ok(true)
}

/// Drops a pin if it exists. Used when its entry is deleted outright, so the
/// menu can't keep offering something the history no longer has.
pub fn remove_pin(content: &str) -> Result<()> {
    let mut pins = read_pins()?;
    let before = pins.len();
    pins.retain(|p| p != content);
    if pins.len() != before {
        write_pins(&pins)?;
    }
    Ok(())
}
