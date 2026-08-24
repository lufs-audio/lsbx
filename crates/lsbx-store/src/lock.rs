use fs4::fs_std::FileExt;
use lsbx_kernel::error::LsbxError;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Maps a raw `std::io::Error` onto `LsbxError` per house convention:
/// `NotFound` I/O errors become `LsbxError::NotFound`, everything else
/// (permissions, transient I/O failures, etc.) becomes
/// `LsbxError::ContractViolated` since none of the other variants fit.
fn map_io_err(context: &str, e: std::io::Error) -> LsbxError {
    if e.kind() == std::io::ErrorKind::NotFound {
        LsbxError::NotFound(format!("{}: {}", context, e))
    } else {
        LsbxError::ContractViolated(format!("{}: {}", context, e))
    }
}

/// A held advisory lock on a fixed, never-unlinked sentinel file.
///
/// Dropping this guard closes the underlying file descriptor, which is what
/// releases the `flock`. The lock file itself is never unlinked — see the
/// module-level rationale in `LockSentinel`.
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

/// The one locking primitive every process-level lock in `lsbx` is built on
/// (the sandbox-operation lock, and `CiJobStore::broker_lock`).
///
/// Lock files are permanent sentinels: a `LockGuard` never unlinks the path
/// it locked. This is deliberate, not an oversight — the classic failure
/// this avoids is: process A `flock`s path P; something unlinks P and a new
/// file appears at the same path; a late process opens the "new" P and gets
/// an uncontended lock while A still (incorrectly) believes it holds
/// exclusivity. Because the file is never unlinked by this code, that race
/// can only happen if something *external* to this primitive recreates the
/// path — which is exactly the scenario the fstat/stat `(dev, ino)`
/// comparison below detects and retries around.
pub struct LockSentinel;

impl LockSentinel {
    fn open(path: &Path) -> Result<File, LsbxError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| map_io_err("failed to open lock file", e))
    }

    /// Compares the held fd's `fstat` against a fresh `stat` of `path`.
    /// `Ok(true)` means the path still refers to the same inode the fd is
    /// locked on (the acquire is valid); `Ok(false)` means the path was
    /// unlinked-and-recreated (or removed and not yet recreated) underneath
    /// us and the caller must reopen and retry.
    fn still_same_file(file: &File, path: &Path) -> Result<bool, LsbxError> {
        let fstat = file
            .metadata()
            .map_err(|e| map_io_err("failed to fstat held lock fd", e))?;

        match std::fs::metadata(path) {
            Ok(stat) => Ok(fstat.dev() == stat.dev() && fstat.ino() == stat.ino()),
            // The path was unlinked out from under us (and not yet
            // recreated) — definitely not "still the same file".
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(map_io_err("failed to stat lock path", e)),
        }
    }

    /// Blocking acquire. Retries internally if an unlink-and-recreate race
    /// is detected.
    pub fn acquire(path: &Path) -> Result<LockGuard, LsbxError> {
        loop {
            let file = Self::open(path)?;

            file.lock_exclusive()
                .map_err(|e| map_io_err("failed to acquire exclusive lock", e))?;

            if Self::still_same_file(&file, path)? {
                return Ok(LockGuard {
                    _file: file,
                    path: path.to_path_buf(),
                });
            }
            // The file was unlinked and recreated underneath us (or
            // vanished entirely). Unlock (implicit on drop of `file`) and
            // retry against the current path contents.
        }
    }

    /// Non-blocking acquire. Returns `LsbxError::LockContention` immediately
    /// if held elsewhere.
    pub fn try_acquire(path: &Path) -> Result<LockGuard, LsbxError> {
        let file = Self::open(path)?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(LsbxError::LockContention(format!(
                    "lock held elsewhere: {}",
                    path.display()
                )));
            }
            Err(e) => return Err(map_io_err("failed to try-acquire exclusive lock", e)),
        }

        if Self::still_same_file(&file, path)? {
            Ok(LockGuard {
                _file: file,
                path: path.to_path_buf(),
            })
        } else {
            // Someone recreated (or removed) the path between our open and
            // our stat. We hold a lock on a now-orphaned inode that no
            // longer represents the sentinel at `path` — report contention
            // rather than handing back a guard whose fstat/stat invariant
            // is already broken for the *next* acquirer.
            Err(LsbxError::LockContention(format!(
                "lock path was recreated during acquire: {}",
                path.display()
            )))
        }
    }
}
