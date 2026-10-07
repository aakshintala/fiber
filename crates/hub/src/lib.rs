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
mod error;
#[cfg(test)]
pub(crate) mod fake;
mod idle;
mod listen;
mod prompt_history;
mod relay;
mod resume;
mod recent;
mod start;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::shapes::Failure;

use crate::connection::Hub;
use crate::diag::Diag;
use crate::listen::Held;

pub use crate::error::StartError;
pub use crate::recent::{Left, RecentRow, append};

/// Starts a session the hub was asked for, or resumes one a relayed command
/// names: runs the internal session command in `workspace` with `id`, so
/// the starter knows the id before the process runs and nothing is read
/// back (`docs/invocation.md`, "The hub").
pub trait Starter: Send + Sync {
    /// Starts the session command for `id` in `workspace`, with `model`
    /// when the client named one.
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
    ) -> io::Result<Box<dyn Started>>;

    /// Starts the session command resuming `id` in `workspace`, the one
    /// its log recorded (`docs/invocation.md`, "Lifecycle").
    fn resume(&self, id: &SessionId, workspace: &Path) -> io::Result<Box<dyn Started>>;
}

/// A session process [`Starter::start`] started.
pub trait Started: Send {
    /// `None` while the process runs; `Some` once it exited: the failure
    /// from the `fiber_exited` line it printed last on stdout, or
    /// `io_failed` naming the session when there is none.
    fn exited(&self) -> Option<Failure>;
}

/// Runs the hub in `home` until it exits for idleness: `Ok(0)` on idle
/// exit, or when another hub holds the `run/` lock or answers on `run/hub`.
/// The signal path exits the process with 128 plus the signal, as
/// `doors::signal_code` does.
///
/// The hub takes the `run/` lock, opens `logs/hub.log`, then calls
/// `configure` for `hub.idle_exit_ms` and binds `run/hub`. A hub that loses
/// the lock returns `Ok(0)` having written nothing and never calls
/// `configure`; a failure to take the lock is returned unlogged, since only
/// the lock's holder writes `hub.log`. Every later failure is written to
/// `hub.log` once, as an `error` line with its code, while the lock is still
/// held, and returned for the caller to print.
pub fn serve(
    home: &Path,
    configure: impl FnOnce() -> Result<Duration, Failure>,
    fiber_version: &str,
    starter: Arc<dyn Starter>,
    clock: Arc<dyn Clock>,
) -> Result<i32, StartError> {
    let Some(lock) = listen::lock(home)? else {
        return Ok(0);
    };
    let diag = Diag::open(home, Arc::clone(&clock));
    let (bound, idle_exit) = match configure_and_bind(&lock, home, configure) {
        Ok(Some(bound)) => bound,
        Ok(None) => return Ok(0),
        Err(error) => {
            diag.error(&start::code_name(&error.code()), &error.to_string());
            return Err(error);
        }
    };
    let held = Held::new(lock, bound);
    let hub = Arc::new(Hub::new(home, fiber_version, starter, clock, diag));
    hub.diag.info("hub_started", "The hub started.");
    let got = Arc::new(AtomicI32::new(0));
    arm(&got, hub.waker());
    let exit = idle::run(&hub, &held, idle_exit, &got);
    held.stop();
    Ok(exit.code())
}

/// Reads the configuration through `configure`, then binds `run/hub` for
/// the holder of `lock`, with `hub.idle_exit_ms`.
fn configure_and_bind(
    lock: &listen::Lock,
    home: &Path,
    configure: impl FnOnce() -> Result<Duration, Failure>,
) -> Result<Option<(listen::Bound, Duration)>, StartError> {
    let idle_exit = configure().map_err(StartError::Configure)?;
    Ok(listen::bind(lock, home)?.map(|bound| (bound, idle_exit)))
}

/// Arms SIGTERM, SIGINT and SIGHUP to end the hub through `got`, waking the
/// idle wait through `wake`. Tests simulate signals the same way; one test
/// raises a real signal at itself to prove the arm records it and wakes.
fn arm(got: &Arc<AtomicI32>, wake: Arc<dyn Wake>) {
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
                wake.wake();
            }
        });
    if spawned.is_err() {}
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
