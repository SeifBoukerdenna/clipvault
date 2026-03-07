# Scoping notes

What I wanted out of this before writing any of it. Kept around because the
shape barely changed.

## Core behavior (watch mode)

Poll the system clipboard on an interval (start around 500ms–1s)
Only record when the content actually changed since the last poll — no duplicate spam if you don't touch the clipboard
Skip empty/whitespace-only content
Each captured entry needs a timestamp + the text itself

## Storage

Persist somewhere in the home dir, e.g. ~/.clipvault/history.jsonl
Append-only, one entry per line, so you're never rewriting the whole file on each capture
Each line: timestamp + content, in whatever serialized form you like

## Commands

watch (default) — runs the polling loop, prints each new capture as it happens
list -n N — show the last N entries with timestamp + a readable preview
search <term> — case-insensitive substring match across history, print matches with timestamps

## Edge cases worth handling

Truncate/flatten long or multi-line entries for display (raw newlines will wreck your terminal output)
Non-text clipboard content (images, files) — the clipboard call should fail gracefully there, not crash your loop
Don't log the same content twice in a row
Clean exit on Ctrl+C since this runs indefinitely

Stretch goals once it works

launchd plist so it runs as a background service instead of a terminal window
clipvault copy <index> to push an old entry back onto the clipboard
Fuzzy search instead of plain substring
A little TUI (ratatui) to browse instead of scrolling raw text
Cap history size / auto-prune