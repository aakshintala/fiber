//! The hub's environment for the sessions it starts
//! (`docs/invocation.md`, "A session's environment"): one login-shell
//! capture when the hub starts, falling back to the hub's inherited
//! environment, passed to every session the hub starts, resumes or rewinds.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use contract::clock::Clock;
use contract::events::{Variables, VariablesSource};
use log::diag::{Diag, Severity};
use serde_json::{Map, Value};
use support::lock;

use crate::connection::Hub;

pub(crate) mod capture;

/// One published environment: exactly what a session process gets, the
/// `FIBER_HOME` overlay included.
#[derive(Clone)]
pub struct Environment {
    /// The variables, with `FIBER_HOME` set to the hub's own home.
    pub vars: Vec<(OsString, OsString)>,
    /// Whether the login-shell capture succeeded.
    pub source: VariablesSource,
}

impl Environment {
    /// `variables` as `session_started` records them.
    pub fn variables(&self) -> Variables {
        Variables::of(self.vars.clone(), self.source)
    }

    /// The published value of the requested placeholder variable `name`,
    /// or `None` when the published environment lacks it: absent names
    /// stay out of the `placeholder_env` answer, never `null`.
    pub fn value(&self, name: &str) -> Option<String> {
        self.vars
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    }
}

impl std::fmt::Debug for Environment {
    /// Names only, never values: a type that holds a secret prints it
    /// redacted in `Debug`, including `env` values
    /// (`docs/code-quality.md`, "Errors").
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&OsString> = self.vars.iter().map(|(name, _)| name).collect();
        f.debug_struct("Environment")
            .field("names", &names)
            .field("source", &self.source)
            .finish()
    }
}

/// The hub's shared environment: the first capture publishes once, and
/// every session the hub starts, resumes or rewinds waits for that publish
/// and runs with it. A published environment never changes.
pub struct Environments {
    /// The hub's inherited environment, read once: the fallback.
    inherited: Vec<(OsString, OsString)>,
    /// The hub's own home: the `FIBER_HOME` overlay.
    home: PathBuf,
    /// The publish, the admission and the running count, under one lock.
    state: Mutex<State>,
    /// Notified on every publish, admission change and capture end.
    changed: Condvar,
    /// One capture runs at a time.
    capture_lock: Mutex<()>,
    /// Test-only: a one-shot pause run after a capture ends and before it
    /// takes the lock to publish.
    #[cfg(test)]
    pub(crate) before_publish: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot signal run after a `refresh` is admitted
    /// and before it takes the serializing lock, so a test can force the
    /// admitted-but-queued interleaving with `close`.
    #[cfg(test)]
    pub(crate) after_admit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot signal run when `get` finds nothing
    /// published and is about to wait, so a test can prove a waiter is
    /// parked before asserting it has answered nothing yet.
    #[cfg(test)]
    pub(crate) before_get_wait: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// What [`Environments`] shares, under one lock.
struct State {
    /// The published environment, once the first capture ends.
    published: Option<Arc<Environment>>,
    /// [`Environments::close`] ran: no capture starts after it.
    closed: bool,
    /// The captures admitted and not yet ended.
    running: usize,
    /// Every running capture's wait: `close` wakes each at once.
    cancels: Vec<Arc<capture::Cancel>>,
}

impl Environments {
    /// Pending: the first capture is admitted, and [`Environments::get`]
    /// blocks until it publishes. Spawns nothing: a hub that never reaches
    /// its capture thread never captures.
    pub fn new(inherited: Vec<(OsString, OsString)>, home: &Path) -> Arc<Self> {
        let cancel = capture::Cancel::new();
        Arc::new(Self {
            inherited,
            home: home.to_path_buf(),
            state: Mutex::new(State {
                published: None,
                closed: false,
                running: 1,
                cancels: vec![cancel],
            }),
            changed: Condvar::new(),
            capture_lock: Mutex::new(()),
            #[cfg(test)]
            before_publish: Mutex::new(None),
            #[cfg(test)]
            after_admit: Mutex::new(None),
            #[cfg(test)]
            before_get_wait: Mutex::new(None),
        })
    }

