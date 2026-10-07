//! The feed (`docs/invocation.md`, "The hub"): the hub holds a `summary`
//! connection to every running session it finds in `run/`, and serves each
//! feed subscriber the latest `session_status` of every running or waiting
//! top-level session, then every change, then `session_left` when one ends.
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
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::events::SessionStatus;
use contract::{Envelope, ErrorCode, HubLine, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

use crate::recent::{self, Left, PageError, RecentRow};
use crate::relay::valid_session_id;

/// How often the hub rescans `run/` for new sessions.
pub(crate) const RUN_SCAN: Duration = Duration::from_millis(500);

/// The most of a log's end read to find its last line: `fiber_exited` and
/// `rewound` are far shorter, so a longer last line is neither.
const TAIL: u64 = 64 * 1024;

/// What the hub sends on each session's socket.
const SUBSCRIBE: &[u8] =
    b"{\"id\":\"c_hub_feed\",\"command\":\"subscribe\",\"args\":{\"level\":\"summary\"}}\n";

/// One line queued for a subscriber.
type Line = Arc<[u8]>;

/// A rejected feed command: its code and sentence.
pub(crate) type Refusal = (ErrorCode, String);

/// The feed: its registry, the scanner and the summary connections.
pub(crate) struct Feed {
    home: PathBuf,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    tick: Arc<Tick>,
    /// `tick` as the clock's subscriber: kept alive so advances wake the
    /// scanner.
    _wake: Arc<dyn Wake>,
    scanner: Mutex<Option<JoinHandle<()>>>,
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
        Self {
            home: home.to_path_buf(),
            clock,
            state: Mutex::new(State::default()),
            tick,
            _wake: wake,
            scanner: Mutex::new(None),
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
        let (tx, rx) = mpsc::channel::<Line>();
        let handle = thread::Builder::new()
            .name("hub-feed-out".to_owned())
            .spawn(move || {
                for line in rx {
                    let mut out = lock(&writer);
                    if out.write_all(&line).and_then(|()| out.flush()).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
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

    /// `dismiss`: drops a crashed session from the feed.
    pub(crate) fn dismiss(&self, args: &Map<String, Value>) -> Result<Value, Refusal> {
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
            Ok(Value::Object(Map::new()))
        } else {
            Err((
                ErrorCode::StaleRequest,
                format!("`{session}` is not a crashed session in the feed."),
            ))
        }
    }

    /// `recent`: a page of exited sessions, newest first.
    pub(crate) fn recent(&self, args: &Map<String, Value>) -> Result<Value, Refusal> {
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
            Ok(rows) => Ok(serde_json::json!({ "sessions": rows })),
            Err(PageError::UnknownBefore) => Err((
                ErrorCode::InvalidArguments,
                "`before` names no session in the list.".to_owned(),
            )),
        }
    }

    /// One scan of `run/`, then a wait of [`RUN_SCAN`] on the clock.
    /// False once the feed stopped.
    fn scan_and_wait(self: &Arc<Self>) -> bool {
        self.scan();
        let until = self.clock.now().checked_add(RUN_SCAN);
        let mut guard = lock(&self.tick.held);
        loop {
            if lock(&self.state).stopped {
                return false;
            }
            if until.is_none_or(|until| self.clock.now() >= until) {
                return true;
            }
            let mut slot = Some(guard);
            self.clock.wait_until(until, &mut |bound| {
                let Some(held) = slot.take() else {
                    return;
                };
                slot = Some(match bound {
                    Some(limit) => {
                        self.tick
                            .moved
                            .wait_timeout(held, limit)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => self
                        .tick
                        .moved
                        .wait(held)
                        .unwrap_or_else(PoisonError::into_inner),
                });
            });
            let Some(held) = slot else {
                return true;
            };
            guard = held;
        }
    }

    /// Connects to every session socket in `run/` not already followed,
    /// and joins the threads that have ended.
    fn scan(self: &Arc<Self>) {
        let names: BTreeSet<String> = fs::read_dir(self.home.join("run"))
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| valid_session_id(name))
                    .collect()
            })
            .unwrap_or_default();
        let (fresh, done) = {
            let mut state = lock(&self.state);
            state.delegates.retain(|id| names.contains(id));
            let (done, live) = std::mem::take(&mut state.threads)
                .into_iter()
                .partition(JoinHandle::is_finished);
            state.threads = live;
            let fresh: Vec<String> = names
                .iter()
                .filter(|id| !state.tracked.contains_key(*id) && !state.delegates.contains(*id))
                .cloned()
                .collect();
            (fresh, done)
        };
        done.into_iter().for_each(join);
        for id in fresh {
            self.follow(id);
        }
    }

    /// Subscribes `summary` to session `id` and follows it on a thread. A
    /// connect that fails is not a crash: the next scan tries again.
    fn follow(self: &Arc<Self>, id: String) {
        let Ok(stream) = UnixStream::connect(self.socket(&id)) else {
            return;
        };
        let Ok(shutdown) = stream.try_clone() else {
            return;
        };
        if (&stream).write_all(SUBSCRIBE).is_err() {
            return;
        }
        let found = recent::find(&self.home, &id);
        let mut state = lock(&self.state);
        if state.stopped || state.tracked.contains_key(&id) {
            return;
        }
        let feed = Arc::clone(self);
        let session = id.clone();
        let spawned = thread::Builder::new()
            .name("hub-feed-session".to_owned())
            .spawn(move || feed.read_session(&session, stream, found));
        if let Ok(handle) = spawned {
            state.tracked.insert(id, shutdown);
            state.threads.push(handle);
        }
    }

    /// Reads session `id`'s summary lines until its socket closes.
    fn read_session(&self, id: &str, stream: UnixStream, found: Option<(String, PathBuf)>) {
        let mut read = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match read.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if !self.on_line(id, &buf) {
                        read.get_ref().shutdown(Shutdown::Both).unwrap_or(());
                        return;
                    }
                }
            }
        }
        self.on_left(id, found);
    }

    /// Takes one summary line. False once it shows a delegate.
    fn on_line(&self, id: &str, bytes: &[u8]) -> bool {
        let Some(payload) = parse_status(bytes) else {
            return true;
        };
        let mut state = lock(&self.state);
        if state.stopped {
            return true;
        }
        if payload.parent.is_some() {
            state.tracked.remove(id);
            state.delegates.insert(id.to_owned());
            return false;
        }
        let line: Line = Arc::from(bytes);
        broadcast(&mut state, &line);
        state
            .entries
            .insert(id.to_owned(), Entry::Running(Status { line, payload }));
        true
    }

    /// Session `id`'s socket closed: decides how it left, tells every
    /// subscriber, and appends a crashed session's row. A session whose
    /// directory is gone was never prompted: it exited, leaving nothing.
    fn on_left(&self, id: &str, found: Option<(String, PathBuf)>) {
        let how = found
            .as_ref()
            .filter(|(_, dir)| dir.is_dir())
            .map(|(_, dir)| how_left(dir));
        // The row is written before `session_left` is sent, so a client
        // that sees the crash finds it in `recent`. `tracked` still holds
        // the session meanwhile, so no scan connects to it again.
        let status = {
            let state = lock(&self.state);
            if state.stopped {
                return;
            }
            state
                .entries
                .get(id)
                .map(|(Entry::Running(status) | Entry::Left(status, _))| status.payload.clone())
        };
        if let (Some(Left::Crashed), Some((project, _))) = (how, &found) {
            self.append_crashed(id, project, status.as_ref());
        }
        let mut state = lock(&self.state);
        if state.stopped {
            return;
        }
        state.tracked.remove(id);
        if let Some(Entry::Running(status) | Entry::Left(status, _)) = state.entries.remove(id) {
            let left = how.unwrap_or(Left::Exited);
            let line = self.left_line(id, left);
            broadcast(&mut state, &line);
            let stays = match how {
                Some(Left::Crashed) => true,
                Some(Left::Exited) => recent::is_waiting(&status.payload),
                None => false,
            };
            if stays {
                state
                    .entries
                    .insert(id.to_owned(), Entry::Left(status, left));
            }
        }
    }

    /// Appends crashed session `id`'s row: a process that died cannot.
    fn append_crashed(&self, id: &str, project: &str, status: Option<&SessionStatus>) {
        let row = RecentRow {
            session_id: SessionId(id.to_owned()),
            ts: crate::diag::wall_ms(self.clock.wall()),
            project: project.to_owned(),
            workspace: status
                .map(|status| status.workspace.clone())
                .unwrap_or_default(),
            name: status.map(|status| status.name.clone()).unwrap_or_default(),
            how: Left::Crashed,
            status: status.cloned(),
        };
        // Losing the row loses only the listing; the log stays.
        recent::append(&self.home, &row).unwrap_or(());
    }

    /// A `session_left` hub line for `id`.
    fn left_line(&self, id: &str, how: Left) -> Line {
        let mut payload = Map::new();
        payload.insert("session_id".to_owned(), Value::String(id.to_owned()));
        payload.insert(
            "how".to_owned(),
            serde_json::to_value(how).unwrap_or(Value::Null),
        );
        let line = HubLine {
            kind: "session_left".to_owned(),
            ts: crate::diag::wall_ms(self.clock.wall()),
            schema_version: SCHEMA_VERSION,
            payload,
        };
        to_line(&line)
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.home.join("run").join(id)
    }
}

