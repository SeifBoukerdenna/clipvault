//! The polling loop: watch the clipboard and record what it holds.

use std::thread;
use std::time::Duration;

use arboard::Clipboard;

use crate::{Result, display, history, poll};

/// Milliseconds between clipboard polls.
const POLL_INTERVAL: Duration = Duration::from_millis(750);

pub fn run() -> Result<()> {
    let mut clipboard = Clipboard::new()?;

    println!("clipvault watching the clipboard — Ctrl+C to stop");

    loop {
        thread::sleep(POLL_INTERVAL);

        let text = match poll::fetch_clipboard(&mut clipboard) {
            Ok(text) => text,
            // Non-text content (an image, a file) or a transient read failure:
            // nothing to record this tick, and no reason to stop watching.
            Err(_) => continue,
        };

        match history::append_history(&text) {
            Ok(()) => println!("  + {}", display::preview(&text, display::PREVIEW_WIDTH)),
            // A failed write shouldn't kill a long-running watcher. Report it
            // and move on.
            Err(e) => eprintln!("clipvault: could not record entry: {e}"),
        }
    }
}
