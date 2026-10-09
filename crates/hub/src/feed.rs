//! The feed (`docs/invocation.md`, "The hub"): the hub holds a `summary`
//! connection to every running session it finds in `run/`, and serves each
//! feed subscriber the latest `session_status` of every running or waiting
//! top-level session, then every change, then `session_left` when one ends.
//!
//! The summary lines also drive `attention` (`crate::attention`): a status
//! that turns to `waiting`, or a turn end that turns the session `idle`,
//! is told to every connection's attention listener.
//!
//! The hub rescans `run/` every [`RUN_SCAN`] on its clock. On a summary
//! connection's end it reads the session log's last line: `fiber_exited` or
//! `rewound` is `exited`, anything else `crashed`, and it appends a crashed
//! session's `recent.jsonl` row, since a process that died cannot. A
//! crashed session stays until resumed or dismissed; one that exited
//! waiting on a person stays until resumed. A delegate, whose status names
//! a `parent`, is never in the feed.
//!
//! Each subscriber has its own channel and writer thread. The registry
//! lock is held only to update entries and queue lines, never across a
//! socket write, so one slow or dead client blocks no other.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::events::SessionStatus;
use contract::{CommandId, ErrorCode, SessionId};
use serde_json::{Map, Value};

use crate::attention::Attention;
use crate::connection::{Hub, accept_result, reject, send as send_line};
use crate::recent::{self, Left, PageError};

pub(super) mod follow;
mod settle;

use follow::status_line;

/// How often the hub rescans `run/` for new sessions.
pub(crate) const RUN_SCAN: Duration = Duration::from_millis(500);

/// The most of a log's end read to find its last line: `fiber_exited` and
/// `rewound` are far shorter, so a longer last line is neither.
pub(crate) const TAIL: u64 = 64 * 1024;

/// One line queued for a subscriber.
pub(crate) type Line = Arc<[u8]>;

/// A rejected feed command: its code and sentence.
pub(crate) type Refusal = (ErrorCode, String);

/// Called at the end of every scan of `run/` with the names it found.
pub(crate) type ScanHook = Box<dyn Fn(&BTreeSet<String>) + Send + Sync>;

/// The feed: its registry, the scanner and the summary connections.
pub(crate) struct Feed {
    home: PathBuf,
    clock: Arc<dyn Clock>,
    pub(crate) attention: Attention,
    state: Mutex<State>,
    tick: Arc<Tick>,
    /// `tick` as the clock's subscriber: kept alive so advances wake the
    /// scanner.
    _wake: Arc<dyn Wake>,
    scanner: Mutex<Option<JoinHandle<()>>>,
    /// Starts the session a `rewound` last line names, set once by the hub
    /// serving it, so a rewind no client relays still starts it.
    pub(crate) on_rewound: OnceLock<Box<dyn Fn(SessionId, SessionId) + Send + Sync>>,
    /// Called at the end of every scan of `run/` with the names it found,
    /// set once by the hub serving it: the rejoin sweep collects resumed
    /// sessions there. Never under the state lock.
    pub(crate) on_scan: OnceLock<ScanHook>,
    #[cfg(test)]
    settle_pause: Mutex<Option<SettlePause>>,
}

#[cfg(test)]
pub(super) struct SettlePause {
    pub(super) arrived: mpsc::Sender<()>,
    pub(super) release: mpsc::Receiver<()>,
}

#[derive(Default)]
struct State {
    stopped: bool,
    entries: BTreeMap<String, Entry>,
    /// Sessions with a live summary connection, and a handle to shut it.
    tracked: BTreeMap<String, UnixStream>,
    /// Sessions whose status named a parent, while their socket is in
    /// `run/`: never connected again.
    delegates: BTreeSet<String>,
    subscribers: Vec<Subscriber>,
    next_subscriber: u64,
    /// Summary and finished subscriber threads, joined at stop or once done.
    threads: Vec<JoinHandle<()>>,
    /// Whether the first scan of `run/` has finished.
    scanned: bool,
    /// Sessions the first scan followed that have sent no status yet.
    awaited: BTreeSet<String>,
}

impl State {
    /// The sessions among `names` to connect to: neither followed already
    /// nor a delegate.
    fn fresh(&self, names: &BTreeSet<String>) -> Vec<String> {
        names
            .iter()
            .filter(|id| !self.tracked.contains_key(*id) && !self.delegates.contains(*id))
            .cloned()
            .collect()
    }
}

/// A session in the feed and its latest `session_status`.
enum Entry {
    Running(Status),
    Left(Status, Left),
}

/// A `session_status` line as the session sent it, and its payload.
struct Status {
    line: Line,
    payload: SessionStatus,
}

struct Subscriber {
    id: u64,
    tx: Sender<Line>,
    writer: JoinHandle<()>,
}

