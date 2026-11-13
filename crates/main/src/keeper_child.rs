//! The keeper process (`docs/tools.md`, "The keeper"). No providers or hooks.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::events::EventKind;
use contract::keeper::{Exit, FromKeeper, KeptJob, Spawn, Stream, ToKeeper};
use jobs::keeper::{Step, Supervisor};
use log::Log;
use rustix::process::Signal;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Appends one takeover marker to the held log. A failed append stops the
/// log like any other failed write, and the takeover carries on: the error
/// is bound to a name and dropped because the log is already stopped, and
/// the keeper's next durable append reports it through the path it already
/// has.
fn append_marker(log: &Log, event: contract::events::Event) {
    if let Err(error) = log
        .append(&event, None, None)
        .map_err(|error| io::Error::other(error.to_string()))
    {
        // The log is already stopped, and the next durable append reports it.
        drop(error);
    }
}

/// How long after a crash the keeper holds its jobs before stopping them
/// shutdown-style (`docs/tools.md`, "The keeper").
const KEEPER_BOUND: Duration = Duration::from_secs(7_200);

/// The keeper's post-crash hold bound: 2 hours, or the debug-only
/// millisecond hook when it parses. Release builds always enforce 2 hours.
#[cfg(debug_assertions)]
fn keeper_bound() -> Duration {
    std::env::var("FIBER_KEEPER_BOUND_MS")
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(KEEPER_BOUND)
}

/// The keeper's post-crash hold bound: 2 hours.
#[cfg(not(debug_assertions))]
fn keeper_bound() -> Duration {
    KEEPER_BOUND
}

/// The exact bytes `fiber stop` and `/close all` write on a crashed
/// session's socket to stop its keeper (`docs/invocation.md`, "Commands and
/// flags"). Only this line acts; anything else is dropped unread, so a
/// probe's read sees EOF exactly as before.
const CRASHED_STOP_LINE: &str = "{\"shutdown\":true}";

enum Sink {
    Control,
    Hold(Vec<(Stream, Vec<u8>)>),
    Files { output: File, errors: Option<File> },
    Discard,
}
struct Adoption {
    output: PathBuf,
    errors: Option<PathBuf>,
    lines: bool,
}
struct State {
    sink: Sink,
    adoption: Option<Adoption>,
    count: u64,
    cap: u64,
    timed_out: bool,
    output_cap: bool,
    stopping: Option<Instant>,
    ended: bool,
    readers: usize,
    seq: u64,
}
struct Process {
    pgid: u32,
    state: Mutex<State>,
    cv: Condvar,
    input: Option<contract::jobs::Input>,
}
impl Wake for Process {
    fn wake(&self) {
        let mut state = lock(&self.state);
        state.seq = state.seq.wrapping_add(1);
        self.cv.notify_all();
    }
}
// The socket belongs to this writer. Unlink before Log's field drop clears
// the label and releases the session lock (`docs/state.md`, "What each part holds").
struct HeldLog {
    socket: PathBuf,
    log: Log,
}

impl Drop for HeldLog {
    fn drop(&mut self) {
        std::fs::remove_file(&self.socket).unwrap_or(());
    }
}

/// The socket the keeper accepts resuming sessions on while it holds the log.
struct Listening {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
}

/// A candidate that asked for the handover and awaits the serialized decision.
struct Candidate {
    ticket: u64,
    stream: UnixStream,
}

/// Test-only pause and notification points, each with the race it forces.
#[cfg(test)]
#[derive(Default)]
struct Hooks {
    /// Runs just before a recovery opens the log, so a test decides which
    /// of two recoveries reaches the lock first (the lock-loss race).
    before_open: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs once a recovery holds the log and accepts candidates.
    held: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs after a takeover opens the log and before it appends its start
    /// record, so a test fails that append.
    #[allow(clippy::type_complexity, reason = "the hook takes the just-opened log")]
    before_start_append: Option<Box<dyn Fn(&Log) + Send + Sync>>,
    /// Runs after a takeover appends its start record and before it binds,
    /// so a test lands keeper death in that window.
    before_bind: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs after a candidate registered and before it contends for the log,
    /// so a test lands the last job's exit in that window.
    registered: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs once a candidate holds the log, before it decides, so a test
    /// reads what had happened by then.
    acquired: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs immediately before a handover switches each job's sink from its
    /// files to the session, so a test lands a write in that window.
    before_switch: Option<Box<dyn Fn() + Send + Sync>>,
    /// Runs as a bound watcher ends, with its generation.
    watcher_done: Option<Box<dyn Fn(u64) + Send + Sync>>,
    accept_error: Option<Box<dyn Fn() -> Option<io::ErrorKind> + Send + Sync>>,
}

