//! One stdio MCP server: the child Fiber started, one parked thread per
//! pipe, requests answered by id, and a stop that closes stdin, waits a
//! grace on the clock, then kills and reaps (`docs/mcp.md`, "Starting
//! servers"). A signal during startup stops every start through
//! [`stop_every_start`]. Time comes only from the injected
//! [`contract::clock::Clock`]; no process group is ever signalled: only a
//! server's own process, by pid.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use rustix::process::{Pid, Signal};
use serde_json::Value;

use crate::effects::Hints;
use crate::rpc::{
    Incoming, Outcome, decode_line, encode_error, encode_notification, encode_request,
    encode_result,
};

/// The protocol version Fiber speaks (`docs/mcp.md` has no number; the
/// current draft does).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// A stdout line past this long ends the reader, and the server counts as
/// gone: a flood cannot grow memory without bound.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// The grace between closing stdin and killing the child: the shell's
/// `GRACE` (`crates/tools/src/shell/command.rs`).
const GRACE: Duration = Duration::from_millis(800);

/// Every server child's pid, from its spawn until its reap: what
/// [`kill_every_server`] reaches.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Stops every server still starting, and every start after it: the flag
/// is sticky for the life of the process. It sets the flag and wakes the
/// parked handshakes; the stop and reap run on each start's own thread.
/// Idempotent: a second call wakes nobody new.
pub fn stop_every_start() {
    // Taken and dropped before any wake: a wake takes the shared lock, so
    // waking under this lock would join the two.
    let waiting = {
        let mut stopping = lock(&STOPPING);
        stopping.stopped = true;
        std::mem::take(&mut stopping.waiting)
    };
    for waker in waiting {
        if let Some(waker) = waker.upgrade() {
            waker.wake();
        }
    }
}

#[derive(Default)]
struct StoppingState {
    stopped: bool,
    waiting: Vec<Weak<dyn Wake>>,
}

static STOPPING: Mutex<StoppingState> = Mutex::new(StoppingState {
    stopped: false,
    waiting: Vec::new(),
});

/// The handshake's cancel, fired by [`stop_every_start`].
struct Stopping;

impl Cancel for Stopping {
    fn is_cancelled(&self) -> bool {
        lock(&STOPPING).stopped
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut stopping = lock(&STOPPING);
        // A finished start drops its bridge and leaves a dead entry: pruned
        // here, so the list holds only the starts still running.
        stopping.waiting.retain(|listed| listed.upgrade().is_some());
        stopping.waiting.push(waker);
    }
}

/// Sends SIGKILL to every server child not yet reaped, at once
/// (`docs/invocation.md`, "Shutdown": the bound). A pid of 1 or less is
/// never signalled.
pub fn kill_every_server() {
    // Held while `kill` runs: a reap unlists its pid under this lock before
    // it waits, so every pid signalled here is still unreaped and cannot
    // have been reused.
    let live = lock(&LIVE);
    before_signal();
    signal(&live, Signal::KILL);
}

/// Runs while a signaller holds the lock that keeps its pids unreaped, just
/// before `kill`: the seam a test pauses on to force a reap against it.
#[cfg(test)]
fn before_signal() {
    tests::before_signal();
}

#[cfg(not(test))]
fn before_signal() {}

/// Runs as a reap is about to take `which` lock (`child` or `live`): the
/// seam a test waits on to know the reap contends before it asserts.
#[cfg(test)]
fn before_lock(which: &'static str) {
    tests::before_lock(which);
}

