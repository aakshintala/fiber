//! One stdio MCP server: the child Fiber started, one parked thread per
//! pipe, requests answered by id, and a stop that closes stdin, waits a
//! grace on the clock, then kills and reaps (`docs/mcp.md`, "Starting
//! servers"); a signal during startup stops every start through
//! [`crate::registry::stop_every_start`]. Time comes only from the injected
//! [`contract::clock::Clock`]; no process group is ever signalled: only a
//! server's own process, by pid.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use rustix::process::Signal;
use serde_json::Value;

use crate::effects::Hints;
use crate::registry::{LIVE, Stopping, before_lock, before_signal, lock, signal};
use crate::rpc::{Outcome, encode_notification, encode_request};

/// The protocol version Fiber speaks (`docs/mcp.md` has no number; the
/// current draft does).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The grace between closing stdin and killing the child: the shell's
/// `GRACE` (`crates/tools/src/shell/command.rs`).
const GRACE: Duration = Duration::from_millis(800);

/// One tool the server lists: its name, description, schema and hints, as
/// [`crate::tool`] declares them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListedTool {
    /// The server's own name for it.
    pub name: String,
    /// Its description; `""` when the server gave none.
    pub description: String,
    /// Its input schema; `{"type":"object"}` when the server gave none.
    pub schema: Value,
    /// Its hints; absent when the server gave none.
    pub hints: Hints,
}

/// Why [`Server::start`] failed: the session records it as
/// `mcp_server_failed` and leaves the server out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StartError {
    /// The spawn, `initialize` or `tools/list` failed. The message is
    /// Fiber's own sentence.
    StartFailed(String),
    /// Nothing answered before the startup deadline.
    Deadline,
}

/// A start refused because Fiber is shutting down.
fn shutting_down() -> StartError {
    StartError::StartFailed("Fiber is shutting down.".to_owned())
}

/// Why [`Server::call`] failed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallError {
    /// Nothing answered before the call's deadline.
    Timeout,
    /// The call was cancelled. `notifications/cancelled` was sent.
    Cancelled,
    /// The server is gone: it exited, or its reader ended.
    Gone,
    /// The server answered with a JSON-RPC error.
    JsonRpc {
        /// The error's code.
        code: i64,
        /// The error's message.
        message: String,
    },
}

/// A running server and its end of the wire.
pub(crate) struct Server {
    /// `None` once [`Server::stop`] ran: the child below was reaped exactly
    /// once, and [`Drop`] does nothing.
    inner: Option<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    /// Dropping the sender closes stdin: the writer thread's receive fails
    /// and it drops the pipe. The reader holds only a [`Weak`] to it, so
    /// closing here really closes: a strong clone in the reader would keep
    /// the channel open and EOF would never arrive during the grace.
    writer: Mutex<Option<std::sync::Arc<mpsc::Sender<Vec<u8>>>>>,
    /// The child, until [`Server::stop`] or [`Drop`] takes, kills and reaps
    /// it exactly once.
    child: Mutex<Option<Child>>,
    clock: Arc<dyn Clock>,
}

/// A server [`Server::start`] opened, with the tools it listed.
pub(crate) struct OpenServer {
    /// The running server.
    pub server: Server,
    /// Its raw `tools/list` entries, in the order listed.
    pub tools: Vec<Value>,
}

