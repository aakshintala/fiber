//! One temporary directory per test, removed on drop
//! (`docs/testing.md`, "Running tests").

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::BuildHasher;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// How many names [`TempDir::new`] tries before giving up.
const ATTEMPTS: usize = 64;

static NEXT: AtomicU64 = AtomicU64::new(0);
static STATE: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// A directory under the system temporary directory, owned by the caller.
///
/// Drop removes it, including a nested directory left mode `0o000` or
/// `0o555`. A directory the caller already removed is left alone.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates `<temp>/<prefix>-<8 lowercase hex digits>`.
    ///
    /// The directory did not exist: `create_dir` fails when the path is
    /// taken, and another suffix is drawn. Panics if creating the directory
    /// fails for any other reason, or after [`ATTEMPTS`] names were taken.
    pub fn new(prefix: &str) -> Self {
        let path = create(&system_temp(), prefix, std::iter::from_fn(suffix));
        Self { path }
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // `NotFound` is not success: "already gone" is distinct from
        // "could not remove".
        if fs::remove_dir_all(&self.path).is_ok() {
            return;
        }
        // A test may have removed the directory itself. Any other error
        // is "not missing".
        match fs::symlink_metadata(&self.path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Ok(_) | Err(_) => {}
        }
        restore_modes(&self.path);
        match fs::remove_dir_all(&self.path) {
            Ok(()) | Err(_) => {}
        }
    }
}

fn system_temp() -> PathBuf {
    std::env::temp_dir()
}

/// Makes every directory under `path` writable, then readable, so a later
/// `remove_dir_all` can finish. A symlink is left alone: following it would
/// change a tree outside `path`.
fn restore_modes(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return;
    }
    let mut perms = meta.permissions();
    perms.set_mode(0o755);
    if fs::set_permissions(path, perms).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        restore_modes(&entry.path());
    }
}

/// Eight lowercase hex digits. A process-wide counter is hashed so two calls
/// in one process differ; [`RandomState`]'s keys differ per process.
fn suffix() -> Option<String> {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let hash = STATE.hash_one(n) & 0xffff_ffff;
    Some(format!("{hash:08x}"))
}

/// Creates `parent`/`prefix`-`suffix` with `create_dir`. An existing path,
/// including one a killed run left behind, is skipped. `suffixes` ends or
/// [`ATTEMPTS`] names are taken, then this panics naming `prefix`.
#[allow(
    clippy::panic,
    reason = "a test directory that cannot be created cannot run its test"
)]
fn create(parent: &Path, prefix: &str, suffixes: impl Iterator<Item = String>) -> PathBuf {
    for suffix in suffixes.take(ATTEMPTS) {
        let path = parent.join(format!("{prefix}-{suffix}"));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => panic!("creating {}: {err}", path.display()),
        }
    }
    panic!("no unique directory for prefix {prefix}");
}

#[cfg(test)]
#[path = "temp_dir_tests.rs"]
mod tests;
