//! Turns history entries into single safe terminal lines.

use crate::history::HistoryEntry;

/// Max width, in characters, of a rendered content preview.
pub const PREVIEW_WIDTH: usize = 80;

/// Flattens `content` to a single line and truncates it to `max_chars`.
///
/// Every run of whitespace and control characters collapses to one space, so a
/// multi-line entry (or one carrying escape sequences) can't wreck the terminal
/// layout. Truncation counts characters rather than bytes so we never split a
/// UTF-8 sequence; the trailing ellipsis fits inside `max_chars`.
pub fn preview(content: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut len = 0; // characters pushed so far
    let mut pending_space = false;
    let mut truncated = false;

    for c in content.chars() {
        if c.is_whitespace() || c.is_control() {
            // Note the gap, but only emit it once real text follows — that
            // drops leading/trailing padding for free.
            pending_space = len > 0;
            continue;
        }

        if len + usize::from(pending_space) + 1 > max_chars {
            truncated = true;
            break;
        }

        if pending_space {
            out.push(' ');
            len += 1;
            pending_space = false;
        }
        out.push(c);
        len += 1;
    }

    if truncated {
        while len >= max_chars {
            out.pop();
            len -= 1;
        }
        out.push('…');
    }

    out
}

/// Renders the stored RFC 3339 timestamp in the local timezone.
/// Falls back to the raw value so a malformed timestamp still shows its row.
pub fn format_timestamp(raw: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => raw.to_string(),
    }
}

/// One history entry as `index  timestamp  preview`.
pub fn format_entry(index: usize, entry: &HistoryEntry) -> String {
    format!(
        "{:>5}  {}  {}",
        index,
        format_timestamp(&entry.timestamp),
        preview(&entry.content, PREVIEW_WIDTH),
    )
}