#[cfg(not(test))]
fn before_lock(_which: &'static str) {}

/// Sends `signal` to each of `pids`, leaving out every id [`refused`]
/// names.
fn signal(pids: &[u32], signal: Signal) {
    for pid in pids.iter().filter(|pid| !refused(**pid)) {
        if let Some(pid) = i32::try_from(*pid).ok().and_then(Pid::from_raw) {
            // A server already gone refuses the signal; its reap still runs.
            match rustix::process::kill_process(pid, signal) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// A pid of 1 or less is never a server's: `kill(-1)` reaches every
/// process the user owns, and `kill(0)` this process's own group. Tested
/// as a function, so a mutant of it signals nothing.
fn refused(pid: u32) -> bool {
    pid <= 1
}

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
    /// Its tools, in the order listed.
    pub tools: Vec<ListedTool>,
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
            return Err(StartError::StartFailed("Fiber is shutting down.".to_owned()));
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
        thread::spawn(move || write_stdin(stdin, incoming));
        let writer = Arc::new(writer);
        let reading = Arc::clone(&shared);
        let answering = Arc::downgrade(&writer);
        thread::spawn(move || {
            if let Some(stdout) = stdout {
                read_stdout(stdout, &reading, &answering);
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
        let handshake = |server: &Server| -> Result<Vec<ListedTool>, StartError> {
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
                    Err(_) => {
                        return Err(StartError::StartFailed(
                            "The server's tool list was not a result.".to_owned(),
                        ));
                    }
                };
                let object = match page.as_object() {
                    Some(object) => object,
                    None => {
                        return Err(StartError::StartFailed(
                            "The server's tool list was not a result.".to_owned(),
                        ));
                    }
                };
                match object.get("tools").and_then(Value::as_array) {
                    Some(listed) => tools.extend(listed.iter().map(ListedTool::read)),
                    None => {
                        return Err(StartError::StartFailed(
                            "The server's tool list was not a result.".to_owned(),
                        ));
                    }
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
                    // A signal during startup: the documented stop, then
                    // the failure the door never writes.
                    server.stop();
                    Err(StartError::StartFailed("Fiber is shutting down.".to_owned()))
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
            inner
                .child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
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
    fn read(entry: &Value) -> Self {
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
struct Shared {
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
    fn deliver(&self, id: u64, outcome: Outcome) {
        let mut state = lock(&self.inner);
        if let Some(slot) = state.pending.get_mut(&id) {
            *slot = Some(outcome);
            state.seq = state.seq.wrapping_add(1);
            drop(state);
            self.cv.notify_all();
        }
    }

    /// Marks the server gone and wakes every waiter.
    fn gone(&self) {
        let mut state = lock(&self.inner);
        state.gone = true;
        state.seq = state.seq.wrapping_add(1);
        drop(state);
        self.cv.notify_all();
    }
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes request lines to stdin. A write that never finishes blocks only
/// this thread: the waiting call still times out or cancels on the clock.
/// The thread ends when the sender is dropped, closing stdin.
fn write_stdin(stdin: Option<ChildStdin>, incoming: mpsc::Receiver<Vec<u8>>) {
    let Some(mut stdin) = stdin else {
        return;
    };
    for mut line in incoming {
        line.push(b'\n');
        if stdin.write_all(&line).is_err() {
            return;
        }
    }
}

/// Reads response lines and routes them: responses to their id's slot,
/// `ping` answered `{}`, any other server method answered `-32601`. A line
/// that is not a JSON object is ignored. Past [`MAX_LINE`] bytes on one
/// line, or EOF, the server counts as gone.
fn read_stdout(stdout: ChildStdout, shared: &Shared, writer: &Weak<mpsc::Sender<Vec<u8>>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_line(&mut reader) {
            ReadLine::Line(line) => match decode_line(&line) {
                Incoming::Response(response) => {
                    shared.deliver(response.id, response.outcome);
                }
                Incoming::ServerRequest(request) => {
                    let answer = match request.method.as_str() {
                        "ping" => encode_result(&request.id, &serde_json::json!({})),
                        _ => encode_error(&request.id, -32601, "Method not found"),
                    };
                    if let Some(writer) = writer.upgrade() {
                        match writer.send(answer.into_bytes()) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                }
                Incoming::Ignored => {}
            },
            ReadLine::Eof | ReadLine::TooLong => {
                shared.gone();
                return;
            }
        }
    }
}

enum ReadLine {
    Line(String),
    Eof,
    TooLong,
}

/// Reads one newline-delimited line, capped at [`MAX_LINE`] bytes: past the
/// cap the line is abandoned and the reader ends.
fn read_line(reader: &mut impl BufRead) -> ReadLine {
    let mut buf = Vec::new();
    match reader
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
    {
        Ok(0) => ReadLine::Eof,
        Ok(_) if buf.ends_with(b"\n") => {
            buf.pop();
            ReadLine::Line(String::from_utf8_lossy(&buf).into_owned())
        }
        Ok(_) if buf.len() > MAX_LINE => ReadLine::TooLong,
        Ok(_) => ReadLine::Line(String::from_utf8_lossy(&buf).into_owned()),
        Err(_) if buf.is_empty() => ReadLine::Eof,
        Err(_) => ReadLine::Line(String::from_utf8_lossy(&buf).into_owned()),
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
mod tests;
