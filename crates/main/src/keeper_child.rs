//! The keeper process (`docs/tools.md`, "The keeper"). No providers or hooks.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::Instant;

use base64::Engine as _;
use contract::SessionId;
use contract::clock::{Clock, Wake};
use contract::events::EventKind;
use contract::keeper::{Exit, FromKeeper, Spawn, Stream, ToKeeper};
use jobs::keeper::{Step, Supervisor};
use log::Log;
use rustix::process::Signal;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

enum Sink {
    Control,
    Hold(Vec<(Stream, Vec<u8>)>),
    Files { output: File, errors: Option<File> },
    Discard,
}
struct Adoption {
    output: PathBuf,
    errors: Option<PathBuf>,
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
// the label and releases the session lock (docs/state.md, "What each part holds").
struct HeldLog {
    socket: PathBuf,
    log: Log,
}

impl Drop for HeldLog {
    fn drop(&mut self) {
        std::fs::remove_file(&self.socket).unwrap_or(());
    }
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
    fn send(&self, line: FromKeeper) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(&line)?;
        bytes.push(b'\n');
        match lock(&self.out).write_all(&bytes) {
            Err(error) if disconnected(&error) => Ok(()),
            result => result,
        }
    }

    fn decide(&self, choose: impl FnOnce(&mut Supervisor) -> Vec<Step>) -> io::Result<()> {
        let mut log = lock(&self.log);
        let steps = choose(&mut lock(&self.supervisor));
        self.apply(&mut log, steps)
    }

    fn apply(&self, log: &mut Option<HeldLog>, steps: Vec<Step>) -> io::Result<()> {
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
                    match Log::open_as(
                        sessions,
                        self.id.clone(),
                        Arc::clone(&self.clock),
                        &format!("keeper {}", std::process::id()),
                    ) {
                        Ok(held) => {
                            let completed = log::read(&dir)
                                .map_err(|error| io::Error::other(error.to_string()))?;
                            *lock(&self.logged) = completed
                                .into_iter()
                                .filter(|line| {
                                    EventKind::from_name(&line.kind) == EventKind::JobCompleted
                                })
                                .filter_map(|line| {
                                    line.payload
                                        .get("job_id")
                                        .and_then(|value| value.as_str())
                                        .map(str::to_owned)
                                })
                                .collect();
                            let (socket, listener) = doors::bind(&self.home, &dir)
                                .map_err(|error| io::Error::other(error.to_string()))?;
                            *log = Some(HeldLog { socket, log: held });
                            thread::spawn(move || {
                                for accepted in listener.incoming() {
                                    match accepted {
                                        Ok(stream) => drop(stream),
                                        Err(_) => break,
                                    }
                                }
                            });
                            self.files()?;
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
                Step::Exit => {
                    self.discard();
                    *log = None;
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
                match runtime.send(FromKeeper::Exit {
                    id: spec.id.clone(),
                    exit: exit.clone(),
                }) {
                    Ok(()) | Err(_) => {}
                }
                runtime.decide(|sup| sup.exited(&spec.id, exit))
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

    fn control(self: &Arc<Self>, control: UnixStream) -> io::Result<()> {
        let mut reader = BufReader::new(control);
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
                } => {
                    let _log = lock(&self.log);
                    if lock(&self.supervisor).adopt(&id, job_id.clone())
                        && let Some(process) = lock(&self.processes).get(&id)
                    {
                        lock(&process.state).adoption = Some(Adoption {
                            output: output_path,
                            errors: errors_path,
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
    mut lifeline: impl Read,
    control: UnixStream,
) -> io::Result<()> {
    let (done, result) = mpsc::channel();
    let runtime = Arc::new(Runtime {
        home,
        id,
        clock,
        out: Mutex::new(control.try_clone()?),
        log: Mutex::new(None),
        supervisor: Mutex::new(Supervisor::new()),
        processes: Mutex::new(BTreeMap::new()),
        logged: Mutex::new(HashSet::new()),
        done,
    });
    let reader = Arc::clone(&runtime);
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
    for process in lock(&runtime.processes).values() {
        let mut state = lock(&process.state);
        if matches!(state.sink, Sink::Control) {
            state.sink = Sink::Hold(Vec::new());
        }
    }
    control
        .join()
        .map_err(|_| io::Error::other("keeper control reader panicked"))??;
    runtime.decide(Supervisor::lifeline_eof)?;
    result
        .recv()
        .map_err(|_| io::Error::other("keeper ending lost"))?
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
