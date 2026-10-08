//! One client connection: `hub_hello` first, hub-command dispatch for
//! `start`, `status`, `prompt_history`, `feed`, `dismiss`, `recent`,
//! `sessions` and `delete`, and the relay to session sockets. Every connection also hears
//! `attention` (`crate::attention`) after its hello.
//!
//! A command with a `session_id` is for that session: the relay passes it
//! to the session's socket (`crate::relay`). A command without one is for
//! the hub. Every connection
//! opens with `hub_hello`, before any acknowledgement.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake, wall_ms};
use contract::events::CommandResult;
use contract::{CommandId, ErrorCode, HubLine, SCHEMA_VERSION};
use serde_json::{Map, Value};

use crate::Starter;
use crate::diag::Diag;
use crate::feed::{Feed, answer, on_feed};
use crate::relay::Relays;
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
    /// The injected clock, for `ts`, `start`'s deadline and the idle wait.
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) diag: Diag,
    /// The feed `feed`, `dismiss` and `recent` answer from.
    pub(crate) feed: Arc<Feed>,
    next_client: AtomicU64,
    /// The open connections and the idle timer, under one lock.
    conns: Mutex<Conns>,
    /// Woken on every clock move, every change to `conns` and every signal.
    pub(crate) tick: Arc<Tick>,
    /// `tick` as the clock's subscriber: kept alive so advances wake the
    /// idle wait.
    wake: Arc<dyn Wake>,
    /// Held across a resume: one at a time per hub, so two commands for
    /// one exited session start one process.
    pub(crate) resume_gate: Mutex<()>,
    /// Tests only: a one-shot pause run before `hub_hello` is sent.
    #[cfg(test)]
    pub(crate) before_hello: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot pause before the next forwarded session line.
    #[cfg(test)]
    pub(crate) before_forward: ForwardHook,
    /// Tests only: a one-shot pause before an accepted `subscribe` is kept.
    #[cfg(test)]
    pub(crate) before_accepted: ForwardHook,
    /// Tests only: a one-shot pause after a failed write, before the reconnect reads the kept subscription.
    #[cfg(test)]
    pub(crate) before_join: JoinHook,
}

/// The open connections and the idle timer. `zero_since` is `Some` exactly
/// while `open` is empty: the instant the count last reached 0.
struct Conns {
    /// Every connection the idle wait counts, and its shutdown handle.
    open: Vec<(u64, UnixStream)>,
    zero_since: Option<Instant>,
}

/// What one pass of [`Hub::idle_wait`] found.
pub(crate) enum Idle {
    /// A signal arrived: its number.
    Signal(i32),
    /// No client for the whole `idle_exit`: the exit is claimed.
    Expired,
    /// Woken by a clock move or a change: check again.
    Woken,
}

impl Hub {
    /// Opens the hub in `home`: the diagnostic log, pruned, before the
    /// caller writes `hub_started`. The open count starts at 0, so the idle
    /// timer starts now.
    pub(crate) fn new(
        home: &Path,
        fiber_version: &str,
        starter: Arc<dyn Starter>,
        clock: Arc<dyn Clock>,
        diag: Diag,
    ) -> Self {
        let tick = Arc::new(Tick::default());
        let wake = Arc::clone(&tick);
        let wake: Arc<dyn Wake> = wake;
        clock.subscribe(Arc::downgrade(&wake));
        let zero_since = Some(clock.now());
        let feed = Arc::new(Feed::new(home, Arc::clone(&clock)));
        Self {
            home: home.to_path_buf(),
            fiber_version: fiber_version.to_owned(),
            starter,
            clock,
            diag,
            feed,
            next_client: AtomicU64::new(0),
            conns: Mutex::new(Conns {
                open: Vec::new(),
                zero_since,
            }),
            tick,
            wake,
            resume_gate: Mutex::new(()),
            #[cfg(test)]
            before_hello: Mutex::new(None),
            #[cfg(test)]
            before_forward: Mutex::new(None),
            #[cfg(test)]
            before_accepted: Mutex::new(None),
            #[cfg(test)]
            before_join: Mutex::new(None),
        }
    }

    /// The open client connections, this connection's asker included.
    pub(crate) fn clients(&self) -> usize {
        lock(&self.conns).open.len()
    }

    /// When the open count last reached 0; `None` while a client is open.
    #[cfg(test)]
    pub(crate) fn zero_since(&self) -> Option<Instant> {
        lock(&self.conns).zero_since
    }

    /// Wakes the idle wait: the signal arm calls it after recording a
    /// signal.
    pub(crate) fn waker(&self) -> Arc<dyn Wake> {
        Arc::clone(&self.wake)
    }

