//! Reads a pty master to end of file on a thread (`docs/testing.md`, "Screens").

use std::ffi::OsStr;
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// Opens a pseudo-terminal pair: the master and the slave's path. Opening
/// the slave itself is the caller's job. The master is close-on-exec, so a
/// child the caller starts never inherits it.
pub fn open() -> std::io::Result<(std::fs::File, PathBuf)> {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let map = |what: &'static str| {
        move |errno: rustix::io::Errno| std::io::Error::other(format!("{what}: {errno}"))
    };
    let main = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(map("opening a pty"))?;
    // Not inherited: a hub the terminal starts would hold the master open.
    rustix::io::fcntl_setfd(&main, rustix::io::FdFlags::CLOEXEC)
        .map_err(map("marking the pty close-on-exec"))?;
    grantpt(&main).map_err(map("granting the pty"))?;
    unlockpt(&main).map_err(map("unlocking the pty"))?;
    let name = ptsname(&main, Vec::new()).map_err(map("naming the pty"))?;
    Ok((
        std::fs::File::from(main),
        PathBuf::from(OsStr::from_bytes(name.as_bytes())),
    ))
}

/// Reads `source` on a thread from now to end of file, calling `chunk` with
/// each non-empty read. Never stops because the consumer is gone: a closed
/// channel inside `chunk` is the closure's business. Retries `Interrupted`;
/// any other error or a zero read ends the thread (a pty master errors when
/// the slave closes on Linux).
pub fn read_to_eof<R: Read + Send + 'static>(
    mut source: R,
    mut chunk: impl FnMut(&[u8]) + Send + 'static,
) {
    let reader = move || {
        let mut buf = [0u8; 4096];
        loop {
            match source.read(&mut buf) {
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Ok(0) => break,
                Ok(n) => {
                    let Some(bytes) = buf.get(..n) else { break };
                    chunk(bytes);
                }
                Err(_) => break,
            }
        }
    };
    // Detached: the thread ends on EOF or error. When the spawn itself
    // fails there is no reader, so the test's own wait reports it.
    if let Ok(handle) = std::thread::Builder::new()
        .name("pty-read-to-eof".to_owned())
        .spawn(reader)
    {
        drop(handle);
    }
}

#[cfg(test)]
#[path = "pty_tests.rs"]
mod tests;
