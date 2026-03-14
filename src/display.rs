//! Turns history entries into single safe terminal lines.

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
