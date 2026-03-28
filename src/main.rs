use std::process::ExitCode;

use clipvault::{Result, display, history, watch};

const DEFAULT_LIST_COUNT: usize = 20;

const USAGE: &str = "\
clipvault — a clipboard history keeper

USAGE:
    clipvault [watch]           Poll the clipboard and record every change (default)
    clipvault list [-n N]       Show the last N entries (default 20)
    clipvault help              Show this message

History lives in ~/.clipvault/history.jsonl.

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
