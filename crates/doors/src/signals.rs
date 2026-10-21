//! The signals thread (`docs/architecture.md`, "The threads"): SIGTERM,
//! SIGINT and SIGHUP start one bounded shutdown of the session process
//! (`docs/invocation.md`, "Shutdown"). What a signal does depends on how far
//! the process has come:
//!
//! - booting, before any child process starts: the process exits at once
//!   with the signal's code, writing nothing to the session log but one
//!   diagnostic line (`docs/invocation.md`, "Shutdown");
//! - armed, from then until `fiber_started` is about to be written: the
//!   signal is recorded, one diagnostic line is written, the door's
//!   startup is told to stop, and [`Signals::start`] hands the code to
//!   the door, which stops what it started and exits writing nothing to
//!   the session log;
//! - committing, while the first lines are written: a first signal starts
//!   the bound at once and reaches the shutdown once the write returns,
//!   so a recorded signal writes nothing and one during the write still
//!   shuts down;
//! - started: the shutdown the door registered runs, and a second SIGTERM
//!   or SIGINT kills every live process group at once.
//!
//! The first signal in the last two also starts the bound. At 4.9 s
//! whatever is still alive is killed; with a writer held, one diagnostic
//! line gets until 5 s, and the process exits with the code.
//!
//! A `close` with `now` takes the same path with exit code 0
//! (`docs/invocation.md`, "Shutdown"): [`Signals::close_now`] records
//! while armed, defers while committing, and starts the shutdown only
//! once started with no signal seen yet. Anywhere else it does nothing,
//! so a later SIGTERM or SIGINT is a second signal.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use contract::SessionId;
use contract::clock::{Clock, Wake};
use log::diag::{Diag, Severity};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

/// From the signal to exit, per process (`docs/invocation.md`, "Shutdown").
/// A delegate's stop waits this long too: `main` passes it into `jobs`
/// as the stop bound, so there is one constant.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(5);

/// How long the bound's diagnostic line may take after the kill point:
/// the process exits this long after [`KILL_AT`], with the line or
/// without it.
const LINE_CAP: Duration = Duration::from_millis(100);

/// From the signal to the kill: whatever is still alive is killed here,
/// [`LINE_CAP`] before [`SHUTDOWN_BOUND`].
const KILL_AT: Duration = SHUTDOWN_BOUND.saturating_sub(LINE_CAP);

/// The exit code a `close` with `now` ends the process with: success, as an
/// ordinary `close` would (`docs/invocation.md`, "Lifecycle").
const CLOSE_NOW_CODE: i32 = 0;

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
    /// The first lines are being written; a first signal's code waits here
    /// until the write returns.
    Committing { deferred: Option<i32> },
    /// The session is running.
    Started,
}

/// What one signal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Exit at once with the code.
    Exit(i32),
    /// Keep the code for [`Signals::start`] or [`Signals::commit`], and
    /// start the bound.
    Record(i32),
    /// Keep the code in the committing phase for [`Signals::commit`], and
    /// start the bound now; the shutdown runs once the write returns.
    Defer(i32),
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
        Phase::Committing { .. } if seen == 0 => Action::Defer(code),
        Phase::Started if seen == 0 => Action::Shutdown(code),
        // The doc names only these two for a second signal.
        Phase::Committing { .. } | Phase::Started if signal == SIGTERM || signal == SIGINT => {
            Action::KillGroups
        }
        Phase::Armed | Phase::Committing { .. } | Phase::Started => Action::Nothing,
    }
}

