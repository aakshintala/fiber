//! Reads a pty master to end of file on a thread (`docs/testing.md`, "Screens").

use std::io::{ErrorKind, Read};

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
