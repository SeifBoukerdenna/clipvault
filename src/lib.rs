//! Shared core behind the `clipvault` CLI.

pub mod display;
pub mod history;
pub mod poll;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
