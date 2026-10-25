//! A fake [`Starter`][crate::Starter] for tests: it binds `run/<id>`
//! itself, answers the hub's `prompt` handshake, and records what the hub
//! sent.

use std::collections::BTreeSet;
use std::fs::DirBuilder;
use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use contract::commands::hub::PruneExpect;
use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;
use support::lock;

use crate::{PruneOutcome, Started, Starter};

/// What the fake answers the hub's `prompt` handshake with.
#[derive(Debug, Clone)]
pub(crate) struct Handshake {
    /// Whether the session accepts the first prompt.
    pub(crate) accept: bool,
    /// With `accept` false, the rejection code.
    pub(crate) code: String,
    /// With `accept` false, the rejection message.
    pub(crate) message: String,
}

/// Which commands a fake session answers `closing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Closing {
    /// None: every command gets its usual answer.
    Never,
    /// Every command but `subscribe`, as a session after `close` does.
    AllButSubscribe,
    /// Every command, `subscribe` too, as a session whose log is gone does.
    Every,
}

/// A fake session starter: it binds the session's socket itself instead of
/// spawning a process.
#[derive(Debug, Clone)]
pub(crate) struct FakeStarter {
    home: PathBuf,
    bind: bool,
    exited: Option<Failure>,
    handshake: Arc<Mutex<Option<Handshake>>>,
    /// Which commands the session answers `closing`.
    closing: Closing,
    /// Whether each `resume` appends `fiber_started` to the session log,
    /// as a resumed process writing its durable start does.
    append_started: bool,
    received: Arc<Received>,
    /// How many `subscribe` lines the fake sessions have accepted, and a
    /// wake for each new one: raised only after the acceptance write.
    accepted: Arc<Accepted>,
    /// Each `resume` call's session and workspace, in order.
    resumed: Arc<Mutex<Vec<(SessionId, PathBuf)>>>,
    /// Each `resume` call's asked credential label, in order: `Some` when
    /// the opening subscribe named one, beside [`FakeStarter::resumed`].
    asked: Arc<Mutex<Vec<Option<String>>>>,
    /// Each `rewind` call's new session, workspace and old session, in order.
    rewound: Arc<Mutex<Vec<(SessionId, PathBuf, SessionId)>>>,
    /// Each `start` call's worktree flag, in order.
    started_worktrees: Arc<Mutex<Vec<bool>>>,
    /// Each `start` call's continued session, in order, beside
    /// [`FakeStarter::started_worktrees`].
    started_continues: Arc<Mutex<Vec<Option<SessionId>>>>,
    /// The connections the fake sessions accepted and still serve.
    serving: Arc<Serving>,
    /// How many more `resume` calls exit `session_held` without binding,
    /// as a resume does while the exiting process still holds the lock.
    held: Arc<Mutex<usize>>,
    /// Whether a non-subscribe first line per connection is answered
    /// `command_rejected` `not_subscribed`, as a session answers one: off
    /// keeps the accept-any-command arm every current test uses.
    strict: bool,
    /// Whether the fake session exits once its `close` is accepted: the
    /// socket is gone, as a real session's is. Off holds the socket open
    /// after answering, until the test releases it.
    exit_on_close: bool,
    /// Each `kill` call's session, in order.
    killed: Arc<Mutex<Vec<SessionId>>>,
}

/// Every line the fake sessions received, and a wake for each new one.
#[derive(Debug, Default)]
struct Received {
    lines: Mutex<Vec<String>>,
    grew: Condvar,
    /// The same lines grouped by the connection that sent them, in accept
    /// order: only connections that sent at least one line appear.
    by_conn: Mutex<Vec<Vec<String>>>,
}

impl Received {
    fn push(&self, index: usize, line: String) {
        lock(&self.lines).push(line.clone());
        let mut by_conn = lock(&self.by_conn);
        while by_conn.len() <= index {
            by_conn.push(Vec::new());
        }
        by_conn[index].push(line);
        self.grew.notify_all();
    }
}

/// How many `subscribe` lines the fake sessions have accepted.
#[derive(Debug, Default)]
struct Accepted {
    count: Mutex<usize>,
    grew: Condvar,
}

/// The connections the fake sessions serve: a clone of each, to shut it
/// down, and how many are still served.
#[derive(Debug, Default)]
struct Serving {
    streams: Mutex<Vec<UnixStream>>,
    live: Mutex<usize>,
    ended: Condvar,
}

