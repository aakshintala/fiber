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
mod listen;
mod start;

use std::io;
use std::path::Path;
use std::sync::Arc;
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
    _idle_exit: Duration,
    fiber_version: &str,
    starter: Arc<dyn Starter>,
    clock: Arc<dyn Clock>,
) -> i32 {
    let held = match listen::listen(home) {
        Ok(Some(held)) => held,
        Ok(None) => return 0,
        Err(_) => return 1,
    };
    // debt: the idle wait arrives with the idle handling; until then the
    // accept loop blocks. Ceiling: the idle handling lands.
    let hub = Arc::new(Hub::new(home, fiber_version, starter, clock));
    hub.diag.info("hub_started", "The hub started.");
    for stream in held.listener.incoming().flatten() {
        let hub = Arc::clone(&hub);
        let spawned = thread::Builder::new()
            .name("hub-conn".to_owned())
            .spawn(move || connection::serve_connection(stream, hub));
        if spawned.is_err() {
            continue;
        }
    }
    hub.diag.info("hub_stopped", "The hub stopped: idle.");
    held.stop();
    0
}