struct Runtime {
    home: PathBuf,
    id: SessionId,
    clock: Arc<dyn Clock>,
    out: Mutex<UnixStream>,
    log: Mutex<Option<HeldLog>>,
    supervisor: Mutex<Supervisor>,
    processes: Mutex<BTreeMap<String, Arc<Process>>>,
    logged: Mutex<HashSet<String>>,
    done: mpsc::Sender<io::Result<()>>,
    listening: Mutex<Option<Listening>>,
    candidate: Mutex<Option<Candidate>>,
    tickets: AtomicU64,
    /// The current takeover's generation: a bound watcher acts only while it
    /// still holds this value, and a handover moves it on.
    bound: AtomicU64,
    watcher: Mutex<Option<Arc<support::clock::Parker>>>,
    #[cfg(test)]
    hooks: Hooks,
}
// A session may die with starts queued or replies unread. Losing its
// control transport must not bypass the lifeline's crash/shutdown decision.
fn disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    )
}

impl Runtime {
    fn new(
        home: PathBuf,
        id: SessionId,
        clock: Arc<dyn Clock>,
        out: UnixStream,
        done: mpsc::Sender<io::Result<()>>,
        #[cfg(test)] hooks: Hooks,
    ) -> Arc<Self> {
        Arc::new(Self {
            home,
            id,
            clock,
            out: Mutex::new(out),
            log: Mutex::new(None),
            supervisor: Mutex::new(Supervisor::new()),
            processes: Mutex::new(BTreeMap::new()),
            logged: Mutex::new(HashSet::new()),
            done,
            listening: Mutex::new(None),
            candidate: Mutex::new(None),
            tickets: AtomicU64::new(0),
            bound: AtomicU64::new(0),
            watcher: Mutex::new(None),
            #[cfg(test)]
            hooks,
        })
    }