/// What a `close` with `now` does with no signal seen yet: while armed it
/// records exit code 0 for [`Signals::commit`], while committing it defers
/// 0 until the write returns, and once started it starts the same shutdown
/// a first signal starts. Anywhere else it does nothing, and the shutdown
/// keeps the first signal's code.
fn decide_close(phase: Phase, seen: u32) -> Action {
    match phase {
        Phase::Armed if seen == 0 => Action::Record(CLOSE_NOW_CODE),
        Phase::Committing { .. } if seen == 0 => Action::Defer(CLOSE_NOW_CODE),
        Phase::Started if seen == 0 => Action::Shutdown(CLOSE_NOW_CODE),
        Phase::Booting | Phase::Armed | Phase::Committing { .. } | Phase::Started => {
            Action::Nothing
        }
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
    /// The code whose early diagnostic line is still being written:
    /// `None` once the line is on disk, so a code without one is
    /// unrepresentable. Only a signal while booting or armed publishes it,
    /// and only with a writer held; `close_now` codes carry no line.
    early_pending: Option<i32>,
    on_record: Option<Callback>,
    on_bound: Option<Callback>,
    on_signal: Option<OnSignal>,
    on_second: Option<Callback>,
    /// Writes the bound's diagnostic line: the pid-named writer `install`
    /// held, or `None` where none is wired in yet.
    diag: Option<Arc<Diag>>,
    /// The session-named writer `attach` set, once the session exists.
    session_diag: Option<Arc<Diag>>,
}

/// The process's signal handling. One per process.
pub struct Signals {
    state: Mutex<State>,
    /// Released when `early_pending` clears: readers wait on it rather
    /// than returning a recorded code before its line is written.
    settled: Condvar,
    clock: Arc<dyn Clock>,
    /// Ends the process: `std::process::exit`, or a test's recorder.
    exit: Box<dyn Fn(i32) + Send + Sync>,
    spawn: Spawn,
}

impl Signals {
    /// Takes over SIGTERM, SIGINT and SIGHUP and starts the thread that
    /// handles them, booting: a signal now exits at once. The bound runs on
    /// `clock`, and its diagnostic line goes through `diag` when one is
    /// held: without one no bound line is written.
    pub fn install(clock: Arc<dyn Clock>, diag: Option<Diag>) -> io::Result<Arc<Self>> {
        let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT, SIGHUP])?;
        let handling = Arc::new(Self::new(
            clock,
            Box::new(|code| std::process::exit(code)),
            diag.map(Arc::new),
        ));
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

    fn new(
        clock: Arc<dyn Clock>,
        exit: Box<dyn Fn(i32) + Send + Sync>,
        diag: Option<Arc<Diag>>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Booting,
                seen: 0,
                recorded: None,
                early_pending: None,
                on_record: None,
                on_bound: None,
                on_signal: None,
                on_second: None,
                diag,
                session_diag: None,
            }),
            settled: Condvar::new(),
            clock,
            exit,
            spawn: Box::new(spawn_bound),
        }
    }

    /// Names the session once its log exists: the bound line goes to the
    /// session-named file from here on. With no writer held it does
    /// nothing. The writer is set under the state lock, so a bound line
    /// racing `attach` lands in exactly one file.
    pub fn attach(&self, session: SessionId) {
        let mut state = lock(&self.state);
        if let Some(diag) = state.diag.clone() {
            state.session_diag = Some(Arc::new(diag.for_session(&session)));
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

    /// The code a signal left while armed, if one came: read under the
    /// state lock (`docs/invocation.md`, "Shutdown"). It waits while the
    /// signal's line is still being written, so a code never returns
    /// before its line is on disk.
    pub fn recorded(&self) -> Option<i32> {
        self.settled_state().recorded
    }

    /// The state with no early line owed: waits while one's in flight, so
    /// a recorded code never returns before its line is on disk. The wait
    /// has no deadline: the bound ends the process by 5 s.
    fn settled_state(&self) -> MutexGuard<'_, State> {
        let mut state = lock(&self.state);
        while state.early_pending.is_some() {
            state = self
                .settled
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state
    }

    /// Called just before the session's first line: the code of a signal
    /// that came while armed, for the door to exit with, writing nothing
    /// to the session log. Otherwise the session stays armed until [`Signals::commit`]: a first
    /// signal from the commit on calls `on_signal` with its code, and a
    /// second SIGTERM or SIGINT calls `on_second`. A signal is either
    /// returned here or given to `on_signal`, never both.
    pub fn start(
        &self,
        on_signal: Box<dyn Fn(i32) + Send + Sync>,
        on_second: Box<dyn Fn() + Send + Sync>,
    ) -> Option<i32> {
        let mut state = self.settled_state();
        if let Some(code) = state.recorded {
            return Some(code);
        }
        state.on_signal = Some(Arc::from(on_signal));
        state.on_second = Some(Arc::from(on_second));
        None
    }

    /// Writes the session's first lines unless a signal was recorded: with
    /// no signal and no `close` with `now` recorded before the commit takes
    /// the lock, `write` runs once with no lock held and the phase becomes
    /// started afterwards, whatever `write` returned. The recorded check
    /// waits while the signal's line is still being written, so the code
    /// never returns before its line. A first signal or
    /// `close` with `now` during `write` starts the bound at that moment
    /// and reaches `on_signal` exactly once the write returns, outside the
    /// lock. Called only after [`Signals::start`] returned `None`, at most
    /// once per process.
    pub fn commit<R>(&self, write: impl FnOnce() -> R) -> Result<R, i32> {
        {
            let mut state = self.settled_state();
            if let Some(code) = state.recorded {
                return Err(code);
            }
            state.phase = Phase::Committing { deferred: None };
        }
        let output = write();
        let (deferred, on_signal) = {
            let mut state = lock(&self.state);
            let deferred = match state.phase {
                Phase::Committing { deferred } => deferred,
                Phase::Booting | Phase::Armed | Phase::Started => None,
            };
            state.phase = Phase::Started;
            (deferred, state.on_signal.clone())
        };
        if let Some(code) = deferred
            && let Some(on_signal) = on_signal
        {
            on_signal(code);
        }
        Ok(output)
    }

    /// Starts the same shutdown a first signal starts, with exit code 0
    /// (`docs/invocation.md`, "Shutdown"). While armed it records 0 for
    /// [`Signals::commit`], while committing it defers 0 until the write
    /// returns, and once started with no signal seen yet it starts the
    /// bound and calls `on_signal` with 0. Anywhere else it does nothing.
    /// The callbacks run outside the lock, as in [`Signals::handle`].
    pub fn close_now(self: &Arc<Self>) {
        let (action, on_record, on_signal) = {
            let mut state = lock(&self.state);
            let action = decide_close(state.phase, state.seen);
            match action {
                Action::Record(code) => {
                    state.seen = state.seen.saturating_add(1);
                    state.recorded = Some(code);
                }
                Action::Defer(code) => {
                    state.seen = state.seen.saturating_add(1);
                    state.phase = Phase::Committing {
                        deferred: Some(code),
                    };
                }
                Action::Shutdown(_) => {
                    state.seen = state.seen.saturating_add(1);
                }
                Action::Exit(_) | Action::KillGroups | Action::Nothing => {}
            }
            (action, state.on_record.clone(), state.on_signal.clone())
        };
        self.run(action, on_record, on_signal, None);
    }

    /// Starts the lifeline: a thread that reads `input` to EOF or an
    /// error, then handles SIGHUP (`docs/delegates.md`, "Lifetime"). A
    /// Fiber delegate's parent holds the write end open and never writes
    /// to it, so end of file means the parent is gone, however it died:
    /// a shutdown with exit 129, a hangup. Before [`Signals::commit`] the
    /// armed and booting phases exit or record as for a real SIGHUP. Any
    /// byte the parent writes is ignored.
    pub fn lifeline(self: &Arc<Self>, input: Box<dyn Read + Send>) {
        let signals = Arc::clone(self);
        let watched = thread::Builder::new()
            .name("lifeline".to_owned())
            .spawn(move || {
                drain(input);
                signals.handle(SIGHUP);
            });
        // No thread, no lifeline: without it a dead parent would leave
        // the delegate running, so the hangup is taken at once instead.
        if watched.is_err() {
            self.handle(SIGHUP);
        }
    }

    /// Handles one signal. The callbacks run outside the lock. A first
    /// signal while booting or armed claims one diagnostic line through
    /// the pid-named writer, if one is held (`docs/invocation.md`,
    /// "Shutdown"): the claim happens under the same lock as the
    /// decision, so at most one line is written per process.
    fn handle(self: &Arc<Self>, signal: i32) {
        let (action, on_record, on_signal, on_second, early) = {
            let mut state = lock(&self.state);
            let mut action = decide(state.phase, signal, state.seen);
            // At most one early line: a booting signal after a recorded
            // code writes nothing more.
            if matches!(action, Action::Exit(_)) && state.diag.is_some() && state.recorded.is_some()
            {
                action = Action::Nothing;
            }
            state.seen = state.seen.saturating_add(1);
            let early = match (action, state.diag.clone()) {
                (Action::Record(code) | Action::Exit(code), Some(diag)) => {
                    state.recorded = Some(code);
                    state.early_pending = Some(code);
                    Some((signal, code, diag))
                }
                (Action::Record(code), None) => {
                    state.recorded = Some(code);
                    None
                }
                (Action::Defer(code), _) => {
                    state.phase = Phase::Committing {
                        deferred: Some(code),
                    };
                    None
                }
                (
                    Action::Exit(_) | Action::Shutdown(_) | Action::KillGroups | Action::Nothing,
                    _,
                ) => None,
            };
            (
                action,
                state.on_record.clone(),
                state.on_signal.clone(),
                state.on_second.clone(),
                early,
            )
        };
        if let Some((signal, code, diag)) = early {
            self.run_early(action, signal, code, diag, on_record);
        } else {
            self.run(action, on_record, on_signal, on_second);
        }
    }

    /// Runs a booting or armed signal with a writer held: the bound starts
    /// before the early append, so a stalled open cannot eat it, then
    /// `on_record` runs for an armed signal, one diagnostic line goes to
    /// the pid-named writer, the readers are released, and a booting
    /// signal exits. When the bound thread cannot start, the readers are
    /// released before `end` writes the timeout line and exits on this
    /// thread instead, and the early line is never written. The callbacks
    /// and the append run outside the lock.
    fn run_early(
        self: &Arc<Self>,
        action: Action,
        signal: i32,
        code: i32,
        diag: Arc<Diag>,
        on_record: Option<Callback>,
    ) {
        match self.start_bound(code) {
            Ok(()) => {
                if matches!(action, Action::Record(_))
                    && let Some(on_record) = on_record
                {
                    on_record();
                }
                diag.line(
                    Severity::Error,
                    None,
                    "signal",
                    &early_message(signal, code),
                );
                self.clear_early();
                if matches!(action, Action::Exit(_)) {
                    (self.exit)(code);
                }
            }
            Err(exit_by) => {
                // No thread, no bound and no early line: release the
                // readers before `end` writes the timeout line and exits
                // on this thread, as on the bound path.
                self.clear_early();
                self.end(code, exit_by);
                if matches!(action, Action::Record(_))
                    && let Some(on_record) = on_record
                {
                    on_record();
                }
            }
        }
    }

    /// Releases the readers waiting on the early line: the line is on
    /// disk, or never will be, so a recorded code may go out.
    fn clear_early(&self) {
        let mut state = lock(&self.state);
        state.early_pending = None;
        self.settled.notify_all();
    }

    /// Runs what `action` decided, outside the lock.
    fn run(
        self: &Arc<Self>,
        action: Action,
        on_record: Option<Callback>,
        on_signal: Option<OnSignal>,
        on_second: Option<Callback>,
    ) {
        match action {
            Action::Exit(code) => (self.exit)(code),
            Action::Record(code) => {
                self.bound(code);
                if let Some(on_record) = on_record {
                    on_record();
                }
            }
            Action::Defer(code) => {
                self.bound(code);
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

    /// Starts the bound from one reading of the clock: at [`KILL_AT`] the
    /// bound thread kills whatever is still alive, and at
    /// [`SHUTDOWN_BOUND`] the process exits with `code`. It writes nothing
    /// to the session log: a line it wrote could be followed by a stuck
    /// loop's.
    fn bound(self: &Arc<Self>, code: i32) {
        // No thread, no bound: rather than a shutdown with no limit, the
        // kill point is reached now, with only the line's own cap left.
        if let Err(exit_by) = self.start_bound(code) {
            self.end(code, exit_by);
        }
    }

    /// Spawns the bound thread for `code`: at [`KILL_AT`] it kills
    /// whatever is still alive, and at [`SHUTDOWN_BOUND`] the process
    /// exits with `code`. `Ok` once started; `Err` with the line's own cap
    /// when no thread started, for the caller to reach the kill point on
    /// its own thread instead.
    fn start_bound(self: &Arc<Self>, code: i32) -> Result<(), Instant> {
        let now = self.clock.now();
        let kill_at = now.checked_add(KILL_AT).unwrap_or(now);
        let exit_by = now.checked_add(SHUTDOWN_BOUND).unwrap_or(now);
        let signals = Arc::clone(self);
        match (self.spawn)(Box::new(move || {
            sleep_until(signals.clock.as_ref(), kill_at);
            signals.end(code, exit_by);
        })) {
            Ok(()) => Ok(()),
            Err(_) => Err(now.checked_add(LINE_CAP).unwrap_or(now)),
        }
    }

    /// The kill point reached: whatever is alive is killed, then, with a
    /// writer held, one diagnostic line gets until `exit_by`, and the
    /// process exits with `code`. The calling thread never touches the
    /// writer: only the `diag` thread does, outside the state lock. The
    /// writer is the session one once `attach` has run, else the
    /// pid-named one, chosen under the state lock so a line racing
    /// `attach` lands in exactly one file.
    fn end(&self, code: i32, exit_by: Instant) {
        let (on_bound, diag) = {
            let state = lock(&self.state);
            (
                state.on_bound.clone(),
                state.session_diag.clone().or_else(|| state.diag.clone()),
            )
        };
        if let Some(on_bound) = on_bound {
            on_bound();
        }
        let Some(diag) = diag else {
            (self.exit)(code);
            return;
        };
        let tick = Arc::new(support::clock::Parker::new());
        let wake: Arc<dyn Wake> = tick.clone();
        self.clock.subscribe(Arc::downgrade(&wake));
        let done = Arc::new(AtomicBool::new(false));
        let message = kill_point_message(code);
        let bumped = Arc::clone(&tick);
        let written = Arc::clone(&done);
        // No branch on the spawn: with no thread the wait below still ends
        // the process at `exit_by`, with no line, and the line is never
        // written inline, where a stuck loop could hold it up.
        drop(
            thread::Builder::new()
                .name("diag".to_owned())
                .spawn(move || {
                    diag.line(Severity::Error, None, "timeout", &message);
                    written.store(true, Ordering::SeqCst);
                    bumped.bump();
                }),
        );
        park_until(self.clock.as_ref(), exit_by, &tick, &|| {
            done.load(Ordering::SeqCst)
        });
        (self.exit)(code);
    }
}

fn spawn_bound(run: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    thread::Builder::new()
        .name("bound".to_owned())
        .spawn(run)
        .map(|_| ())
}

/// The early diagnostic line's message for `signal` ending the process
/// with `code` before `fiber_started` (`docs/invocation.md`, "Shutdown"):
/// the lifeline's end of file takes the SIGHUP sentence, as it takes the
/// SIGHUP path.
fn early_message(signal: i32, code: i32) -> String {
    format!(
        "{} arrived before the session started; the process exits {}.",
        support::group::signal_name(signal),
        code
    )
}

/// The bound's diagnostic line for `code`.
fn kill_point_message(code: i32) -> String {
    format!(
        "The shutdown reached its 4.9-second kill point; every process group still alive was killed and the process exits {code}."
    )
}

/// Blocks until `until` on `clock`, or until `done` holds, whichever comes
/// first. The loop reads `generation` before either check, so a bump that
/// lands between the checks still ends the park at once, and the clock is
/// read before the park, so a move that lands in between cannot be missed.
fn park_until(
    clock: &dyn Clock,
    until: Instant,
    tick: &support::clock::Parker,
    done: &dyn Fn() -> bool,
) {
    loop {
        let seen = tick.generation();
        if done() {
            return;
        }
        if clock.now() >= until {
            return;
        }
        tick.park(clock, Some(until), seen);
    }
}

/// Blocks until `until` on `clock`.
fn sleep_until(clock: &dyn Clock, until: Instant) {
    let tick = Arc::new(support::clock::Parker::new());
    let wake: Arc<dyn Wake> = tick.clone();
    clock.subscribe(Arc::downgrade(&wake));
    park_until(clock, until, &tick, &|| false);
}

/// Reads `input` to EOF or an error, ignoring every byte: the parent
/// never writes to the lifeline. An interrupted read is retried, as
/// `io::copy` does; any other error ends it like EOF.
fn drain(mut input: Box<dyn Read + Send>) {
    let _copied = io::copy(&mut input, &mut io::sink());
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "signals_tests.rs"]
mod tests;
