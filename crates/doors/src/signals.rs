//! The signals thread (`docs/architecture.md`, "The threads"): SIGTERM,
//! SIGINT and SIGHUP start one bounded shutdown of the session process
//! (`docs/invocation.md`, "Shutdown"). What a signal does depends on how far
//! the process has come:
//!
//! - booting, before any child process starts: the process exits at once
//!   with the signal's code, writing nothing;
//! - armed, from then until `fiber_started` is about to be written: the
//!   signal is recorded, the door's startup is told to stop, and
//!   [`Signals::start`] hands it to the door, which stops what it started
//!   and exits writing nothing;
//! - started: the shutdown the door registered runs, and a second SIGTERM
//!   or SIGINT kills every live process group at once.
//!
//! The first signal in the last two also starts the bound: 5 seconds later
//! whatever is still alive is killed and the process exits with the code.

use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

/// From the signal to exit, per process (`docs/invocation.md`, "Shutdown").
const BOUND: Duration = Duration::from_secs(5);

/// The exit code a signal ends the process with: 128 plus its number, so
/// 143 for SIGTERM, 130 for SIGINT and 129 for SIGHUP
/// (`docs/invocation.md`, "Lifecycle").
pub fn signal_code(signal: i32) -> i32 {
    128 + signal
}

/// How far the process has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Nothing to stop yet.
    Booting,
    /// Children may run; `fiber_started` is not written yet.
    Armed,
    /// The session is running.
    Started,
}

/// What one signal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Exit at once with the code.
    Exit(i32),
    /// Keep the code for [`Signals::start`], and start the bound.
    Record(i32),
    /// Start the shutdown, and the bound.
    Shutdown(i32),
    /// Kill every live process group at once.
    KillGroups,
    /// Nothing more.
    Nothing,
}

/// What `signal` does in `phase` after `seen` earlier signals. The exit code
/// is always the first signal's: no later one changes it.
fn decide(phase: Phase, signal: i32, seen: u32) -> Action {
    let code = signal_code(signal);
    match phase {
        Phase::Booting => Action::Exit(code),
        Phase::Armed if seen == 0 => Action::Record(code),
        Phase::Started if seen == 0 => Action::Shutdown(code),
        // The doc names only these two for a second signal.
        Phase::Started if signal == SIGTERM || signal == SIGINT => Action::KillGroups,
        Phase::Armed | Phase::Started => Action::Nothing,
    }
}

type Callback = Arc<dyn Fn() + Send + Sync>;
/// Starts the bound's thread: [`spawn_bound`], or a test's.
type Spawn = Box<dyn Fn(Box<dyn FnOnce() + Send>) -> io::Result<()> + Send + Sync>;
type OnSignal = Arc<dyn Fn(i32) + Send + Sync>;

struct State {
    phase: Phase,
    /// Signals handled so far.
    seen: u32,
    /// The code a signal left while armed.
    recorded: Option<i32>,
    on_record: Option<Callback>,
    on_bound: Option<Callback>,
    on_signal: Option<OnSignal>,
    on_second: Option<Callback>,
}

/// The process's signal handling. One per process.
pub struct Signals {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    /// Ends the process: `std::process::exit`, or a test's recorder.
    exit: Box<dyn Fn(i32) + Send + Sync>,
    spawn: Spawn,
}

