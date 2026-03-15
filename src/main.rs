use std::process::ExitCode;

use clipvault::watch;

fn main() -> ExitCode {
    match watch::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clipvault: {e}");
            ExitCode::FAILURE
        }
    }
}