impl FakeStarter {
    /// Binds `run/<id>` on `start` and holds it; never exits.
    pub(crate) fn bind_and_hold(home: &Path) -> Self {
        Self::new(home, true, None, None)
    }

    /// Binds `run/<id>` and answers the hub's `prompt` with `handshake`.
    pub(crate) fn with_handshake(home: &Path, handshake: Handshake) -> Self {
        Self::new(home, true, None, Some(handshake))
    }

    /// Binds nothing; `exited` is what the process exited with.
    pub(crate) fn exit_with(home: &Path, failure: Failure) -> Self {
        Self::new(home, false, Some(failure), None)
    }

    /// Binds nothing and never exits: `start` waits out its deadline.
    pub(crate) fn hang(home: &Path) -> Self {
        Self::new(home, false, None, None)
    }

    /// Binds `run/<id>` and reports the process exited with `failure`, as
    /// a process that lost the session to another that bound it does.
    pub(crate) fn bind_and_exit(home: &Path, failure: Failure) -> Self {
        Self::new(home, true, Some(failure), None)
    }

    /// Binds `run/<id>` on `resume`, after its first `held` calls each
    /// exit `session_held` without binding.
    pub(crate) fn held_then_bind(home: &Path, held: usize) -> Self {
        let starter = Self::bind_and_hold(home);
        *lock(&starter.held) = held;
        starter
    }

    /// Binds `run/<id>` and answers every command but `subscribe` with
    /// `command_rejected` `closing`, as a session after `close` does.
    pub(crate) fn closing(home: &Path) -> Self {
        let mut starter = Self::bind_and_hold(home);
        starter.closing = Closing::AllButSubscribe;
        starter
    }

    /// As [`FakeStarter::closing`], and `subscribe` too, as a session
    /// whose log is gone does.
    pub(crate) fn closing_every_command(home: &Path) -> Self {
        let mut starter = Self::bind_and_hold(home);
        starter.closing = Closing::Every;
        starter
    }

    /// Binds `run/<id>` and holds it; each `resume` also appends
    /// `fiber_started` to the session log, as a resumed process writing
    /// its durable start does.
    pub(crate) fn bind_hold_and_append_started(home: &Path) -> Self {
        let mut starter = Self::bind_and_hold(home);
        starter.append_started = true;
        starter
    }

    /// Binds `run/<id>` and rejects a non-subscribe first line per
    /// connection with `command_rejected` `not_subscribed`, as a session
    /// does: the loop must subscribe before it closes.
    pub(crate) fn strict(home: &Path) -> Self {
        let mut starter = Self::bind_and_hold(home);
        starter.strict = true;
        starter
    }

    /// Binds `run/<id>` and exits the fake session once its `close` is
    /// accepted: the socket is gone, as a real session's is.
    pub(crate) fn exit_on_close(home: &Path) -> Self {
        let mut starter = Self::bind_and_hold(home);
        starter.exit_on_close = true;
        starter
    }

