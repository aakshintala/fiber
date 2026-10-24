//! The hub (`docs/invocation.md`, "The hub"): it lists sessions, starts and
//! resumes them, and relays every client connection to a session's socket.
//! It holds no session. Every session it starts runs with one environment,
//! built from a login-shell capture when the hub starts
//! (`docs/invocation.md`, "A session's environment").
//!
//! [`serve`] listens on `run/hub`, answers `start`, `status`,
//! `refresh`, `prompt_history`, `feed`, `dismiss`, `recent`, `sessions`, `delete`,
//! `directories`, `read_file`, `pairing_code`, `devices`, `revoke`,
//! `configure`, `read_config`, `write_config`, `search_files`, `device_login`,
//! `device_login_result` and `device_login_cancel`, and relays
//! session commands to `run/<session_id>` (`docs/invocation.md`, "What the hub speaks").
//! It sends `attention` and `device_changed` to every connection
//! (`docs/invocation.md`, "Attention"). With `port` set, an installed hub
//! also serves websockets on `127.0.0.1:<port>` (`docs/invocation.md`,
//! "The hub"). A hub a client started exits once no
//! client has been connected for `hub.idle_exit_ms`
//! (`docs/configuration.md`); an installed hub never exits for being idle.

mod attention;
mod closed;
mod connection;
mod delete;
mod device_login;
mod devices;
mod diag;
mod directories;
mod environment;
mod error;
#[cfg(test)]
pub(crate) mod fake;
mod feed;
mod file_search;
mod first;
mod idle;
mod listen;
mod machine;
mod pairing;
mod port;
mod prompt_history;
mod read_file;
mod recent;
mod rejoin;
mod relay;
mod resume;
mod retire;
mod rewind;
mod sessions;
mod sessions_stop;
mod spawn;
mod start;
#[cfg(test)]
pub(crate) mod testkit;
mod tick;
#[cfg(test)]
#[path = "timing_tests.rs"]
mod timing_tests;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::thread;
use std::time::Duration;

use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::shapes::Failure;
use log::diag::Severity;

use crate::connection::Hub;
use crate::listen::Held;

pub use crate::environment::{Environment, Environments};
pub use crate::error::StartError;
pub use crate::machine::{DeviceLogin, FileSearch, Machine, ShowCode};
pub use crate::recent::{Left, RecentError, RecentRow, append, append_exited};
pub use crate::spawn::{HUB_SESSION_MARKER, SpawnStarter};

/// What the hub reads from the configuration when it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `hub.idle_exit_ms`.
    pub idle_exit: Duration,
    /// `diagnostics.level`, read once: a changed value reaches the next hub.
    pub level: log::diag::Level,
    /// The `127.0.0.1` port an installed hub also listens on, from Fiber
    /// home's global `config.json` only; unset, it listens on its local
    /// socket only (`docs/configuration.md`, "Keys").
    pub port: Option<PortSettings>,
}

/// What an installed hub serves on `127.0.0.1:<number>`
/// (`docs/invocation.md`, "The hub").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSettings {
    /// The port of `127.0.0.1` the hub listens on (`hub.port`).
    pub number: u16,
    /// The web page origins whose websockets the hub accepts
    /// (`hub.allowed_origins`).
    pub allowed_origins: Vec<String>,
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
    /// With `continues` naming a session, the session runs in that
    /// session's worktree instead, which `start` checked is the
    /// workspace (`docs/invocation.md`, "Isolation"). With `supervised`
    /// true, the session is driven by a program (`docs/permissions.md`,
    /// "Supervised sessions").
    fn start(
        &self,
        id: &SessionId,
        workspace: &Path,
        model: Option<&str>,
        overrides: &[&str],
        worktree: bool,
        continues: Option<&SessionId>,
        supervised: bool,
    ) -> io::Result<Box<dyn Started>>;

    /// Starts the session command resuming `id` in `workspace`, the launch
    /// directory the hub selected from the session's log
    /// (`docs/invocation.md`, "Lifecycle"). With `credential`, the
    /// resumed session starts on that credential label
    /// (`docs/model-routing.md`, "Which credential a session uses").
    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> io::Result<Box<dyn Started>>;

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

    /// Kills the process without waiting for it: the drain reaps it, as
    /// for every exit, so a caller that holds the child lock for a read
    /// is never blocked. The default is a no-op, for a `Started` whose
    /// exit is already settled.
    fn kill(&self) {}

    /// Blocks until the process has exited, then returns what [`exited`](Self::exited)
    /// would then return. The default returns [`exited`](Self::exited) at once,
    /// for a `Started` whose exit is already settled; one backed by a running
    /// process overrides it.
    fn wait_exited(&self) -> Option<Failure> {
        self.exited()
    }
}

