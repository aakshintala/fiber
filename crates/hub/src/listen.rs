//! The hub's socket at `run/hub` (`docs/state.md`, "Sockets"): the single-hub
//! lock on `run/`, stale-socket removal, and the 0600 bind.
//!
//! `hub serve` opens `run/` (created 0700 if missing) as a `File` and takes
//! an exclusive, non-blocking lock for its lifetime. Failing to get it means
//! another hub is starting or running, so this one exits 0 at once, writing
//! nothing. With the lock held, it removes any `run/hub` that is not a live
//! socket and binds `run/hub` mode 0600. The lock, not the connect probe,
//! decides the race.

use std::fs::{self, DirBuilder, File, Permissions, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// The hub's hold on `run/`: the lock, the bound socket, and its path. Only
/// the holder binds or removes `run/hub`.
pub(crate) struct Held {
    _lock: File,
    pub(crate) listener: UnixListener,
    pub(crate) socket: PathBuf,
}

impl Held {
    /// Removes `run/hub` while still holding the lock, then releases it.
    pub(crate) fn stop(self) {
        drop(self.listener);
        fs::remove_file(&self.socket).unwrap_or(());
    }
}

/// Locks `run/` and binds `run/hub`. `None` means another hub is starting
/// or running: exit 0 at once, writing nothing.
pub(crate) fn listen(home: &Path) -> io::Result<Option<Held>> {
    let run = home.join("run");
    let socket = run.join("hub");
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "FIBER_HOME is too long: the hub's socket path must fit in \
                 {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
            ),
        ));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(|e| refused(&run, &e))?;
    let lock = File::open(&run).map_err(|e| refused(&run, &e))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(source)) => return Err(source),
    }
    match UnixStream::connect(&socket) {
        Ok(_) => return Ok(None),
        Err(error) if replaceable(&socket, &error) => remove_socket(&socket),
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&socket).map_err(|e| refused(&socket, &e))?;
    if let Err(e) = fs::set_permissions(&socket, Permissions::from_mode(0o600)) {
        remove_socket(&socket);
        return Err(refused(&socket, &e));
    }
    Ok(Some(Held {
        _lock: lock,
        listener,
        socket,
    }))
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

fn refused(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

#[cfg(test)]
#[path = "listen_tests.rs"]
mod tests;
