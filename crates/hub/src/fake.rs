//! A fake [`Starter`][crate::Starter] for tests: it binds `run/<id>`
//! itself, answers the hub's `prompt` handshake, and records what the hub
//! sent.

use std::fs::DirBuilder;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use contract::shapes::Failure;
use contract::{ErrorCode, SessionId};
use serde_json::Value;

use crate::{Started, Starter};

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
    /// Each `resume` call's session and workspace, in order.
    resumed: Arc<Mutex<Vec<(SessionId, PathBuf)>>>,
    /// The connections the fake sessions accepted and still serve.
    serving: Arc<Serving>,
    /// How many more `resume` calls exit `session_held` without binding,
    /// as a resume does while the exiting process still holds the lock.
    held: Arc<Mutex<usize>>,
}

/// Every line the fake sessions received, and a wake for each new one.
#[derive(Debug, Default)]
struct Received {
    lines: Mutex<Vec<String>>,
    grew: Condvar,
}

impl Received {
    fn push(&self, line: String) {
        lock(&self.lines).push(line);
        self.grew.notify_all();
    }
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

    fn new(home: &Path, bind: bool, exited: Option<Failure>, handshake: Option<Handshake>) -> Self {
        Self {
            home: home.to_path_buf(),
            bind,
            exited,
            handshake: Arc::new(Mutex::new(handshake)),
            closing: Closing::Never,
            append_started: false,
            received: Arc::new(Received::default()),
            resumed: Arc::new(Mutex::new(Vec::new())),
            serving: Arc::new(Serving::default()),
            held: Arc::new(Mutex::new(0)),
        }
    }

    /// Every line the fake session received, in order.
    pub(crate) fn received(&self) -> Vec<String> {
        lock(&self.received.lines).clone()
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
            let handshake = Arc::clone(&self.handshake);
            let serving = Arc::clone(&self.serving);
            let closing = self.closing;
            // The listener lives in the accept loop's thread.
            thread::Builder::new()
                .name("fake-session".to_owned())
                .spawn(move || accept_loop(listener, &received, &handshake, &serving, closing))
                .map_err(|error| {
                    std::io::Error::new(error.kind(), format!("fake session: {error}"))
                })?;
        }
        Ok(Box::new(FakeStarted {
            exited: self.exited.clone(),
        }))
    }
}

impl Starter for FakeStarter {
    fn start(
        &self,
        id: &SessionId,
        _workspace: &Path,
        _model: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
        self.launch(id)
    }

    fn resume(&self, id: &SessionId, workspace: &Path) -> std::io::Result<Box<dyn Started>> {
        lock(&self.resumed).push((id.clone(), workspace.to_path_buf()));
        {
            let mut held = lock(&self.held);
            if *held > 0 {
                *held -= 1;
                return Ok(Box::new(FakeStarted {
                    exited: Some(failure(
                        ErrorCode::SessionHeld,
                        "Another process holds this session.",
                    )),
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
}

impl Started for FakeStarted {
    fn exited(&self) -> Option<Failure> {
        self.exited.clone()
    }
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

fn accept_loop(
    listener: UnixListener,
    received: &Arc<Received>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
    serving: &Arc<Serving>,
    closing: Closing,
) {
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
        let handshake = Arc::clone(handshake);
        let serving = Arc::clone(serving);
        let spawned = thread::Builder::new()
            .name("fake-session-conn".to_owned())
            .spawn(move || {
                serve_one(stream, &received, &handshake, closing);
                *lock(&serving.live) -= 1;
                serving.ended.notify_all();
            });
        if spawned.is_err() {
            return;
        }
    }
}

fn serve_one(
    stream: UnixStream,
    received: &Arc<Received>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
    closing: Closing,
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
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            // The hub's liveness check closed without sending.
            Ok(0) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&buf).into_owned();
                let command = command_of(&text);
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
                    received.push(text);
                    continue;
                }
                received.push(text.clone());
                match command.as_deref() {
                    // The hub's handshake subscribes before its first prompt.
                    Some("subscribe") => {
                        let level = subscribe_level(&text);
                        if level.as_deref() == Some("summary") || level.as_deref() == Some("full") {
                            write_accepted(&mut writer, &text);
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

fn hold(read: &mut BufReader<UnixStream>) {
    if read.get_mut().set_read_timeout(None).is_err() {
        return;
    }
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
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
    let mut bytes = serde_json::to_vec(&ack).unwrap_or_default();
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .unwrap_or(());
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
    let mut bytes = serde_json::to_vec(&ack).unwrap_or_default();
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .unwrap_or(());
}

fn refused(path: &Path, error: &std::io::Error) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
        "since": 1, "spend": usage, "delegates": 0, "jobs": 0, "clients": 0,
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
    let mut buf = Vec::new();
    // A probe that sends nothing closes; only a subscriber is held.
    match read.read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => return,
        Ok(_) => {}
    }
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
