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
//! connection's end it reads the session log's last line: `fiber_exited`,
//! `rewound` or `moved` is `exited`, anything else `crashed`, and it
//! appends a crashed
//! session's `recent.jsonl` row, since a process that died cannot. A
//! crashed session stays until resumed or dismissed; one that exited
//! waiting on a person stays until resumed. A delegate, whose status names
//! a `parent`, is never in the feed.
//!
//! Each subscriber has its own channel and writer thread (`fanout`). The
//! registry lock is held only to update entries and queue lines, never
//! across a socket write, so one slow or dead client blocks no other.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::BufReader;
#[cfg(test)]
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use contract::clock::{Clock, Wake, wall_ms};
use contract::events::{SessionState, SessionStatus};
use contract::{CommandId, Envelope, ErrorCode, HubLine, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};
use support::lock;

use crate::attention::{Attention, Seen};
use crate::connection::{Hub, Refused, accept_result, reject, send as send_line};
use crate::recent::{self, Left, PageError, RecentRow};
use crate::relay::valid_session_id;
use crate::tick::Tick;

mod fanout;
mod settle;
use settle::Settle;

/// The bound, for tests: production capacity lives in [`spawn_writer`].
#[cfg(test)]
pub(crate) use fanout::QUEUE;
pub(crate) use fanout::{
    Closer, Fanout, Line, Targeted, WriterEnd, close_connections, spawn_writer, to_line,
};

/// How often the hub rescans `run/` for new sessions.
pub(crate) const RUN_SCAN: Duration = Duration::from_millis(500);

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
    /// Starts the session a `rewound` or `moved` last line names, set once
    /// by the hub serving it, so a transition no client relays still
    /// starts it.
    pub(crate) on_rewound: OnceLock<Box<dyn Fn(SessionId, SessionId) + Send + Sync>>,
    /// Called at the end of every scan of `run/` with the names it found,
    /// set once by the hub serving it: the rejoin sweep collects resumed
    /// sessions there. Never under the state lock.
    pub(crate) on_scan: OnceLock<ScanHook>,
    /// Skips the `shutdown` in [`Feed::stop`]: with the stoppable read
    /// wired, the stop's pipe ends a silent reader on its own.
    #[cfg(test)]
    pub(super) skip_shutdown: AtomicBool,
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
    /// Sessions with a live summary connection: the stream to shut, and
    /// the stop that ends its reader.
    tracked: BTreeMap<String, Tracked>,
    /// Sessions whose status named a parent, while their socket is in
    /// `run/`: never connected again.
    delegates: BTreeSet<String>,
    fanout: Fanout,
    /// Summary reader threads and disconnected writers parked for
    /// joining, joined at stop or once done.
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

/// A followed session's summary connection: the stream to shut, and the
/// stop that ends its reader. The reader holds the stream's other clone
/// through its own stoppable wrapper, and is its only reader.
struct Tracked {
    stream: UnixStream,
    stop: support::stoppable::Stop,
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
            skip_shutdown: AtomicBool::new(false),
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

    /// Stops the scanner, stops every summary reader and shuts every
    /// summary connection, then ends every subscriber, joining their
    /// threads. Every tracked `Stop` is stopped before the first join of a
    /// summary reader or a subscriber writer.
    pub(crate) fn stop(&self) {
        let (tracked, fanout, threads) = {
            let mut state = lock(&self.state);
            state.stopped = true;
            (
                std::mem::take(&mut state.tracked),
                std::mem::take(&mut state.fanout),
                std::mem::take(&mut state.threads),
            )
        };
        self.tick.wake();
        if let Some(scanner) = lock(&self.scanner).take() {
            join(scanner);
        }
        for tracked in tracked.values() {
            tracked.stop.stop();
        }
        #[cfg(test)]
        let skipped = self.skip_shutdown.load(Ordering::Relaxed);
        for tracked in tracked.values() {
            #[cfg(test)]
            if skipped {
                continue;
            }
            tracked.stream.shutdown(Shutdown::Both).unwrap_or(());
        }
        fanout.close().into_iter().for_each(|end| {
            close_connections(vec![end.closer], UnixStream::shutdown);
            if let Some(writer) = end.writer {
                join(writer);
            }
        });
        self.attention.stop();
        threads.into_iter().for_each(join);
    }