    /// Counts a connection the accept loop holds: under the connection
    /// lock, so the idle exit's claim sees it. Exiting once the hub is
    /// exiting: the caller drops the stream unanswered, EOF with no
    /// `hub_hello`, and the client retries. Dropped when the shutdown clone
    /// fails: count nothing, serve nothing, and continue accepting.
    pub(crate) fn poll_accept(&self, stream: &UnixStream, stop: &AtomicBool) -> Accept {
        let mut conns = lock(&self.conns);
        if stop.load(Ordering::SeqCst) {
            return Accept::Exiting;
        }
        let counted = self.register_with(|| stream.try_clone(), &mut conns);
        drop(conns);
        // Woken after the connection lock is released: the idle wait takes
        // the tick lock before the connection lock.
        self.tick.wake();
        match counted {
            Some(n) => Accept::Counted(n),
            None => Accept::Dropped,
        }
    }

    /// Counts one connection, for a caller with no exit to check.
    /// `None` when the shutdown clone fails: count nothing, serve nothing.
    #[cfg(test)]
    pub(crate) fn register(&self, stream: &UnixStream) -> Option<u64> {
        let mut conns = lock(&self.conns);
        let counted = self.register_with(|| stream.try_clone(), &mut conns);
        drop(conns);
        self.tick.wake();
        counted
    }

    fn register_with(
        &self,
        clone: impl FnOnce() -> std::io::Result<UnixStream>,
        conns: &mut Conns,
    ) -> Option<u64> {
        // Clone first: a failure counts nothing, serves nothing, and
        // consumes no client number.
        let Ok(shutdown) = clone() else {
            return None;
        };
        let n = self.next_client.fetch_add(1, Ordering::SeqCst) + 1;
        conns.open.push((n, shutdown));
        conns.zero_since = None;
        Some(n)
    }

    /// Rolls back a counted connection whose serving thread never started.
    /// Never connected, so no `client_disconnected` line.
    pub(crate) fn rollback(&self, n: u64) {
        self.release(n);
    }

    /// Removes connection `n`; when the count reaches 0 the idle timer
    /// starts now. Wakes the idle wait.
    fn release(&self, n: u64) {
        let mut conns = lock(&self.conns);
        if let Some(at) = conns.open.iter().position(|(id, _)| *id == n) {
            conns.open.remove(at);
            if conns.open.is_empty() {
                conns.zero_since = Some(self.clock.now());
            }
        }
        drop(conns);
        self.tick.wake();
    }

