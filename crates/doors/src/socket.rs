//! A session's socket at `run/<session_id>` (`docs/state.md`, "Sockets"):
//! binding it, and telling a live one from a dead process's.

use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::shapes::Failure;

use crate::failure;

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// Binds the session's socket at `run/<session_id>`, mode 0600 in a 0700
/// directory (`docs/state.md`, "Sockets"). A socket a connect reaches is
/// live: fail `session_held` and leave it alone; a refusal, a missing path,
/// or a regular file is removed before binding. Any other error
/// may hide a live session, so the path stays and this fails `io_failed`.
pub(crate) fn bind(home: &Path, dir: &Path) -> Result<(PathBuf, UnixListener), Failure> {
    let run = home.join("run");
    let socket = run.join(dir.file_name().unwrap_or_default());
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(failure(
            ErrorCode::Usage,
            format!(
                "FIBER_HOME is too long: a session's socket path must fit in \
                 {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
            ),
        ));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(|e| io_failed(&run, &e))?;
    match UnixStream::connect(&socket) {
        Ok(_) => {
            let held = format!("{}: another session is running", socket.display());
            return Err(failure(ErrorCode::SessionHeld, held));
        }
        Err(error) if replaceable(&socket, &error) => remove_socket(&socket),
        Err(error) => return Err(io_failed(&socket, &error)),
    }
    let listener = UnixListener::bind(&socket).map_err(|e| io_failed(&socket, &e))?;
    if let Err(e) = fs::set_permissions(&socket, Permissions::from_mode(0o600)) {
        remove_socket(&socket);
        return Err(io_failed(&socket, &e));
    }
    Ok((socket, listener))
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

pub(crate) fn remove_socket(socket: &Path) {
    // Nothing there is the usual case.
    fs::remove_file(socket).unwrap_or(());
}

fn io_failed(path: &Path, e: &io::Error) -> Failure {
    failure(ErrorCode::IoFailed, format!("{}: {e}", path.display()))
}
