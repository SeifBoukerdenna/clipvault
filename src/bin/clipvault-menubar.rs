//! Entry point for the ClipVault menu bar app.

use std::process::ExitCode;

#[cfg(target_os = "macos")]
fn main() -> ExitCode {
    match clipvault::menubar::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("clipvault-menubar: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> ExitCode {
    eprintln!("clipvault-menubar: the menu bar app is macOS-only — use `clipvault watch` here");
    ExitCode::FAILURE
}