    fn new(home: &Path, bind: bool, exited: Option<Failure>, handshake: Option<Handshake>) -> Self {
        Self {
            home: home.to_path_buf(),
            bind,
            exited,
            handshake: Arc::new(Mutex::new(handshake)),
            closing: Closing::Never,
            append_started: false,
            received: Arc::new(Received::default()),
            accepted: Arc::new(Accepted::default()),
            resumed: Arc::new(Mutex::new(Vec::new())),
            asked: Arc::new(Mutex::new(Vec::new())),
            rewound: Arc::new(Mutex::new(Vec::new())),
            started_worktrees: Arc::new(Mutex::new(Vec::new())),
            started_continues: Arc::new(Mutex::new(Vec::new())),
            serving: Arc::new(Serving::default()),
            held: Arc::new(Mutex::new(0)),
            strict: false,
            exit_on_close: false,
            killed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every line the fake session received, in order.
    pub(crate) fn received(&self) -> Vec<String> {
        lock(&self.received.lines).clone()
    }

    /// Every line the fake sessions received, grouped by the connection
    /// that sent it, in accept order.
    pub(crate) fn received_by_connection(&self) -> Vec<Vec<String>> {
        lock(&self.received.by_conn).clone()
    }

    /// Waits, at most `within` of real time, until the fake sessions have
    /// accepted `count` subscribes. True once they have.
    pub(crate) fn await_accepted(&self, count: usize, within: Duration) -> bool {
        let count_now = lock(&self.accepted.count);
        let (count_now, _) = self
            .accepted
            .grew
            .wait_timeout_while(count_now, within, |count_now| *count_now < count)
            .unwrap_or_else(PoisonError::into_inner);
        *count_now >= count
    }

    /// Waits, at most `within` of real time, until the fake sessions have
    /// received `count` lines and answered each. True once they have.
    pub(crate) fn await_received(&self, count: usize, within: Duration) -> bool {
        let lines = lock(&self.received.lines);
        let (lines, _) = self
            .received
            .grew
            .wait_timeout_while(lines, within, |lines| lines.len() < count)
            .unwrap_or_else(PoisonError::into_inner);
        lines.len() >= count
    }

    /// Each `resume` call's session and workspace, in order.
    pub(crate) fn resumed(&self) -> Vec<(SessionId, PathBuf)> {
        lock(&self.resumed).clone()
    }

    /// Each `resume` call's asked credential label, in order.
    pub(crate) fn asked(&self) -> Vec<Option<String>> {
        lock(&self.asked).clone()
    }

    /// Each `start` call's worktree flag, in order.
    pub(crate) fn started_worktrees(&self) -> Vec<bool> {
        lock(&self.started_worktrees).clone()
    }

    /// Each `start` call's continued session, in order: `None` for a
    /// fresh session.
    pub(crate) fn started_continues(&self) -> Vec<Option<SessionId>> {
        lock(&self.started_continues).clone()
    }

    /// Each `rewind` call's new session, workspace and old session, in order.
    pub(crate) fn rewound(&self) -> Vec<(SessionId, PathBuf, SessionId)> {
        lock(&self.rewound).clone()
    }

    /// Each `kill` call's session, in order.
    pub(crate) fn kills(&self) -> Vec<SessionId> {
        lock(&self.killed).clone()
    }

    /// Ends session `id` as an exiting process does: its socket is gone,
    /// and every connection it served is closed, waiting at most `within`
    /// for each to end. True once all have ended.
    pub(crate) fn stop(&self, id: &SessionId, within: Duration) -> bool {
        std::fs::remove_file(self.home.join("run").join(&id.0)).unwrap_or(());
        for stream in lock(&self.serving.streams).drain(..) {
            stream.shutdown(Shutdown::Both).unwrap_or(());
        }
        let live = lock(&self.serving.live);
        let (live, _) = self
            .serving
            .ended
            .wait_timeout_while(live, within, |live| *live > 0)
            .unwrap_or_else(PoisonError::into_inner);
        *live == 0
    }

    /// Binds `run/<id>` when the fake binds, serving each connection on a
    /// thread of its own.
    fn launch(&self, id: &SessionId) -> std::io::Result<Box<dyn Started>> {
        if self.bind {
            let run = self.home.join("run");
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&run)
                .map_err(|error| refused(&run, &error))?;
            let socket = run.join(&id.0);
            let listener = UnixListener::bind(&socket).map_err(|error| refused(&socket, &error))?;
            let received = Arc::clone(&self.received);
            let accepted = Arc::clone(&self.accepted);
            let handshake = Arc::clone(&self.handshake);
            let serving = Arc::clone(&self.serving);
            let closing = self.closing;
            let strict = self.strict;
            let exit = self.exit_on_close.then(|| {
                Arc::new(ExitOnClose {
                    socket: socket.clone(),
                    fired: AtomicBool::new(false),
                })
            });
            // The listener lives in the accept loop's thread.
            thread::Builder::new()
                .name("fake-session".to_owned())
                .spawn(move || {
                    accept_loop(
                        listener, &received, &accepted, &handshake, &serving, closing, strict, exit,
                    )
                })
                .map_err(|error| {
                    std::io::Error::new(error.kind(), format!("fake session: {error}"))
                })?;
        }
        Ok(Box::new(FakeStarted {
            exited: self.exited.clone(),
            id: id.clone(),
            killed: Arc::clone(&self.killed),
        }))
    }
}

impl Starter for FakeStarter {
    fn start(
        &self,
        id: &SessionId,
        _workspace: &Path,
        _model: Option<&str>,
        _overrides: &[&str],
        worktree: bool,
        continues: Option<&SessionId>,
    ) -> std::io::Result<Box<dyn Started>> {
        lock(&self.started_worktrees).push(worktree);
        lock(&self.started_continues).push(continues.cloned());
        self.launch(id)
    }

