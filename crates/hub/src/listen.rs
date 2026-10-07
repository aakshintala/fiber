//! The hub's socket at `run/hub` (`docs/state.md`, "Sockets"): the single-hub
//! lock on `run/`, stale-socket removal, and the 0600 bind.
//!
//! [`lock`] opens `run/` (created 0700 if missing) as a `File` and takes an
//! exclusive, non-blocking lock, which the hub keeps for its lifetime.
//! Failing to get it means another hub is starting or running, so this one
//! exits 0 at once, writing nothing. With the lock held, the hub opens its
//! diagnostic log and reads its configuration, then [`bind`] checks that
//! `run/hub` fits the platform's socket path, removes any `run/hub` that is
//! not a live socket, and binds `run/hub` mode 0600. The lock, not the
//! connect probe, decides the race.

use std::fs::{self, DirBuilder, File, Permissions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::StartError;

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
pub(crate) const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// The single-hub lock on `run/`. Only its holder writes `logs/hub.log`,
/// binds `run/hub` or removes it.
pub(crate) struct Lock {
    _file: File,
}

/// `run/hub`, bound by the lock's holder.
pub(crate) struct Bound {
    listener: UnixListener,
    socket: PathBuf,
}

/// The hub's hold on `run/`: the lock, the bound socket, and its path.
pub(crate) struct Held {
    _lock: Lock,
    pub(crate) listener: UnixListener,
    pub(crate) socket: PathBuf,
}

impl Held {
    pub(crate) fn new(lock: Lock, bound: Bound) -> Self {
        Self {
            _lock: lock,
            listener: bound.listener,
            socket: bound.socket,
        }
    }

    /// Removes `run/hub` while still holding the lock, then releases it.
    pub(crate) fn stop(self) {
        drop(self.listener);
        fs::remove_file(&self.socket).unwrap_or(());
    }
}

/// Creates `run/` and locks it. `None` means another hub is starting or
/// running: exit 0 at once, writing nothing.
pub(crate) fn lock(home: &Path) -> Result<Option<Lock>, StartError> {
    let run = home.join("run");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(|e| refused(&run, e))?;
    let file = File::open(&run).map_err(|e| refused(&run, e))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(source)) => Err(refused(&run, source)),
    }
}

/// Binds `run/hub` for the holder of `_lock`. `None` means a live hub
/// already answers there: exit 0, binding nothing. A failure leaves no
/// socket this call bound.
pub(crate) fn bind(_lock: &Lock, home: &Path) -> Result<Option<Bound>, StartError> {
    let socket = home.join("run").join("hub");
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(StartError::HomeTooLong {
            max: SOCKET_PATH_MAX,
        });
    }
    match UnixStream::connect(&socket) {
        Ok(_) => return Ok(None),
        Err(error) if replaceable(&socket, &error) => remove_socket(&socket),
        Err(error) => return Err(refused(&socket, error)),
    }
    let listener = UnixListener::bind(&socket).map_err(|e| refused(&socket, e))?;
    if let Err(e) = fs::set_permissions(&socket, Permissions::from_mode(0o600)) {
        drop(listener);
        remove_socket(&socket);
        return Err(refused(&socket, e));
    }
    Ok(Some(Bound { listener, socket }))
}

/// Whether nothing live can hide behind `socket`, whose connect failed
/// with `error`: a refusal, a missing path, or a regular file. A symlink,
/// or a path whose type cannot be read, may lead to a live session.
fn replaceable(socket: &Path, error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    ) || fs::symlink_metadata(socket).is_ok_and(|meta| meta.file_type().is_file())
}

fn remove_socket(socket: &Path) {
    // Nothing there is the usual case.
    fs::remove_file(socket).unwrap_or(());
}

fn refused(path: &Path, source: io::Error) -> StartError {
    StartError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
