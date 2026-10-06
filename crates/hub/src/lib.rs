//! The hub (`docs/invocation.md`, "The hub"): it lists sessions, starts and
//! resumes them, and relays every client connection to a session's socket.
//! It holds no session.
//!
//! [`serve`] listens on `run/hub`, answers `start` and `status`, and relays
//! session commands to `run/<session_id>` (`docs/invocation.md`, "What the
//! hub speaks"). It exits once no client has been connected for
//! `hub.idle_exit_ms` (`docs/configuration.md`).

mod connection;
mod diag;
#[cfg(test)]
pub(crate) mod fake;
mod idle;
mod listen;
mod start;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
#[cfg(not(test))]
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::Clock;
use contract::shapes::Failure;

use crate::connection::Hub;

/// Starts a session the hub was asked for: runs the internal session
/// command in `workspace` with `id`, so the starter knows the id before the
/// process runs and nothing is read back (`docs/invocation.md`, "The hub").
pub trait Starter: Send + Sync {
    /// Starts the session command for `id` in `workspace`, with `model`
    /// when the client named one.
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
    ) -> io::Result<Box<dyn Started>>;
}

/// A session process [`Starter::start`] started.
pub trait Started: Send {
    /// `None` while the process runs; `Some` once it exited: the failure
    /// from the `fiber_exited` line it printed last on stdout, or
    /// `io_failed` naming the session when there is none.
    fn exited(&self) -> Option<Failure>;
}

/// Runs the hub in `home` until it exits for idleness: 0 on idle exit, or
/// when another hub holds the `run/` lock. The signal path exits the
/// process with 128 plus the signal, as `doors::signal_code` does.
pub fn serve(
    home: &Path,
    idle_exit: Duration,
    fiber_version: &str,
    starter: Arc<dyn Starter>,
    clock: Arc<dyn Clock>,
) -> i32 {
    let held = match listen::listen(home) {
        Ok(Some(held)) => held,
        Ok(None) => return 0,
        Err(_) => return 1,
    };
    let hub = Arc::new(Hub::new(home, fiber_version, starter, clock));
    hub.diag.info("hub_started", "The hub started.");
    let got = Arc::new(AtomicI32::new(0));
    arm(&got);
    let exit = idle::run(&hub, &held, idle_exit, &hub.clock, &got);
    held.stop();
    exit.code()
}

/// Arms SIGTERM, SIGINT and SIGHUP to end the hub through `got`. Tests
/// simulate signals through the flag, so nothing process-wide is installed
/// there.
#[cfg(not(test))]
fn arm(got: &Arc<AtomicI32>) {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use std::sync::atomic::Ordering;

    let Ok(mut signals) = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP]) else {
        return;
    };
    let got = Arc::clone(got);
    // A thread that ends with the process: the hub never disarms it.
    let spawned = thread::Builder::new()
        .name("hub-signals".to_owned())
        .spawn(move || {
            for signal in signals.forever() {
                got.store(signal, Ordering::SeqCst);
            }
        });
    if spawned.is_err() {}
}

/// No process-wide handlers under test: signals arrive through the flag.
#[cfg(test)]
fn arm(_got: &Arc<AtomicI32>) {}