    fn rewind(
        &self,
        id: &SessionId,
        workspace: &Path,
        from: &SessionId,
    ) -> std::io::Result<Box<dyn Started>> {
        lock(&self.rewound).push((id.clone(), workspace.to_path_buf(), from.clone()));
        self.launch(id)
    }

    fn resume(
        &self,
        id: &SessionId,
        workspace: &Path,
        credential: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        lock(&self.resumed).push((id.clone(), workspace.to_path_buf()));
        lock(&self.asked).push(credential.map(str::to_owned));
        {
            let mut held = lock(&self.held);
            if *held > 0 {
                *held -= 1;
                return Ok(Box::new(FakeStarted {
                    exited: Some(failure(
                        ErrorCode::SessionHeld,
                        "Another process holds this session.",
                    )),
                    id: id.clone(),
                    killed: Arc::clone(&self.killed),
                }));
            }
        }
        if self.append_started {
            append_started(&self.home, id);
        }
        self.launch(id)
    }
}

/// What `exited` returns. The listener lives in the accept loop's thread.
pub(crate) struct FakeStarted {
    exited: Option<Failure>,
    id: SessionId,
    killed: Arc<Mutex<Vec<SessionId>>>,
}

impl Started for FakeStarted {
    fn exited(&self) -> Option<Failure> {
        self.exited.clone()
    }

