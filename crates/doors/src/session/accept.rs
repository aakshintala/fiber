//! Owns the session's accept thread and its retry policy for accept errors.

use std::io;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, mpsc};

use super::{Gate, join, spawn};
use crate::client;

pub(super) fn accept_loop(listener: UnixListener, gate: Arc<Gate>) {
    loop {
        if gate.stopped() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let Ok(shutdown_stream) = stream.try_clone() else {
                    if gate.stopped() {
                        return;
                    }
                    continue;
                };
                let (tx, rx) = mpsc::channel();
                let child = Arc::clone(&gate);
                if let Ok(handle) = spawn("client", move || {
                    let Ok(id) = rx.recv() else {
                        return;
                    };
                    client::serve(stream, child, id);
                }) {
                    // Atomic: `push_reader` checks the stop under the same
                    // lock `mark_stopped`/`join_clients` share, so a reader
                    // admitted after the stop is rejected instead of leaked:
                    // its stream is already shut, dropping `tx` ends its
                    // thread, and joining reaps it.
                    match gate.push_reader(handle, client::shutdown_both(shutdown_stream)) {
                        Ok(id) => {
                            if tx.send(id).is_err() {
                                gate.finish(id);
                            }
                        }
                        Err(handle) => {
                            drop(tx);
                            join(handle);
                            return;
                        }
                    }
                }
            }
            // `Interrupted` is a stale wake. Any other error, such as too
            // many open files, waits until a connection ends or the session
            // stops, so the loop does not spin.
            Err(error) => {
                if gate.stopped() {
                    return;
                }
                if accept_error_waits(error.kind()) {
                    gate.wait_for_room();
                }
            }
        }
    }
}

/// `Interrupted` is a stale wake and is retried. Any other accept error waits
/// so the loop does not spin.
pub(super) fn accept_error_waits(kind: io::ErrorKind) -> bool {
    kind != io::ErrorKind::Interrupted
}
