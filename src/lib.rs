//! Shared core behind the `clipvault` CLI and the `clipvault-menubar` app.

pub mod display;
pub mod history;
pub mod hotkey;
pub mod lock;
pub mod poll;
pub mod source;
pub mod watch;

/// The macOS menu bar app.
#[cfg(target_os = "macos")]
pub mod menubar;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
