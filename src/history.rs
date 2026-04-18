//! Append-only history storage at ~/.clipvault/history.jsonl.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u128,
    pub timestamp: String,
    pub content: String,
}

/// Owner-only. Clipboard history is a record of everything you have copied, so
/// it has no business being readable by other accounts on the machine — which
/// is what the default umask would give it.
#[cfg(unix)]
const OWNER_ONLY_FILE: u32 = 0o600;
#[cfg(unix)]
const OWNER_ONLY_DIR: u32 = 0o700;

/// Tightens permissions on a path we own, if they aren't already tight.
///
/// Applied on every write rather than only at creation, so files left behind by
/// an older version get repaired instead of staying world-readable forever.
#[cfg(unix)]
pub(crate) fn restrict(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.permissions().mode() & 0o777 == mode {
        return;
    }
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
pub(crate) fn restrict(_path: &std::path::Path, _mode: u32) {}

/// Resolves ~/.clipvault, creating it if needed.
pub(crate) fn clipvault_dir() -> Result<PathBuf> {
    let mut dir = dirs::home_dir().ok_or("could not resolve home directory")?;
    dir.push(".clipvault");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    restrict(&dir, OWNER_ONLY_DIR);
    Ok(dir)
}

/// Resolves ~/.clipvault/history.jsonl, creating the directory if needed.
///
/// Permissions are repaired here rather than only on write, so a file left
/// world-readable by an older version is tightened by any operation — including
/// a plain `list` — instead of waiting for the next copy.
fn history_path() -> Result<PathBuf> {
    let mut path = clipvault_dir()?;
    path.push("history.jsonl");
    #[cfg(unix)]
    restrict(&path, OWNER_ONLY_FILE);
    Ok(path)
}

/// Appends one clipboard change as a single JSON line.
/// Opens, writes, and closes the file each call — simplest and crash-safe,
/// since we're only writing on actual changes, not every poll.
pub fn append_history(content: &str) -> Result<()> {
    let path = history_path()?;

    // Nanos-since-epoch id: monotonic in practice, and needs no lookup
    // of the previous entry on restart (unlike a simple incrementing counter).
    let id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();

    let entry = HistoryEntry {
        id,
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: content.to_string(),
    };

    let line = serde_json::to_string(&entry)?;

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    // Again after creation: the path resolution above ran before the file
    // existed on the very first capture.
    #[cfg(unix)]
    restrict(&path, OWNER_ONLY_FILE);

    writeln!(file, "{line}")?;
    Ok(())
}

/// Reads the whole history, oldest first.
///
/// A missing file just means nothing has been captured yet. Unparseable lines
/// are skipped rather than fatal: a half-written final line (from a kill during
/// a write) or a hand-edit shouldn't make the rest of the history unreadable.
pub fn read_history() -> Result<Vec<HistoryEntry>> {
    let path = history_path()?;

    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str(&line) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

/// Deletes the whole history file.
/// A missing file is already the desired end state, so that isn't an error.
pub fn clear_history() -> Result<()> {
    let path = history_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Rewrites the history with `keep` applied to every entry.
///
/// Writes to a sibling temp file and renames over the original, so an
/// interrupted rewrite leaves the old history intact rather than a half-written
/// one — the whole point of the append-only format is not losing data.
fn rewrite<F>(keep: F) -> Result<usize>
where
    F: Fn(&HistoryEntry) -> bool,
{
    let path = history_path()?;
    let entries = read_history()?;

    let kept: Vec<&HistoryEntry> = entries.iter().filter(|e| keep(e)).collect();
    let removed = entries.len() - kept.len();
    if removed == 0 {
        return Ok(0);
    }

    let mut temp = path.clone();
    temp.set_extension("jsonl.tmp");

    {
        let mut file = File::create(&temp)?;
        for entry in kept {
            writeln!(file, "{}", serde_json::to_string(entry)?)?;
        }
        file.sync_all()?;
    }

    fs::rename(&temp, &path)?;
    #[cfg(unix)]
    restrict(&path, OWNER_ONLY_FILE);
    Ok(removed)
}

/// Removes every entry whose content matches, returning how many went.
pub fn delete_entry(content: &str) -> Result<usize> {
    rewrite(|entry| entry.content != content)
}

/// Trims the history to its newest `limit` entries. `limit` of 0 means keep
/// everything. Returns how many were dropped.
pub fn prune(limit: usize) -> Result<usize> {
    if limit == 0 {
        return Ok(0);
    }

    let total = read_history()?.len();
    if total <= limit {
        return Ok(0);
    }

    // `rewrite` walks entries oldest-first, so drop everything before the cut.
    let cutoff = total - limit;
    let seen = std::cell::Cell::new(0usize);
    rewrite(move |_| {
        let index = seen.get();
        seen.set(index + 1);
        index >= cutoff
    })
}

#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &std::path::Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn restrict_tightens_a_world_readable_file() {
        let path = std::env::temp_dir().join(format!("cv-perm-{}", std::process::id()));
        fs::write(&path, b"secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644);

        // The repair path: a file left behind by an older version.
        restrict(&path, 0o600);
        assert_eq!(mode_of(&path), 0o600);

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn restrict_is_a_no_op_on_a_missing_path() {
        // Runs on every write, including before the file exists.
        restrict(
            std::path::Path::new("/nonexistent/clipvault/history.jsonl"),
            0o600,
        );
    }
}