impl Signals {
    /// Takes over SIGTERM, SIGINT and SIGHUP and starts the thread that
    /// handles them, booting: a signal now exits at once. The bound runs on
    /// `clock`.
    pub fn install(clock: Arc<dyn Clock>) -> io::Result<Arc<Self>> {
        let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])?;
        let handling = Arc::new(Self::new(clock, Box::new(|code| std::process::exit(code))));
        let handler = Arc::clone(&handling);
        thread::Builder::new()
            .name("signals".to_owned())
            .spawn(move || {
                for signal in signals.forever() {
                    handler.handle(signal);
                }
            })?;
        Ok(handling)
    }

    fn new(clock: Arc<dyn Clock>, exit: Box<dyn Fn(i32) + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Booting,
                seen: 0,
                recorded: None,
                on_record: None,
                on_bound: None,
                on_signal: None,
                on_second: None,
            }),
            clock,
            exit,
            spawn: Box::new(spawn_bound),
        }
    }

    /// Called just before the first child process starts: from now a signal
    /// is recorded, the door's startup is told to stop through `on_record`,
    /// and at the bound `on_bound` kills whatever is still alive before the
    /// process exits.
    pub fn arm(
        &self,
        on_record: Box<dyn Fn() + Send + Sync>,
        on_bound: Box<dyn Fn() + Send + Sync>,
    ) {
        let mut state = lock(&self.state);
        state.on_record = Some(Arc::from(on_record));
        state.on_bound = Some(Arc::from(on_bound));
        if state.phase == Phase::Booting {
            state.phase = Phase::Armed;
        }
    }

    /// Called just before the session's first line: the code of a signal
    /// that came while armed, for the door to exit with, writing nothing.
    /// Otherwise the session is started: a first signal from now calls
    /// `on_signal` with its code, and a second SIGTERM or SIGINT calls
    /// `on_second`. A signal is either returned here or given to
    /// `on_signal`, never both.
    pub fn start(
        &self,
        on_signal: Box<dyn Fn(i32) + Send + Sync>,
        on_second: Box<dyn Fn() + Send + Sync>,
    ) -> Option<i32> {
        let mut state = lock(&self.state);
        if let Some(code) = state.recorded {
            return Some(code);
        }
        state.on_signal = Some(Arc::from(on_signal));
        state.on_second = Some(Arc::from(on_second));
        state.phase = Phase::Started;
        None
    }

    /// Handles one signal. The callbacks run outside the lock.
    fn handle(self: &Arc<Self>, signal: i32) {
        let (action, on_record, on_signal, on_second) = {
            let mut state = lock(&self.state);
            let action = decide(state.phase, signal, state.seen);
            state.seen = state.seen.saturating_add(1);
            if let Action::Record(code) = action {
                state.recorded = Some(code);
            }
            (
                action,
                state.on_record.clone(),
                state.on_signal.clone(),
                state.on_second.clone(),
            )
        };
        match action {
            Action::Exit(code) => (self.exit)(code),
            Action::Record(code) => {
                self.bound(code);
                if let Some(on_record) = on_record {
                    on_record();
                }
            }
            Action::Shutdown(code) => {
                self.bound(code);
                if let Some(on_signal) = on_signal {
                    on_signal(code);
                }
            }
            Action::KillGroups => {
                if let Some(on_second) = on_second {
                    on_second();
                }
            }
            Action::Nothing => {}
        }
    }

    /// Starts the bound: [`BOUND`] from now on the clock, `on_bound` runs
    /// and the process exits with `code`. It writes nothing: a line it
    /// wrote could be followed by a stuck loop's.
    fn bound(self: &Arc<Self>, code: i32) {
        let now = self.clock.now();
        let until = now.checked_add(BOUND).unwrap_or(now);
        let signals = Arc::clone(self);
        let started = (self.spawn)(Box::new(move || {
            sleep_until(signals.clock.as_ref(), until);
            signals.end(code);
        }));
        // No thread, no bound: rather than a shutdown with no limit, the
        // bound is reached now.
        if started.is_err() {
            self.end(code);
        }
    }

    /// The bound reached: whatever is alive is killed, and the process
    /// exits with `code`.
    fn end(&self, code: i32) {
        let on_bound = lock(&self.state).on_bound.clone();
        if let Some(on_bound) = on_bound {
            on_bound();
        }
        (self.exit)(code);
    }
}

fn spawn_bound(run: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    thread::Builder::new()
        .name("bound".to_owned())
        .spawn(run)
        .map(|_| ())
}

/// Woken on every clock move.
#[derive(Default)]
struct Tick {
    held: Mutex<()>,
    moved: Condvar,
}

impl Wake for Tick {
    fn wake(&self) {
        // Taken before the notify, so a waiter that has read the clock and
        // not yet parked cannot miss it.
        let _held = lock(&self.held);
        self.moved.notify_all();
    }
}

/// Blocks until `until` on `clock`.
fn sleep_until(clock: &dyn Clock, until: Instant) {
    let tick = Arc::new(Tick::default());
    let wake: Arc<dyn Wake> = tick.clone();
    clock.subscribe(Arc::downgrade(&wake));
    loop {
        // Taken before the clock is read and held into the wait, so a move
        // that lands in between blocks on it instead of waking nobody.
        let guard = lock(&tick.held);
        if clock.now() >= until {
            return;
        }
        let mut slot = Some(guard);
        clock.wait_until(Some(until), &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            let _woken = match bound {
                Some(bound) => {
                    tick.moved
                        .wait_timeout(guard, bound)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => tick
                    .moved
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "signals_tests.rs"]
mod tests;