    /// Published, with no first capture: tests and the hub's default use
    /// only. Publishes exactly what it is given, overlay included or not.
    pub fn ready(environment: Environment) -> Arc<Self> {
        Arc::new(Self {
            inherited: Vec::new(),
            home: PathBuf::new(),
            state: Mutex::new(State {
                published: Some(Arc::new(environment)),
                closed: false,
                running: 0,
                cancels: Vec::new(),
            }),
            changed: Condvar::new(),
            capture_lock: Mutex::new(()),
            #[cfg(test)]
            before_publish: Mutex::new(None),
            #[cfg(test)]
            after_admit: Mutex::new(None),
            #[cfg(test)]
            before_get_wait: Mutex::new(None),
        })
    }

    /// The hub's home for `~/` paths (`docs/invocation.md`, "What the hub
    /// speaks"): the first `HOME` in the inherited environment, `None`
    /// when none is there. Reads only the immutable `inherited`, never
    /// the capture state, so a command before the first capture publishes
    /// answers at once.
    pub(crate) fn user_home(&self) -> Option<PathBuf> {
        self.inherited
            .iter()
            .find(|(name, _)| name == "HOME")
            .map(|(_, value)| PathBuf::from(value))
    }

    /// The published environment, waiting for the first publish when none
    /// is there yet.
    pub fn get(&self) -> Arc<Environment> {
        let mut state = lock(&self.state);
        loop {
            if let Some(environment) = state.published.clone() {
                return environment;
            }
            #[cfg(test)]
            {
                let hook = lock(&self.before_get_wait).take();
                if let Some(hook) = hook {
                    drop(state);
                    hook();
                    state = lock(&self.state);
                    continue;
                }
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Runs the admitted first capture, publishing it with the
    /// `FIBER_HOME` overlay and writing one `environment_built` line. A
    /// no-op when already published: `ready` never captures. Ends the
    /// reserved admission on every path, including the early returns after
    /// `close`, so a `close` before `first` runs never hangs.
    pub(crate) fn first(&self, clock: &dyn Clock, diag: &Diag) {
        let cancel = {
            let state = lock(&self.state);
            match state.cancels.first() {
                Some(cancel) => Arc::clone(cancel),
                None => return,
            }
        };
        let _serial = lock(&self.capture_lock);
        if self.is_closed() {
            self.end(&cancel);
            return;
        }
        {
            let state = lock(&self.state);
            if state.published.is_some() {
                drop(state);
                self.end(&cancel);
                return;
            }
        }
        let outcome = capture::capture(&self.inherited, clock, &cancel);
        self.run_before_publish();
        self.finish(outcome, &cancel, diag);
    }

    /// Publishes the fallback for the admitted first capture when its
    /// thread could not start, with the failure's line.
    pub(crate) fn first_failed(&self, failed: capture::Failed, diag: &Diag) {
        let cancel = {
            let state = lock(&self.state);
            match state.cancels.first() {
                Some(cancel) => Arc::clone(cancel),
                None => return,
            }
        };
        self.finish(Err(failed), &cancel, diag);
    }

    /// Rebuilds the hub's environment for the sessions started
    /// afterwards: one capture, serialized with every other capture,
    /// published with the overlay once it ends. A failed refresh
    /// publishes the fallback. After `close` nothing is spawned and the
    /// published environment is returned. Running sessions keep theirs:
    /// a published environment never changes.
    pub(crate) fn refresh(&self, clock: &dyn Clock, diag: &Diag) -> Arc<Environment> {
        // Admitted before taking the serializing lock, and checked for
        // `close` only after holding it: a refresh admitted just before
        // `close` ends without spawning once the running capture releases
        // the lock, so no shell spawns after `close` begins.
        let cancel = capture::Cancel::new();
        {
            let mut state = lock(&self.state);
            state.running = state.running.saturating_add(1);
            state.cancels.push(Arc::clone(&cancel));
        }
        #[cfg(test)]
        {
            if let Some(hook) = lock(&self.after_admit).take() {
                hook();
            }
        }
        let _serial = lock(&self.capture_lock);
        if self.is_closed() {
            self.end(&cancel);
            return self.get();
        }
        let outcome = capture::capture(&self.inherited, clock, &cancel);
        self.run_before_publish();
        self.finish(outcome, &cancel, diag);
        self.get()
    }

    /// Stops the environment: no capture starts after it, every running
    /// capture's wait ends at once, a `get` waiting on the first capture
    /// gets the fallback, and this returns once no capture runs. The
    /// fallback publish is silent: a cancelled capture writes no
    /// `environment_built` line. Idempotent.
    pub(crate) fn close(&self) {
        let cancels = {
            let mut state = lock(&self.state);
            state.closed = true;
            if state.published.is_none() {
                state.published = Some(Arc::new(self.fallback()));
            }
            self.changed.notify_all();
            state.cancels.clone()
        };
        for cancel in &cancels {
            cancel.cancel();
        }
        let mut state = lock(&self.state);
        while state.running > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Whether [`Environments::close`] ran. The shutdown race waits for
    /// it; `first` re-checks it before spawning.
    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    /// The fallback: the hub's inherited environment, with the overlay.
    fn fallback(&self) -> Environment {
        Environment {
            vars: with_home(&self.inherited, &self.home),
            source: VariablesSource::Inherited,
        }
    }

    /// Publishes `outcome` with the overlay and writes its
    /// `environment_built` line, unless `close` began: then nothing is
    /// published and nothing is written. Ends the capture's admission
    /// either way.
    fn finish(
        &self,
        outcome: Result<Vec<(OsString, OsString)>, capture::Failed>,
        cancel: &Arc<capture::Cancel>,
        diag: &Diag,
    ) {
        let line = match &outcome {
            Ok(_) => (
                Severity::Info,
                "Sessions start with the login shell's environment.".to_owned(),
            ),
            Err(failed) => (
                Severity::Warn,
                format!(
                    "The login-shell capture failed because {}, so sessions start with the hub's inherited environment.",
                    failed.reason()
                ),
            ),
        };
        let published = match outcome {
            Ok(vars) => Environment {
                vars: with_home(&vars, &self.home),
                source: VariablesSource::LoginShell,
            },
            Err(_) => self.fallback(),
        };
        let mut state = lock(&self.state);
        if !state.closed {
            state.published = Some(Arc::new(published));
            let (severity, message) = line;
            diag.line(severity, None, "environment_built", &message);
        }
        self.end_locked(&mut state, cancel);
    }

    /// Ends one capture's admission without publishing: `close` no longer
    /// waits for it. Every early return in `first` after admission ends
    /// here, so `close` before `first` runs still returns.
    fn end(&self, cancel: &Arc<capture::Cancel>) {
        let mut state = lock(&self.state);
        self.end_locked(&mut state, cancel);
    }

    /// Ends one capture's admission: it no longer runs, and every waiter
    /// re-checks. The caller holds the lock.
    fn end_locked(&self, state: &mut State, cancel: &Arc<capture::Cancel>) {
        state.running = state.running.saturating_sub(1);
        state.cancels.retain(|held| !Arc::ptr_eq(held, cancel));
        self.changed.notify_all();
    }

    /// Runs the test-only pause between a capture's end and its publish.
    fn run_before_publish(&self) {
        #[cfg(test)]
        {
            let hook = lock(&self.before_publish).take();
            if let Some(hook) = hook {
                hook();
            }
        }
        #[cfg(not(test))]
        {}
    }
}

/// Answers `refresh` (`docs/invocation.md`, "What the hub speaks"):
/// rebuilds the hub's environment on this connection's thread,
/// serialized with every other capture. Accepted with no `result` once
/// the new environment is published, whichever way the capture ended.
/// Any argument is `invalid_arguments`, as `status`.
pub(crate) fn refresh(
    hub: &Hub,
    args: &Map<String, Value>,
) -> Result<Option<Value>, crate::connection::Refused> {
    if !args.is_empty() {
        return Err(crate::feed::invalid());
    }
    hub.environments.refresh(&*hub.clock, &hub.diag);
    Ok(None)
}

/// Locks, never poisoned: no thread holding this lock panics while
/// holding it.
/// `vars` with exactly one `FIBER_HOME`, equal to the hub's home:
/// any captured or inherited value is replaced, never kept beside it.
fn with_home(vars: &[(OsString, OsString)], home: &Path) -> Vec<(OsString, OsString)> {
    let mut kept: Vec<(OsString, OsString)> = vars
        .iter()
        .filter(|(name, _)| name != "FIBER_HOME")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    kept.push((OsString::from("FIBER_HOME"), home.as_os_str().to_owned()));
    kept
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
