//! One client connection: `hub_hello` first, hub-command dispatch for
//! `start` and `status`, and the relay map to session sockets.
//!
//! A command with a `session_id` is for that session: the hub passes the
//! line to the session's socket without the key and passes back what the
//! session sends. A command without one is for the hub. Every connection
//! opens with `hub_hello`, before any acknowledgement.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;

use contract::clock::Clock;
use contract::{CommandId, ErrorCode, HubLine, SCHEMA_VERSION};
use serde_json::{Map, Value};

use crate::Starter;
use crate::diag::Diag;
use crate::start::{self, Outcome};

/// What the hub shares across its connections: home, the version it
/// reports, how it starts sessions, the clock, the diagnostic log, and the
/// open-connection count `status` answers with.
pub(crate) struct Hub {
    /// Fiber home: `run/<session_id>` is resolved under it.
    pub(crate) home: PathBuf,
    fiber_version: String,
    /// How `start` runs the session command.
    pub(crate) starter: Arc<dyn Starter>,
    /// The injected clock, for `ts` and `start`'s deadline.
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) diag: Diag,
    clients: AtomicUsize,
    next_client: AtomicU64,
}

impl Hub {
    /// Opens the hub in `home`: the diagnostic log, pruned, before the
    /// caller writes `hub_started`.
    pub(crate) fn new(
        home: &Path,
        fiber_version: &str,
        starter: Arc<dyn Starter>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            home: home.to_path_buf(),
            fiber_version: fiber_version.to_owned(),
            starter,
            clock: Arc::clone(&clock),
            diag: Diag::open(home, clock),
            clients: AtomicUsize::new(0),
            next_client: AtomicU64::new(0),
        }
    }
}
/// One relay: the session connection, and the writer the next command for
/// it uses. The relay thread owns the reader; both halves close together.
struct Relay {
    session: String,
    epoch: u64,
    writer: UnixStream,
}

/// Serves one client connection: `hub_hello`, then one acknowledgement per
/// hub command, and a relay thread per session commanded on it. Returning
/// shuts down every relay stream of the connection, so each session sees
/// this client leave.
pub(crate) fn serve_connection(stream: UnixStream, hub: Arc<Hub>) {
    let n = hub.next_client.fetch_add(1, Ordering::SeqCst) + 1;
    hub.clients.fetch_add(1, Ordering::SeqCst);
    hub.diag
        .info("client_connected", &format!("Client {n} connected."));
    let writer = match stream.try_clone() {
        Ok(writer) => Arc::new(Mutex::new(writer)),
        Err(_) => {
            disconnect(&hub, n);
            return;
        }
    };
    send(
        &writer,
        &hub,
        "hub_hello",
        Map::from_iter([(
            "fiber_version".to_owned(),
            Value::String(hub.fiber_version.clone()),
        )]),
    );
    let relays: Arc<Mutex<Vec<Relay>>> = Arc::new(Mutex::new(Vec::new()));
    let mut read = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while buf
                    .last()
                    .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
                {
                    buf.pop();
                }
                on_command(&buf, &hub, &writer, &relays);
            }
        }
    }
    let mut relays = lock(&relays);
    for entry in relays.drain(..) {
        match entry.writer.shutdown(Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
    }
    drop(relays);
    disconnect(&hub, n);
}

fn disconnect(hub: &Hub, n: u64) {
    hub.clients.fetch_sub(1, Ordering::SeqCst);
    hub.diag
        .info("client_disconnected", &format!("Client {n} disconnected."));
}

fn on_command(
    bytes: &[u8],
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Vec<Relay>>>,
) {
    let line = match classify(bytes) {
        Ok(line) => line,
        Err(id) => {
            reject(
                writer,
                hub,
                id.as_ref(),
                &ErrorCode::Malformed,
                "A command is one JSON object per line, with a string `id` and `command`.",
            );
            return;
        }
    };
    if let Some(session) = line.session_id {
        relay_command(&line.id, &session, &line.value, hub, writer, relays);
        return;
    }
    match line.command.as_str() {
        "start" => on_start(&line.id, &line.args, hub, writer),
        "status" => on_status(&line.id, &line.args, hub, writer),
        command => {
            let message = format!("`{command}` is not a hub command.");
            reject(
                writer,
                hub,
                Some(&line.id),
                &ErrorCode::UnknownCommand,
                &message,
            );
        }
    }
}

