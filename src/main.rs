use std::process::ExitCode;

use clipvault::{Result, display, history, poll, watch};

const DEFAULT_LIST_COUNT: usize = 20;

const USAGE: &str = "\
clipvault — a clipboard history keeper

USAGE:
    clipvault [watch]           Poll the clipboard and record every change (default)
    clipvault list [-n N]       Show the last N entries (default 20)
    clipvault search <term>     Case-insensitive substring search across history
    clipvault copy <index>      Put a past entry back on the clipboard
    clipvault help              Show this message

History lives in ~/.clipvault/history.jsonl. Indices shown by `list` and
`search` are absolute positions in that file, so they stay valid as new
entries arrive and can be passed straight to `copy`.

The menu bar app (ClipVault.app) watches and reads the same file. Build it
with ./scripts/bundle.sh --install, then press Cmd+Shift+V for the history.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clipvault: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let (command, rest): (&str, &[String]) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => ("watch", &[]),
    };

    match command {
        "watch" => watch::run(),
        "list" => list(rest),
        "search" => search(rest),
        "copy" => copy(rest),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command '{other}'\n\n{USAGE}").into()),
    }
}

fn list(args: &[String]) -> Result<()> {
    let count = parse_count(args)?;
    let entries = history::read_history()?;

    if entries.is_empty() {
        println!("no history yet — run `clipvault watch` to start capturing");
        return Ok(());
    }

    let start = entries.len().saturating_sub(count);
    for (i, entry) in entries.iter().enumerate().skip(start) {
        println!("{}", display::format_entry(i + 1, entry));
    }

    Ok(())
}

fn parse_count(args: &[String]) -> Result<usize> {
    let mut count = DEFAULT_LIST_COUNT;
    let mut args = args.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-n" | "--number" => {
                let raw = args
                    .next()
                    .ok_or("`-n` needs a number, e.g. `clipvault list -n 20`")?;
                count = raw
                    .parse()
                    .map_err(|_| format!("invalid count '{raw}' — expected a whole number"))?;
            }
            other => return Err(format!("unexpected argument '{other}' for `list`").into()),
        }
    }

    Ok(count)
}

fn search(args: &[String]) -> Result<()> {
    let term = match args {
        [term] => term,
        [] => return Err("`search` needs a term, e.g. `clipvault search ssh`".into()),
        _ => return Err("`search` takes a single term — quote it if it contains spaces".into()),
    };

    let needle = term.to_lowercase();
    let entries = history::read_history()?;

    let matches: Vec<(usize, &history::HistoryEntry)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.content.to_lowercase().contains(&needle))
        .collect();

    if matches.is_empty() {
        println!("no matches for '{term}'");
        return Ok(());
    }

    for (index, entry) in matches {
        println!("{}", display::format_entry(index + 1, entry));
    }

    Ok(())
}

fn copy(args: &[String]) -> Result<()> {
    let raw = match args {
        [index] => index,
        _ => {
            return Err(
                "`copy` needs one index from `clipvault list`, e.g. `clipvault copy 42`".into(),
            );
        }
    };

    let index: usize = raw
        .parse()
        .map_err(|_| format!("invalid index '{raw}' — expected a whole number"))?;

    let entries = history::read_history()?;
    let entry = index
        .checked_sub(1)
        .and_then(|i| entries.get(i))
        .ok_or_else(|| format!("no entry {index} — history holds {} entries", entries.len()))?;

    poll::set_clipboard(&entry.content)?;
    println!(
        "copied {}",
        display::preview(&entry.content, display::PREVIEW_WIDTH)
    );

    Ok(())
}
