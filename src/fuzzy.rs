//! Subsequence scoring, so typing `hlo` finds `hello world`.
//!
//! Deliberately a small greedy matcher rather than a full fzf port: the corpus
//! is one clipboard history, and the scoring only has to be good enough to put
//! the obvious hit first.

/// Characters scanned per entry before giving up.
///
/// A clipboard entry can be an entire file. Scoring all of it on every
/// keystroke is what would make the palette feel slow, and a match thousands of
/// characters into a blob isn't something you'd recognise in a one-line preview.
const SCAN_LIMIT: usize = 4096;

/// Every matched character is worth this much before bonuses.
const BASE: i32 = 1;
/// Adjacent to the previous match. This has to outweigh [`WORD_START`], or a
/// query spread across word boundaries ("h e l l o") outranks the contiguous
/// run the user actually meant.
const CONSECUTIVE: i32 = 15;
/// First character after a separator.
const WORD_START: i32 = 10;
/// A lowercase-to-uppercase transition, for camelCase identifiers.
const CAMEL_HUMP: i32 = 6;
/// Matching the very first character of the entry.
const LEADING: i32 = 12;
/// Charged per skipped character, so tight matches win.
const GAP_PENALTY: i32 = 1;
/// Ceiling on the gap penalty, so one huge gap can't dominate the score.
const MAX_GAP_PENALTY: i32 = 10;

/// Scores `text` against `query`, or `None` if the query isn't a subsequence.
/// Higher is better. An empty query matches everything at a flat score.
///
/// Known limitation: the scan is greedy, taking the earliest match for each
/// character. Searching `gcm` locks onto the `m` inside "co**m**mit" rather than
/// the `-m` that was meant, so that entry scores lower than it deserves. Fixing
/// it properly means optimal alignment (dynamic programming, as fzf does); a
/// second "initials only" pass was tried and rejected, because a flat acronym
/// bonus ranks "h e l l o" above "hello".
pub fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let needle: Vec<char> = query.to_lowercase().chars().collect();
    let hay: Vec<char> = text.chars().take(SCAN_LIMIT).collect();

    let mut total = 0i32;
    let mut matched = 0usize;
    let mut previous: Option<usize> = None;

    for (index, raw) in hay.iter().enumerate() {
        if matched == needle.len() {
            break;
        }
        if !raw.to_lowercase().eq(std::iter::once(needle[matched])) {
            continue;
        }

        let mut bonus = BASE;
        if index == 0 {
            bonus += LEADING;
        } else {
            let before = hay[index - 1];
            if !before.is_alphanumeric() {
                // Start of a word — usually what someone is aiming at.
                bonus += WORD_START;
            } else if before.is_lowercase() && raw.is_uppercase() {
                bonus += CAMEL_HUMP;
            }
        }

        if let Some(previous) = previous {
            let gap = index - previous - 1;
            if gap == 0 {
                // Runs of adjacent characters are the strongest signal that this
                // is the match the user meant.
                bonus += CONSECUTIVE;
            }
            total -= (gap as i32 * GAP_PENALTY).min(MAX_GAP_PENALTY);
        }

        total += bonus;
        previous = Some(index);
        matched += 1;
    }

    (matched == needle.len()).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_plain_substring() {
        assert!(score("world", "hello world").is_some());
    }

    #[test]
    fn matches_a_scattered_subsequence() {
        assert!(score("hlo", "hello").is_some());
        assert!(score("gcm", "git commit -m").is_some());
    }

    #[test]
    fn rejects_characters_out_of_order() {
        assert!(score("olleh", "hello").is_none());
        assert!(score("xyz", "hello").is_none());
    }

    #[test]
    fn is_case_insensitive() {
        assert!(score("HELLO", "hello").is_some());
        assert!(score("hello", "HELLO").is_some());
    }

    #[test]
    fn an_empty_query_matches_anything() {
        assert_eq!(score("", "whatever"), Some(0));
    }

    #[test]
    fn a_contiguous_run_outscores_a_scattered_one() {
        // Both match, but "hello" as one piece is what you meant.
        let tight = score("hello", "hello there").unwrap();
        let loose = score("hello", "h e l l o").unwrap();
        assert!(tight > loose, "tight {tight} should beat loose {loose}");
    }

    #[test]
    fn a_word_start_outscores_a_mid_word_hit() {
        let start = score("cat", "the cat sat").unwrap();
        let middle = score("cat", "concatenate").unwrap();
        assert!(start > middle, "start {start} should beat middle {middle}");
    }

    #[test]
    fn scoring_stops_at_the_scan_limit() {
        // A match past the cutoff isn't found, which is the documented trade.
        let text = format!("{}needle", "x".repeat(SCAN_LIMIT));
        assert!(score("needle", &text).is_none());

        let early = format!("needle{}", "x".repeat(SCAN_LIMIT));
        assert!(score("needle", &early).is_some());
    }

    #[test]
    fn multibyte_text_does_not_panic_or_mismatch() {
        assert!(score("caf", "café ☕ latte").is_some());
        assert!(score("☕", "café ☕ latte").is_some());
    }
}