    /// Subscribes `writer` to the feed: queues every left session's last
    /// status and its `session_left`, then every running one's status,
    /// then every change. The caller has written `command_accepted`.
    /// `closer` shuts the connection down; its retained clone frees a
    /// retired writer. Both are cloned before admission, never at cleanup.
    /// A stopped feed subscribes nothing and spawns no writer.
    pub(crate) fn subscribe(&self, writer: Arc<Mutex<UnixStream>>, closer: Closer) -> Option<u64> {
        let retired = Closer {
            stream: closer.stream.try_clone().ok()?,
            web: closer.web.clone(),
        };
        let mut state = lock(&self.state);
        if state.stopped {
            return None;
        }
        let spawned = spawn_writer(writer, "hub-feed-out")?;
        let snapshot = self.snapshot(&state);
        Some(state.fanout.push(spawned, closer, retired, snapshot))
    }

    /// The current snapshot as one line: every left session's last
    /// status and its `session_left`, then every running one's status,
    /// each group in map order. One queue item however many sessions it
    /// holds, never truncated: an oversized snapshot still subscribes.
    fn snapshot(&self, state: &State) -> Line {
        let mut bytes = Vec::new();
        for (id, entry) in &state.entries {
            if let Entry::Left(status, how) = entry {
                bytes.extend_from_slice(&status.line);
                bytes.extend_from_slice(&self.left_line(id, *how));
            }
        }
        for entry in state.entries.values() {
            if let Entry::Running(status) = entry {
                bytes.extend_from_slice(&status.line);
            }
        }
        Arc::from(bytes)
    }

    /// Re-queues the current snapshot for the existing `id` registration,
    /// behind its command acknowledgement. Hands back the Full shutdown
    /// when its queue is full, for the caller to run outside the state
    /// lock; anything else leaves no cleanup behind. A disconnected
    /// writer's handle parks with the summary threads, joined at the next
    /// scan or at stop.
    fn replace_snapshot(&self, id: u64) -> Vec<Closer> {
        let mut state = lock(&self.state);
        if state.stopped {
            return Vec::new();
        }
        let snapshot = self.snapshot(&state);
        match state.fanout.send_to(id, &snapshot) {
            Targeted::Sent | Targeted::Gone => Vec::new(),
            Targeted::Full(closer) => vec![closer],
            Targeted::DeadWriter(writer) => {
                state.threads.push(writer);
                Vec::new()
            }
        }
    }

    /// Ends subscriber `id`: no more lines are queued; its connection is
    /// shut so a writer blocked on a stalled reader ends at once, and its
    /// thread is joined outside the state lock.
    pub(crate) fn unsubscribe(&self, id: u64) {
        let gone = lock(&self.state).fanout.remove(id);
        if let Some(end) = gone {
            close_connections(vec![end.closer], UnixStream::shutdown);
            if let Some(writer) = end.writer {
                join(writer);
            }
        }
    }

    /// How many subscribers the feed holds.
    #[cfg(test)]
    pub(crate) fn subscribers(&self) -> usize {
        lock(&self.state).fanout.ids().len()
    }

