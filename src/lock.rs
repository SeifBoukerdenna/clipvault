//! A single-instance lock.
//!
//! Without this, running `clipvault watch` while the menu bar app is open means
//! two pollers appending the same copy to the same file — every entry recorded
//! twice. The lock is advisory and held for as long as the process lives; the
//! OS releases it automatically if the process is killed, so a crash can't
//! leave a stale lock behind the way a PID file would.

use std::fs::{File, OpenOptions};

use crate::Result;
use crate::history::clipvault_dir;

/// Holds the lock. Releasing happens when this is dropped, or when the process
/// exits for any reason.
pub struct InstanceLock {
    _file: File,
}

/// Takes the lock, or returns `Ok(None)` if another instance already holds it.
#[cfg(unix)]
pub fn acquire() -> Result<Option<InstanceLock>> {
    use std::os::unix::io::AsRawFd;

    let mut path = clipvault_dir()?;
    path.push("instance.lock");

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;

    // SAFETY: a plain flock on a file descriptor we own and keep alive.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;

    if !locked {
        return Ok(None);
    }

    Ok(Some(InstanceLock { _file: file }))
}