    fn kill(&self) {
        lock(&self.killed).push(self.id.clone());
    }
}

/// Exits the fake session once its `close` is accepted: the first close
/// unlinks the socket and shuts every served connection, as a real
/// session's exit does. Later closes find nothing to do.
struct ExitOnClose {
    socket: PathBuf,
    fired: AtomicBool,
}

/// A failure with Fiber's own sentence.
pub(crate) fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

/// Appends `fiber_started` as the last line of `id`'s log, as a resumed
/// process writing its durable start does.
fn append_started(home: &Path, id: &SessionId) {
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return;
    };
    let mut projects: Vec<PathBuf> = projects
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    projects.sort();
    for project in projects {
        let log = project.join("sessions").join(&id.0).join("events.jsonl");
        if log.is_file()
            && let Ok(mut text) = std::fs::read_to_string(&log)
        {
            text.push_str(&format!(
                "{{\"kind\":\"fiber_started\",\"session_id\":\"{}\",\"payload\":{{}}}}\n",
                id.0
            ));
            std::fs::write(log, text).unwrap_or(());
            return;
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the fake threads every shared table to each serving connection"
)]
fn accept_loop(
    listener: UnixListener,
    received: &Arc<Received>,
    accepted: &Arc<Accepted>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
    serving: &Arc<Serving>,
    closing: Closing,
    strict: bool,
    exit: Option<Arc<ExitOnClose>>,
) {
    let mut next_conn = 0;
    loop {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let Ok(clone) = stream.try_clone() else {
            return;
        };
        lock(&serving.streams).push(clone);
        *lock(&serving.live) += 1;
        let received = Arc::clone(received);
        let accepted = Arc::clone(accepted);
        let index = next_conn;
        let handshake = Arc::clone(handshake);
        let serving = Arc::clone(serving);
        let exit = exit.clone();
        let spawned = thread::Builder::new()
            .name("fake-session-conn".to_owned())
            .spawn(move || {
                serve_one(
                    stream, &received, &accepted, &handshake, closing, strict, exit, index,
                );
                *lock(&serving.live) -= 1;
                serving.ended.notify_all();
            });
        if spawned.is_err() {
            return;
        }
        next_conn += 1;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the fake threads every shared table to each serving connection"
)]
fn serve_one(
    stream: UnixStream,
    received: &Arc<Received>,
    accepted: &Arc<Accepted>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
    closing: Closing,
    strict: bool,
    exit: Option<Arc<ExitOnClose>>,
    index: usize,
) {
    if stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .is_err()
    {
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let mut read = BufReader::new(stream);
    // Whether this connection has sent `subscribe`: a strict fake
    // rejects any other first command, as a session does.
    let mut subscribed = false;
    loop {
        match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            // The hub's liveness check closed without sending.
            Ok(None) => return,
            Ok(Some(mut buf)) => {
                // Received lines keep their one trailing line feed.
                buf.push(b'\n');
                let text = String::from_utf8_lossy(&buf).into_owned();
                let command = command_of(&text);
                if strict && !subscribed && command.as_deref() != Some("subscribe") {
                    // Answered before it is recorded, as the closing
                    // arm below is, so a test that saw it received knows
                    // the rejection is sent.
                    write_ack(&mut writer, &text, &not_subscribed());
                    received.push(index, text);
                    continue;
                }
                if command.as_deref() == Some("subscribe") {
                    subscribed = true;
                }
                let closes = match closing {
                    Closing::Never => false,
                    Closing::AllButSubscribe => {
                        command.as_deref().is_some_and(|name| name != "subscribe")
                    }
                    Closing::Every => command.is_some(),
                };
                if closes {
                    // Answered before it is recorded, so a test that saw it
                    // received knows the answer is sent.
                    write_ack(&mut writer, &text, &closing_reply());
                    received.push(index, text);
                    continue;
                }
                received.push(index, text.clone());
                if command.as_deref() == Some("close")
                    && let Some(exit) = exit.as_ref()
                {
                    // The answer goes out before the exit: the hub reads
                    // the acceptance, then the socket's close. Only this
                    // connection ends: every other session keeps its own
                    // socket, as processes do, and the unlinked path
                    // refuses every later connect.
                    write_accepted(&mut writer, &text);
                    if !exit.fired.swap(true, Ordering::SeqCst) {
                        std::fs::remove_file(&exit.socket).unwrap_or(());
                    }
                    writer.shutdown(Shutdown::Both).unwrap_or(());
                    return;
                }
                match command.as_deref() {
                    // The hub's handshake subscribes before its first prompt.
                    Some("subscribe") => {
                        let level = subscribe_level(&text);
                        if level.as_deref() == Some("summary") || level.as_deref() == Some("full") {
                            write_accepted(&mut writer, &text);
                            *lock(&accepted.count) += 1;
                            accepted.grew.notify_all();
                        } else {
                            write_ack(&mut writer, &text, &invalid_arguments());
                        }
                    }
                    Some("prompt") => {
                        if let Some(reply) = lock(handshake).clone() {
                            write_ack(&mut writer, &text, &reply);
                            return;
                        }
                    }
                    // Any other command is accepted, as a session that
                    // takes it does.
                    Some(_) => write_accepted(&mut writer, &text),
                    // Anything else is held until EOF.
                    None => hold(&mut read),
                }
            }
            // Nothing sent yet: hold until EOF.
            Err(_) => hold(&mut read),
        }
    }
}

/// What a session after `close` answers a command with.
fn closing_reply() -> Handshake {
    Handshake {
        accept: false,
        code: "closing".to_owned(),
        message: "The session is closing.".to_owned(),
    }
}

/// What a strict fake answers a non-subscribe first line with, as a
/// session answers one (`crates/doors/src/client.rs`, `NOT_SUBSCRIBED`):
/// the hub must subscribe before any other command.
fn not_subscribed() -> Handshake {
    Handshake {
        accept: false,
        code: "not_subscribed".to_owned(),
        message: "Send `subscribe` first.".to_owned(),
    }
}

fn hold(read: &mut BufReader<UnixStream>) {
    if read.get_mut().set_read_timeout(None).is_err() {
        return;
    }
    loop {
        match support::framing::read_line(&mut *read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => return,
            Ok(Some(_)) => {}
        }
    }
}

fn command_of(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|line| line.get("command").cloned())
        .and_then(|command| command.as_str().map(str::to_owned))
}

/// The `args.level` of a `subscribe` line, when it parses.
fn subscribe_level(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|line| line.get("args")?.get("level")?.as_str().map(str::to_owned))
}

/// What a session answers a `subscribe` whose level is neither `summary`
/// nor `full` with.
fn invalid_arguments() -> Handshake {
    Handshake {
        accept: false,
        code: "invalid_arguments".to_owned(),
        message: "The arguments do not fit this command.".to_owned(),
    }
}

fn write_accepted(writer: &mut UnixStream, command: &str) {
    let id = serde_json::from_str::<Value>(command)
        .ok()
        .and_then(|line| line.get("id").cloned())
        .unwrap_or(Value::Null);
    let ack = serde_json::json!({
        "kind": "command_accepted",
        "ts": 1,
        "schema_version": 1,
        "payload": { "command_id": id },
    });
    let bytes = serde_json::to_vec(&ack).unwrap_or_default();
    support::framing::write_line(writer, &bytes).unwrap_or(());
}