    fn send(&self, line: FromKeeper) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        match lock(&self.out).write_all(&bytes) {
            Err(error) if disconnected(&error) => Ok(()),
            result => result,
        }
    }

    fn decide(
        self: &Arc<Self>,
        choose: impl FnOnce(&mut Supervisor) -> Vec<Step>,
    ) -> io::Result<()> {
        let mut log = lock(&self.log);
        let steps = choose(&mut lock(&self.supervisor));
        self.apply(&mut log, steps)
    }

    fn apply(self: &Arc<Self>, log: &mut Option<HeldLog>, steps: Vec<Step>) -> io::Result<()> {
        for step in steps {
            match step {
                Step::Stop { id } => {
                    if let Some(process) = lock(&self.processes).get(&id) {
                        self.stop(process);
                    }
                }
                Step::HoldLog => {
                    let Some((_, dir)) = log::find_session(&self.home, &self.id) else {
                        let steps = lock(&self.supervisor).lock_lost();
                        return self.apply(log, steps);
                    };
                    let sessions = dir
                        .parent()
                        .ok_or_else(|| io::Error::other("session has no parent"))?;
                    #[cfg(test)]
                    if let Some(pause) = &self.hooks.before_open {
                        pause();
                    }
                    match Log::open_as(
                        sessions,
                        self.id.clone(),
                        Arc::clone(&self.clock),
                        &format!("keeper {}", std::process::id()),
                    ) {
                        Ok(held) => {
                            #[cfg(test)]
                            if let Some(hook) = &self.hooks.before_start_append {
                                hook(&held);
                            }
                            let completed = log::read(&dir)
                                .map_err(|error| io::Error::other(error.to_string()))?;
                            *lock(&self.logged) = completed
                                .into_iter()
                                .filter(|line| line.kind() == EventKind::JobCompleted)
                                .filter_map(|line| {
                                    line.payload
                                        .get("job_id")
                                        .and_then(|value| value.as_str())
                                        .map(str::to_owned)
                                })
                                .collect();
                            append_marker(
                                &held,
                                contract::events::Event::KeeperStarted(contract::events::Empty {}),
                            );
                            #[cfg(test)]
                            if let Some(pause) = &self.hooks.before_bind {
                                pause();
                            }
                            let (socket, listener) = doors::bind(&self.home, &dir)
                                .map_err(|error| io::Error::other(error.to_string()))?;
                            *log = Some(HeldLog {
                                socket: socket.clone(),
                                log: held,
                            });
                            self.listen(socket, listener);
                            if let Some(log) = log.as_ref() {
                                append_marker(
                                    &log.log,
                                    contract::events::Event::KeeperBound(
                                        contract::events::Empty {},
                                    ),
                                );
                            }
                            self.files()?;
                            #[cfg(test)]
                            if let Some(held) = &self.hooks.held {
                                held();
                            }
                        }
                        Err(log::Error::Held { .. }) => {
                            self.discard();
                            let steps = lock(&self.supervisor).lock_lost();
                            return self.apply(log, steps);
                        }
                        Err(error) => return Err(io::Error::other(error.to_string())),
                    }
                }
                Step::WriteJobCompleted {
                    job_id,
                    exit,
                    unless_logged,
                    ..
                } => {
                    if unless_logged && lock(&self.logged).contains(&job_id.0) {
                        continue;
                    }
                    if let Some(log) = log.as_ref() {
                        log.log
                            .append(
                                &contract::events::Event::JobCompleted(tools::kept_end(
                                    job_id, &exit,
                                )),
                                None,
                                None,
                            )
                            .map_err(|error| io::Error::other(error.to_string()))?;
                    }
                }
                Step::WriteKeeperTimeout { job_id, exit, .. } => {
                    if lock(&self.logged).contains(&job_id.0) {
                        continue;
                    }
                    if let Some(log) = log.as_ref() {
                        let mut completed = tools::kept_end(job_id, &exit);
                        completed.status = contract::events::Outcome::Failed;
                        completed.error = Some(contract::shapes::Failure {
                            code: contract::ErrorCode::KeeperTimeout,
                            message: "The session stayed gone for 2 hours after a crash, so its keeper stopped the job.".into(),
                            retry_after_ms: None,
                            provider: None,
                        });
                        log.log
                            .append(
                                &contract::events::Event::JobCompleted(completed),
                                None,
                                None,
                            )
                            .map_err(|error| io::Error::other(error.to_string()))?;
                    }
                }
                Step::Exit => {
                    self.discard();
                    self.release(log);
                    // A candidate still waiting for its answer learns the
                    // keeper holds nothing: the log is already released.
                    if let Some(candidate) = lock(&self.candidate).take() {
                        let mut stream = &candidate.stream;
                        match write_frame(
                            &mut stream,
                            &FromKeeper::Handover {
                                running: Vec::new(),
                            },
                        ) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                    match self.done.send(Ok(())) {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn discard(&self) {
        for process in lock(&self.processes).values() {
            lock(&process.state).sink = Sink::Discard;
        }
    }

    fn files(&self) -> io::Result<()> {
        for process in lock(&self.processes).values() {
            let mut state = lock(&process.state);
            let old = std::mem::replace(&mut state.sink, Sink::Discard);
            let Some(adoption) = state.adoption.as_ref() else {
                continue;
            };
            let output = OpenOptions::new().append(true).open(&adoption.output)?;
            let errors = adoption
                .errors
                .as_ref()
                .map(|path| OpenOptions::new().append(true).open(path))
                .transpose()?;
            let mut sink = Sink::Files { output, errors };
            if let Sink::Hold(held) = old {
                for (stream, bytes) in held {
                    write_sink(&mut sink, stream, &bytes)?;
                }
            }
            state.sink = sink;
        }
        Ok(())
    }

    fn stop(&self, process: &Process) {
        let mut state = lock(&process.state);
        if state.stopping.is_none() && !state.ended {
            state.stopping = Some(self.clock.now());
            match support::group::signal(process.pgid, Signal::TERM) {
                Ok(()) | Err(_) => {}
            }
            state.seq = state.seq.wrapping_add(1);
            process.cv.notify_all();
        }
    }

    fn start(self: &Arc<Self>, spec: Spawn) -> io::Result<()> {
        if lock(&self.processes).contains_key(&spec.id) {
            return self.send(FromKeeper::Failed {
                id: spec.id,
                error: "duplicate hosted call".into(),
            });
        }
        let (mut child, listing, streams) = match tools::keeper_spawn(&spec) {
            Ok(started) => started,
            Err(error) => {
                return self.send(FromKeeper::Failed {
                    id: spec.id,
                    error: error.to_string(),
                });
            }
        };
        let start = self.clock.now();
        let process = Arc::new(Process {
            pgid: child.id(),
            input: streams.input,
            state: Mutex::new(State {
                sink: Sink::Control,
                adoption: None,
                count: 0,
                cap: spec.output_cap,
                timed_out: false,
                output_cap: false,
                stopping: None,
                ended: false,
                readers: if streams.errors.is_some() { 2 } else { 1 },
                seq: 0,
            }),
            cv: Condvar::new(),
        });
        let wake: Arc<dyn Wake> = process.clone();
        self.clock.subscribe(Arc::downgrade(&wake));
        lock(&self.processes).insert(spec.id.clone(), Arc::clone(&process));
        self.decide(|sup| {
            sup.started(spec.id.clone());
            Vec::new()
        })?;
        self.send(FromKeeper::Started {
            id: spec.id.clone(),
            pgid: process.pgid,
        })?;
        self.reader(
            spec.id.clone(),
            Arc::clone(&process),
            Stream::Out,
            streams.read,
        );
        if let Some(errors) = streams.errors {
            self.reader(
                spec.id.clone(),
                Arc::clone(&process),
                Stream::Err,
                Box::new(errors),
            );
        }
        let runtime = Arc::clone(self);
        let timed = Arc::clone(&process);
        let timeout_at = spec
            .timeout_ms
            .and_then(|ms| start.checked_add(std::time::Duration::from_millis(ms)));
        thread::spawn(move || runtime.timer(&timed, timeout_at));
        let runtime = Arc::clone(self);
        thread::spawn(move || {
            let status = child.wait();
            let result = status.and_then(|status| {
                // Reap first: a zombie must not keep the group looking occupied.
                loop {
                    let state = lock(&process.state);
                    if !support::group::alive(process.pgid) {
                        break;
                    }
                    let kill_bound = state.stopping.and_then(|at| {
                        at.checked_add(support::group::GRACE + support::group::DRAIN)
                    });
                    if kill_bound.is_some_and(|at| runtime.clock.now() >= at) {
                        break;
                    }
                    let seen = state.seq;
                    drop(support::clock::park(
                        &*runtime.clock,
                        kill_bound,
                        Some(support::group::GROUP_POLL),
                        &process.cv,
                        state,
                        |state| state.seq != seen,
                    ));
                }
                support::group::live().unlist(listing);
                let until = runtime.clock.now().checked_add(support::group::DRAIN);
                let mut state = lock(&process.state);
                while state.readers > 0 && until.is_none_or(|at| runtime.clock.now() < at) {
                    let seen = state.seq;
                    state = support::clock::park(
                        &*runtime.clock,
                        until,
                        None,
                        &process.cv,
                        state,
                        |state| state.seq != seen,
                    )
                    .unwrap_or_else(|| lock(&process.state));
                }
                state.ended = true;
                state.seq = state.seq.wrapping_add(1);
                process.cv.notify_all();
                let exit = Exit {
                    code: status.code(),
                    signal: status.signal(),
                    timed_out: state.timed_out,
                    output_cap: state.output_cap,
                };
                drop(state);
                // Reader completion precedes exit, and the log mutex precedes sup.
                // The frame goes out under the log mutex: a handover that
                // swaps the connection decides which one carries the exit,
                // and the ending is then written by exactly one side.
                let mut log = lock(&runtime.log);
                match runtime.send(FromKeeper::Exit {
                    id: spec.id.clone(),
                    exit: exit.clone(),
                }) {
                    Ok(()) | Err(_) => {}
                }
                let steps = lock(&runtime.supervisor).exited(&spec.id, exit);
                runtime.apply(&mut log, steps)
            });
            if let Err(error) = result {
                match runtime.done.send(Err(error)) {
                    Ok(()) | Err(_) => {}
                }
            }
        });
        Ok(())
    }

    fn reader(
        self: &Arc<Self>,
        id: String,
        process: Arc<Process>,
        stream: Stream,
        mut read: Box<dyn Read + Send>,
    ) {
        let runtime = Arc::clone(self);
        thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let mut buffer = [0; 8192];
                loop {
                    let count = match read.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => count,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    };
                    let mut state = lock(&process.state);
                    let room = usize::try_from(state.cap.saturating_sub(state.count))
                        .unwrap_or(usize::MAX);
                    let kept = buffer.get(..count.min(room)).unwrap_or_default();
                    state.count = state
                        .count
                        .saturating_add(u64::try_from(kept.len()).unwrap_or(u64::MAX));
                    if count > room {
                        state.output_cap = true;
                    }
                    match &mut state.sink {
                        Sink::Control => {
                            match runtime.send(FromKeeper::Output {
                                id: id.clone(),
                                stream,
                                bytes: base64::engine::general_purpose::STANDARD.encode(kept),
                            }) {
                                Ok(()) | Err(_) => {}
                            }
                        }
                        Sink::Hold(held) => held.push((stream, kept.to_vec())),
                        sink @ (Sink::Files { .. } | Sink::Discard) => {
                            write_sink(sink, stream, kept)?
                        }
                    }
                    let capped = state.output_cap;
                    drop(state);
                    if capped {
                        runtime.stop(&process);
                    }
                }
                Ok(())
            })();
            let mut state = lock(&process.state);
            state.readers = state.readers.saturating_sub(1);
            state.seq = state.seq.wrapping_add(1);
            process.cv.notify_all();
            drop(state);
            if let Err(error) = result {
                match runtime.done.send(Err(error)) {
                    Ok(()) | Err(_) => {}
                }
            }
        });
    }

    fn timer(&self, process: &Process, timeout: Option<Instant>) {
        let mut state = lock(&process.state);
        loop {
            if state.ended {
                return;
            }
            let now = self.clock.now();
            if state.stopping.is_none() && timeout.is_some_and(|at| now >= at) {
                state.timed_out = true;
                drop(state);
                self.stop(process);
                state = lock(&process.state);
                continue;
            }
            let kill_at = state
                .stopping
                .and_then(|at| at.checked_add(support::group::GRACE));
            if kill_at.is_some_and(|at| now >= at) {
                match support::group::signal(process.pgid, Signal::KILL) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            let until = if state.stopping.is_some() {
                kill_at
            } else {
                timeout
            };
            let seen = state.seq;
            state = support::clock::park(&*self.clock, until, None, &process.cv, state, |state| {
                state.seq != seen
            })
            .unwrap_or_else(|| lock(&process.state));
        }
    }

    /// Parks until the post-crash bound, then stops every held job. After
    /// each return the clock is re-read and the bound decides only when it
    /// has passed, so an early wake re-parks instead of firing. The watcher
    /// belongs to one takeover generation: a handover moves the generation
    /// on and wakes it, and it then ends without acting, so an old crash's
    /// bound never expires jobs a later takeover holds.
    fn bound_watch(
        self: &Arc<Self>,
        generation: u64,
        bound_at: Instant,
        parker: &Arc<support::clock::Parker>,
    ) {
        let wake: Arc<dyn contract::clock::Wake> = parker.clone();
        self.clock.subscribe(Arc::downgrade(&wake));
        loop {
            let seen = parker.generation();
            if self.bound.load(Ordering::Acquire) != generation {
                break;
            }
            if self.clock.now() >= bound_at {
                let mut log = lock(&self.log);
                // The log mutex orders this against a handover's move.
                if self.bound.load(Ordering::Acquire) == generation {
                    let steps = lock(&self.supervisor).bound_elapsed();
                    match self.apply(&mut log, steps) {
                        Ok(()) | Err(_) => {}
                    }
                }
                break;
            }
            parker.park(&*self.clock, Some(bound_at), seen);
        }
        #[cfg(test)]
        if let Some(done) = &self.hooks.watcher_done {
            done(generation);
        }
    }

    /// Starts the bound watcher for a takeover that holds the log.
    fn watch_bound(self: &Arc<Self>, crash_at: Instant) {
        let log = lock(&self.log);
        if log.is_none() {
            return;
        }
        // Moved under the log mutex, as a handover moves it.
        let generation = self.bound.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let parker = Arc::new(support::clock::Parker::new());
        *lock(&self.watcher) = Some(Arc::clone(&parker));
        drop(log);
        let bound_at = crash_at + keeper_bound();
        let watcher = Arc::clone(self);
        thread::spawn(move || watcher.bound_watch(generation, bound_at, &parker));
    }

    /// Output of a job whose session is gone waits in memory until the log
    /// is held and its files can take it.
    fn hold_output(&self) {
        for process in lock(&self.processes).values() {
            let mut state = lock(&process.state);
            if matches!(state.sink, Sink::Control) {
                state.sink = Sink::Hold(Vec::new());
            }
        }
    }

    /// The session behind a control connection is gone: hold what the jobs
    /// write, take the log back and watch the bound.
    fn crashed(self: &Arc<Self>) -> io::Result<()> {
        self.hold_output();
        self.takeover()
    }

    fn takeover(self: &Arc<Self>) -> io::Result<()> {
        let crash_at = self.clock.now();
        self.decide(Supervisor::lifeline_eof)?;
        // The crash left jobs behind: watch the bound from the crash instant.
        self.watch_bound(crash_at);
        Ok(())
    }

    /// Accepts resuming sessions on the keeper's socket until released.
    fn listen(self: &Arc<Self>, socket: PathBuf, listener: UnixListener) {
        let stop = Arc::new(AtomicBool::new(false));
        *lock(&self.listening) = Some(Listening {
            stop: Arc::clone(&stop),
            socket,
        });
        let runtime = Arc::clone(self);
        thread::spawn(move || {
            let backoff = Arc::new(support::clock::Parker::new());
            let waker: Arc<dyn Wake> = backoff.clone();
            runtime.clock.subscribe(Arc::downgrade(&waker));
            let mut delay = Duration::from_millis(10);
            let mut incoming = listener.incoming();
            #[cfg(test)]
            let mut inject = true;
            loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                #[cfg(test)]
                let accepted = if inject {
                    inject = false;
                    runtime
                        .hooks
                        .accept_error
                        .as_ref()
                        .and_then(|hook| hook())
                        .map_or_else(
                            || {
                                incoming
                                    .next()
                                    .unwrap_or_else(|| Err(io::Error::other("listener ended")))
                            },
                            |kind| Err(io::Error::from(kind)),
                        )
                } else {
                    incoming
                        .next()
                        .unwrap_or_else(|| Err(io::Error::other("listener ended")))
                };
                #[cfg(not(test))]
                let accepted = incoming
                    .next()
                    .unwrap_or_else(|| Err(io::Error::other("listener ended")));
                let Ok(stream) = accepted else {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let seen = backoff.generation();
                    let until = runtime.clock.now() + delay;
                    backoff.park(&*runtime.clock, Some(until), seen);
                    delay = delay.saturating_mul(2).min(Duration::from_secs(1));
                    continue;
                };
                delay = Duration::from_millis(10);
                // One thread per candidate: a stalled one holds up nothing.
                let runtime = Arc::clone(&runtime);
                let Ok(_candidate) = thread::Builder::new()
                    .name("keeper-candidate".into())
                    .spawn(move || runtime.candidate(stream))
                else {
                    continue;
                };
            }
        });
    }

    /// Stops accepting, unlinks the socket and releases the log, in that
    /// order, while still owning it: the socket never outlives the lock.
    fn release(&self, log: &mut Option<HeldLog>) {
        if let Some(listening) = lock(&self.listening).take() {
            listening.stop.store(true, Ordering::Release);
            // `accept` only returns for a connection: make one, so the
            // acceptor ends and drops its listener.
            match UnixStream::connect(&listening.socket) {
                Ok(_) | Err(_) => {}
            }
        }
        *log = None;
    }

    /// One connection on the keeper's socket: a frozen stop line stops
    /// the keeper, and anything else may identify the session and ask
    /// for the handover, within one deadline. The stop message predates
    /// versioning, so its bytes are checked before any parse
    /// (`docs/invocation.md`, "Commands and flags").
    fn candidate(self: &Arc<Self>, stream: UnixStream) {
        let Ok(reader) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(reader);
        let expiry =
            crate::keeper::Expiry::start(&self.clock, &stream, crate::keeper::KEEPER_START);
        let asked = (|| -> io::Result<bool> {
            let Some(frame) = crate::keeper::read_frame(&mut reader)? else {
                return Ok(false);
            };
            if frame == CRASHED_STOP_LINE.as_bytes() {
                self.discard();
                match self.decide(Supervisor::shutdown_requested) {
                    Ok(()) | Err(_) => {}
                }
                match self.decide(|_| vec![Step::Exit]) {
                    Ok(()) | Err(_) => {}
                }
                return Ok(false);
            }
            let Some(ToKeeper::Identify(asked)) =
                serde_json::from_slice(&frame).map_err(io::Error::from)?
            else {
                return Ok(false);
            };
            if asked != self.id {
                return Ok(false);
            }
            write_frame(
                &mut &stream,
                &FromKeeper::Identity {
                    session_id: self.id.clone(),
                },
            )?;
            Ok(matches!(
                crate::keeper::read_request(&mut reader)?,
                Some(ToKeeper::Handover(true))
            ))
        })();
        let expired = expiry.finish();
        if matches!(asked, Ok(true)) && !expired {
            self.hand_over(stream, reader);
        }
    }

    /// The decision, serialized with every append and exit: either this
    /// candidate takes the log and the jobs, or the last job's exit won and
    /// has already answered it with none.
    fn hand_over(self: &Arc<Self>, stream: UnixStream, mut reader: BufReader<UnixStream>) {
        let ticket = self.tickets.fetch_add(1, Ordering::AcqRel);
        {
            let mut candidate = lock(&self.candidate);
            let Ok(reply) = stream.try_clone() else {
                return;
            };
            if candidate.is_some() {
                return;
            }
            *candidate = Some(Candidate {
                ticket,
                stream: reply,
            });
        }
        #[cfg(test)]
        if let Some(registered) = &self.hooks.registered {
            registered();
        }
        let kept = {
            let mut log = lock(&self.log);
            #[cfg(test)]
            if let Some(acquired) = &self.hooks.acquired {
                acquired();
            }
            {
                let mut candidate = lock(&self.candidate);
                if candidate.as_ref().map(|waiting| waiting.ticket) != Some(ticket) {
                    return;
                }
                candidate.take();
            }
            let mut writer = &stream;
            if log.is_none() {
                // The keeper released everything before this candidate asked.
                match write_frame(
                    &mut writer,
                    &FromKeeper::Handover {
                        running: Vec::new(),
                    },
                ) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            let Some(jobs) = lock(&self.supervisor).hand_over() else {
                return;
            };
            let Ok(kept) = self.snapshot(&jobs) else {
                return;
            };
            // The crash's bound ends with the crash: move its generation on
            // and wake the watcher.
            self.bound.fetch_add(1, Ordering::AcqRel);
            if let Some(parker) = lock(&self.watcher).take() {
                parker.bump();
            }
            self.release(&mut log);
            let Ok(reply) = stream.try_clone() else {
                return;
            };
            *lock(&self.out) = reply;
            match write_frame(
                &mut writer,
                &FromKeeper::Handover {
                    running: kept.clone(),
                },
            ) {
                Ok(()) | Err(_) => {}
            }
            kept
        };
        // Until the session says it is ready, the jobs keep writing their
        // files. A session that goes away first is a crash again.
        let ready = matches!(
            crate::keeper::read_request(&mut reader),
            Ok(Some(ToKeeper::Adopted(true)))
        );
        let result = if ready {
            // No runtime or job lock is held here: the handover's log was
            // released above, and the switch takes the locks itself.
            #[cfg(test)]
            if let Some(pause) = &self.hooks.before_switch {
                pause();
            }
            self.switch(&kept);
            self.send(FromKeeper::AdoptDone(true))
                .and_then(|()| self.control_from(reader))
        } else {
            Ok(())
        };
        match result.and_then(|()| self.crashed()) {
            Ok(()) => {}
            Err(error) => match self.done.send(Err(error)) {
                Ok(()) | Err(_) => {}
            },
        }
    }

    /// The jobs to hand over, with the files each one keeps writing.
    fn snapshot(&self, jobs: &[(String, contract::JobId)]) -> io::Result<Vec<KeptJob>> {
        let processes = lock(&self.processes);
        jobs.iter()
            .map(|(id, job_id)| {
                let process = processes
                    .get(id)
                    .ok_or_else(|| io::Error::other("an adopted job has no process"))?;
                let state = lock(&process.state);
                let adoption = state
                    .adoption
                    .as_ref()
                    .ok_or_else(|| io::Error::other("an adopted job has no files"))?;
                Ok(KeptJob {
                    id: id.clone(),
                    job_id: job_id.clone(),
                    output_path: adoption.output.clone(),
                    errors_path: adoption.errors.clone(),
                    tty: process.input.is_some(),
                    lines: adoption.lines,
                })
            })
            .collect()
    }

    /// Each job's reader takes the same state lock for every write, so
    /// every byte read before this is in the file and every one after goes
    /// out as a frame.
    fn switch(&self, kept: &[KeptJob]) {
        let processes = lock(&self.processes);
        for job in kept {
            if let Some(process) = processes.get(&job.id) {
                let mut state = lock(&process.state);
                if matches!(state.sink, Sink::Files { .. }) {
                    state.sink = Sink::Control;
                }
            }
        }
    }

    fn control(self: &Arc<Self>, control: UnixStream) -> io::Result<()> {
        self.control_from(BufReader::new(control))
    }

    /// Serves a session's control connection until it ends. The reader may
    /// already hold bytes read ahead of the handshake.
    fn control_from(self: &Arc<Self>, mut reader: BufReader<UnixStream>) -> io::Result<()> {
        let mut chunk = Vec::new();
        loop {
            chunk.clear();
            match reader.read_until(b'\n', &mut chunk) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if disconnected(&error) => break,
                Err(error) => return Err(error),
            }
            // A chunk without the framing newline is a torn tail: the session
            // died mid-write. Break and proceed to takeover, never parse it.
            if chunk.last() != Some(&b'\n') {
                break;
            }
            chunk.pop();
            let line = String::from_utf8(std::mem::take(&mut chunk))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let request: ToKeeper = serde_json::from_str(&line)?;
            match request {
                ToKeeper::Start(spec) => self.start(spec)?,
                ToKeeper::Stop(id) => {
                    if let Some(process) = lock(&self.processes).get(&id) {
                        self.stop(process);
                    }
                }
                ToKeeper::Write { id, text } => {
                    if let Some(process) = lock(&self.processes).get(&id)
                        && let Some(input) = &process.input
                    {
                        (input.0)(
                            text.as_bytes(),
                            &*self.clock,
                            &support::cancel::Token::new(),
                        )?;
                    }
                }
                ToKeeper::Adopt {
                    id,
                    job_id,
                    output_path,
                    errors_path,
                    lines,
                } => {
                    let _log = lock(&self.log);
                    if lock(&self.supervisor).adopt(&id, job_id.clone())
                        && let Some(process) = lock(&self.processes).get(&id)
                    {
                        lock(&process.state).adoption = Some(Adoption {
                            output: output_path,
                            errors: errors_path,
                            lines,
                        });
                    }
                }
                ToKeeper::Confirmed(id) => {
                    self.decide(|sup| sup.confirmed(&id))?;
                }
                ToKeeper::Shutdown(true) => {
                    self.discard();
                    self.decide(Supervisor::shutdown_requested)?;
                }
                ToKeeper::Shutdown(false) => return Err(io::Error::other("shutdown must be true")),
                // Only a candidate on the keeper's socket may negotiate; the
                // session's own control connection never does.
                ToKeeper::Identify(_) | ToKeeper::Handover(_) | ToKeeper::Adopted(_) => {
                    return Err(io::Error::other("handshake frame on the session's control"));
                }
            }
        }
        Ok(())
    }
}