/// How a session whose directory is `dir` left: `exited` when its log's
/// last line is `fiber_exited` or `rewound`, otherwise `crashed`.
fn how_left(dir: &Path) -> Left {
    match last_kind(&dir.join("events.jsonl")).as_deref() {
        Some("fiber_exited" | "rewound") => Left::Exited,
        _ => Left::Crashed,
    }
}

/// The `kind` of `log`'s last line, read from at most [`TAIL`] bytes of
/// its end. `None` when it cannot be read or does not parse.
fn last_kind(log: &Path) -> Option<String> {
    let mut file = File::open(log).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
    let mut lines = body.rsplitn(2, |byte| *byte == b'\n');
    let line = lines.next()?;
    // The whole tail is one line only when it is the whole file.
    if lines.next().is_none() && from > 0 {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    value.get("kind")?.as_str().map(str::to_owned)
}

/// The payload of a `session_status` line; `None` for any other line.
fn parse_status(bytes: &[u8]) -> Option<SessionStatus> {
    let line: Envelope = serde_json::from_slice(bytes).ok()?;
    if line.kind != "session_status" {
        return None;
    }
    serde_json::from_value(Value::Object(line.payload)).ok()
}

/// A `session_status` line rebuilt from a `recent.jsonl` row.
fn status_line(row: &RecentRow, payload: &SessionStatus) -> Line {
    let payload = match serde_json::to_value(payload) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    to_line(&Envelope {
        kind: "session_status".to_owned(),
        session_id: row.session_id.clone(),
        ts: row.ts,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

fn to_line(value: &impl serde::Serialize) -> Line {
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
}

impl Wake for Tick {
    fn wake(&self) {
        // Taken before the notify, so a scanner that has checked and not
        // yet parked cannot miss it.
        let _held = lock(&self.held);
        self.moved.notify_all();
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
