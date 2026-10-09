use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::thread::{self, JoinHandle};

use contract::clock::wall_ms;
use contract::events::{SessionState, SessionStatus};
use contract::{Envelope, HubLine, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

use crate::attention::Seen;
use crate::recent::{self, Left, RecentRow};
use crate::relay::valid_session_id;

use super::settle::Settle;
use super::{Entry, Feed, Line, RUN_SCAN, Status, TAIL, broadcast, join, lock, to_line};

impl Feed {
    /// One scan of `run/`, then a wait of [`RUN_SCAN`] on the clock.
    /// False once the feed stopped.
    pub(super) fn scan_and_wait(self: &Arc<Self>) -> bool {
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
            (state.fresh(&names), done)
        };
        done.into_iter().for_each(join);
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
    pub(super) fn follow(self: &Arc<Self>, id: String) {
        // Where the log ended before connecting: any earlier run's exit
        // line is already before that point, and the followed run's
        // `fiber_exited` or `rewound`, if it writes one, comes after.
        let found = recent::find(&self.home, &id).map(|(project, dir)| {
            let from = fs::metadata(dir.join("events.jsonl")).map_or(0, |meta| meta.len());
            (project, dir, from)
        });
        let Ok(stream) = UnixStream::connect(self.socket(&id)) else {
            return;
        };
        let Ok(shutdown) = stream.try_clone() else {
            return;
        };
        // A fresh id per subscribe: a session keeps every accepted
        // command id, so a repeated id is rejected `duplicate_command`.
        // The `c_hub_feed_` prefix keeps the feed's own connection
        // distinct from a relay rejoin.
        let subscribe = format!(
            "{{\"id\":\"{}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}\n",
            crate::start::mint("c_hub_feed_")
        );
        if (&stream).write_all(subscribe.as_bytes()).is_err() {
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
            .spawn(move || feed.read_session(&session, stream, found));
        if let Ok(handle) = spawned {
            if !state.scanned {
                state.awaited.insert(id.clone());
            }
            state.tracked.insert(id, shutdown);
            state.threads.push(handle);
        }
    }

    /// Reads session `id`'s summary lines until its socket closes.
    fn read_session(&self, id: &str, stream: UnixStream, found: Option<(String, PathBuf, u64)>) {
        let log = found.as_ref().map(|(_, dir, _)| dir.join("events.jsonl"));
        let mut read = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match read.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if !self.on_line(id, &buf, log.as_deref()) {
                        read.get_ref().shutdown(Shutdown::Both).unwrap_or(());
                        return;
                    }
                }
            }
        }
        self.on_left(id, found);
    }

    /// Takes one summary line. False once it shows a delegate.
    pub(super) fn on_line(&self, id: &str, bytes: &[u8], log: Option<&Path>) -> bool {
        let Some(payload) = parse_status(bytes) else {
            return true;
        };
        let _settle = Settle(self, id);
        // The unseen-turn probe, with no lock held: only a non-delegate `idle`
        // with a known log and no live entry reads the log.
        let unseen = if !may_be_unseen(&payload) {
            false
        } else if let Some(log) = log {
            let live = matches!(lock(&self.state).entries.get(id), Some(Entry::Running(_)));
            !live && self.attention.ended_unseen(log, &payload)
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
        self.attention.notify(id, seen, &payload, unseen);
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
    pub(super) fn on_left(&self, id: &str, found: Option<(String, PathBuf, u64)>) {
        let _settle = Settle(self, id);
        crate::rewind::notify_left(&self.on_rewound, id, &found);
        let how = found
            .as_ref()
            .filter(|(_, dir, _)| dir.is_dir())
            .map(|(_, dir, from)| match how_left(dir) {
                // Resumed already: the log's last line is the new run's,
                // and this run's exit line is past where it was followed
                // from. A socket that accepts is no sign: a killed
                // process's listener can outlive its summary connection.
                Left::Crashed if closed_since(&dir.join("events.jsonl"), *from) => Left::Exited,
                Left::Crashed => Left::Crashed,
                Left::Exited => Left::Exited,
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
    pub(super) fn left_line(&self, id: &str, how: Left) -> Line {
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

    pub(super) fn socket(&self, id: &str) -> PathBuf {
        self.home.join("run").join(id)
    }
}

/// How a session whose directory is `dir` left: `exited` when its log's
/// last line is `fiber_exited` or `rewound`, otherwise `crashed`.
pub(super) fn how_left(dir: &Path) -> Left {
    match last_kind(&dir.join("events.jsonl")).as_deref() {
        Some("fiber_exited" | "rewound") => Left::Exited,
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

/// The `kind` of `log`'s last line, read from at most [`TAIL`] bytes of
/// its end. `None` when it cannot be read or does not parse.
pub(crate) fn last_kind(log: &Path) -> Option<String> {
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
    kind_of(line)
}

/// Whether `log` holds a `fiber_exited` or `rewound` line from byte `from`
/// on.
fn closed_since(log: &Path, from: u64) -> bool {
    let Ok(mut file) = File::open(log) else {
        return false;
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return false;
    }
    BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .any(|line| matches!(kind_of(&line).as_deref(), Some("fiber_exited" | "rewound")))
}

/// The `kind` of one log line. `None` when it does not parse.
pub(crate) fn kind_of(line: &[u8]) -> Option<String> {
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
pub(super) fn status_line(row: &RecentRow, payload: &SessionStatus) -> Line {
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
