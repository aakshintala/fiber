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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::{CommandId, ErrorCode, HubLine, SCHEMA_VERSION};
use serde_json::{Map, Value};

use crate::Starter;
use crate::diag::Diag;
use crate::start::{self, Outcome};

/// What the hub shares across its connections: home, the version it
/// reports, how it starts sessions, the clock, the diagnostic log, and the
/// open connections `status` answers with.
pub(crate) struct Hub {
    /// Fiber home: `run/<session_id>` is resolved under it.
    pub(crate) home: PathBuf,
    fiber_version: String,
    /// How `start` runs the session command.
    pub(crate) starter: Arc<dyn Starter>,
    /// The injected clock, for `ts` and `start`'s deadline.
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) diag: Diag,
    next_client: AtomicU64,
    /// Every connection the idle wait counts, and its shutdown handle.
    conns: Mutex<Vec<(u64, UnixStream)>>,
    /// Bumped on every arrival and departure: the idle wait exits only
    /// when no client has been connected for the whole `idle_exit`.
    activity: AtomicU64,
    /// Woken on every clock move and every arrival and departure.
    tick: Arc<Tick>,
    /// The clock's subscriber: kept alive so advances wake the idle wait.
    _wake: Arc<dyn Wake>,
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
        let tick = Arc::new(Tick::default());
        let wake = Arc::clone(&tick);
        let wake: Arc<dyn Wake> = wake;
        clock.subscribe(Arc::downgrade(&wake));
        Self {
            home: home.to_path_buf(),
            fiber_version: fiber_version.to_owned(),
            starter,
            clock: Arc::clone(&clock),
            diag: Diag::open(home, clock),
            next_client: AtomicU64::new(0),
            conns: Mutex::new(Vec::new()),
            activity: AtomicU64::new(0),
            tick,
            _wake: wake,
        }
    }

    /// The open client connections, this connection's asker included.
    pub(crate) fn clients(&self) -> usize {
        lock(&self.conns).len()
    }

    /// Every arrival and departure, in order.
    pub(crate) fn activity(&self) -> u64 {
        self.activity.load(Ordering::SeqCst)
    }

    /// Counts a connection the accept loop holds: under the connection
    /// lock, so the idle exit's check and claim see it. Exiting once the hub
    /// is exiting: the caller drops the stream unanswered, EOF with no
    /// `hub_hello`, and the client retries. Dropped when the shutdown
    /// clone fails: count nothing, serve nothing, and continue accepting.
    pub(crate) fn poll_accept(&self, stream: &UnixStream, stop: &AtomicBool) -> Accept {
        let mut conns = lock(&self.conns);
        if stop.load(Ordering::SeqCst) {
            return Accept::Exiting;
        }
        match self.register_with(|| stream.try_clone(), &mut conns) {
            Some(n) => Accept::Counted(n),
            None => Accept::Dropped,
        }
    }

    /// Counts one connection, for a caller with no exit to check.
    /// `None` when the shutdown clone fails: count nothing, serve nothing.
    #[cfg(test)]
    pub(crate) fn register(&self, stream: &UnixStream) -> Option<u64> {
        let mut conns = lock(&self.conns);
        self.register_with(|| stream.try_clone(), &mut conns)
    }

    fn register_with(
        &self,
        clone: impl FnOnce() -> std::io::Result<UnixStream>,
        conns: &mut Vec<(u64, UnixStream)>,
    ) -> Option<u64> {
        // Clone first: a failure counts nothing, serves nothing, and
        // consumes no client number.
        let Ok(shutdown) = clone() else {
            return None;
        };
        let n = self.next_client.fetch_add(1, Ordering::SeqCst) + 1;
        conns.push((n, shutdown));
        self.activity.fetch_add(1, Ordering::SeqCst);
        self.tick.wake();
        Some(n)
    }

    /// Rolls back a counted connection whose serving thread never started:
    /// removes it, bumps activity, and wakes, before the stream is dropped.
    /// Never connected, so no `client_disconnected` line.
    pub(crate) fn rollback(&self, n: u64) {
        let mut conns = lock(&self.conns);
        if let Some(at) = conns.iter().position(|(id, _)| *id == n) {
            conns.remove(at);
        }
        drop(conns);
        self.activity.fetch_add(1, Ordering::SeqCst);
        self.tick.wake();
    }

    /// Whether no client is connected and none arrived since `seen`.
    pub(crate) fn quiet(&self, seen: u64) -> bool {
        self.activity.load(Ordering::SeqCst) == seen && lock(&self.conns).is_empty()
    }

    /// Claims the idle exit under the connection lock: no client connected
    /// and none arrived since `seen`. An accept racing the claim either
    /// counts first, and the claim fails, or sees the claim, and its
    /// stream is dropped unanswered.
    pub(crate) fn claim_exit(&self, stop: &AtomicBool, seen: u64) -> bool {
        let conns = lock(&self.conns);
        if self.activity.load(Ordering::SeqCst) == seen && conns.is_empty() {
            stop.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// Parks until `until` on `clock`, woken early by client arrivals and
    /// departures and by clock moves.
    pub(crate) fn park_until(&self, clock: &dyn Clock, until: Instant) {
        // Taken before the clock is read and held into the wait: an
        // arrival or departure that lands in between blocks on it instead
        // of waking nobody.
        let guard = lock(&self.tick.held);
        if clock.now() >= until {
            return;
        }
        let mut slot = Some(guard);
        clock.wait_until(Some(until), &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            slot = Some(match bound {
                // The real clock never moves on its own: bound each park
                // so a signal is noticed within `POLL`.
                Some(limit) => {
                    self.tick
                        .moved
                        .wait_timeout(guard, limit.min(POLL))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .tick
                    .moved
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner),
            });
        });
    }

    /// Shuts down every client connection: each session sees its clients
    /// leave. Sessions are untouched.
    pub(crate) fn shutdown_clients(&self) {
        let conns = lock(&self.conns);
        for (_, stream) in conns.iter() {
            match stream.shutdown(Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// What `poll_accept` decided for one accepted stream.
pub(crate) enum Accept {
    /// Counted under the connection lock: serve it as `n`.
    Counted(u64),
    /// The shutdown clone failed: count nothing, serve nothing.
    Dropped,
    /// The hub is exiting: drop unanswered.
    Exiting,
}

/// How often the idle wait re-checks signals on the real clock. The fake
/// clock parks until woken, unaffected.
const POLL: Duration = Duration::from_millis(100);

/// Woken on every clock move and every client arrival or departure.
#[derive(Default)]
pub(crate) struct Tick {
    held: Mutex<()>,
    moved: Condvar,
}

impl Wake for Tick {
    fn wake(&self) {
        // Taken before the notify, so a waiter that has read the clock
        // and not yet parked cannot miss it.
        let _held = lock(&self.held);
        self.moved.notify_all();
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
/// this client leave. Tests only: production counts through
/// [`Hub::poll_accept`], then serves here.
#[cfg(test)]
pub(crate) fn serve_connection(stream: UnixStream, hub: Arc<Hub>) {
    let Some(n) = hub.register(&stream) else {
        return;
    };
    serve_counted(stream, hub, n);
}

/// Serves an accepted connection already counted by [`Hub::poll_accept`].
pub(crate) fn serve_counted(stream: UnixStream, hub: Arc<Hub>, n: u64) {
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
    let mut conns = lock(&hub.conns);
    if let Some(at) = conns.iter().position(|(id, _)| *id == n) {
        conns.remove(at);
    }
    drop(conns);
    hub.activity.fetch_add(1, Ordering::SeqCst);
    hub.tick.wake();
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
        Outcome::Accepted { session_id } => accept_result(
            writer,
            hub,
            id,
            serde_json::json!({"session_id": session_id.0}),
        ),
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
    if !args.is_empty() {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    }
    accept_result(
        writer,
        hub,
        id,
        serde_json::json!({"clients": hub.clients(), "fiber_version": hub.fiber_version, "running": true}),
    );
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

/// Whether `session` names a session the hub can reach: `s_` plus 16
/// lowercase hex digits, the shape the hub mints and `parse_session_id`
/// accepts. Anything else names no session, so it is rejected without
/// touching the filesystem: an absolute path or `..` never escapes `run/`,
/// and `"hub"` never routes back to the hub.
fn valid_session_id(session: &str) -> bool {
    let hex = session.strip_prefix("s_").unwrap_or("");
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
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
    if !valid_session_id(session) {
        not_found(writer, hub, id, session);
        return;
    }
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
            let epoch = next_epoch(&entries);
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

/// One more than the highest relay epoch in use, so a stale relay thread
/// never removes a fresh entry.
fn next_epoch(entries: &[Relay]) -> u64 {
    entries.iter().map(|entry| entry.epoch).max().unwrap_or(0) + 1
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
    if let Some(at) = relay_slot(&relays, session, epoch) {
        relays.remove(at);
    }
}

/// The entry a finished relay thread drops: its own session and epoch, so
/// a stale thread never drops a reconnect's entry.
fn relay_slot(entries: &[Relay], session: &str, epoch: u64) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry.session == session && entry.epoch == epoch)
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
