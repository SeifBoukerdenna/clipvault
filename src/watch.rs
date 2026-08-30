//! The polling loop: watch the clipboard, record genuine changes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arboard::Clipboard;

use crate::{Result, config, display, history, lock, poll, source};

/// Captures between prunes, matching the menu bar app.
const PRUNE_EVERY_CAPTURES: u32 = 25;

/// Slice length for the interruptible sleep, so Ctrl+C is acted on promptly
/// instead of after a full poll interval.
const TICK: Duration = Duration::from_millis(50);

pub fn run() -> Result<()> {
    // The menu bar app polls the same clipboard into the same file. Without
    // this, running both records every copy twice.
    let Some(_instance) = lock::acquire()? else {
        return Err(
            "clipvault is already watching (the menu bar app, or another \
                    `clipvault watch`) — only one watcher can run at a time"
                .into(),
        );
    };

    // The same settings the menu bar app uses; running the two watchers with
    // different intervals or retention would be surprising.
    let settings = config::load();
    let poll_interval = Duration::from_millis(settings.poll_interval_ms);

    let mut clipboard = Clipboard::new()?;

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))?;
    }

    // Seed with whatever is already on the clipboard so restarting the watcher
    // doesn't re-record content that was copied before it started. Only changes
    // observed from here on are captured.
    let mut last = poll::fetch_clipboard(&mut clipboard).ok();

    println!("clipvault watching the clipboard — Ctrl+C to stop");

    let mut captured = 0usize;
    let mut since_prune = 0u32;

    // Trim anything a previous run left over the limit.
    if let Err(e) = history::prune(settings.history_limit) {
        eprintln!("clipvault: could not prune history: {e}");
    }

    while running.load(Ordering::SeqCst) {
        sleep_interruptibly(poll_interval, &running);
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let text = match poll::fetch_clipboard(&mut clipboard) {
            Ok(text) => text,
            // Non-text content (image, file) or a transient read failure:
            // nothing to record this tick, and no reason to stop watching.
            Err(_) => continue,
        };

        if !is_new_content(&text, last.as_deref()) {
            continue;
        }

        // Passwords and other secrets are flagged on the pasteboard itself.
        // `last` still advances, so this isn't re-examined every poll.
        if poll::is_concealed() {
            last = Some(text);
            continue;
        }

        match history::append_history(&text, source::frontmost().as_ref()) {
            Ok(()) => {
                captured += 1;
                println!("  + {}", display::preview(&text, display::PREVIEW_WIDTH));

                // Pruning rewrites the whole file, so it runs on a cadence
                // rather than on every capture.
                since_prune += 1;
                if since_prune >= PRUNE_EVERY_CAPTURES {
                    since_prune = 0;
                    if let Err(e) = history::prune(settings.history_limit) {
                        eprintln!("clipvault: could not prune history: {e}");
                    }
                }
            }
            // A failed write shouldn't kill a long-running watcher. Report it and
            // move on; `last` still advances so one bad entry can't spam the log.
            Err(e) => eprintln!("clipvault: could not record entry: {e}"),
        }

        last = Some(text);
    }

    let noun = if captured == 1 { "entry" } else { "entries" };
    println!("\nstopped — {captured} {noun} captured this session");
    Ok(())
}

/// Whether a poll's text is worth recording, given what the previous poll saw.
///
/// Pulled out of the loop because it is the only part of a tick that is a
/// decision rather than an effect, which is what makes it worth testing.
/// Whitespace-only content is not something anyone meant to copy, and identical
/// content means the user simply hasn't copied anything new since the last poll.
fn is_new_content(text: &str, last: Option<&str>) -> bool {
    !text.trim().is_empty() && last != Some(text)
}

/// Sleeps for `total`, waking every `TICK` to check whether we've been asked to stop.
fn sleep_interruptibly(total: Duration, running: &AtomicBool) {
    let mut slept = Duration::ZERO;
    while slept < total && running.load(Ordering::SeqCst) {
        let slice = TICK.min(total - slept);
        std::thread::sleep(slice);
        slept += slice;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_content_is_recorded() {
        assert!(is_new_content("hello", None));
        assert!(is_new_content("hello", Some("something else")));
    }

    #[test]
    fn repeating_the_last_capture_is_not() {
        // The clipboard reads the same on every poll until you copy again, so
        // this is the common case, not the edge case.
        assert!(!is_new_content("hello", Some("hello")));
    }

    #[test]
    fn whitespace_only_content_is_skipped() {
        assert!(!is_new_content("", None));
        assert!(!is_new_content("   ", None));
        assert!(!is_new_content("\n\t\n", None));
    }

    #[test]
    fn surrounding_whitespace_still_counts_as_content() {
        // Copying an indented line of code is normal; only the empty case is
        // uninteresting, and the entry is stored with its whitespace intact.
        assert!(is_new_content("    indented", None));
    }

    #[test]
    fn a_whitespace_difference_is_a_real_change() {
        // Trimming is a test for emptiness, not the comparison key — copying
        // the same word with different indentation is a genuine new capture.
        assert!(is_new_content("hello ", Some("hello")));
    }
}