impl Server {
    /// Starts `command` with `args` in `workspace`, inheriting Fiber's
    /// environment with `env` overriding keys, and runs `initialize`,
    /// `notifications/initialized` and `tools/list` under
    /// `startup_timeout` on `clock`. `client_version` is Fiber's own
    /// version, sent as `clientInfo`. A failure kills and reaps the child
    /// exactly once.
    pub(crate) fn start(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        workspace: &Path,
        clock: &Arc<dyn Clock>,
        startup_timeout: Duration,
        client_version: &str,
    ) -> Result<OpenServer, StartError> {
        // Sticky: a start after the stop spawns nothing.
        if Stopping.is_cancelled() {
            return Err(shutting_down());
        }
        let stopping = Stopping;
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().map_err(|error| {
            StartError::StartFailed(format!("The server could not be started: {error}."))
        })?;
        lock(&LIVE).push(child.id());
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let shared = Arc::new(Shared::default());
        let (writer, incoming) = mpsc::channel();
        thread::spawn(move || crate::pipes::write_stdin(stdin, incoming));
        let writer = Arc::new(writer);
        let reading = Arc::clone(&shared);
        let answering = Arc::downgrade(&writer);
        thread::spawn(move || {
            if let Some(stdout) = stdout {
                crate::pipes::read_stdout(stdout, &reading, &answering);
            } else {
                reading.gone();
            }
        });
        clock.subscribe(Arc::downgrade(&(Arc::clone(&shared) as Arc<dyn Wake>)));
        let mut server = Server {
            inner: Some(Inner {
                shared,
                writer: Mutex::new(Some(writer)),
                child: Mutex::new(Some(child)),
                clock: Arc::clone(clock),
            }),
        };
        let deadline = clock
            .now()
            .checked_add(startup_timeout)
            .unwrap_or(clock.now());
        // One shutdown on `Err`: every failure path below returns through
        // here, so no arm repeats `shutdown`. `NoCancel` never fires, so
        // `Cancelled` is just another failed start, not a deadline.
        let not_a_list =
            || StartError::StartFailed("The server's tool list was not a result.".to_owned());
        let handshake = |server: &Server| -> Result<Vec<Value>, StartError> {
            match server.request(
                "initialize",
                &serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "fiber", "version": client_version},
                }),
                deadline,
                &stopping,
            ) {
                Ok(value) if value.is_object() => {}
                Err(CallError::Timeout) => return Err(StartError::Deadline),
                Ok(_) | Err(_) => {
                    return Err(StartError::StartFailed(
                        "The server's `initialize` reply was not a result.".to_owned(),
                    ));
                }
            }
            server.notify("notifications/initialized", &serde_json::json!({}));
            let mut tools = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let params = match &cursor {
                    Some(cursor) => serde_json::json!({"cursor": cursor}),
                    None => serde_json::json!({}),
                };
                let page = match server.request("tools/list", &params, deadline, &stopping) {
                    Ok(value) => value,
                    Err(CallError::Timeout) => return Err(StartError::Deadline),
                    Err(_) => return Err(not_a_list()),
                };
                let object = match page.as_object() {
                    Some(object) => object,
                    None => return Err(not_a_list()),
                };
                match object.get("tools").and_then(Value::as_array) {
                    Some(listed) => tools.extend(listed.iter().cloned()),
                    None => return Err(not_a_list()),
                }
                cursor = object
                    .get("nextCursor")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
            Ok(tools)
        };
        match handshake(&server) {
            Ok(tools) => Ok(OpenServer { server, tools }),
            Err(error) => {
                if stopping.is_cancelled() {
                    // The documented stop, then the failure the door never writes.
                    server.stop();
                    Err(shutting_down())
                } else {
                    server.shutdown();
                    Err(error)
                }
            }
        }
    }

    /// Calls `tool` with `arguments`, waiting until `timeout` passes on the
    /// clock or `cancel` fires. A late response to a timed-out or cancelled
    /// id is discarded: the slot is removed on every exit path.
    pub(crate) fn call(
        &self,
        tool: &str,
        arguments: &Value,
        timeout: Duration,
        cancel: &dyn Cancel,
    ) -> Result<Value, CallError> {
        let Some(inner) = self.inner.as_ref() else {
            return Err(CallError::Gone);
        };
        let deadline = inner
            .clock
            .now()
            .checked_add(timeout)
            .unwrap_or(inner.clock.now());
        self.request(
            "tools/call",
            &serde_json::json!({"name": tool, "arguments": arguments}),
            deadline,
            cancel,
        )
    }

    /// Whether the server's output ended (it exited), or it has no connection.
    pub(crate) fn is_gone(&self) -> bool {
        self.inner
            .as_ref()
            .is_none_or(|inner| lock(&inner.shared.inner).gone)
    }

    /// Stops the server: closes stdin, sends SIGTERM to its process, waits
    /// on the clock until its output ends or the grace passes, then kills
    /// and reaps the child (`docs/invocation.md`, "Shutdown"). Idempotent:
    /// the child is taken, killed and reaped exactly once, and [`Drop`]
    /// repeats only what is left. `&self` because tools share the
    /// connection while [`Servers`] owns the shutdown.
    pub(crate) fn stop(&self) {
        if let Some(inner) = self.inner.as_ref() {
            inner.close_stdin();
            // Under the child's lock: a reap takes the child under it, so
            // the pid is unreaped while `kill` runs.
            let child = lock(&inner.child);
            if let Some(child) = child.as_ref() {
                before_signal();
                signal(&[child.id()], Signal::TERM);
            }
            drop(child);
            inner.wait_gone();
        }
        self.reap();
    }

    /// Sends one request and waits for its response.
    fn request(
        &self,
        method: &str,
        params: &Value,
        deadline: Instant,
        cancel: &dyn Cancel,
    ) -> Result<Value, CallError> {
        let Some(inner) = self.inner.as_ref() else {
            return Err(CallError::Gone);
        };
        let id = inner.shared.next_id();
        inner.shared.insert(id);
        // The slot is removed on every exit path below, so a late response
        // finds no slot and is discarded.
        let line = encode_request(id, method, Some(params));
        inner.send(line);
        let bridge = CancelBridge::arm(&inner.shared);
        cancel.subscribe(Arc::downgrade(&(Arc::clone(&bridge) as Arc<dyn Wake>)));
        let outcome = loop {
            let view = inner.shared.view(id, cancel);
            if let Some(outcome) = view.response {
                break Ok(outcome);
            }
            if view.gone {
                break Err(CallError::Gone);
            }
            if view.cancelled {
                inner.send(encode_notification(
                    "notifications/cancelled",
                    Some(&serde_json::json!({"requestId": id})),
                ));
                break Err(CallError::Cancelled);
            }
            if inner.clock.now() >= deadline {
                break Err(CallError::Timeout);
            }
            park(
                inner.clock.as_ref(),
                &inner.shared,
                cancel,
                deadline,
                view.seq,
            );
        };
        drop(bridge);
        inner.shared.remove(id);
        match outcome {
            Ok(Outcome::Result(value)) => Ok(value),
            Ok(Outcome::Error { code, message }) => Err(CallError::JsonRpc { code, message }),
            Err(error) => Err(error),
        }
    }

    /// Sends a notification: no id, no answer.
    fn notify(&self, method: &str, params: &Value) {
        if let Some(inner) = self.inner.as_ref() {
            inner.send(encode_notification(method, Some(params)));
        }
    }

    /// Closes stdin by dropping the writer's sender.
    fn shutdown(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.close_stdin();
        }
    }

    /// Kills and reaps the child exactly once; later calls find none.
    fn reap(&self) {
        let child = self.inner.as_ref().and_then(|inner| {
            before_lock("child");
            lock(&inner.child).take()
        });
        if let Some(mut child) = child {
            // An exited child refuses the kill; the reap below still runs.
            match child.kill() {
                Ok(()) | Err(_) => {}
            }
            // Unlisted before the reap frees the pid for reuse.
            let pid = child.id();
            before_lock("live");
            lock(&LIVE).retain(|listed| *listed != pid);
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
        self.reap();
    }
}

