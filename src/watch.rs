//! The polling loop: watch the clipboard and record what it holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use arboard::Clipboard;

use crate::{Result, display, history, poll};

/// Milliseconds between clipboard polls.
const POLL_INTERVAL: Duration = Duration::from_millis(750);

/// Slice length for the interruptible sleep, so Ctrl+C is acted on promptly
/// instead of after a full poll interval.
const TICK: Duration = Duration::from_millis(50);

pub fn run() -> Result<()> {
    let mut clipboard = Clipboard::new()?;

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))?;
    }

    println!("clipvault watching the clipboard — Ctrl+C to stop");

    let mut captured = 0usize;

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

        match history::append_history(&text) {
            Ok(()) => {
                captured += 1;
                println!("  + {}", display::preview(&text, display::PREVIEW_WIDTH));
            }
            // A failed write shouldn't kill a long-running watcher. Report it
            // and move on.
            Err(e) => eprintln!("clipvault: could not record entry: {e}"),
        }
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
