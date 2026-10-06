//! Idle exit and signal shutdown: the accept loop, the wait while no
//! client is connected, and stopping on the first SIGTERM, SIGINT or SIGHUP.
//!
//! The hub counts open client connections. While the count is 0 it waits on
//! the injected clock until `idle_since + idle_exit`; a connection arriving
//! resets it. At expiry with 0 clients it writes `hub_stopped`, removes
//! `run/hub` while still holding the lock, and returns 0. On the first
//! SIGTERM, SIGINT or SIGHUP it shuts down every client connection and
//! relay stream, writes `hub_stopped`, removes `run/hub`, and returns
//! 128 plus the signal. Sessions are untouched either way.

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;

use crate::connection::{Hub, serve_counted};
use crate::listen::Held;

/// How the hub stopped.
pub(crate) enum Exit {
    /// No client connected for the whole `idle_exit`.
    Idle,
    /// A signal arrived: its number.
    Signal(i32),
}

impl Exit {
    /// The process exit code: 0 for idleness, 128 plus the signal.
    pub(crate) fn code(self) -> i32 {
        match self {
            Self::Idle => 0,
            Self::Signal(signal) => 128 + signal,
        }
    }
}

/// Serves `held` until idleness or a signal. `got` carries the signal's
/// number once one arrives; tests store one to simulate it.
pub(crate) fn run(
    hub: &Arc<Hub>,
    held: &Held,
    idle_exit: Duration,
    clock: &Arc<dyn Clock>,
    got: &Arc<AtomicI32>,
) -> Exit {
    let Ok(accept) = held.listener.try_clone() else {
        return Exit::Idle;
    };
    let socket = held.socket.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let Ok(acceptor) = thread::Builder::new().name("hub-accept".to_owned()).spawn({
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
                            Some(n) => {
                                let hub = Arc::clone(&hub);
                                let spawned = thread::Builder::new()
                                    .name("hub-conn".to_owned())
                                    .spawn(move || serve_counted(stream, hub, n));
                                if spawned.is_err() {}
                            }
                            None => return,
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
    }) else {
        return Exit::Idle;
    };
    let mut idle_since = clock.now();
    let mut seen = hub.activity();
    loop {
        let signal = got.swap(0, Ordering::SeqCst);
        if signal != 0 {
            stop.store(true, Ordering::SeqCst);
            wake(&socket);
            match acceptor.join() {
                Ok(()) | Err(_) => {}
            }
            hub.shutdown_clients();
            hub.diag.info("hub_stopped", "The hub stopped: signal.");
            return Exit::Signal(signal);
        }
        let now = clock.now();
        let quiet = hub.quiet(seen);
        if quiet && now >= idle_since + idle_exit && hub.claim_exit(&stop, seen) {
            wake(&socket);
            match acceptor.join() {
                Ok(()) | Err(_) => {}
            }
            hub.diag.info("hub_stopped", "The hub stopped: idle.");
            return Exit::Idle;
        }
        if !quiet {
            idle_since = now;
            seen = hub.activity();
        }
        hub.park_until(&**clock, idle_since + idle_exit);
    }
}

/// Wakes the acceptor's blocking `accept`, which sees `stop` and returns.
/// The connection is dropped unanswered; the client retries.
fn wake(socket: &std::path::Path) {
    match UnixStream::connect(socket) {
        Ok(_) | Err(_) => {}
    }
}

#[cfg(test)]
#[path = "idle_tests.rs"]
mod tests;