impl ListedTool {
    /// Reads one `tools/list` entry. A nameless entry becomes `""`, and a
    /// missing description or schema takes the default the tool declares.
    pub(crate) fn read(entry: &Value) -> Self {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let schema = entry
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        let hints = entry
            .get("annotations")
            .map(Hints::from_annotations)
            .unwrap_or_default();
        Self {
            name,
            description,
            schema,
            hints,
        }
    }
}

impl Inner {
    /// Closes stdin by dropping the writer's sender.
    fn close_stdin(&self) {
        lock(&self.writer).take();
    }

    /// Waits until the server's output ends or [`GRACE`] passes on the
    /// clock.
    fn wait_gone(&self) {
        let until = self
            .clock
            .now()
            .checked_add(GRACE)
            .unwrap_or(self.clock.now());
        loop {
            let (gone, seq) = {
                let state = lock(&self.shared.inner);
                (state.gone, state.seq)
            };
            if gone || self.clock.now() >= until {
                return;
            }
            park(self.clock.as_ref(), &self.shared, &NoCancel, until, seq);
        }
    }

    /// Sends `line` to the writer thread. A failed send means the thread
    /// is gone, and the pending wait ends through `gone`.
    fn send(&self, line: String) {
        if let Some(writer) = lock(&self.writer).as_ref() {
            match writer.send(line.into_bytes()) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// What a wait loop sees, read under the shared lock after subscribing, so
/// a response that lands between the subscribe and the read is still
/// visible.
struct View {
    response: Option<Outcome>,
    gone: bool,
    cancelled: bool,
    seq: u64,
}

#[derive(Default)]
struct SharedState {
    seq: u64,
    next_id: u64,
    pending: BTreeMap<u64, Option<Outcome>>,
    gone: bool,
}

#[derive(Default)]
pub(crate) struct Shared {
    inner: Mutex<SharedState>,
    cv: Condvar,
}

impl Wake for Shared {
    fn wake(&self) {
        // The sequence moves under the same lock as the wait, so a clock
        // advance that lands before the condvar wait is still visible when
        // the waiter checks.
        let mut guard = lock(&self.inner);
        guard.seq = guard.seq.wrapping_add(1);
        drop(guard);
        self.cv.notify_all();
    }
}

impl Shared {
    /// The next request id. Ids start at 1 and are never reused: a `u64`
    /// counter a session cannot exhaust.
    fn next_id(&self) -> u64 {
        let mut state = lock(&self.inner);
        state.next_id += 1;
        state.next_id
    }

    fn insert(&self, id: u64) {
        lock(&self.inner).pending.insert(id, None);
    }

    fn remove(&self, id: u64) {
        lock(&self.inner).pending.remove(&id);
    }

    fn view(&self, id: u64, cancel: &dyn Cancel) -> View {
        let state = lock(&self.inner);
        View {
            response: state.pending.get(&id).and_then(|slot| slot.clone()),
            gone: state.gone,
            cancelled: cancel.is_cancelled(),
            seq: state.seq,
        }
    }

    /// Delivers `response` to its id's slot, or discards it when the slot
    /// is gone: a late response to a timed-out id never misroutes.
    pub(crate) fn deliver(&self, id: u64, outcome: Outcome) {
        let mut state = lock(&self.inner);
        if let Some(slot) = state.pending.get_mut(&id) {
            *slot = Some(outcome);
            state.seq = state.seq.wrapping_add(1);
            drop(state);
            self.cv.notify_all();
        }
    }

    /// Marks the server gone and wakes every waiter.
    pub(crate) fn gone(&self) {
        let mut state = lock(&self.inner);
        state.gone = true;
        state.seq = state.seq.wrapping_add(1);
        drop(state);
        self.cv.notify_all();
    }
}

/// True when the waiter stops waiting: a response landed (`seq` moved) or
/// the call was cancelled. A pure function of its inputs so a test pins all
/// four combinations without parking a thread.
fn should_stop(seq: u64, seen: u64, cancelled: bool) -> bool {
    seq != seen || cancelled
}

/// Blocks until woken or `until` passes on the clock, releasing every lock
/// first: the response lands under the shared lock, so joining ahead of
/// that release would deadlock.
fn park(clock: &dyn Clock, shared: &Shared, cancel: &dyn Cancel, until: Instant, seen: u64) {
    // Taken before `wait_until`, and held until the condvar wait, so a wake
    // blocks on this lock instead of notifying nobody.
    let mut slot = Some(lock(&shared.inner));
    clock.wait_until(Some(until), &mut |bound| {
        let Some(guard) = slot.take() else {
            return;
        };
        if should_stop(guard.seq, seen, cancel.is_cancelled()) {
            slot = Some(guard);
            return;
        }
        slot = Some(match bound {
            Some(bound) => {
                shared
                    .cv
                    .wait_timeout(guard, bound)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => shared
                .cv
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner),
        });
    });
}

/// The call's cancel reaches the wait through this bridge. It holds the
/// shared state weakly and is dropped when the wait ends, so a fired cancel
/// wakes only the waits still running.
struct CancelBridge(Weak<Shared>);

impl CancelBridge {
    fn arm(shared: &Arc<Shared>) -> Arc<Self> {
        Arc::new(Self(Arc::downgrade(shared)))
    }
}

impl Wake for CancelBridge {
    fn wake(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.wake();
        }
    }
}

/// A cancel that never fires, for `initialize` and `tools/list`: the
/// startup deadline, not a person, ends those waits.
struct NoCancel;

impl Cancel for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

#[cfg(test)]
#[path = "server_tests.rs"]
pub(crate) mod tests;
