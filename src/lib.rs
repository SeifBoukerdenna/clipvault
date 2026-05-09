//! Shared core behind the `clipvault` CLI.

pub mod display;
pub mod history;
pub mod lock;
pub mod poll;
pub mod source;
pub mod watch;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