struct Classified {
    id: CommandId,
    command: String,
    session_id: Option<String>,
    value: Value,
    args: Map<String, Value>,
}

/// `Err` carries `command_id` when the line had a string `id`.
fn classify(bytes: &[u8]) -> Result<Classified, Option<CommandId>> {
    let text = std::str::from_utf8(bytes).map_err(|_| None)?;
    let value: Value = serde_json::from_str(text).map_err(|_| None)?;
    let Some(map) = value.as_object() else {
        return Err(None);
    };
    let id = match map.get("id") {
        Some(Value::String(id)) => Some(CommandId(id.clone())),
        _ => None,
    };
    if map
        .keys()
        .any(|key| key != "id" && key != "command" && key != "session_id" && key != "args")
    {
        return Err(id);
    }
    let Some(id) = id else {
        return Err(None);
    };
    let Some(Value::String(command)) = map.get("command") else {
        return Err(Some(id));
    };
    let args = match map.get("args") {
        None => Map::new(),
        Some(Value::Object(args)) => args.clone(),
        Some(_) => return Err(Some(id)),
    };
    let session_id = match map.get("session_id") {
        None => None,
        Some(Value::String(session)) => Some(session.clone()),
        Some(_) => return Err(Some(id)),
    };
    Ok(Classified {
        id,
        command: command.clone(),
        session_id,
        value,
        args,
    })
}

fn on_start(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
) {
    let Some((workspace, model, content)) = start_args(args) else {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    };
    match start::run(hub, workspace, model, content) {
        Outcome::Accepted { session_id } => {
            accept(writer, hub, id, Value::String(session_id.0), "session_id")
        }
        Outcome::Rejected { code, message } => {
            reject(writer, hub, Some(id), &code, &message);
        }
    }
}

fn on_status(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
) {
    if contains_null(&Value::Object(args.clone())) || !args.is_empty() {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    }
    let mut result = Map::new();
    result.insert(
        "clients".to_owned(),
        Value::from(hub.clients.load(Ordering::SeqCst)),
    );
    result.insert(
        "fiber_version".to_owned(),
        Value::String(hub.fiber_version.clone()),
    );
    result.insert("running".to_owned(), Value::Bool(true));
    accept_result(writer, hub, id, Value::Object(result));
}

/// `start`'s `args`: `workspace` (required string), `model` (optional
/// string), `content` (optional, passed through as JSON). A wrong,
/// missing or extra key, or an explicit `null`, is `None`.
fn start_args(args: &Map<String, Value>) -> Option<(&str, Option<&str>, Option<&Value>)> {
    if contains_null(&Value::Object(args.clone())) {
        return None;
    }
    if args
        .keys()
        .any(|key| key != "workspace" && key != "model" && key != "content")
    {
        return None;
    }
    let workspace = args.get("workspace")?.as_str()?;
    let model = match args.get("model") {
        None => None,
        Some(Value::String(model)) => Some(model.as_str()),
        Some(_) => return None,
    };
    Some((workspace, model, args.get("content")))
}