impl Feed {
    /// A feed over `home` that has not started: [`Feed::start`] seeds it
    /// and starts the scanner.
    pub(crate) fn new(home: &Path, clock: Arc<dyn Clock>) -> Self {
        let tick = Arc::new(Tick::default());
        let wake: Arc<dyn Wake> = Arc::clone(&tick) as Arc<dyn Wake>;
        clock.subscribe(Arc::downgrade(&wake));
        let attention = Attention::new(Arc::clone(&clock));
        Self {
            home: home.to_path_buf(),
            clock,
            attention,
            state: Mutex::new(State::default()),
            tick,
            _wake: wake,
            scanner: Mutex::new(None),
            on_rewound: OnceLock::new(),
            on_scan: OnceLock::new(),
            #[cfg(test)]
            settle_pause: Mutex::new(None),
        }
    }

    /// Seeds the feed from `recent.jsonl`: each crashed or waiting session
    /// that is not running is in it as it left. Then starts the scanner,
    /// which scans `run/` at once and every [`RUN_SCAN`] after.
    pub(crate) fn start(self: &Arc<Self>) {
        for row in recent::seeds(&self.home) {
            if UnixStream::connect(self.socket(&row.session_id.0)).is_ok() {
                continue;
            }
            let Some(payload) = row.status.clone() else {
                continue;
            };
            let line = status_line(&row, &payload);
            lock(&self.state).entries.insert(
                row.session_id.0.clone(),
                Entry::Left(Status { line, payload }, row.how),
            );
        }
        let feed = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("hub-feed-scan".to_owned())
            .spawn(move || while feed.scan_and_wait() {});
        if let Ok(handle) = spawned {
            *lock(&self.scanner) = Some(handle);
        }
    }

    /// Stops the scanner, shuts every summary connection and ends every
    /// subscriber, joining their threads.
    pub(crate) fn stop(&self) {
        let (tracked, subscribers, threads) = {
            let mut state = lock(&self.state);
            state.stopped = true;
            (
                std::mem::take(&mut state.tracked),
                std::mem::take(&mut state.subscribers),
                std::mem::take(&mut state.threads),
            )
        };
        self.tick.wake();
        if let Some(scanner) = lock(&self.scanner).take() {
            join(scanner);
        }
        for stream in tracked.values() {
            stream.shutdown(Shutdown::Both).unwrap_or(());
        }
        for subscriber in subscribers {
            drop(subscriber.tx);
            join(subscriber.writer);
        }
        threads.into_iter().for_each(join);
    }

    /// Subscribes `writer` to the feed: queues every left session's last
    /// status and its `session_left`, then every running one's status,
    /// then every change. The caller has written `command_accepted`.
    pub(crate) fn subscribe(&self, writer: Arc<Mutex<UnixStream>>) -> Option<u64> {
        let (tx, handle) = spawn_writer(writer, "hub-feed-out")?;
        let mut state = lock(&self.state);
        if state.stopped {
            return None;
        }
        for (id, entry) in &state.entries {
            if let Entry::Left(status, how) = entry {
                send(&tx, &status.line);
                send(&tx, &self.left_line(id, *how));
            }
        }
        for entry in state.entries.values() {
            if let Entry::Running(status) = entry {
                send(&tx, &status.line);
            }
        }
        state.next_subscriber += 1;
        let id = state.next_subscriber;
        state.subscribers.push(Subscriber {
            id,
            tx,
            writer: handle,
        });
        Some(id)
    }

    /// Ends subscriber `id`: no more lines are queued, and its writer
    /// thread is joined once it has written what was queued.
    pub(crate) fn unsubscribe(&self, id: u64) {
        let gone = {
            let mut state = lock(&self.state);
            let at = state.subscribers.iter().position(|sub| sub.id == id);
            at.map(|at| state.subscribers.remove(at))
        };
        if let Some(subscriber) = gone {
            drop(subscriber.tx);
            join(subscriber.writer);
        }
    }

    /// How many subscribers the feed holds.
    #[cfg(test)]
    pub(crate) fn subscribers(&self) -> usize {
        lock(&self.state).subscribers.len()
    }

    /// `dismiss`: drops a crashed session from the feed.
    pub(crate) fn dismiss(&self, args: &Map<String, Value>) -> Result<Option<Value>, Refusal> {
        let session = match (args.len(), args.get("session")) {
            (1, Some(Value::String(session))) => session,
            _ => return Err(invalid()),
        };
        let mut state = lock(&self.state);
        if matches!(
            state.entries.get(session),
            Some(Entry::Left(_, Left::Crashed))
        ) {
            state.entries.remove(session);
            Ok(None)
        } else {
            Err((
                ErrorCode::StaleRequest,
                format!("`{session}` is not a crashed session in the feed."),
            ))
        }
    }

    /// Drops any entry for `session`: its directory was deleted.
    pub(crate) fn forget(&self, session: &str) {
        lock(&self.state).entries.remove(session);
        self.attention.forget(session);
    }

