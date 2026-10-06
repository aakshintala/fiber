//! A fake [`Starter`][crate::Starter] for tests: it binds `run/<id>`
//! itself, answers the hub's `prompt` handshake, and records what the hub
//! sent.

use std::fs::DirBuilder;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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

    fn new(home: &Path, bind: bool, exited: Option<Failure>, handshake: Option<Handshake>) -> Self {
        Self {
            home: home.to_path_buf(),
            bind,
            exited,
            handshake: Arc::new(Mutex::new(handshake)),
            received: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every line the fake session received, in order.
    pub(crate) fn received(&self) -> Vec<String> {
        lock(&self.received).clone()
    }
}

impl Starter for FakeStarter {
    fn start(
        &self,
        id: &SessionId,
        _workspace: &Path,
        _model: Option<&str>,
    ) -> std::io::Result<Box<dyn Started>> {
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
            // The listener lives in the accept loop's thread.
            thread::Builder::new()
                .name("fake-session".to_owned())
                .spawn(move || accept_loop(listener, &received, &handshake))
                .map_err(|error| {
                    std::io::Error::new(error.kind(), format!("fake session: {error}"))
                })?;
        }
        Ok(Box::new(FakeStarted {
            exited: self.exited.clone(),
        }))
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
) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let received = Arc::clone(received);
        let handshake = Arc::clone(handshake);
        let spawned = thread::Builder::new()
            .name("fake-session-conn".to_owned())
            .spawn(move || serve_one(stream, &received, &handshake));
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
                    // Any other connection is held until EOF.
                    _ => hold(&mut read),
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
