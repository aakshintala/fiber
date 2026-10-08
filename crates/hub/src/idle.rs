//! Idle exit and signal shutdown: the accept loop, the wait while no
//! client is connected, and stopping on the first SIGTERM, SIGINT or SIGHUP.
//!
//! The hub counts open client connections. When the count reaches 0, at
//! start or on a departure, it records the instant; while the count stays 0
//! it waits on the injected clock until that instant plus `idle_exit`, and
//! while a client is open it waits with no deadline. At expiry with 0
//! clients it writes `peak_memory` at the debug level, then `hub_stopped`,
//! removes `run/hub` while still holding the lock, and returns 0. On the
//! first SIGTERM, SIGINT or SIGHUP it shuts down every client connection and
//! relay stream, writes `peak_memory` at the debug level, then
//! `hub_stopped`, removes `run/hub`, and returns 128 plus the signal.
//! Sessions are untouched either way. A hub that cannot start accepting
//! writes an `error` line and returns 1: it never served, so it did not
//! exit for idleness.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use crate::connection::{Accept, Hub, Idle, serve_counted};
use crate::listen::Held;

/// How the hub stopped.
pub(crate) enum Exit {
    /// No client connected for the whole `idle_exit`.
    Idle,
    /// A signal arrived: its number.
    Signal(i32),
    /// The hub could not accept connections at all.
    Failed,
}

impl Exit {
    /// The process exit code: 0 for idleness, 128 plus the signal, 1 for
    /// a hub that could not accept.
    pub(crate) fn code(self) -> i32 {
        match self {
            Self::Idle => 0,
            Self::Signal(signal) => 128 + signal,
            Self::Failed => 1,
        }
    }
}

/// Serves `held` until idleness or a signal. `got` carries the signal's
/// number once one arrives, then [`Hub::waker`] wakes the wait; tests do the
/// same to simulate one.
pub(crate) fn run(hub: &Arc<Hub>, held: &Held, idle_exit: Duration, got: &AtomicI32) -> Exit {
    let accept = match held.listener.try_clone() {
        Ok(accept) => accept,
        Err(error) => return failed(hub, &format!("cloning run/hub's listener: {error}")),
    };
    let socket = held.socket.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let acceptor = thread::Builder::new().name("hub-accept".to_owned()).spawn({
        let hub = Arc::clone(hub);
        let stop = Arc::clone(&stop);
        move || {
            loop {
                match accept.accept() {
                    Ok((stream, _)) => {
                        // Counted before its thread starts, under the same
                        // lock the exit claim takes: a connection racing the
                        // exit is either counted, and the claim fails, or
                        // dropped here unanswered, EOF with no `hub_hello`,
                        // and the client retries.
                        match hub.poll_accept(&stream, &stop) {
                            Accept::Counted(n) => {
                                let for_thread = Arc::clone(&hub);
                                // The closure owns `stream`: on spawn failure
                                // it is dropped with the closure, after the
                                // registration is rolled back below.
                                match thread::Builder::new()
                                    .name("hub-conn".to_owned())
                                    .spawn(move || serve_counted(stream, for_thread, n))
                                {
                                    Ok(_) => {}
                                    Err(_) => hub.rollback(n),
                                }
                            }
                            Accept::Dropped => {}
                            Accept::Exiting => return,
                        }
                    }
                    Err(_) => {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                    }
                }
            }
        }
    });
    let acceptor = match acceptor {
        Ok(acceptor) => acceptor,
        Err(error) => return failed(hub, &format!("starting the accept thread: {error}")),
    };
    loop {
        match hub.idle_wait(idle_exit, &stop, got) {
            Idle::Signal(signal) => {
                stop.store(true, Ordering::SeqCst);
                join_after_wake(&socket, acceptor);
                hub.shutdown_clients();
                hub.diag.stopped("The hub stopped: signal.");
                return Exit::Signal(signal);
            }
            Idle::Expired => {
                join_after_wake(&socket, acceptor);
                hub.diag.stopped("The hub stopped: idle.");
                return Exit::Idle;
            }
            Idle::Woken => {}
        }
    }
}

/// Writes an `error` line naming what kept the hub from accepting.
fn failed(hub: &Hub, what: &str) -> Exit {
    hub.diag.error(
        "io_failed",
        &format!("The hub cannot accept connections: {what}"),
    );
    Exit::Failed
}

/// Wakes the acceptor's blocking `accept`, which sees `stop` and returns,
/// then joins it. The wake connection is dropped unanswered; the client
/// retries. When the socket path was removed the connect fails and nothing
/// can wake `accept`: the acceptor is left blocked, and the process exit
/// ends it.
fn join_after_wake(socket: &std::path::Path, acceptor: thread::JoinHandle<()>) {
    if UnixStream::connect(socket).is_ok() {
        match acceptor.join() {
            Ok(()) | Err(_) => {}
        }
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod tests;
