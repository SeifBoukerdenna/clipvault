//! Append-only history storage at ~/.clipvault/history.jsonl.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::source::Source;

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u128,
    pub timestamp: String,
    pub content: String,
    /// Name of the app that was frontmost at capture time. Optional so entries
    /// written before source tracking existed still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Bundle id of that app, used to look up its icon for the menu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bundle_id: Option<String>,
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
pub fn append_history(content: &str, source: Option<&Source>) -> Result<()> {
    let path = history_path()?;

    // Nanos-since-epoch id: monotonic in practice, and needs no lookup
    // of the previous entry on restart (unlike a simple incrementing counter).
    let id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();

    let entry = HistoryEntry {
        id,
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: content.to_string(),
        source: source.and_then(|s| s.name.clone()),
        source_bundle_id: source.and_then(|s| s.bundle_id.clone()),
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

/// How much to pull per backwards step in [`read_tail`].
const TAIL_CHUNK_BYTES: u64 = 8 * 1024;

/// Reads at most `limit` entries from the end of the history, oldest first.
///
/// The menu only ever shows a handful of rows, and the file is append-only, so
/// the newest entries are always at the end. Seeking backwards keeps opening
/// the menu O(limit) instead of O(history), which matters once the log has
/// tens of thousands of lines.
pub fn read_tail(limit: usize) -> Result<Vec<HistoryEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let path = history_path()?;
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    let len = file.metadata()?.len();
    tail_from(&mut file, len, limit)
}

/// The seek-backwards half of [`read_tail`], split out so it can be tested
/// against an in-memory buffer instead of a real file.
fn tail_from<R: Read + Seek>(reader: &mut R, len: u64, limit: usize) -> Result<Vec<HistoryEntry>> {
    let mut position = len;
    let mut tail: Vec<u8> = Vec::new();

    // Buffer one line more than asked for: after a partial chunk the first line
    // in the buffer is usually truncated, and the extra absorbs that.
    while position > 0 && tail.iter().filter(|b| **b == b'\n').count() <= limit {
        let step = TAIL_CHUNK_BYTES.min(position);
        position -= step;

        let mut chunk = vec![0u8; step as usize];
        reader.seek(SeekFrom::Start(position))?;
        reader.read_exact(&mut chunk)?;
        chunk.append(&mut tail);
        tail = chunk;
    }

    // A truncated leading line simply fails to parse and drops out here, as does
    // any lossy replacement char a chunk boundary introduced mid-sequence.
    let text = String::from_utf8_lossy(&tail);
    let mut entries: Vec<HistoryEntry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
    }

    Ok(entries)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn line(id: u128, content: &str) -> String {
        serde_json::to_string(&HistoryEntry {
            id,
            timestamp: "2026-08-19T14:00:00+00:00".into(),
            content: content.into(),
            source: None,
            source_bundle_id: None,
        })
        .unwrap()
            + "\n"
    }

    fn buffer(count: usize) -> Vec<u8> {
        (0..count)
            .map(|i| line(i as u128, &format!("entry {i}")))
            .collect::<String>()
            .into_bytes()
    }

    #[test]
    fn tail_returns_the_newest_entries_oldest_first() {
        let data = buffer(10);
        let len = data.len() as u64;
        let got = tail_from(&mut Cursor::new(data), len, 3).unwrap();

        let contents: Vec<&str> = got.iter().map(|e| e.content.as_str()).collect();
        assert_eq!(contents, ["entry 7", "entry 8", "entry 9"]);
    }

    #[test]
    fn tail_handles_a_history_shorter_than_the_limit() {
        let data = buffer(2);
        let len = data.len() as u64;
        let got = tail_from(&mut Cursor::new(data), len, 25).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn tail_of_an_empty_file_is_empty() {
        let got = tail_from(&mut Cursor::new(Vec::new()), 0, 5).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn tail_spans_many_chunks_without_losing_or_splitting_entries() {
        // Each entry is padded well past the chunk size so the scan has to walk
        // backwards several times and stitch partial lines together.
        let padded: String = (0..40)
            .map(|i| line(i, &format!("{}{}", "x".repeat(900), i)))
            .collect();
        let data = padded.into_bytes();
        assert!(
            data.len() as u64 > TAIL_CHUNK_BYTES * 3,
            "test needs multiple chunks"
        );

        let len = data.len() as u64;
        let got = tail_from(&mut Cursor::new(data), len, 5).unwrap();

        assert_eq!(got.len(), 5);
        assert!(got[4].content.ends_with("39"));
        assert!(got[0].content.ends_with("35"));
    }

    #[test]
    fn tail_skips_a_partial_line_left_by_a_chunk_boundary() {
        // What the backwards scan actually sees when it lands mid-line: a JSON
        // fragment terminated by the newline of the line it was cut out of.
        let mut data = b"{\"id\":0,\"timesta\n".to_vec();
        data.extend_from_slice(&buffer(3));
        let len = data.len() as u64;

        let got = tail_from(&mut Cursor::new(data), len, 10).unwrap();
        assert_eq!(
            got.len(),
            3,
            "the fragment should drop, the 3 whole lines stay"
        );
    }

    #[test]
    fn tail_skips_a_final_line_left_half_written_by_a_kill() {
        let mut data = buffer(3);
        data.extend_from_slice(b"{\"id\":99,\"timestamp\":\"2026");
        let len = data.len() as u64;

        let got = tail_from(&mut Cursor::new(data), len, 10).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].content, "entry 2");
    }

    #[test]
    fn entries_without_a_source_still_deserialize() {
        // Lines written before source tracking existed must keep working.
        let old = r#"{"id":1,"timestamp":"2026-08-19T14:00:00+00:00","content":"hi"}"#;
        let entry: HistoryEntry = serde_json::from_str(old).unwrap();
        assert_eq!(entry.content, "hi");
        assert!(entry.source.is_none());
        assert!(entry.source_bundle_id.is_none());
    }

    #[test]
    fn a_source_free_entry_does_not_serialize_null_fields() {
        let json = serde_json::to_string(&HistoryEntry {
            id: 1,
            timestamp: "t".into(),
            content: "c".into(),
            source: None,
            source_bundle_id: None,
        })
        .unwrap();
        assert!(!json.contains("source"), "unexpected: {json}");
    }
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