    /// One pass of the idle wait. A pending signal returns it. With no
    /// client open since `zero_since` for the whole `idle_exit`, claims the
    /// exit under the connection lock: sets `stop`, so an accept racing the
    /// claim sees it and drops its stream unanswered. Otherwise parks on the
    /// clock until `zero_since + idle_exit`, or with no deadline while a
    /// client is open, woken by clock moves, changes and signals.
    pub(crate) fn idle_wait(
        &self,
        idle_exit: Duration,
        stop: &AtomicBool,
        got: &AtomicI32,
    ) -> Idle {
        // Taken before the checks and held into the wait: a change or a
        // signal that lands in between blocks in `Tick::wake` until this
        // thread waits, instead of waking nobody.
        let guard = lock(&self.tick.held);
        let signal = got.swap(0, Ordering::SeqCst);
        if signal != 0 {
            return Idle::Signal(signal);
        }
        let until = {
            let conns = lock(&self.conns);
            // An `idle_exit` past the end of time never expires.
            match conns
                .zero_since
                .and_then(|since| since.checked_add(idle_exit))
            {
                Some(until) if self.clock.now() >= until => {
                    stop.store(true, Ordering::SeqCst);
                    return Idle::Expired;
                }
                until => until,
            }
        };
        let mut slot = Some(guard);
        self.clock.wait_until(until, &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            slot = Some(match bound {
                Some(limit) => {
                    self.tick
                        .moved
                        .wait_timeout(guard, limit)
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
        Idle::Woken
    }

    /// Shuts down every client connection: each session sees its clients
    /// leave. Sessions are untouched.
    pub(crate) fn shutdown_clients(&self) {
        let conns = lock(&self.conns);
        for (_, stream) in &conns.open {
            match stream.shutdown(Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// Tests only: a one-shot pause before the next forwarded session line:
/// the line about to be written, and the relays, with no lock held.
#[cfg(test)]
type ForwardHook = Mutex<Option<Box<dyn FnOnce(&[u8], &Arc<Mutex<Relays>>) + Send>>>;

/// Tests only: a one-shot pause after a failed write, before the reconnect
/// reads the kept subscription, with no lock held.
#[cfg(test)]
type JoinHook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

/// What `poll_accept` decided for one accepted stream.
pub(crate) enum Accept {
    /// Counted under the connection lock: serve it as `n`.
    Counted(u64),
    /// The shutdown clone failed: count nothing, serve nothing.
    Dropped,
    /// The hub is exiting: drop unanswered.
    Exiting,
}

/// Woken on every clock move, every change to the open connections and
/// every signal.
#[derive(Default)]
pub(crate) struct Tick {
    held: Mutex<()>,
    moved: Condvar,
}

impl Wake for Tick {
    fn wake(&self) {
        // Taken before the notify, so a waiter that has checked and not
        // yet parked cannot miss it.
        let _held = lock(&self.held);
        self.moved.notify_all();
    }
}

impl Tick {
    /// Returns once `clock` reads `until` or later.
    pub(crate) fn until(&self, clock: &dyn Clock, until: Instant) {
        loop {
            let guard = lock(&self.held);
            if clock.now() >= until {
                return;
            }
            let mut slot = Some(guard);
            clock.wait_until(Some(until), &mut |bound| {
                let Some(guard) = slot.take() else {
                    return;
                };
                slot = Some(match bound {
                    Some(limit) => {
                        self.moved
                            .wait_timeout(guard, limit)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => self
                        .moved
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner),
                });
            });
        }
    }
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
    #[cfg(test)]
    if let Some(pause) = lock(&hub.before_hello).take() {
        pause();
    }
    send(
        &writer,
        &hub,
        "hub_hello",
        Map::from_iter([(
            "fiber_version".to_owned(),
            Value::String(hub.fiber_version.clone()),
        )]),
    );
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let heard = hub.feed.attention.listen(Arc::clone(&writer));
    let mut read = BufReader::new(stream);
    let mut buf = Vec::new();
    let mut fed = None;
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                on_command(&buf, &hub, &writer, &relays, &mut fed);
            }
        }
    }
    // Fails a blocked writer: the write half only, since a client that
    // half-closed already disconnected the socket, and shutting down a
    // disconnected read half fails without unblocking anything.
    read.get_ref().shutdown(Shutdown::Write).unwrap_or(());
    if let Some(fed) = fed {
        hub.feed.unsubscribe(fed);
    }
    if let Some(heard) = heard {
        hub.feed.attention.unlisten(heard);
    }
    lock(&relays).close_all();
    disconnect(&hub, n);
}

/// Logs the departure, then releases the count: once the count shows it
/// gone, the line is written, and the idle exit's `hub_stopped` follows it.
fn disconnect(hub: &Hub, n: u64) {
    hub.diag
        .info("client_disconnected", &format!("Client {n} disconnected."));
    hub.release(n);
}

fn on_command(
    bytes: &[u8],
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
    fed: &mut Option<u64>,
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
        crate::relay::relay_command(&line.id, &session, &line.value, hub, writer, relays);
        return;
    }
    match line.command.as_str() {
        "start" => on_start(&line.id, &line.args, hub, writer),
        "status" => on_status(&line.id, &line.args, hub, writer),
        "prompt_history" => match crate::prompt_history::answer(&hub.home, &line.args) {
            Ok(result) => accept_result(writer, hub, &line.id, result),
            Err((code, message)) => reject(writer, hub, Some(&line.id), &code, message),
        },
        "feed" => on_feed(&line.id, &line.args, hub, writer, fed),
        "dismiss" => answer(writer, hub, &line.id, hub.feed.dismiss(&line.args)),
        "recent" => answer(writer, hub, &line.id, hub.feed.recent(&line.args)),
        "sessions" => answer(
            writer,
            hub,
            &line.id,
            crate::sessions::answer(&hub.feed, &hub.home, &line.args),
        ),
        "delete" => answer(
            writer,
            hub,
            &line.id,
            crate::delete::delete(hub, &line.args),
        ),
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
            let result = CommandResult::Start { session_id };
            accept_result(
                writer,
                hub,
                id,
                serde_json::to_value(&result).unwrap_or(Value::Null),
            );
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
    let result = CommandResult::Status {
        running: true,
        fiber_version: hub.fiber_version.clone(),
        clients: hub.clients(),
    };
    accept_result(
        writer,
        hub,
        id,
        serde_json::to_value(&result).unwrap_or(Value::Null),
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

pub(crate) fn accept_result(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: &CommandId,
    result: Value,
) {
    let mut payload = Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.0.clone()));
    payload.insert("result".to_owned(), result);
    send(writer, hub, "command_accepted", payload);
}

pub(crate) fn reject(
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

pub(crate) fn send(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    kind: &str,
    payload: Map<String, Value>,
) {
    let line = HubLine {
        kind: kind.to_owned(),
        ts: wall_ms(hub.clock.wall()),
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

pub(crate) fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
