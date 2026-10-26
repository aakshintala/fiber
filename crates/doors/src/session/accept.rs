//! Owns the session's accept thread and its retry policy for accept errors.

use std::io;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, mpsc};

use super::conns::shutter;
use super::{Gate, join, spawn};
use crate::client;

// Exempt: `Interrupted` needs a signal delivered mid-accept, which no test stages; the non-`Interrupted` wait is pinned through this loop by `accept_waits_for_room_on_a_non_interrupted_error`.
#[cfg_attr(false, mutants::skip)]
pub(super) fn accept_loop(listener: UnixListener, gate: Arc<Gate>) {
    loop {
        if gate.conns.stopped() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // The kernel peer decides the session-socket refusals, so
                // it is read here, on the accept thread: a failed read
                // stays distinct from a read peer through the spawned
                // client thread.
                let peer = support::peer::peer_pid(&stream);
                let (Ok(shutdown_stream), Ok(shut_stream)) =
                    (stream.try_clone(), stream.try_clone())
                else {
                    if gate.conns.stopped() {
                        return;
                    }
                    continue;
                };
                let Ok((read, stop)) = support::stoppable::reader(stream) else {
                    if gate.conns.stopped() {
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
                    client::serve(read, child, id, peer);
                }) {
                    // Atomic: `push_reader` checks the stop under the same
                    // lock `mark_stopped`/`join_clients` share, so a reader
                    // admitted after the stop is rejected instead of leaked:
                    // its stream is already shut, dropping `tx` ends its
                    // thread, and joining reaps it.
                    match gate.conns.push_reader(
                        handle,
                        client::ender(shutdown_stream, stop),
                        shutter(shut_stream),
                    ) {
                        Ok(id) => {
                            if tx.send(id).is_err() {
                                gate.conns.finish(id);
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
                if gate.conns.stopped() {
                    return;
                }
                // `Interrupted` is a stale wake and is retried. Any other
                // accept error waits so the loop does not spin.
                if error.kind() != io::ErrorKind::Interrupted {
                    gate.conns.wait_for_room();
                }
            }
        }
    }
}
