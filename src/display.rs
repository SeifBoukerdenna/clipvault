//! Turns history entries into single safe terminal lines.

use crate::history::HistoryEntry;

/// Max width, in characters, of a rendered content preview.
pub const PREVIEW_WIDTH: usize = 80;

/// Width of the source-app column. Fixed so the preview column stays aligned
/// whether or not an entry has a recorded source.
pub const SOURCE_WIDTH: usize = 10;

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

/// A short, human-scale age like "just now", "5m ago", "3d ago".
///
/// Falls back to an absolute date past a week, where "43d ago" stops being
/// easier to read than the date itself.
pub fn relative_time(raw: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(raw) else {
        return raw.to_string();
    };

    let seconds = chrono::Utc::now()
        .signed_duration_since(then.with_timezone(&chrono::Utc))
        .num_seconds();

    // A clock adjustment can put a timestamp slightly in the future; treat that
    // as "now" rather than printing a negative age.
    if seconds < 45 {
        return "just now".to_string();
    }

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    match seconds {
        s if s < HOUR => format!("{}m ago", s / MINUTE),
        s if s < DAY => format!("{}h ago", s / HOUR),
        s if s < 7 * DAY => format!("{}d ago", s / DAY),
        _ => then
            .with_timezone(&chrono::Local)
            .format("%b %-d")
            .to_string(),
    }
}

/// One history entry as `index  timestamp  source  preview`.
pub fn format_entry(index: usize, entry: &HistoryEntry) -> String {
    let source = entry.source.as_deref().unwrap_or("");
    format!(
        "{:>5}  {}  {:<width$}  {}",
        index,
        format_timestamp(&entry.timestamp),
        preview(source, SOURCE_WIDTH),
        preview(&entry.content, PREVIEW_WIDTH),
        width = SOURCE_WIDTH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_newlines_and_tabs_to_single_spaces() {
        assert_eq!(preview("one\ntwo\t\tthree", 80), "one two three");
        assert_eq!(preview("a\r\n\r\nb", 80), "a b");
    }

    #[test]
    fn trims_leading_and_trailing_whitespace() {
        assert_eq!(preview("  \t padded \n ", 80), "padded");
        assert_eq!(preview("   \n\t ", 80), "");
        assert_eq!(preview("", 80), "");
    }

    #[test]
    fn strips_control_characters() {
        // An entry carrying escape sequences must not be able to move the cursor
        // or recolor the terminal.
        let out = preview("red\u{1b}[31mtext\u{0}end", 80);
        assert_eq!(out, "red [31mtext end");
        assert!(!out.contains('\u{1b}'));
    }

    #[test]
    fn truncation_fits_within_the_budget_including_the_ellipsis() {
        let out = preview(&"x".repeat(200), 10);
        assert_eq!(out, "xxxxxxxxx…");
        assert_eq!(out.chars().count(), 10);
    }

    #[test]
    fn content_at_exactly_the_budget_is_not_truncated() {
        let out = preview(&"x".repeat(10), 10);
        assert_eq!(out, "x".repeat(10));
        assert!(!out.ends_with('…'));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // Each of these is multi-byte; a byte-based cut would split one and panic.
        let out = preview(&"é".repeat(50), 10);
        assert_eq!(out.chars().count(), 10);
        assert_eq!(out, format!("{}…", "é".repeat(9)));
    }

    #[test]
    fn zero_width_yields_nothing() {
        assert_eq!(preview("anything", 0), "");
    }

    fn entry(source: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            id: 1,
            timestamp: "2026-08-19T14:00:00+00:00".into(),
            content: "hello".into(),
            source: source.map(str::to_string),
            source_bundle_id: None,
        }
    }

    #[test]
    fn source_column_keeps_the_preview_aligned() {
        // A missing source and a long one must leave the content in the same
        // column, otherwise `list` output goes ragged.
        let none = format_entry(1, &entry(None));
        let long = format_entry(1, &entry(Some("Some Very Long App Name")));
        let short = format_entry(1, &entry(Some("Safari")));

        // Byte offsets would differ purely because "…" is three bytes, so
        // measure the column the way the terminal does: in characters.
        let column = |line: &str| {
            let byte = line.find("hello").unwrap();
            line[..byte].chars().count()
        };
        assert_eq!(column(&none), column(&long));
        assert_eq!(column(&none), column(&short));
        assert!(
            long.contains('\u{2026}'),
            "long source should be truncated: {long}"
        );
    }

    fn ago(seconds: i64) -> String {
        let when = chrono::Utc::now() - chrono::Duration::seconds(seconds);
        relative_time(&when.to_rfc3339())
    }

    #[test]
    fn relative_time_buckets_by_magnitude() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(120), "2m ago");
        assert_eq!(ago(3 * 3600), "3h ago");
        assert_eq!(ago(2 * 86400), "2d ago");
    }

    #[test]
    fn relative_time_switches_to_a_date_past_a_week() {
        let old = ago(30 * 86400);
        assert!(!old.ends_with("ago"), "expected a date, got {old}");
    }

    #[test]
    fn a_future_timestamp_reads_as_now_rather_than_negative() {
        // Clock adjustments happen; "-3m ago" would be nonsense.
        let ahead = chrono::Utc::now() + chrono::Duration::seconds(90);
        assert_eq!(relative_time(&ahead.to_rfc3339()), "just now");
    }

    #[test]
    fn malformed_relative_timestamp_falls_back_to_the_raw_value() {
        assert_eq!(relative_time("nope"), "nope");
    }

    #[test]
    fn malformed_timestamp_falls_back_to_the_raw_value() {
        assert_eq!(format_timestamp("not-a-date"), "not-a-date");
    }

    #[test]
    fn valid_timestamp_is_reformatted() {
        let out = format_timestamp("2026-08-19T14:00:00+00:00");
        assert!(out.starts_with("2026-08-19"), "unexpected: {out}");
        assert_eq!(out.len(), 19);
    }
}