fn write_sink(sink: &mut Sink, stream: Stream, bytes: &[u8]) -> io::Result<()> {
    if let Sink::Files { output, errors } = sink {
        if stream == Stream::Err
            && let Some(errors) = errors
        {
            return errors.write_all(bytes);
        }
        output.write_all(bytes)?;
    }
    Ok(())
}

pub(crate) fn serve(
    home: PathBuf,
    id: SessionId,
    clock: Arc<dyn Clock>,
    lifeline: impl Read,
    control: UnixStream,
) -> io::Result<()> {
    let (done, result) = mpsc::channel();
    let runtime = Runtime::new(
        home,
        id,
        clock,
        control.try_clone()?,
        done,
        #[cfg(test)]
        Hooks::default(),
    );
    drive(&runtime, lifeline, control, &result)
}

/// Serves the session's control connection until its lifeline ends, takes
/// the crash's decision, then waits for the keeper's ending.
fn drive(
    runtime: &Arc<Runtime>,
    mut lifeline: impl Read,
    control: UnixStream,
    result: &mpsc::Receiver<io::Result<()>>,
) -> io::Result<()> {
    let reader = Arc::clone(runtime);
    let control = thread::spawn(move || reader.control(control));
    let mut bytes = [0; 64];
    loop {
        match lifeline.read(&mut bytes) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    runtime.hold_output();
    control
        .join()
        .map_err(|_| io::Error::other("keeper control reader panicked"))??;
    runtime.takeover()?;
    result
        .recv()
        .map_err(|_| io::Error::other("keeper ending lost"))?
}

fn write_frame(writer: &mut impl Write, line: &FromKeeper) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(line)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)
}

pub(crate) fn run(id: String, clock: Arc<dyn Clock>) -> i32 {
    let home = match config::fiber_home(std::env::var_os("FIBER_HOME"), std::env::var_os("HOME")) {
        Ok(home) => home,
        Err(_) => return 1,
    };
    let control = match rustix::io::dup(std::io::stdout()) {
        Ok(fd) => UnixStream::from(fd),
        Err(_) => return 1,
    };
    match serve(home, SessionId(id), clock, std::io::stdin().lock(), control) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[cfg(test)]
#[path = "keeper_child_tests.rs"]
mod tests;