/// Passes the line to the session's socket without the `session_id` key. A
/// session the hub cannot reach is rejected `session_not_found`.
fn relay_command(
    id: &CommandId,
    session: &str,
    value: &Value,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Vec<Relay>>>,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    let mut stripped = object.clone();
    stripped.remove("session_id");
    let mut bytes = match serde_json::to_vec(&stripped) {
        Ok(bytes) => bytes,
        Err(_) => return,
    };
    bytes.push(b'\n');
    {
        let mut entries = lock(relays);
        if let Some(at) = entries.iter().position(|entry| entry.session == session) {
            if entries
                .get(at)
                .is_some_and(|entry| write_all(&entry.writer, &bytes).is_ok())
            {
                return;
            }
            entries.remove(at);
        }
    }
    let socket = hub.home.join("run").join(session);
    match UnixStream::connect(&socket) {
        Ok(stream) => {
            if write_all(&stream, &bytes).is_err() {
                not_found(writer, hub, id, session);
                return;
            }
            let reader = match stream.try_clone() {
                Ok(reader) => reader,
                Err(_) => {
                    not_found(writer, hub, id, session);
                    return;
                }
            };
            let mut entries = lock(relays);
            let epoch = entries.iter().map(|entry| entry.epoch).max().unwrap_or(0) + 1;
            // The thread may run before its entry is pushed: on session
            // EOF it only removes an entry it finds.
            let relayed = thread::Builder::new().name("hub-relay".to_owned()).spawn({
                let writer = Arc::clone(writer);
                let relays = Arc::clone(relays);
                let session = session.to_owned();
                move || relay(epoch, &session, reader, &writer, &relays)
            });
            // A thread that never started leaves no entry: the next
            // command for the session reconnects.
            if relayed.is_ok() {
                entries.push(Relay {
                    session: session.to_owned(),
                    epoch,
                    writer: stream,
                });
            }
        }
        Err(_) => not_found(writer, hub, id, session),
    }
}

fn not_found(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, session: &str) {
    reject(
        writer,
        hub,
        Some(id),
        &ErrorCode::SessionNotFound,
        &format!("No running session `{session}`."),
    );
}

/// Copies every session line back verbatim onto the client's shared writer.
/// The session closing its socket drops the map entry; the next command
/// for it reconnects.
fn relay(
    epoch: u64,
    session: &str,
    reader: UnixStream,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Vec<Relay>>>,
) {
    let mut read = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let mut out = lock(writer);
                if out.write_all(&buf).and_then(|()| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    let mut relays = lock(relays);
    if let Some(at) = relays
        .iter()
        .position(|entry| entry.session == session && entry.epoch == epoch)
    {
        relays.remove(at);
    }
}

fn accept(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, value: Value, key: &str) {
    let mut result = Map::new();
    result.insert(key.to_owned(), value);
    accept_result(writer, hub, id, Value::Object(result));
}

fn accept_result(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, id: &CommandId, result: Value) {
    let mut payload = Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.0.clone()));
    payload.insert("result".to_owned(), result);
    send(writer, hub, "command_accepted", payload);
}

fn reject(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: Option<&CommandId>,
    code: &ErrorCode,
    message: &str,
) {
    let mut payload = Map::new();
    payload.insert(
        "code".to_owned(),
        serde_json::to_value(code).unwrap_or(Value::String("io_failed".to_owned())),
    );
    if let Some(id) = id {
        payload.insert("command_id".to_owned(), Value::String(id.0.clone()));
    }
    payload.insert("message".to_owned(), Value::String(message.to_owned()));
    send(writer, hub, "command_rejected", payload);
}

fn send(writer: &Arc<Mutex<UnixStream>>, hub: &Hub, kind: &str, payload: Map<String, Value>) {
    let line = HubLine {
        kind: kind.to_owned(),
        ts: crate::diag::wall_ms(hub.clock.wall()),
        schema_version: SCHEMA_VERSION,
        payload,
    };
    let mut bytes = serde_json::to_vec(&line).unwrap_or_default();
    bytes.push(b'\n');
    let mut writer = lock(writer);
    writer
        .write_all(&bytes)
        .and_then(|()| writer.flush())
        .unwrap_or(());
}

fn write_all(stream: &UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    let mut stream = stream;
    stream.write_all(bytes)?;
    stream.flush()
}

/// Whether `value` holds an explicit `null`: an optional key is absent,
/// never `null`.
fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
