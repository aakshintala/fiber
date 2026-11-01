//! An atomic whole-file write: bytes go to a temporary file beside the
//! destination, are synced, and are renamed over it, so a reader sees the
//! old file or the new one, never half (`docs/state.md`, "Concurrent
//! access"). A write that fails before its rename leaves the old file; one
//! that fails after it, while syncing the directory, returns an error
//! carrying [`Unsynced`] with the new file already in place. This module
//! never creates directories: a missing parent fails the temporary create
//! with `NotFound`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many fresh temporary names one write draws before giving up with
/// [`std::io::ErrorKind::AlreadyExists`].
const TEMP_TRIES: u32 = 64;

/// The counter behind temporary names: each candidate name takes the next
/// value, so two writers in one process never draw the same name, while
/// `create_new` arbitrates names against writers in other processes.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A directory-sync failure after the rename, carried as the `io::Error`'s
/// inner error. The display shows the sync failure's message and `source`
/// returns the sync failure itself, so the kind, message and error payload
/// survive the trip through `io::Error::new`.
#[derive(Debug)]
pub struct Unsynced(pub std::io::Error);

impl std::fmt::Display for Unsynced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Unsynced {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

// Pause point between creating the temporary file and renaming it over the
// destination (`docs/testing.md`, "Waits and timeouts"): a test installs a
// hook to hold the writer there, so the race between the temporary file
// being visible and the rename happens on every run instead of being waited
// for. Test-only; non-test builds never call it.
#[cfg(debug_assertions)]
thread_local! {
    static BEFORE_RENAME: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

/// Installs the pause-point hook run between creating the temporary file and
/// renaming it (`docs/testing.md`, "Waits and timeouts"), on the current
/// thread. Test-only; non-test builds never call it.
#[cfg(debug_assertions)]
pub fn before_rename(hook: impl Fn() + 'static) {
    BEFORE_RENAME.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

/// Where [`fail_at`] injects a write failure in `write_atomic`.
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Before the rename: the file is left unchanged.
    BeforeRename,
    /// After the rename, syncing the directory: the new content is in place.
    AfterRename,
}

#[cfg(debug_assertions)]
thread_local! {
    static FAIL_AT: std::cell::Cell<Option<Stage>> = const { std::cell::Cell::new(None) };
}

#[cfg(debug_assertions)]
thread_local! {
    static FAIL_NTH: std::cell::Cell<Option<(usize, Stage, usize)>> =
        const { std::cell::Cell::new(None) };
}

/// Injects an I/O failure at `stage` in `write_atomic` on this thread;
/// `None` clears it. Test-only; non-test builds never fail.
#[cfg(debug_assertions)]
pub fn fail_at(stage: Option<Stage>) {
    FAIL_AT.with(|cell| cell.set(stage));
}

#[cfg(debug_assertions)]
fn fail_stage() -> Option<Stage> {
    FAIL_AT.with(|cell| cell.get())
}

/// Fails only the `n`th [`write_atomic`] call on this thread at `stage`:
/// `BeforeRename` leaves the file unchanged, `AfterRename` fails the
/// directory sync with the new content already in place. Calls count from
/// one; every other call runs as [`fail_at`] says. Test-only; non-test
/// builds never fail.
#[cfg(debug_assertions)]
pub fn fail_nth(n: usize, stage: Stage) {
    FAIL_NTH.with(|cell| cell.set(Some((n, stage, 0))));
}

/// Counts this thread's [`write_atomic`] calls, starting at one, so
/// [`fail_nth`] names one of them; zero when no `fail_nth` is armed.
#[cfg(debug_assertions)]
fn call_count() -> usize {
    FAIL_NTH.with(|cell| match cell.get() {
        Some((n, stage, seen)) => {
            cell.set(Some((n, stage, seen.saturating_add(1))));
            seen.saturating_add(1)
        }
        None => 0,
    })
}

// Every `write_atomic` call on this thread, whatever its outcome, so a
// test can assert how many writes an operation made. Unlike the private
// count behind `fail_nth`, this one runs whether or not a failure is
// armed. Test-only; non-test builds never count.
#[cfg(debug_assertions)]
thread_local! {
    static WRITE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Bumps the [`write_count`] counter once per [`write_atomic`] entry.
#[cfg(debug_assertions)]
fn count_write() {
    WRITE_COUNT.with(|cell| cell.set(cell.get().saturating_add(1)));
}

/// How many [`write_atomic`] calls this thread made since the last
/// [`reset_write_count`]. Test-only; non-test builds never count.
#[cfg(debug_assertions)]
pub fn write_count() -> usize {
    WRITE_COUNT.with(|cell| cell.get())
}

/// Zeroes this thread's [`write_count`]. Test-only; non-test builds never
/// count.
#[cfg(debug_assertions)]
pub fn reset_write_count() {
    WRITE_COUNT.with(|cell| cell.set(0));
}

/// Whether [`fail_nth`] armed the `call`th [`write_atomic`] call to fail
/// at `stage`.
#[cfg(debug_assertions)]
fn fail_nth_stage(call: usize, stage: Stage) -> bool {
    FAIL_NTH.with(|cell| matches!(cell.get(), Some((n, at, _)) if n == call && at == stage))
}

/// Writes `bytes` to a temporary file created with `mode` beside `file`,
/// syncs it, and renames it over `file`, so a reader sees the old file or the
/// new one, never half.
pub fn write_atomic(file: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    #[cfg(debug_assertions)]
    let call = call_count();
    #[cfg(debug_assertions)]
    count_write();
    let pid = std::process::id();
    let (tmp, mut out) = {
        let mut tried = 0;
        loop {
            if tried >= TEMP_TRIES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "{}: no free temporary name after {tried} tries",
                        file.display()
                    ),
                ));
            }
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let mut tmp_name = file.as_os_str().to_owned();
            tmp_name.push(format!(".{pid}-{n}.tmp"));
            let candidate = PathBuf::from(tmp_name);
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(mode)
                .open(&candidate)
            {
                Ok(out) => break (candidate, out),
                // A name this call did not create stays put; the next
                // counter value names the retry.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    tried += 1;
                }
                Err(error) => return Err(error),
            }
        }
    };
    // Syncing the temporary file makes its bytes survive a crash; syncing
    // the directory afterwards makes the rename itself survive one.
    let written = out
        .write_all(bytes)
        .and_then(|()| out.sync_all())
        .and_then(|()| {
            #[cfg(debug_assertions)]
            BEFORE_RENAME.with(|cell| {
                if let Some(hook) = cell.borrow().as_ref() {
                    hook();
                }
            });
            #[cfg(debug_assertions)]
            if fail_stage() == Some(Stage::BeforeRename)
                || fail_nth_stage(call, Stage::BeforeRename)
            {
                return Err(std::io::Error::other("injected"));
            }
            fs::rename(&tmp, file)
        });
    // Only the temporary this call opened is ever removed: a candidate that
    // lost the `create_new` race belongs to another writer.
    if written.is_err() {
        fs::remove_file(&tmp).unwrap_or(());
    }
    written?;
    let dir = file.parent().unwrap_or(Path::new("."));
    (|| {
        #[cfg(debug_assertions)]
        if fail_stage() == Some(Stage::AfterRename) || fail_nth_stage(call, Stage::AfterRename) {
            return Err(std::io::Error::other("injected"));
        }
        File::open(dir)?.sync_all()
    })()
    .map_err(|source| {
        let kind = source.kind();
        std::io::Error::new(kind, Unsynced(source))
    })
}

#[cfg(test)]
#[path = "atomic_tests.rs"]
mod tests;
