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
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::tool::Cancel;
use rustix::process::Signal;
use serde::Serialize;
use serde_json::Value;

use crate::cache::Cached;
use crate::registry::{LIVE, Stopping, before_lock, before_signal, lock, signal};
use crate::rpc::{Named, Outcome, encode_notification, encode_request};
use crate::server_json::{ListedPrompt, ListedTool, entries};
use crate::wait::{CancelBridge, NoCancel, Shared, deadline, park};

/// The protocol version Fiber speaks (`docs/mcp.md` has no number; the
/// current draft does).
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The grace between closing stdin and killing the child: the shell's
/// `GRACE` (`crates/tools/src/shell/command.rs`).
const GRACE: Duration = Duration::from_millis(800);

/// How many request lines wait for the writer thread. Picked, not
/// measured: no doc sets a number. A full queue means the server is
/// wedged, so it counts as gone.
const WRITE_QUEUE: usize = 64;

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

/// Pages `method` (`tools/list` or `prompts/list`) to its end under the
/// handshake's shared startup `deadline`: the deadline is checked before
/// every page request, so a server answering every page at once with a
/// fresh cursor still ends at it (`docs/mcp.md`, "Starting servers"). A
/// missed deadline fails the start; any other failure runs `fail`, which
/// fails the start for tools and keeps no prompts for prompts.
fn list_pages<T: serde::de::DeserializeOwned>(
    server: &Server,
    clock: &Arc<dyn Clock>,
    method: &str,
    entry: &str,
    deadline: Instant,
    cancel: &dyn Cancel,
    fail: impl Fn() -> Result<Vec<T>, StartError>,
) -> Result<Vec<T>, StartError> {
    let mut listed = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        if clock.now() >= deadline {
            return Err(StartError::Deadline);
        }
        let params = match &cursor {
            Some(cursor) => serde_json::json!({"cursor": cursor}),
            None => serde_json::json!({}),
        };
        let page = match server.request(method, &params, deadline, cancel) {
            Ok(value) => value,
            Err(CallError::Timeout) => return Err(StartError::Deadline),
            Err(_) => return fail(),
        };
        let object = match page.as_object() {
            Some(object) => object,
            None => return fail(),
        };
        match object.get(entry).and_then(Value::as_array) {
            Some(found) => listed.extend(entries(found.clone())),
            None => return fail(),
        }
        cursor = object
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(listed)
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
    inner: Inner,
}

struct Inner {
    shared: Arc<Shared>,
    /// Dropping the sender closes stdin: the writer thread's receive fails
    /// and it drops the pipe. The reader holds only a [`Weak`] to it, so
    /// closing here really closes: a strong clone in the reader would keep
    /// the channel open and EOF would never arrive during the grace.
    writer: Mutex<Option<std::sync::Arc<mpsc::SyncSender<Vec<u8>>>>>,
    /// The child, until [`Server::stop`] or [`Drop`] takes, kills and reaps
    /// it exactly once.
    child: Mutex<Option<Child>>,
    clock: Arc<dyn Clock>,
}

/// A server [`Server::start`] opened, with the tools and prompts it listed.
pub(crate) struct OpenServer {
    /// The running server.
    pub server: Server,
    /// The tools and prompts it listed.
    pub listed: Cached,
}

