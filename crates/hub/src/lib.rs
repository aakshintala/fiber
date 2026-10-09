//! The hub (`docs/invocation.md`, "The hub"): it lists sessions, starts and
//! resumes them, and relays every client connection to a session's socket.
//! It holds no session.
//!
//! [`serve`] listens on `run/hub`, answers `start`, `status`,
//! `prompt_history`, `feed`, `dismiss`, `recent`, `sessions`, `delete` and
//! `read_file`, and relays session commands to `run/<session_id>` (`docs/invocation.md`,
//! "What the hub speaks"). It sends `attention` to every connection
//! (`docs/invocation.md`, "Attention"). A hub a client started exits once no
//! client has been connected for `hub.idle_exit_ms`
//! (`docs/configuration.md`); an installed hub never exits for being idle.

mod attention;
mod connection;
mod delete;
mod diag;
mod error;
#[cfg(test)]
pub(crate) mod fake;
mod feed;
mod first;
mod idle;
mod listen;
mod prompt_history;
mod read_file;
mod recent;
mod rejoin;
mod relay;
mod resume;
mod retire;
mod rewind;
mod sessions;
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

/// What the hub reads from the configuration when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// `hub.idle_exit_ms`.
    pub idle_exit: Duration,
    /// `diagnostics.level`, read once: a changed value reaches the next hub.
    pub level: log::diag::Level,
}

/// How the hub was started (`docs/invocation.md`, "The hub").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A client started it: it exits 0 when another hub holds the `run/`
    /// lock or answers on `run/hub`, and once no client has been connected
    /// for `hub.idle_exit_ms`.
    OnDemand,
    /// The login service runs it: it waits for the `run/` lock, never exits
    /// for being idle, and never exits 0.
    Installed,
}

/// Starts a session the hub was asked for, or resumes one a relayed command
/// names: runs the internal session command in `workspace` with `id`, so
/// the starter knows the id before the process runs and nothing is read
/// back (`docs/invocation.md`, "The hub").
pub trait Starter: Send + Sync {
    /// Starts the session command for `id` in `workspace`, with `model`
    /// when the client named one, and each of `overrides` passed to the
    /// session as `-c`, in order. With `worktree` true, the session runs
    /// in a new worktree of the workspace (`docs/invocation.md`,
    /// "Isolation"); the session command creates the worktree itself.
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
    ) -> io::Result<Box<dyn Started>>;

    /// Starts the session command resuming `id` in `workspace`, the one
    /// its log recorded (`docs/invocation.md`, "Lifecycle").
    fn resume(&self, id: &SessionId, workspace: &Path) -> io::Result<Box<dyn Started>>;

    /// Starts the session command for a rewind's new session `id` in
    /// `workspace`, the old session's, for the rewind of `from`
    /// (`docs/invocation.md`, "`rewind` starts a new session process").
    fn rewind(
        &self,
        id: &SessionId,
        workspace: &Path,
        from: &SessionId,
    ) -> io::Result<Box<dyn Started>>;
}

/// A session process [`Starter::start`] started.
pub trait Started: Send {
    /// `None` while the process runs; `Some` once it exited: the failure
    /// from the `fiber_exited` line it printed last on stdout, or
    /// `io_failed` naming the session when there is none.
    fn exited(&self) -> Option<Failure>;
}

/// Runs the hub in `home` in `mode` until it stops, returning the exit
/// code. The signal path returns 128 plus the signal, as
/// `doors::signal_code` does, and a hub that cannot accept returns 1.
///
/// The hub takes the `run/` lock, opens `logs/hub.log`, then calls
/// `configure` for its [`Settings`] and binds `run/hub`. A hub a client
/// started returns `Ok(0)` on idle exit, and when another hub holds the
/// lock or answers on `run/hub`; one that loses the lock has written
/// nothing and never calls `configure`. An installed hub blocks until the
/// lock is free, ignores `Settings::idle_exit`, and treats a process
/// answering on `run/hub` as a start failure. A failure to take the lock is
/// returned unlogged, since only the lock's holder writes `hub.log`. Every
/// later failure is written to `hub.log` once, as an `error` line with its
/// code, while the lock is still held, and returned for the caller to print.
pub fn serve(
    home: &Path,
    mode: Mode,
    configure: impl FnOnce() -> Result<Settings, Failure>,
    fiber_version: &str,
    starter: Arc<dyn Starter>,
    clock: Arc<dyn Clock>,
) -> Result<i32, StartError> {
    let lock = match mode {
        Mode::OnDemand => match listen::lock(home)? {
            Some(lock) => lock,
            None => return Ok(0),
        },
        Mode::Installed => listen::lock_wait(home)?,
    };
    let diag = Diag::open(home, Arc::clone(&clock));
    let bound = match configure_and_bind(&lock, home, configure) {
        Ok(Some(bound)) => Ok(bound),
        Ok(None) => match mode {
            Mode::OnDemand => return Ok(0),
            Mode::Installed => Err(StartError::Io {
                path: home.join("run").join("hub"),
                source: io::Error::other(
                    "another process answers on run/hub without holding the run/ lock",
                ),
            }),
        },
        Err(error) => Err(error),
    };
    let (bound, settings) = match bound {
        Ok(bound) => bound,
        Err(error) => {
            diag.error(&start::code_name(&error.code()), &error.to_string());
            return Err(error);
        }
    };
    let idle_exit = match mode {
        Mode::OnDemand => settings.idle_exit,
        // Past the end of time: the idle wait never expires.
        Mode::Installed => Duration::MAX,
    };
    let held = Held::new(lock, bound);
    let diag = diag.with_level(settings.level);
    let hub = Arc::new(Hub::new(home, fiber_version, starter, clock, diag));
    // The feed starts a rewound session even when no client relayed the
    // rewind: the callback holds the hub weakly, so nothing runs after
    // the hub drops, and the start runs on a thread of its own, never on
    // the feed's scanner.
    let feed_hub = Arc::downgrade(&hub);
    hub.feed.on_rewound.get_or_init(|| {
        Box::new(move |from, next| {
            let Some(hub) = feed_hub.upgrade() else {
                return;
            };
            let failed = next.clone();
            let spawned = {
                let hub = Arc::clone(&hub);
                thread::Builder::new()
                    .name("hub-rewind".to_owned())
                    .spawn(move || drop(crate::rewind::reach(&hub, &from, &next)))
            };
            if spawned.is_err() {
                hub.diag.warn_session(
                    &failed,
                    "io_failed",
                    &format!("Session {} could not start.", failed.0),
                );
            }
        })
    });
    hub.diag.info("hub_started", "The hub started.");
    crate::rejoin::wire(&hub);
    let got = Arc::new(AtomicI32::new(0));
    arm(&got, hub.waker());
    hub.feed.start();
    let exit = idle::run(&hub, &held, idle_exit, &got);
    hub.feed.stop();
    held.stop();
    Ok(exit.code())
}

/// Reads the configuration through `configure`, then binds `run/hub` for
/// the holder of `lock`, with the hub's settings.
fn configure_and_bind(
    lock: &listen::Lock,
    home: &Path,
    configure: impl FnOnce() -> Result<Settings, Failure>,
) -> Result<Option<(listen::Bound, Settings)>, StartError> {
    let settings = configure().map_err(StartError::Configure)?;
    Ok(listen::bind(lock, home)?.map(|bound| (bound, settings)))
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