/// Runs the hub in `home` in `mode` until it stops, returning the exit
/// code. The signal path returns 128 plus the signal, as
/// `doors::signal_code` does, and a hub that cannot accept returns 1.
///
/// The hub takes the `run/` lock, opens `logs/hub.log`, then calls
/// `configure` for its [`Settings`] and binds `run/hub`. `fiber_commit` is
/// the commit the build recorded, if any; `hub_hello` carries it beside
/// `fiber_version`. A hub a client
/// started returns `Ok(0)` on idle exit, and when another hub holds the
/// lock or answers on `run/hub`; one that loses the lock has written
/// nothing and never calls `configure`. An installed hub blocks until the
/// lock is free, ignores `Settings::idle_exit`, and treats a process
/// answering on `run/hub` as a start failure. A failure to take the lock is
/// returned unlogged, since only the lock's holder writes `hub.log`. Every
/// later failure is written to `hub.log` once, as an `error` line with its
/// code, while the lock is still held, and returned for the caller to print.
#[allow(
    clippy::too_many_arguments,
    reason = "the composition root supplies each independent hub seam"
)]
pub fn serve(
    home: &Path,
    mode: Mode,
    configure: impl FnOnce() -> Result<Settings, Failure>,
    fiber_version: &str,
    fiber_commit: Option<&str>,
    starter: Arc<dyn Starter>,
    clock: Arc<dyn Clock>,
    environments: Arc<Environments>,
    machine: Arc<dyn Machine>,
) -> Result<i32, StartError> {
    let lock = match mode {
        Mode::OnDemand => match listen::lock(home)? {
            Some(lock) => lock,
            None => return Ok(0),
        },
        Mode::Installed => listen::lock_wait(home)?,
    };
    let diag = crate::diag::open(home, Arc::clone(&clock));
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
            diag.line(
                Severity::Error,
                None,
                &start::code_name(&error.code()),
                &error.to_string(),
            );
            return Err(error);
        }
    };
    let idle_exit = match mode {
        Mode::OnDemand => settings.idle_exit,
        // Past the end of time: the idle wait never expires.
        Mode::Installed => Duration::MAX,
    };
    let held = Held::new(lock, bound);
    // Only an installed hub listens on the port, which it reads from
    // Fiber home's global `config.json` only (`Settings::port`): a hub a
    // client started ignores it.
    let held = match mode {
        Mode::Installed => match &settings.port {
            Some(port) => match listen::bind_port(port.number) {
                Ok(tcp) => held.with_port(crate::port::Listener {
                    tcp,
                    origins: Arc::from(port.allowed_origins.clone()),
                }),
                Err(source) => {
                    let error = StartError::Port {
                        port: port.number,
                        source,
                    };
                    diag.line(
                        Severity::Error,
                        None,
                        &start::code_name(&error.code()),
                        &error.to_string(),
                    );
                    held.stop();
                    return Err(error);
                }
            },
            None => held,
        },
        Mode::OnDemand => held,
    };
    let diag = diag.with_level(settings.level);
    let hub = Arc::new(
        Hub::new(home, fiber_version, starter, Arc::clone(&clock), diag)
            .with_commit(fiber_commit)
            .with_environments(Arc::clone(&environments))
            .with_machine(machine),
    );
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
                hub.diag.line(
                    Severity::Warn,
                    Some(&failed),
                    "io_failed",
                    &format!("Session {} could not start.", failed.0),
                );
            }
        })
    });
    hub.diag
        .line(Severity::Info, None, "hub_started", "The hub started.");
    // The first capture runs beside the hub: binding comes first, since a
    // client that started the hub waits only 5 seconds for it to answer.
    // A thread that cannot start publishes the fallback at once.
    {
        let hub = Arc::clone(&hub);
        let environments = Arc::clone(&environments);
        let clock = Arc::clone(&clock);
        let failed_environments = Arc::clone(&environments);
        let failed_hub = Arc::clone(&hub);
        match thread::Builder::new()
            .name("hub-environment".to_owned())
            .spawn(move || environments.first(&*clock, &hub.diag))
        {
            Ok(_) => {}
            Err(error) => failed_environments.first_failed(
                crate::environment::capture::Failed::Spawn(error.kind()),
                &failed_hub.diag,
            ),
        }
    }
    crate::rejoin::wire(&hub);
    let got = Arc::new(AtomicI32::new(0));
    arm(&got, hub.waker());
    hub.feed.start();
    let exit = idle::run(&hub, &held, idle_exit, &got);
    // `idle::run`'s early returns never reach its own `close`: cover them
    // here, after which no capture outlives `serve`.
    environments.close();
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