fn write_ack(writer: &mut UnixStream, prompt: &str, reply: &Handshake) {
    let id = serde_json::from_str::<Value>(prompt)
        .ok()
        .and_then(|line| line.get("id").cloned())
        .unwrap_or(Value::Null);
    let ack = if reply.accept {
        serde_json::json!({
            "kind": "command_accepted",
            "ts": 1,
            "schema_version": 1,
            "payload": { "command_id": id },
        })
    } else {
        serde_json::json!({
            "kind": "command_rejected",
            "ts": 1,
            "schema_version": 1,
            "payload": {
                "code": reply.code,
                "command_id": id,
                "message": reply.message,
            },
        })
    };
    let bytes = serde_json::to_vec(&ack).unwrap_or_default();
    support::framing::write_line(writer, &bytes).unwrap_or(());
}

fn refused(path: &Path, error: &std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// A `session_status` payload: `state` is `"idle"`, or `"waiting"` with a
/// pending approval; `parent` marks a delegate.
pub(crate) fn status(name: &str, workspace: &str, state: &str, parent: Option<&str>) -> Value {
    let usage = serde_json::json!({
        "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
        "cost": 0.0,
        "subscription_cost": 0.0,
    });
    let mut payload = serde_json::json!({
        "name": name, "workspace": workspace, "project": "-w", "model": "p/m", "state": state,
        "started": 0, "calls": 0, "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "clients": 0,
    });
    if state == "waiting" {
        payload["waiting"] = serde_json::json!({
            "request_id": "r1", "kind": "approval", "summary": "run ls",
        });
    }
    if let Some(parent) = parent {
        payload["parent"] = Value::String(parent.to_owned());
    }
    payload
}

/// A `session_status` line for session `id`, as the session sends it.
pub(crate) fn status_line(id: &str, payload: &Value) -> String {
    let line = serde_json::json!({
        "kind": "session_status", "session_id": id, "ts": 5, "schema_version": 1,
        "payload": payload,
    });
    let mut text = serde_json::to_string(&line).unwrap_or_default();
    text.push('\n');
    text
}

/// A fake running session at `run/<id>`: each connection that sends
/// `subscribe` gets `command_accepted`, then every line [`FakeSession::say`]
/// queued so far and every one after it. [`FakeSession::close`] unlinks the
/// socket and shuts every connection, as a session's exit does.
pub(crate) struct FakeSession {
    socket: PathBuf,
    shared: Arc<(Mutex<SessionState>, std::sync::Condvar)>,
    accept: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct SessionState {
    said: Vec<String>,
    conns: Vec<UnixStream>,
    subscribed: usize,
    closed: bool,
}

impl FakeSession {
    /// Binds `run/<id>` under `home`.
    pub(crate) fn bind(home: &Path, id: &str) -> Self {
        let run = home.join("run");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&run)
            .unwrap_or(());
        let socket = run.join(id);
        let shared: Arc<(Mutex<SessionState>, std::sync::Condvar)> = Arc::default();
        let accept = UnixListener::bind(&socket).ok().and_then(|listener| {
            let for_thread = Arc::clone(&shared);
            thread::Builder::new()
                .name("fake-feed-session".to_owned())
                .spawn(move || session_accept(&listener, &for_thread))
                .ok()
        });
        Self {
            socket,
            shared,
            accept: Mutex::new(accept),
        }
    }

    /// Stops accepting: marks the session closed, wakes the accept loop
    /// and waits until it has dropped the listener. Returns the open
    /// connections for the caller to shut.
    fn stop_accepting(&self) -> Vec<UnixStream> {
        let conns = {
            let mut state = lock(&self.shared.0);
            state.closed = true;
            std::mem::take(&mut state.conns)
        };
        drop(UnixStream::connect(&self.socket));
        if let Some(accept) = lock(&self.accept).take() {
            accept.join().unwrap_or(());
        }
        conns
    }

    /// Sends `line` to every subscriber now and every later one.
    pub(crate) fn say(&self, line: &str) {
        let mut state = lock(&self.shared.0);
        state.said.push(line.to_owned());
        for conn in &state.conns {
            let mut conn = conn;
            conn.write_all(line.as_bytes()).unwrap_or(());
        }
    }

    /// Waits, at most `within` of real time, until `count` connections
    /// have subscribed. True once they have.
    pub(crate) fn await_subscribed(&self, count: usize, within: Duration) -> bool {
        let (state, cv) = &*self.shared;
        let state = lock(state);
        let (state, _) = cv
            .wait_timeout_while(state, within, |state| state.subscribed < count)
            .unwrap_or_else(PoisonError::into_inner);
        state.subscribed >= count
    }

    /// Exits: unlinks the socket, then shuts every connection.
    pub(crate) fn close(&self) {
        let conns = self.stop_accepting();
        std::fs::remove_file(&self.socket).unwrap_or(());
        for conn in conns {
            conn.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }

    /// Exits and is resumed at once: a new session binds `run/<id>`
    /// before this one shuts its connections.
    pub(crate) fn resumed(&self, home: &Path, id: &str) -> Self {
        let conns = self.stop_accepting();
        std::fs::remove_file(&self.socket).unwrap_or(());
        let next = Self::bind(home, id);
        for conn in conns {
            conn.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
        next
    }

    /// Dies: shuts every connection and leaves the socket file behind.
    pub(crate) fn kill(&self) {
        // The listener is gone before any connection ends, as with a
        // process that died; the file stays and refuses connections.
        for conn in self.stop_accepting() {
            conn.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }
}

fn session_accept(
    listener: &UnixListener,
    shared: &Arc<(Mutex<SessionState>, std::sync::Condvar)>,
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        if lock(&shared.0).closed {
            return;
        }
        let shared = Arc::clone(shared);
        thread::Builder::new()
            .name("fake-feed-conn".to_owned())
            .spawn(move || session_serve(stream, &shared))
            .map(drop)
            .unwrap_or(());
    }
}

fn session_serve(stream: UnixStream, shared: &Arc<(Mutex<SessionState>, std::sync::Condvar)>) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut read = BufReader::new(stream);
    // A probe that sends nothing closes; only a subscriber is held.
    match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
        Ok(None) | Err(_) => (),
        Ok(Some(buf)) => {
            let text = String::from_utf8_lossy(&buf).into_owned();
            let mut ack = writer.try_clone().map(Some).unwrap_or(None);
            if let Some(ack) = ack.as_mut() {
                write_accepted(ack, &text);
            }
            {
                let (state, cv) = &**shared;
                let mut state = lock(state);
                if state.closed {
                    return;
                }
                for line in &state.said {
                    let mut out = &writer;
                    out.write_all(line.as_bytes()).unwrap_or(());
                }
                state.conns.push(writer);
                state.subscribed += 1;
                cv.notify_all();
            }
            hold(&mut read);
        }
    }
}

/// Records machine calls and answers configuration operations from a script.
#[derive(Default)]
pub(crate) struct FakeMachine {
    pub(crate) calls: Mutex<Vec<Value>>,
    pub(crate) answers: Mutex<std::collections::VecDeque<Result<Option<Value>, Failure>>>,
    pub(crate) search_gate: Mutex<Option<Arc<SearchGate>>>,
    pub(crate) logins: Mutex<std::collections::VecDeque<Arc<FakeLogin>>>,
    pub(crate) imports: Mutex<std::collections::VecDeque<Result<Option<Value>, Failure>>>,
    pub(crate) prunes: Mutex<std::collections::VecDeque<Result<PruneOutcome, Failure>>>,
}

pub(crate) struct SearchGate {
    pub(crate) started: std::sync::mpsc::Sender<String>,
    pub(crate) release: Mutex<std::sync::mpsc::Receiver<()>>,
    pub(crate) ignore_cancel: bool,
    pub(crate) cancelled: std::sync::atomic::AtomicBool,
}
struct FakeSearch(Option<Arc<SearchGate>>);
impl crate::FileSearch for FakeSearch {
    fn search(
        &mut self,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Result<Vec<String>, String>> {
        if let Some(gate) = &self.0 {
            drop(gate.started.send(text.to_owned()));
            lock(&gate.release).recv().unwrap_or(());
            gate.cancelled
                .store(cancelled(), std::sync::atomic::Ordering::SeqCst);
            if gate.ignore_cancel {
                return Some(Ok(vec![text.to_owned()]));
            }
        }
        if cancelled() {
            None
        } else {
            Some(Ok(vec![text.to_owned()]))
        }
    }
}
pub(crate) struct FakeLogin {
    pub(crate) shows: bool,
    pub(crate) outcome: Result<String, Failure>,
    pub(crate) hold_cancel: bool,
    pub(crate) waiting: std::sync::mpsc::Sender<()>,
    pub(crate) gate: Mutex<std::sync::mpsc::Receiver<()>>,
    pub(crate) release: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    pub(crate) cancelled: std::sync::mpsc::Sender<()>,
    pub(crate) stopped: std::sync::atomic::AtomicBool,
}
use crate::ShowCode;
struct FakeDeviceLogin {
    script: Option<Arc<FakeLogin>>,
    shown: ShowCode,
}
impl crate::DeviceLogin for FakeDeviceLogin {
    fn run(&self) -> Result<String, Failure> {
        let Some(script) = self.script.as_ref() else {
            return Err(Failure {
                code: contract::ErrorCode::Usage,
                message: "no scripted login".into(),
                provider: None,
                retry_after_ms: None,
            });
        };
        if script.shows {
            (self.shown)("https://device", "CODE");
        }
        script.waiting.send(()).unwrap_or(());
        lock(&script.gate).recv().unwrap_or(());
        if script.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            Err(Failure {
                code: contract::ErrorCode::AuthenticationFailed,
                message: "cancelled".into(),
                provider: None,
                retry_after_ms: None,
            })
        } else {
            script.outcome.clone()
        }
    }
    fn cancel(&self) {
        if let Some(script) = &self.script {
            script
                .stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            script.cancelled.send(()).unwrap_or(());
            if let Some(release) = lock(&script.release).take()
                && !script.hold_cancel
            {
                release.send(()).unwrap_or(());
            }
        }
    }
}
impl crate::Machine for FakeMachine {
    fn device_login(
        &self,
        _name: &str,
        shown: ShowCode,
        _clock: Arc<dyn contract::clock::Clock>,
    ) -> Arc<dyn crate::DeviceLogin> {
        Arc::new(FakeDeviceLogin {
            script: lock(&self.logins).pop_front(),
            shown,
        })
    }
    fn file_search(&self, workspace: &Path, home: Option<&Path>) -> Box<dyn crate::FileSearch> {
        lock(&self.calls).push(serde_json::json!({"search":[workspace, home]}));
        Box::new(FakeSearch(lock(&self.search_gate).clone()))
    }
    fn configure(
        &self,
        operation: &str,
        args: &serde_json::Map<String, Value>,
        _clock: Arc<dyn contract::clock::Clock>,
    ) -> Result<Option<Value>, Failure> {
        lock(&self.calls).push(serde_json::json!({"operation":operation,"args":args}));
        lock(&self.answers).pop_front().unwrap_or(Ok(None))
    }
    fn import(
        &self,
        home: &Path,
        source: Option<&str>,
        _clock: Arc<dyn contract::clock::Clock>,
    ) -> Result<Option<Value>, Failure> {
        lock(&self.calls).push(serde_json::json!({"import":{"home":home,"source":source}}));
        lock(&self.imports).pop_front().unwrap_or(Ok(None))
    }
    fn read_config(&self, workspace: &Path, path: &Path) -> Result<(String, String), Failure> {
        lock(&self.calls).push(serde_json::json!({"read":[workspace,path]}));
        Ok(("text".to_owned(), "version".to_owned()))
    }
    fn write_config(
        &self,
        workspace: &Path,
        path: &Path,
        text: &str,
        version: &str,
    ) -> Result<(), Failure> {
        lock(&self.calls).push(serde_json::json!({"write":[workspace,path,text,version]}));
        Ok(())
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "one seam call carries the whole prune: its flags, the previewed items and the session-delete callback"
    )]
    fn prune(
        &self,
        home: &Path,
        older_than: Option<String>,
        cascade: bool,
        force: bool,
        dry_run: bool,
        expect: Option<PruneExpect>,
        _clock: Arc<dyn contract::clock::Clock>,
        _delete: &mut (
                 dyn FnMut(&SessionId, bool, Option<&BTreeSet<SessionId>>) -> Result<(), Failure>
                     + Send
             ),
    ) -> Result<PruneOutcome, Failure> {
        lock(&self.calls).push(serde_json::json!({"prune":{
            "home": home, "older_than": older_than, "cascade": cascade,
            "force": force, "dry_run": dry_run, "expect": expect,
        }}));
        lock(&self.prunes).pop_front().unwrap_or(Ok(PruneOutcome {
            lines: Vec::new(),
            items: PruneExpect {
                sessions: Vec::new(),
                worktrees: Vec::new(),
                pinned: Vec::new(),
                diagnostics: Vec::new(),
            },
            freed: 0,
            total: 0,
            failures: Vec::new(),
        }))
    }
}