    /// `dismiss`: drops a crashed session from the feed.
    pub(crate) fn dismiss(&self, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
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
            Err(Refused {
                code: ErrorCode::StaleRequest,
                message: format!("`{session}` is not a crashed session in the feed."),
            })
        }
    }

    /// Drops any entry for `session`: its directory was deleted.
    pub(crate) fn forget(&self, session: &str) {
        lock(&self.state).entries.remove(session);
        self.attention.forget(session);
    }

    /// `recent`: a page of exited sessions, newest first.
    pub(crate) fn recent(&self, args: &Map<String, Value>) -> Result<Option<Value>, Refused> {
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
        match recent::page_with_total(&self.home, before, project, &running) {
            Ok((rows, total)) => Ok(Some(
                serde_json::json!({ "sessions": rows, "total": total }),
            )),
            Err(PageError::UnknownBefore) => Err(Refused {
                code: ErrorCode::InvalidArguments,
                message: "`before` names no session in the list.".to_owned(),
            }),
        }
    }

    /// One scan of `run/`, then a wait of [`RUN_SCAN`] on the clock.
    /// False once the feed stopped.
    fn scan_and_wait(self: &Arc<Self>) -> bool {
        self.scan();
        let until = self.clock.now().checked_add(RUN_SCAN);
        // Taken before the checks and held into the park: a change that
        // lands in between wakes this thread instead of nobody.
        let mut guard = self.tick.hold();
        loop {
            if lock(&self.state).stopped {
                return false;
            }
            if until.is_none_or(|until| self.clock.now() >= until) {
                return true;
            }
            let Some(held) = self.tick.park(self.clock.as_ref(), guard, until) else {
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
        let (fresh, done, ended) = {
            let mut state = lock(&self.state);
            state.delegates.retain(|id| names.contains(id));
            let (done, live) = std::mem::take(&mut state.threads)
                .into_iter()
                .partition(JoinHandle::is_finished);
            state.threads = live;
            let ended = state.fanout.finished();
            (state.fresh(&names), done, ended)
        };
        done.into_iter().for_each(join);
        ended.into_iter().for_each(join);
        for id in fresh {
            self.follow(id);
        }
        self.scanned();
        if let Some(on_scan) = self.on_scan.get() {
            on_scan(&names);
        }
    }

    /// Subscribes `summary` to session `id` and follows it on a thread. A
    /// connect that fails is not a crash: the next scan tries again.
    fn follow(self: &Arc<Self>, id: String) {
        let Ok(stream) = UnixStream::connect(self.socket(&id)) else {
            return;
        };
        // Where the log ended once connected: any earlier run's exit
        // line is already before that point, and the followed run's
        // `fiber_exited` or `rewound`, if it writes one, comes after.
        // After the connect so a stale socket costs no directory scan,
        // and before any write, so the followed run's exit cannot land
        // before it. The end of the last complete line, never the raw
        // length: a resumed writer truncates a torn tail.
        let found = recent::lookup(&self.home, &SessionId(id.clone())).map(|(project, dir)| {
            let from = log::complete_len_of(&dir);
            (project, dir, from)
        });
        let Ok(shutdown) = stream.try_clone() else {
            return;
        };
        let Ok((read, stop)) = support::stoppable::reader(stream) else {
            return;
        };
        // A fresh id per subscribe: a session keeps every accepted
        // command id, so a repeated id is rejected `duplicate_command`.
        // The `c_hub_feed_` prefix keeps the feed's own connection
        // distinct from a relay rejoin.
        let subscribe = format!(
            "{{\"id\":\"{}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}",
            crate::start::mint("c_hub_feed_")
        );
        if support::framing::write_line(&mut read.get_ref(), subscribe.as_bytes()).is_err() {
            return;
        }
        let mut state = lock(&self.state);
        // Only the scanner adds to `tracked`, so the scan's check still
        // holds here.
        if state.stopped {
            return;
        }
        let feed = Arc::clone(self);
        let session = id.clone();
        let spawned = thread::Builder::new()
            .name("hub-feed-session".to_owned())
            .spawn(move || feed.read_session(&session, read, found));
        if let Ok(handle) = spawned {
            if !state.scanned {
                state.awaited.insert(id.clone());
            }
            state.tracked.insert(
                id,
                Tracked {
                    stream: shutdown,
                    stop,
                },
            );
            state.threads.push(handle);
        }
    }

    /// Reads session `id`'s summary lines until its socket closes or its
    /// stop ends the read.
    fn read_session(
        &self,
        id: &str,
        read: support::stoppable::Reader,
        found: Option<(String, PathBuf, u64)>,
    ) {
        let dir = found.as_ref().map(|(_, dir, _)| dir.clone());
        let mut read = BufReader::new(read);
        loop {
            match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
                Ok(None) | Err(_) => break,
                Ok(Some(mut line)) => {
                    // `snapshot` concatenates stored lines with no separator,
                    // so every line kept ends in exactly one LF.
                    line.push(b'\n');
                    if !self.on_line(id, &line, dir.as_deref()) {
                        read.get_ref()
                            .get_ref()
                            .shutdown(Shutdown::Both)
                            .unwrap_or(());
                        return;
                    }
                }
            }
        }
        self.on_left(id, found);
    }

    /// Takes one summary line. False once it shows a delegate.
    fn on_line(&self, id: &str, bytes: &[u8], dir: Option<&Path>) -> bool {
        let Some(payload) = parse_status(bytes) else {
            return true;
        };
        let _settle = Settle(self, id);
        // The unseen-turn probe, with no lock held: only a non-delegate `idle`
        // with a known directory and no live entry reads the log.
        let unseen = if !may_be_unseen(&payload) {
            false
        } else if let Some(dir) = dir {
            let live = matches!(lock(&self.state).entries.get(id), Some(Entry::Running(_)));
            !live && self.attention.ended_unseen(dir, &payload)
        } else {
            false
        };
        let mut state = lock(&self.state);
        if state.stopped {
            return true;
        }
        if recent::is_delegate(&payload) {
            state.tracked.remove(id);
            state.delegates.insert(id.to_owned());
            return false;
        }
        let seen = match state.entries.get(id) {
            Some(Entry::Running(status)) => Some(Seen {
                status: &status.payload,
                live: true,
            }),
            Some(Entry::Left(status, _)) => Some(Seen {
                status: &status.payload,
                live: false,
            }),
            None => None,
        };
        let mut closers = self.attention.notify(id, seen, &payload, unseen);
        let line: Line = Arc::from(bytes);
        closers.extend(state.fanout.broadcast(&line));
        state
            .entries
            .insert(id.to_owned(), Entry::Running(Status { line, payload }));
        drop(state);
        // Shut outside the state lock, which the attention listeners lock
        // nests inside: a shutdown unblocks a writer holding its mutex.
        close_connections(closers, UnixStream::shutdown);
        true
    }

    /// Session `id`'s socket closed: decides how it left, tells every
    /// subscriber, and appends a crashed session's row. A session whose
    /// directory is gone was never prompted: it exited, leaving nothing.
    fn on_left(&self, id: &str, found: Option<(String, PathBuf, u64)>) {
        let _settle = Settle(self, id);
        crate::rewind::notify_left(&self.on_rewound, id, &found);
        let how = found
            .as_ref()
            .filter(|(_, dir, _)| log::dir_exists(dir))
            .map(|(_, dir, from)| match how_left(dir) {
                Left::Exited => Left::Exited,
                // Resumed already: the log's last line is the new run's,
                // and this run's exit line is past where it was followed
                // from. A socket that accepts is no sign: a killed
                // process's listener can outlive its summary connection.
                Left::Crashed => {
                    let mut exited = false;
                    let scanned = log::lines_from(dir, *from, |line| {
                        if matches!(
                            log::kind_of(line).as_deref(),
                            Some("fiber_exited" | "rewound" | "moved")
                        ) {
                            exited = true;
                        }
                    });
                    if scanned.is_some() && exited {
                        Left::Exited
                    } else {
                        Left::Crashed
                    }
                }
            });
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
        if let (Some(Left::Crashed), Some((project, _, _))) = (how, &found) {
            self.append_crashed(id, project, status.as_ref());
        }
        let mut state = lock(&self.state);
        if state.stopped {
            return;
        }
        state.tracked.remove(id);
        let mut closers = Vec::new();
        if let Some(Entry::Running(status) | Entry::Left(status, _)) = state.entries.remove(id) {
            let left = how.unwrap_or(Left::Exited);
            let line = self.left_line(id, left);
            closers = state.fanout.broadcast(&line);
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
        drop(state);
        close_connections(closers, UnixStream::shutdown);
    }

    /// Appends crashed session `id`'s row: a process that died cannot.
    fn append_crashed(&self, id: &str, project: &str, status: Option<&SessionStatus>) {
        let row = RecentRow {
            session_id: SessionId(id.to_owned()),
            ts: wall_ms(self.clock.wall()),
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
            ts: wall_ms(self.clock.wall()),
            schema_version: SCHEMA_VERSION,
            payload,
        };
        to_line(&line)
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.home.join("run").join(id)
    }
}

/// `feed`: accepted, then this connection's subscription, replacing any
/// earlier one's snapshot on its live registration, so the fresh snapshot
/// follows the acknowledgement with no new writer. A repeated `feed` whose
/// registration is gone or full closes nothing itself: a gone id stays
/// dropped, and a full queue shuts the whole connection outside the lock.
pub(crate) fn on_feed(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    closer: &Closer,
    fed: &mut Option<u64>,
) {
    if !args.is_empty() {
        answer(writer, hub, id, Err(invalid()));
        return;
    }
    answer(writer, hub, id, Ok(None));
    if let Some(earlier) = fed.take() {
        let closers = hub.feed.replace_snapshot(earlier);
        if closers.is_empty() {
            *fed = Some(earlier);
        } else {
            close_connections(closers, UnixStream::shutdown);
        }
        return;
    }
    let owned = closer.stream.try_clone().ok().map(|stream| Closer {
        stream,
        web: closer.web.clone(),
    });
    *fed = owned.and_then(|owned| hub.feed.subscribe(Arc::clone(writer), owned));
}

/// Acknowledges a feed command: `command_accepted`, with `result` only
/// when the command has one (`docs/events.md`, "`command_accepted`").
pub(crate) fn answer(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: &CommandId,
    got: Result<Option<Value>, Refused>,
) {
    match got {
        Ok(Some(result)) => accept_result(writer, hub, id, result),
        Ok(None) => {
            let payload = Map::from_iter([("command_id".to_owned(), Value::String(id.0.clone()))]);
            send_line(writer, hub, "command_accepted", payload);
        }
        Err(refused) => reject(writer, hub, Some(id), &refused.code, &refused.message),
    }
}

/// How a session whose directory is `dir` left: `exited` when its log's
/// last line is `fiber_exited`, `rewound` or `moved`, otherwise `crashed`.
fn how_left(dir: &Path) -> Left {
    match log::last_kind(dir).as_deref() {
        Some("fiber_exited" | "rewound" | "moved") => Left::Exited,
        _ => Left::Crashed,
    }
}

/// Whether `reason` can read `unseen` for `now`: a non-delegate `idle`.
// `reason` returns None for a delegate and reads `unseen` only for `idle`, so
// a mutant of `&&` to `||` changes no attention, only how often the log is read.
#[cfg_attr(false, mutants::skip)]
fn may_be_unseen(now: &SessionStatus) -> bool {
    matches!(now.state, SessionState::Idle) && now.parent.is_none()
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

pub(crate) fn invalid() -> Refused {
    Refused {
        code: ErrorCode::InvalidArguments,
        message: "The arguments do not fit this command.".to_owned(),
    }
}

/// Test seam: armed once by the `join` test; fires when the joiner is
/// inside [`join`], before it blocks. Compiles to nothing outside tests
/// and never alters control flow. Follows the `pairing.rs` `Pause`
/// precedent: the test arms a hook that signals the code under test has
/// reached the waiting point.
#[cfg(test)]
static JOIN_PAUSE: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn pause_join_in_lock(hook: impl FnOnce() + Send + 'static) {
    *lock(&JOIN_PAUSE) = Some(Box::new(hook));
}

#[cfg(test)]
fn paused_join() {
    // Taken then run without the lock held, as `pairing.rs` does: a
    // blocking hook must not stop another thread arming the seam.
    let hook = lock(&JOIN_PAUSE).take();
    if let Some(hook) = hook {
        hook();
    }
}

fn join(handle: JoinHandle<()>) {
    #[cfg(test)]
    paused_join();
    match handle.join() {
        Ok(()) | Err(_) => {}
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