    /// `recent`: a page of exited sessions, newest first.
    pub(crate) fn recent(&self, args: &Map<String, Value>) -> Result<Option<Value>, Refusal> {
        let text = |key: &str| match args.get(key) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.as_str())),
            Some(_) => Err(invalid()),
        };
        let before = text("before")?;
        let project = text("project")?;
        if args.keys().any(|key| key != "before" && key != "project") {
            return Err(invalid());
        }
        let running = lock(&self.state)
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, Entry::Running(_)))
            .map(|(id, _)| id.clone())
            .collect();
        match recent::page(&self.home, before, project, &running) {
            Ok(rows) => Ok(Some(serde_json::json!({ "sessions": rows }))),
            Err(PageError::UnknownBefore) => Err((
                ErrorCode::InvalidArguments,
                "`before` names no session in the list.".to_owned(),
            )),
        }
    }
}

/// `feed`: accepted, then this connection's subscription, replacing any
/// earlier one, so the snapshot follows the acknowledgement.
pub(crate) fn on_feed(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    fed: &mut Option<u64>,
) {
    if !args.is_empty() {
        answer(writer, hub, id, Err(invalid()));
        return;
    }
    answer(writer, hub, id, Ok(None));
    if let Some(earlier) = fed.take() {
        hub.feed.unsubscribe(earlier);
    }
    *fed = hub.feed.subscribe(Arc::clone(writer));
}

/// Acknowledges a feed command: `command_accepted`, with `result` only
/// when the command has one (`docs/events.md`, "`command_accepted`").
pub(crate) fn answer(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: &CommandId,
    got: Result<Option<Value>, Refusal>,
) {
    match got {
        Ok(Some(result)) => accept_result(writer, hub, id, result),
        Ok(None) => {
            let payload = Map::from_iter([("command_id".to_owned(), Value::String(id.0.clone()))]);
            send_line(writer, hub, "command_accepted", payload);
        }
        Err((code, message)) => reject(writer, hub, Some(id), &code, &message),
    }
}

/// A channel whose lines a new thread named `name` writes to `writer`, each flushed;
/// the thread ends on the first failed write or once every sender is dropped.
pub(crate) fn spawn_writer(
    writer: Arc<Mutex<UnixStream>>,
    name: &str,
) -> Option<(Sender<Line>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Line>();
    let handle = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            for line in rx {
                let mut out = lock(&writer);
                if out.write_all(&line).and_then(|()| out.flush()).is_err() {
                    return;
                }
            }
        })
        .ok()?;
    Some((tx, handle))
}

pub(crate) fn to_line(value: &impl serde::Serialize) -> Line {
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    Arc::from(bytes)
}

/// Queues `line` for every subscriber; one whose writer ended is dropped
/// and its thread kept for joining.
fn broadcast(state: &mut State, line: &Line) {
    let (live, dead): (Vec<_>, Vec<_>) = std::mem::take(&mut state.subscribers)
        .into_iter()
        .partition(|sub| sub.tx.send(Arc::clone(line)).is_ok());
    state.subscribers = live;
    state.threads.extend(dead.into_iter().map(|sub| sub.writer));
}

fn send(tx: &Sender<Line>, line: &Line) {
    // A writer that already ended is dropped at the next broadcast.
    tx.send(Arc::clone(line)).unwrap_or(());
}

pub(crate) fn invalid() -> Refusal {
    (
        ErrorCode::InvalidArguments,
        "The arguments do not fit this command.".to_owned(),
    )
}

fn join(handle: JoinHandle<()>) {
    match handle.join() {
        Ok(()) | Err(_) => {}
    }
}

/// Woken on every clock move and at stop.
#[derive(Default)]
struct Tick {
    held: Mutex<()>,
    moved: Condvar,
    /// Told once of the next wake: when it finds `held` taken, or else
    /// once its notify has returned.
    #[cfg(test)]
    attempt: Mutex<Option<Sender<()>>>,
}

impl Wake for Tick {
    fn wake(&self) {
        #[cfg(test)]
        let attempt = self.tell_if_contended();
        // Taken before the notify, so a scanner that has checked and not
        // yet parked cannot miss it.
        let held = lock(&self.held);
        self.moved.notify_all();
        drop(held);
        #[cfg(test)]
        if let Some(attempt) = attempt {
            attempt.send(()).unwrap_or(());
        }
    }
}

#[cfg(test)]
impl Tick {
    /// Tells the armed sender at once when `held` is taken, and otherwise
    /// returns it to be told after the notify.
    fn tell_if_contended(&self) -> Option<Sender<()>> {
        let attempt = lock(&self.attempt).take()?;
        if matches!(
            self.held.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ) {
            attempt.send(()).unwrap_or(());
            return None;
        }
        Some(attempt)
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
