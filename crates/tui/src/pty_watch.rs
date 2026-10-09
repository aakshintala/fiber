//! One shared watcher for pseudo-terminal tests: a test that drives a
//! pseudo-terminal reads it on a thread from the first frame to end of
//! file, and takes the markers it waits for over a channel. A reader that
//! stops after a marker lets the terminal's output queue fill, so the code
//! under test blocks writing a frame and the test hangs on a wait it
//! caused itself (`docs/testing.md`, "Screens").
//!
//! There is no other way to stop the watcher: the thread ends only at end
//! of file or on an error, and after the last marker it keeps reading and
//! discards, whether or not the receiver still exists.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// Watches the pty from now on without ever stopping: one thread reads
/// the main side to EOF, sending the bytes since the previous match up
/// to and including each marker in order, then keeps reading and
/// discards. A test takes each marker with one `recv_timeout` so a full
/// pty never blocks the terminal's frames.
pub(crate) fn watch(main: &File, markers: Vec<&'static [u8]>) -> Receiver<Vec<u8>> {
    if markers.iter().any(|marker| marker.is_empty()) {
        panic!("an empty marker");
    }
    let mut dup = main.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = channel();
    std::thread::Builder::new()
        .name("lib-watch".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut at = 0usize;
            let mut byte = [0u8; 1];
            while at < markers.len() {
                match dup.read(&mut byte) {
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        buf.push(byte[0]);
                        let marker = markers[at];
                        if buf.len() >= marker.len() && buf[buf.len() - marker.len()..] == *marker {
                            let chunk = std::mem::take(&mut buf);
                            if done.send(chunk).is_err() {
                                break;
                            }
                            at += 1;
                        }
                    }
                }
            }
            let mut discard = [0u8; 4096];
            loop {
                match dup.read(&mut discard) {
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

/// Takes one watched marker within [`DEADLINE`].
pub(crate) fn watched(frames: &Receiver<Vec<u8>>, what: &str) -> Vec<u8> {
    frames
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for {what}: {err}"))
}