impl Server {
    /// Starts `command` with `args` in `workspace`, inheriting Fiber's
    /// environment with `env` overriding keys, and runs `initialize`,
    /// `notifications/initialized`, `tools/list` when the server
    /// advertises tools, and `prompts/list` when it advertises prompts,
    /// under `startup_timeout` on `clock`. `client_version` is Fiber's own version, sent as
    /// `clientInfo`. A failure kills and reaps the child exactly once.
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
        let (writer, incoming) = mpsc::sync_channel(WRITE_QUEUE);
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
        let server = Server {
            inner: Inner {
                shared,
                writer: Mutex::new(Some(writer)),
                child: Mutex::new(Some(child)),
                clock: Arc::clone(clock),
            },
        };
        let deadline = deadline(clock.as_ref(), startup_timeout);
        // One shutdown on `Err`: every failure path below returns through
        // here, so no arm repeats `shutdown`. `NoCancel` never fires, so
        // `Cancelled` is just another failed start, not a deadline.
        let not_a_list =
            || StartError::StartFailed("The server's tool list was not a result.".to_owned());
        let handshake = |server: &Server| -> Result<Cached, StartError> {
            let initialize = match server.request(
                "initialize",
                &serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "fiber", "version": client_version},
                }),
                deadline,
                &stopping,
            ) {
                Ok(value) if value.is_object() => value,
                Err(CallError::Timeout) => return Err(StartError::Deadline),
                Ok(_) | Err(_) => {
                    return Err(StartError::StartFailed(
                        "The server's `initialize` reply was not a result.".to_owned(),
                    ));
                }
            };
            server.notify("notifications/initialized", &serde_json::json!({}));
            // `tools/list` runs only when `initialize` advertises the
            // `tools` capability as an object: a server advertising only
            // prompts may reject it, so without the capability the
            // start goes on with no tools (`docs/mcp.md`, "Starting
            // servers").
            let advertises_tools = initialize
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("tools"))
                .is_some_and(Value::is_object);
            let tools: Vec<ListedTool> = if advertises_tools {
                list_pages(
                    server,
                    clock,
                    "tools/list",
                    "tools",
                    deadline,
                    &stopping,
                    || Err(not_a_list()),
                )?
            } else {
                Vec::new()
            };
            // `prompts/list` runs only when `initialize` advertises the
            // `prompts` capability as an object, and a list that errors
            // or is not a result leaves the server with no prompts while
            // the start goes on; only a missed deadline fails it
            // (`docs/mcp.md`, "Prompts and resources").
            let advertises = initialize
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("prompts"))
                .is_some_and(Value::is_object);
            let prompts: Vec<ListedPrompt> = if advertises {
                list_pages(
                    server,
                    clock,
                    "prompts/list",
                    "prompts",
                    deadline,
                    &stopping,
                    || Ok(Vec::new()),
                )?
            } else {
                Vec::new()
            };
            Ok(Cached { tools, prompts })
        };
        match handshake(&server) {
            Ok(listed) => Ok(OpenServer { server, listed }),
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

    /// Calls `method` with `params`, waiting until `timeout` passes on the
    /// clock or `cancel` fires. A late response to a timed-out or cancelled
    /// id is discarded: the slot is removed on every exit path.
    pub(crate) fn call(
        &self,
        method: &str,
        params: &Named<'_>,
        timeout: Duration,
        cancel: &dyn Cancel,
    ) -> Result<Value, CallError> {
        let inner = &self.inner;
        let deadline = deadline(inner.clock.as_ref(), timeout);
        self.request(method, params, deadline, cancel)
    }

    /// Whether the server's output ended (it exited).
    pub(crate) fn is_gone(&self) -> bool {
        lock(&self.inner.shared.inner).gone
    }

    /// Stops the server: closes stdin, sends SIGTERM to its process, waits
    /// on the clock until its output ends or the grace passes, then kills
    /// and reaps the child (`docs/invocation.md`, "Shutdown"). Idempotent:
    /// the child is taken, killed and reaped exactly once, and [`Drop`]
    /// repeats only what is left. `&self` because tools share the
    /// connection while [`Servers`] owns the shutdown.
    pub(crate) fn stop(&self) {
        let inner = &self.inner;
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
        self.reap();
    }

    /// Sends one request and waits for its response.
    fn request<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
        deadline: Instant,
        cancel: &dyn Cancel,
    ) -> Result<Value, CallError> {
        let inner = &self.inner;
        let id = inner.shared.next_id();
        inner.shared.insert(id);
        // The slot is removed on every exit path below, so a late response
        // finds no slot and is discarded.
        let line = encode_request(id, method, params);
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
                    &serde_json::json!({"requestId": id}),
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
    fn notify<P: Serialize + ?Sized>(&self, method: &str, params: &P) {
        self.inner.send(encode_notification(method, params));
    }

    /// Closes stdin by dropping the writer's sender.
    fn shutdown(&self) {
        self.inner.close_stdin();
    }

    /// Kills and reaps the child exactly once; later calls find none.
    fn reap(&self) {
        let child = {
            before_lock("child");
            lock(&self.inner.child).take()
        };
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

impl Inner {
    /// Closes stdin by dropping the writer's sender.
    fn close_stdin(&self) {
        lock(&self.writer).take();
    }

    /// Waits until the server's output ends or [`GRACE`] passes on the
    /// clock.
    fn wait_gone(&self) {
        let until = deadline(self.clock.as_ref(), GRACE);
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

    /// Sends `line` to the writer thread through the bounded queue.
    fn send(&self, line: String) {
        if let Some(writer) = lock(&self.writer).as_ref() {
            crate::pipes::queue(writer, line.into_bytes(), &self.shared);
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
pub(crate) mod tests;
