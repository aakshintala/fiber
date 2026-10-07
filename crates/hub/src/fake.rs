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

/// A fake session starter: it binds the session's socket itself instead of
/// spawning a process.
#[derive(Debug, Clone)]
pub(crate) struct FakeStarter {
    home: PathBuf,
    bind: bool,
    exited: Option<Failure>,
    handshake: Arc<Mutex<Option<Handshake>>>,
    received: Arc<Mutex<Vec<String>>>,
    /// Each `resume` call's session and workspace, in order.
    resumed: Arc<Mutex<Vec<(SessionId, PathBuf)>>>,
    /// The connections the fake sessions accepted and still serve.
    serving: Arc<Serving>,
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

    fn new(home: &Path, bind: bool, exited: Option<Failure>, handshake: Option<Handshake>) -> Self {
        Self {
            home: home.to_path_buf(),
            bind,
            exited,
            handshake: Arc::new(Mutex::new(handshake)),
            received: Arc::new(Mutex::new(Vec::new())),
            resumed: Arc::new(Mutex::new(Vec::new())),
            serving: Arc::new(Serving::default()),
        }
    }

    /// Every line the fake session received, in order.
    pub(crate) fn received(&self) -> Vec<String> {
        lock(&self.received).clone()
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
            // The listener lives in the accept loop's thread.
            thread::Builder::new()
                .name("fake-session".to_owned())
                .spawn(move || accept_loop(listener, &received, &handshake, &serving))
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
        retry_after: None,
        provider: None,
    }
}

fn accept_loop(
    listener: UnixListener,
    received: &Arc<Mutex<Vec<String>>>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
    serving: &Arc<Serving>,
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
                serve_one(stream, &received, &handshake);
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
    received: &Arc<Mutex<Vec<String>>>,
    handshake: &Arc<Mutex<Option<Handshake>>>,
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
                lock(received).push(text.clone());
                match command_of(&text).as_deref() {
                    // The hub's handshake subscribes before its first prompt.
                    Some("subscribe") => write_accepted(&mut writer, &text),
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
        "name": name, "workspace": workspace, "model": "p/m", "state": state,
        "since": 1, "spend": usage, "delegates": 0, "jobs": 0,
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
        if let Ok(listener) = UnixListener::bind(&socket) {
            let for_thread = Arc::clone(&shared);
            thread::Builder::new()
                .name("fake-feed-session".to_owned())
                .spawn(move || session_accept(&listener, &for_thread))
                .map(drop)
                .unwrap_or(());
        }
        Self { socket, shared }
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
        let conns = {
            let mut state = lock(&self.shared.0);
            state.closed = true;
            std::mem::take(&mut state.conns)
        };
        // Wakes the accept loop, which sees `closed` and ends.
        drop(UnixStream::connect(&self.socket));
        std::fs::remove_file(&self.socket).unwrap_or(());
        for conn in conns {
            conn.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }

    /// Dies: shuts every connection and leaves the socket file behind.
    pub(crate) fn kill(&self) {
        let mut state = lock(&self.shared.0);
        state.closed = true;
        for conn in state.conns.drain(..) {
            conn.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
        drop(state);
        // Wakes the accept loop, which ends and drops the listener; the
        // file stays and refuses connections.
        drop(UnixStream::connect(&self.socket));
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
