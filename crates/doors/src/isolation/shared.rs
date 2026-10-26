//! One lock file per worktree (`docs/invocation.md`, "Isolation"): every
//! process that runs in a Fiber worktree it did not just create holds a
//! shared `flock` on `worktrees/<id>.lock` for its life, so an end-of-run
//! removal never takes a worktree another process works in. A recreating
//! resume holds the file exclusively while it checks for the worktree and
//! makes it again, then shares it; a removal takes it exclusively without
//! waiting, and keeps the worktree when another process shares it.
//!
//! Every acquisition opens the same file with create, then checks that the
//! descriptor it locked is still the file at the lock path (the same
//! device and inode): a removal unlinks the file while still holding its
//! exclusive take, so a waiter that locked the old inode drops it and
//! reopens. Dropping a hold releases it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::SessionError;

/// How many times `claim` tries for the worktree before it reports the
/// worktree in use: a path that stays absent while a process holds the
/// lock means the worktree was deleted by hand under a running session.
pub(crate) const ATTEMPTS: usize = 4;

/// A worktree's shared hold, kept until dropped. A second share coexists
/// with it; an exclusive take waits behind it.
#[derive(Debug)]
pub struct Shared {
    _file: File,
}

/// What `claim` found at the worktree path.
#[derive(Debug)]
pub(crate) enum Claim {
    /// The path is absent: the holder keeps `LOCK_EX` until it makes the
    /// worktree, then downgrades.
    Create(Creating),
    /// The path is present: the holder keeps `LOCK_SH` for its life.
    Present(Shared),
}

/// An exclusive hold on a worktree's lock file, kept while the holder
/// makes the worktree at its absent path.
#[derive(Debug)]
pub(crate) struct Creating {
    file: File,
    lock: PathBuf,
}

impl Creating {
    /// Shares the held file: the conversion from the exclusive hold, with
    /// no unlock in between.
    pub(crate) fn downgrade(self) -> Result<Shared, SessionError> {
        self.file
            .lock_shared()
            .map_err(|source| io_failed(&self.lock, source))?;
        Ok(Shared { _file: self.file })
    }
}

/// What `exclusive` found at the lock file.
#[derive(Debug)]
pub(crate) enum Removal {
    /// The file is held exclusively until dropped: no process shares the
    /// worktree.
    Exclusive(File),
    /// Another process shares the worktree: the removal keeps it.
    InUse,
}

/// Shares the worktree's lock file for the process's life, creating the
/// file. Blocks only while another process recreates or removes the
/// worktree, both bounded by one git command.
pub fn share(worktree: &Path) -> Result<Shared, SessionError> {
    share_with(worktree, &|| {})
}

/// [`share`] with a hook the test parks on: it runs before every blocking
/// wait. Production passes a no-op. The hook exists because no outside
/// seam can observe that a process is blocked in `flock`.
pub(crate) fn share_with(worktree: &Path, before_wait: &dyn Fn()) -> Result<Shared, SessionError> {
    let lock = lock_path(worktree).ok_or_else(|| SessionError::ShareFailed {
        path: worktree.to_owned(),
        kind: io::ErrorKind::InvalidInput,
    })?;
    for _ in 0..ATTEMPTS {
        let file = open(&lock, false).map_err(|source| io_failed(&lock, source))?;
        before_wait();
        file.lock_shared()
            .map_err(|source| io_failed(&lock, source))?;
        if same_file(&file, &lock) {
            return Ok(Shared { _file: file });
        }
    }
    Err(SessionError::ShareFailed {
        path: lock,
        kind: io::ErrorKind::TimedOut,
    })
}

