//! A single-instance lock.
//!
//! Without this, running `clipvault watch` while the menu bar app is open means
//! two pollers appending the same copy to the same file — every entry recorded
//! twice. The lock is advisory and held for as long as the process lives; the
//! OS releases it automatically if the process is killed, so a crash can't
//! leave a stale lock behind the way a PID file would.

use std::fs::File;
use std::path::Path;

use crate::Result;
use crate::history::clipvault_dir;

/// Holds the lock. Releasing happens when this is dropped, or when the process
/// exits for any reason.
pub struct InstanceLock {
    _file: File,
}

/// Takes the lock, or returns `Ok(None)` if another instance already holds it.
pub fn acquire() -> Result<Option<InstanceLock>> {
    let mut path = clipvault_dir()?;
    path.push("instance.lock");
    acquire_at(&path)
}

/// The body of [`acquire`], against an explicit path so tests don't have to
/// fight over the one real lock file in the home directory.
#[cfg(unix)]
fn acquire_at(path: &Path) -> Result<Option<InstanceLock>> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;

    // SAFETY: a plain flock on a file descriptor we own and keep alive.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;

    if !locked {
        return Ok(None);
    }

    Ok(Some(InstanceLock { _file: file }))
}

/// Nothing to coordinate with off unix, so the lock is always available.
#[cfg(not(unix))]
fn acquire_at(path: &Path) -> Result<Option<InstanceLock>> {
    Ok(Some(InstanceLock {
        _file: File::create(path)?,
    }))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A lock file per test, so tests can run in parallel and don't collide
    /// with a real ClipVault running on the machine.
    fn lock_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("clipvault-{}-{}.lock", name, std::process::id()));
        path
    }

    #[test]
    fn a_free_lock_is_acquired() {
        let path = lock_path("free");
        let held = acquire_at(&path).unwrap();
        assert!(held.is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_held_lock_turns_the_second_caller_away() {
        let path = lock_path("contended");
        let _first = acquire_at(&path).unwrap().expect("first should get it");

        // flock is per open file description, so this conflicts even though it
        // is the same process asking — which is exactly the case that matters,
        // since the CLI and the menu bar app both call acquire().
        assert!(
            acquire_at(&path).unwrap().is_none(),
            "a second acquire should be turned away while the first is held"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dropping_the_lock_releases_it() {
        let path = lock_path("released");

        let first = acquire_at(&path).unwrap().expect("first should get it");
        drop(first);

        assert!(
            acquire_at(&path).unwrap().is_some(),
            "the lock should be free again once the holder is dropped"
        );
        let _ = std::fs::remove_file(&path);
    }
}
