//! Owns the session's accept thread and its retry policy for accept errors.

use std::io;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, mpsc};

use super::{Gate, spawn};
use crate::client;

pub(super) fn accept_loop(listener: UnixListener, gate: Arc<Gate>) {
    loop {
        if gate.stopped() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if gate.stopped() {
                    return;
                }
                let Ok(shutdown_stream) = stream.try_clone() else {
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
                    let id = gate.push_reader(handle, client::shutdown_both(shutdown_stream));
                    if tx.send(id).is_err() {
                        gate.finish(id);
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