/// Claims the worktree: `Create` when its path is absent, `Present` when
/// another process already runs in it. Loops at most [`ATTEMPTS`] times:
/// an exclusive take that cannot be had waits shared instead, and an
/// absent path after a wait releases the hold and tries again, so a
/// creation that is already running is waited out, never joined. Creates
/// the lock file's parent directory: an absent worktree may have lost its
/// parent too. `before_wait` runs before every blocking wait, as
/// [`share_with`]'s does.
// The NotFound match guards: only a non-NotFound error from
// symlink_metadata reaches the other arm, and the lock beside the path
// shares its parent, so opening the lock fails first; no OS fault the
// tests can inject reaches it.
#[cfg_attr(false, mutants::skip)]
pub(crate) fn claim(worktree: &Path, before_wait: &dyn Fn()) -> Result<Claim, SessionError> {
    let lock = lock_path(worktree).ok_or_else(|| SessionError::ShareFailed {
        path: worktree.to_owned(),
        kind: io::ErrorKind::InvalidInput,
    })?;
    for _ in 0..ATTEMPTS {
        let file = open(&lock, true).map_err(|source| io_failed(&lock, source))?;
        let held = match file.try_lock() {
            Ok(()) => file,
            Err(TryLockError::WouldBlock) => {
                before_wait();
                file.lock_shared()
                    .map_err(|source| io_failed(&lock, source))?;
                if !same_file(&file, &lock) {
                    continue;
                }
                match std::fs::symlink_metadata(worktree) {
                    Ok(_) => return Ok(Claim::Present(Shared { _file: file })),
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    Err(source) => {
                        return Err(SessionError::ShareFailed {
                            path: worktree.to_owned(),
                            kind: source.kind(),
                        });
                    }
                }
            }
            Err(TryLockError::Error(source)) => return Err(io_failed(&lock, source)),
        };
        if !same_file(&held, &lock) {
            continue;
        }
        match std::fs::symlink_metadata(worktree) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(Claim::Create(Creating { file: held, lock }));
            }
            Err(source) => {
                return Err(SessionError::ShareFailed {
                    path: worktree.to_owned(),
                    kind: source.kind(),
                });
            }
            Ok(_) => {
                return match (Creating { file: held, lock }).downgrade() {
                    Ok(shared) => Ok(Claim::Present(shared)),
                    Err(failure) => Err(failure),
                };
            }
        }
    }
    Err(SessionError::WorktreeInUse {
        path: worktree.to_owned(),
    })
}

/// Takes the worktree's lock file exclusively without waiting: `Exclusive`
/// while no process shares the worktree, `InUse` while one does. Creates
/// the file, so the creator and every sharer lock the one inode.
pub(crate) fn exclusive(worktree: &Path) -> Result<Removal, worktree::Error> {
    exclusive_with(worktree, &|| {})
}

/// [`exclusive`] with a hook the test parks on: it runs after the file is
/// opened and before its `try_lock`. Production passes a no-op.
pub(crate) fn exclusive_with(
    worktree: &Path,
    before_wait: &dyn Fn(),
) -> Result<Removal, worktree::Error> {
    let lock = lock_path(worktree).ok_or_else(|| worktree::Error::Io {
        path: worktree.to_owned(),
        source: io::Error::from(io::ErrorKind::InvalidInput),
    })?;
    let stale = || worktree::Error::Io {
        path: lock.clone(),
        source: io::Error::from(io::ErrorKind::TimedOut),
    };
    for _ in 0..ATTEMPTS {
        let file = open(&lock, false).map_err(|source| worktree::Error::Io {
            path: lock.clone(),
            source,
        })?;
        before_wait();
        match file.try_lock() {
            Ok(()) => {
                if same_file(&file, &lock) {
                    return Ok(Removal::Exclusive(file));
                }
            }
            Err(TryLockError::WouldBlock) => return Ok(Removal::InUse),
            Err(TryLockError::Error(source)) => {
                return Err(worktree::Error::Io {
                    path: lock.clone(),
                    source,
                });
            }
        }
    }
    Err(stale())
}

/// The worktree's lock file: the final component with `.lock` appended, so
/// `worktrees/<id>` locks `worktrees/<id>.lock`. `None` when the path has
/// no final component.
pub(crate) fn lock_path(worktree: &Path) -> Option<PathBuf> {
    let name = worktree.file_name()?;
    let mut lock = worktree.to_path_buf();
    lock.set_file_name(format!("{}.lock", name.to_string_lossy()));
    Some(lock)
}

/// Opens the lock file for locking, creating it. With `create_parent`,
/// makes its parent directory first.
// A let chain does not compile with `||`, so the `&&` mutants are unviable.
#[cfg_attr(false, mutants::skip)]
fn open(lock: &Path, create_parent: bool) -> Result<File, io::Error> {
    if create_parent
        && let Some(parent) = lock.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock)
}

/// A lock-file I/O failure: the lock file and the error, rendered as the
/// lock file and the kind only, never the operating system's message.
fn io_failed(lock: &Path, source: io::Error) -> SessionError {
    SessionError::SharedIo {
        path: lock.to_owned(),
        source,
    }
}

/// Whether the locked descriptor is still the file at the lock path: a
/// removal unlinks the file while holding it exclusively, so a waiter
/// that locked the old inode finds another device and inode, or no file
/// at all, at the path.
fn same_file(file: &File, lock: &Path) -> bool {
    let (Ok(current), Ok(locked)) = (std::fs::metadata(lock), file.metadata()) else {
        return false;
    };
    current.dev() == locked.dev() && current.ino() == locked.ino()
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
