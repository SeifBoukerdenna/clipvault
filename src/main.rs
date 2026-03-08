use std::thread;
use std::time::Duration;

use arboard::Clipboard;
use clipvault::{Result, history, poll};

/// Milliseconds between clipboard polls.
const POLL_INTERVAL: Duration = Duration::from_millis(750);

fn main() -> Result<()> {
    let mut clipboard = Clipboard::new()?;

    println!("clipvault watching the clipboard — Ctrl+C to stop");

    loop {
        thread::sleep(POLL_INTERVAL);

        // Non-text content (an image, a file) or a transient read failure:
        // nothing to record this tick, and no reason to stop watching.
        let Ok(text) = poll::fetch_clipboard(&mut clipboard) else {
            continue;
        };

        if let Err(e) = history::append_history(&text) {
            eprintln!("clipvault: could not record entry: {e}");
        }
    }
}
