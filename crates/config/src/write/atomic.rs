use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};

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

static NEXT: AtomicU64 = AtomicU64::new(0);

// Pause point between creating the temporary file and renaming it over the
// destination (`docs/testing.md`, "Waits and timeouts"): the
// credential-mode test installs a hook to hold the writer there, so the race
// between the temporary file being visible and the rename happens on every
// run instead of being waited for. Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static BEFORE_RENAME: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

/// Installs the pause-point hook run between creating the temporary file and
/// renaming it (`docs/testing.md`, "Waits and timeouts"), on the current
/// thread. The credential-mode test uses it to hold the writer with the
/// temporary file visible, so the race happens on every run.
#[cfg(test)]
pub(crate) fn before_rename(hook: impl Fn() + 'static) {
    BEFORE_RENAME.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

/// Where [`fail_at`] injects a write failure in `write_atomic`.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// Before the rename: the file is left unchanged.
    BeforeRename,
    /// After the rename, syncing the directory: the new content is in place.
    AfterRename,
}

#[cfg(test)]
thread_local! {
    static FAIL_AT: std::cell::Cell<Option<Stage>> = const { std::cell::Cell::new(None) };
}

/// Injects an I/O failure at `stage` in `write_atomic` on this thread;
/// `None` clears it. Test-only; non-test builds never fail.
#[cfg(test)]
pub(crate) fn fail_at(stage: Option<Stage>) {
    FAIL_AT.with(|cell| cell.set(stage));
}

#[cfg(test)]
fn fail_stage() -> Option<Stage> {
    FAIL_AT.with(|cell| cell.get())
}

/// Writes `bytes` to a temporary file created with `mode` beside `file`,
/// syncs it, and renames it over `file`, so a reader sees the old file or the
/// new one, never half.
pub fn write_atomic(file: &Path, bytes: &[u8], mode: u32) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    };
    make_parent(file)?;
    let mut tmp_name = file.as_os_str().to_owned();
    tmp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp_name);
    let dir = file.parent().unwrap_or(Path::new("."));
    // Syncing the directory makes the rename itself survive a crash.
    let written = write_synced(&tmp, bytes, mode)
        .and_then(|()| {
            #[cfg(test)]
            BEFORE_RENAME.with(|cell| {
                if let Some(hook) = cell.borrow().as_ref() {
                    hook();
                }
            });
            #[cfg(test)]
            if fail_stage() == Some(Stage::BeforeRename) {
                return Err(std::io::Error::other("injected"));
            }
            fs::rename(&tmp, file)
        })
        .and_then(|()| {
            #[cfg(test)]
            if fail_stage() == Some(Stage::AfterRename) {
                return Err(std::io::Error::other("injected"));
            }
            File::open(dir)?.sync_all()
        });
    if written.is_err() {
        fs::remove_file(&tmp).unwrap_or(());
    }
    written.map_err(io)
}

fn write_synced(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut out: File = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)?;
    out.write_all(bytes)?;
    out.sync_all()
}
