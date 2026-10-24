use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
#[cfg(test)]
use std::sync::atomic::Ordering;

use crate::error::ConfigError;

/// Callers blocked in [`locked`], raised before they wait, so a test can
/// observe that a second write is blocked rather than sleeping.
#[cfg(test)]
static WAITING: AtomicUsize = AtomicUsize::new(0);

/// How many callers are blocked in [`locked`].
///
/// Raised before the wait, so a test can observe that a second write is
/// blocked rather than sleeping.
#[cfg(test)]
pub(crate) fn waiting() -> usize {
    WAITING.load(Ordering::SeqCst)
}

/// Takes the lock for a whole-file write to `file`: creates the parent
/// directory, then holds `file.lock` until the caller renames over `file`
/// (`docs/state.md`, "Concurrent access").
pub(crate) fn locked(file: &Path) -> Result<File, ConfigError> {
    let lock = open_lock(file, 0o666)?;
    #[cfg(test)]
    WAITING.fetch_add(1, Ordering::SeqCst);
    let outcome = lock.lock().map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    });
    #[cfg(test)]
    WAITING.fetch_sub(1, Ordering::SeqCst);
    outcome?;
    Ok(lock)
}

/// Creates the parent directory and opens `file.lock` with `mode`, without
/// locking it, so a caller chooses to wait or to try.
pub(crate) fn open_lock(file: &Path, mode: u32) -> Result<File, ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    let mut lock_name = file.as_os_str().to_owned();
    lock_name.push(".lock");
    make_parent(file)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(mode)
        .open(&lock_name)
        .map_err(io)
}

fn make_parent(file: &Path) -> Result<(), ConfigError> {
    let Some(dir) = file.parent() else {
        return Ok(());
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|source| ConfigError::Io {
            file: dir.to_path_buf(),
            source,
        })
}

/// Maps a [`support::write_atomic`] failure onto the destination file: a
/// post-rename directory-sync failure (carrying
/// [`support::atomic::Unsynced`]) becomes [`ConfigError::Unsynced`] with the
/// new file already in place, anything else becomes [`ConfigError::Io`].
/// The owned inner error is recovered whole, so its kind, message and
/// platform error survive; only the phase decides the variant.
pub fn atomic_error(file: &Path, mut error: std::io::Error) -> ConfigError {
    let unsynced = error
        .get_mut()
        .and_then(|inner| inner.downcast_mut::<support::atomic::Unsynced>())
        .map(|marker| std::mem::replace(&mut marker.0, std::io::Error::other("moved")));
    match unsynced {
        Some(source) => ConfigError::Unsynced {
            file: file.to_path_buf(),
            source,
        },
        None => ConfigError::Io {
            file: file.to_path_buf(),
            source: error,
        },
    }
}

/// Writes `bytes` to a temporary file created with `mode` beside `file`,
/// syncs it, and renames it over `file`, so a reader sees the old file or the
/// new one, never half.
pub fn write_atomic(file: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
    make_parent(file)?;
    support::write_atomic(file, bytes, mode).map_err(|error| atomic_error(file, error))
}

// The failure-injection seam lives in `support` behind its `test-hooks`
// feature, enabled for these tests through the dev-dependency; these names
// re-expose it so every existing test `use` line compiles unedited.
#[cfg(test)]
pub(crate) use support::atomic::{Stage, before_rename, fail_at, fail_nth};

#[cfg(test)]
#[path = "atomic_tests.rs"]
mod tests;
