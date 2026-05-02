//! The polling loop: watch the clipboard and record what it holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arboard::Clipboard;

use crate::{Result, display, history, lock, poll};

/// Milliseconds between clipboard polls.
const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// Entries kept on disk; older ones are pruned as new ones arrive.
const HISTORY_LIMIT: usize = 1000;

/// Captures between prunes.
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
    if let Err(e) = history::prune(HISTORY_LIMIT) {
        eprintln!("clipvault: could not prune history: {e}");
    }

    while running.load(Ordering::SeqCst) {
        sleep_interruptibly(POLL_INTERVAL, &running);
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let text = match poll::fetch_clipboard(&mut clipboard) {
            Ok(text) => text,
            // Non-text content (an image, a file) or a transient read failure:
            // nothing to record this tick, and no reason to stop watching.
            Err(_) => continue,
        };

        if text.trim().is_empty() {
            continue;
        }

        // Same content as last poll — the user simply hasn't copied anything new.
        if last.as_deref() == Some(text.as_str()) {
            continue;
        }

        match history::append_history(&text) {
            Ok(()) => {
                captured += 1;
                println!("  + {}", display::preview(&text, display::PREVIEW_WIDTH));

                // Pruning rewrites the whole file, so it runs on a cadence
                // rather than on every capture.
                since_prune += 1;
                if since_prune >= PRUNE_EVERY_CAPTURES {
                    since_prune = 0;
                    if let Err(e) = history::prune(HISTORY_LIMIT) {
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

/// Sleeps for `total`, waking every `TICK` to check whether we've been asked to stop.
fn sleep_interruptibly(total: Duration, running: &AtomicBool) {
    let mut slept = Duration::ZERO;
    while slept < total && running.load(Ordering::SeqCst) {
        let slice = TICK.min(total - slept);
        std::thread::sleep(slice);
        slept += slice;
    }
}
