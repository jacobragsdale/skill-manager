//! Filesystem mutations that survive Windows' transient sharing failures.
//!
//! Windows keeps a handle open on a file for a short while after it is
//! written: antivirus scanners, the search indexer, and Explorer all take
//! brief opportunistic handles. A rename or delete issued inside that window
//! fails with a sharing violation even though the identical call succeeds a
//! moment later. Skill Manager renames and deletes directories immediately
//! after writing them, so it meets that window regularly on Windows and
//! effectively never on Unix.
//!
//! These wrappers retry only the errors that are known to be transient, and
//! only on Windows. Every other platform calls straight through to `std::fs`.

use std::io;
use std::path::Path;
#[cfg(windows)]
use std::time::Duration;

#[cfg(windows)]
const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(160),
    Duration::from_millis(320),
];

/// `ERROR_ACCESS_DENIED`, `ERROR_SHARING_VIOLATION`, `ERROR_LOCK_VIOLATION`,
/// and `ERROR_DIR_NOT_EMPTY`. The last appears when a directory delete races a
/// scanner that still holds one of the children.
#[cfg(windows)]
fn is_transient(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33 | 145))
}

#[cfg(windows)]
fn retrying<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut outcome = operation();
    for delay in RETRY_DELAYS {
        match &outcome {
            Err(error) if is_transient(error) => {}
            _ => return outcome,
        }
        std::thread::sleep(delay);
        outcome = operation();
    }
    outcome
}

#[cfg(not(windows))]
fn retrying<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    operation()
}

pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    retrying(|| std::fs::rename(from, to))
}

pub(crate) fn remove_dir_all(path: &Path) -> io::Result<()> {
    retrying(|| std::fs::remove_dir_all(path))
}

pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    retrying(|| std::fs::remove_file(path))
}
