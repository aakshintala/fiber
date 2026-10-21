//! One stdio MCP server: the child Fiber started, one parked thread per
//! pipe, requests answered by id, and a stop that closes stdin, waits a
//! grace on the clock, then kills and reaps (`docs/mcp.md`, "Starting
//! servers"); a signal during startup stops every start through
//! [`crate::registry::stop_every_start`]. Time comes only from the injected
//! [`contract::clock::Clock`]; each server runs in its own process session
//! and process group, signalled as a group, so a stop reaches what the
//! server started.

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
use crate::elicit::Asker;
use crate::registry::{Stopping, before_lock, before_signal, lock};
use crate::rpc::{Outcome, encode_notification, encode_request, encode_result};
use crate::server_json::{ListedPrompt, ListedTool, entries};
use crate::wait::{CancelBridge, NoCancel, Shared, deadline, park};
use support::group::Listing;

/// The protocol version Fiber speaks (`docs/mcp.md` has no number; the
/// current draft does).
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";

/// How many request lines wait for the writer thread. Picked, not
/// measured: no doc sets a number. A full queue means the server is
/// wedged, so it counts as gone.
pub(crate) const WRITE_QUEUE: usize = 64;

pub(crate) const INITIALIZED_ACK: u64 = u64::MAX - 1;

/// Why [`Server::start`] failed: the session records it as
/// `mcp_server_failed` and leaves the server out.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum StartError {
    /// The spawn, `initialize` or `tools/list` failed. The message is
    /// Fiber's own sentence.
    #[error("{0}")]
    StartFailed(String),
    /// The remote server needs a usable OAuth login.
    #[error("{0}")]
    Login(String),
    /// Nothing answered before the startup deadline.
    #[error("The server did not answer before its startup deadline.")]
    Deadline,
    /// A `{ "secret" }` env value names no stored credential: the name
    /// of `credentials/<name>`, never the value.
    #[error("The server names the secret `credentials/{name}`, which is not stored.")]
    SecretMissing {
        /// The secret's name, as the env value names it.
        name: String,
    },
    /// A `{ "secret" }` env value names a credential that cannot be
    /// read: the name and the reason, never the value.
    #[error("The server names the secret `credentials/{name}`, which cannot be read: {why}.")]
    SecretUnreadable {
        /// The secret's name, as the env value names it.
        name: String,
        /// Why it cannot be read, never the value.
        why: String,
    },
}

/// A start refused because Fiber is shutting down.
pub(crate) fn shutting_down() -> StartError {
    StartError::StartFailed("Fiber is shutting down.".to_owned())
}

/// Pages `method` (`tools/list` or `prompts/list`) to its end under the
/// handshake's shared startup `deadline`: the deadline is checked before
/// every page request, so a server answering every page at once with a
/// fresh cursor still ends at it (`docs/mcp.md`, "Starting servers"). A
/// missed deadline fails the start; any other failure runs `fail` with
/// the transport error (`None` for a page that is not a result), which
/// fails the start for tools and keeps no prompts for prompts. The
/// reload's re-list shares this with its own outcomes.
pub(crate) fn list_pages<T: serde::de::DeserializeOwned, E>(
    server: &Server,
    clock: &Arc<dyn Clock>,
    method: &str,
    entry: &str,
    deadline: Instant,
    cancel: &dyn Cancel,
    fail: impl Fn(Option<CallError>) -> Result<Vec<T>, E>,
) -> Result<Vec<T>, E> {
    let mut listed = Vec::new();
    let mut cursor: Option<String> = None;
    // The deadline maps without `fail`: every caller ends a missed bound
    // the same way, while `fail` maps the Timeout/Gone/other outcome.
    loop {
        if clock.now() >= deadline {
            return fail(Some(CallError::Timeout));
        }
        let params = match &cursor {
            Some(cursor) => serde_json::json!({"cursor": cursor}),
            None => serde_json::json!({}),
        };
        let page = match server.request(method, &params, deadline, cancel, None) {
            Ok(value) => value,
            Err(error) => return fail(Some(error)),
        };
        let object = match page.as_object() {
            Some(object) => object,
            None => return fail(None),
        };
        match object.get(entry).and_then(Value::as_array) {
            Some(found) => listed.extend(entries(found.clone())),
            None => return fail(None),
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
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum CallError {
    /// Nothing answered before the call's deadline.
    #[error("The server did not answer before the call deadline.")]
    Timeout,
    /// The call was cancelled. `notifications/cancelled` was sent.
    #[error("The call was cancelled.")]
    Cancelled,
    /// The server is gone: it exited, or its reader ended.
    #[error("The server is gone.")]
    Gone,
    /// The server answered with a JSON-RPC error.
    #[error("The server answered JSON-RPC error {code}: {message}")]
    JsonRpc {
        /// The error's code.
        code: i64,
        /// The error's message.
        message: String,
    },
    /// The request failed over HTTP without ending the server; Fiber's sentence.
    #[error("{0}")]
    Http(String),
    /// The request needs a usable OAuth login without ending the server.
    #[error("{0}")]
    Login(String),
    /// The remote server ended its session.
    #[error("The remote server ended its session.")]
    Expired,
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
    /// The far end of the wire: [`Server::stop`] or [`Drop`] takes, kills
    /// and reaps it exactly once.
    peer: Peer,
    clock: Arc<dyn Clock>,
}

/// The far end of one server's wire: the child Fiber started, or a remote
/// server's link (`docs/mcp.md`, "What Fiber does with MCP").
pub(crate) enum Peer {
    /// The child and its process-group registration.
    Child(Mutex<Option<(Child, Listing)>>),
    /// A remote server's session link.
    Remote(std::sync::Arc<crate::http::Link>),
}

impl Peer {
    /// The protocol version the `initialize` result negotiated: stored by
    /// a remote link for every later request, ignored for a child.
    fn negotiated(&self, version: &str) {
        match self {
            Peer::Child(_) => {}
            Peer::Remote(link) => link.negotiated(version),
        }
    }

    /// Shuts request `id`'s socket and refuses any it opens later: a
    /// no-op for a child, whose pipe needs no interrupt.
    fn abandon(&self, id: u64) {
        match self {
            Peer::Child(_) => {}
            Peer::Remote(link) => link.abandon(id),
        }
    }
}

/// A server [`Server::start`] opened, with the tools and prompts it listed.
pub(crate) struct OpenServer {
    /// The running server.
    pub server: Server,
    /// The tools and prompts it listed.
    pub listed: Cached,
    /// Whether `initialize` advertised the tools capability: a re-list
    /// asks again only for advertised sections, since listing an
    /// unadvertised section can hang a server that answers it with
    /// malformed JSON.
    pub advertises_tools: bool,
    /// Whether `initialize` advertised the prompts capability.
    pub advertises_prompts: bool,
}

impl Server {
    /// Starts `command` with `args` in `workspace`, inheriting Fiber's
    /// environment with `env` overriding keys, and runs `initialize`,
    /// `notifications/initialized`, `tools/list` when the server
    /// advertises tools, and `prompts/list` when it advertises prompts,
    /// under `startup_timeout` on `clock`. `client_version` is Fiber's own version, sent as
    /// `clientInfo`. A failure kills and reaps the child exactly once.
    /// A direct start without timing: test-support starts only.
    #[cfg(test)]
    pub(crate) fn start(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        workspace: &Path,
        clock: &Arc<dyn Clock>,
        startup_timeout: Duration,
        client_version: &str,
    ) -> Result<OpenServer, StartError> {
        Self::start_observed(
            command,
            args,
            env,
            workspace,
            clock,
            startup_timeout,
            client_version,
            &mut None,
        )
    }

    /// As [`Server::start`], threading the attempt's connection guard to
    /// the tool-list boundary: the handshake drops it once the full tool
    /// list is in hand, before any prompt discovery, and the caller drops
    /// what remains on the failure path. `None` stays untimed.
    #[allow(
        clippy::too_many_arguments,
        reason = "the observed companion adds only the connection guard"
    )]
    pub(crate) fn start_observed(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        workspace: &Path,
        clock: &Arc<dyn Clock>,
        startup_timeout: Duration,
        client_version: &str,
        connection: &mut Option<Box<dyn Send>>,
    ) -> Result<OpenServer, StartError> {
        // Sticky: a start after the stop spawns nothing.
        if Stopping.is_cancelled() {
            return Err(shutting_down());
        }
        let mut cmd = Command::new(command);
        cmd.args(args)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        // Its own session and process group, listed with the spawn: a
        // kill in the window between the two still reaches it, through
        // the listing the spawn makes under the shared list's lock.
        support::group::detach(&mut cmd, false);
        let (mut child, listing) = support::group::spawn(&mut cmd).map_err(|error| {
            StartError::StartFailed(format!("The server could not be started: {error}."))
        })?;
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
        Self::open_observed(
            shared,
            writer,
            Peer::Child(Mutex::new(Some((child, listing)))),
            clock,
            startup_timeout,
            client_version,
            connection,
        )
    }

    /// Runs the handshake on a wire already open: `initialize`,
    /// `notifications/initialized`, the capability-gated `tools/list` and
    /// `prompts/list` under `startup_timeout` on `clock`, then the opened
    /// server. Consumes the attempt's connection guard once the full tool
    /// list is in hand, before any prompt discovery: a prompt that errors
    /// or pages past the tool boundary writes no second line. A failure
    /// stops or shuts the server exactly once, so neither transport
    /// repeats the failure path: `stop` then "Fiber is shutting down."
    /// when stopping, else `shutdown`.
    pub(crate) fn open_observed(
        shared: Arc<Shared>,
        writer: Arc<mpsc::SyncSender<Vec<u8>>>,
        peer: Peer,
        clock: &Arc<dyn Clock>,
        startup_timeout: Duration,
        client_version: &str,
        connection: &mut Option<Box<dyn Send>>,
    ) -> Result<OpenServer, StartError> {
        let stopping = Stopping;
        clock.subscribe(Arc::downgrade(&(Arc::clone(&shared) as Arc<dyn Wake>)));
        let server = Server {
            inner: Inner {
                shared,
                writer: Mutex::new(Some(writer)),
                peer,
                clock: Arc::clone(clock),
            },
        };
        let deadline = deadline(clock.as_ref(), startup_timeout);
        // One shutdown on `Err`: every failure path below returns through
        // here, so no arm repeats `shutdown`. `NoCancel` never fires, so
        // `Cancelled` is just another failed start, not a deadline.
        let not_a_list =
            || StartError::StartFailed("The server's tool list was not a result.".to_owned());
        let mut handshake = |server: &Server| -> Result<(Cached, bool, bool), StartError> {
            let initialize = match server.request(
                "initialize",
                &serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"elicitation": {}},
                    "clientInfo": {"name": "fiber", "version": client_version},
                }),
                deadline,
                &stopping,
                None,
            ) {
                Ok(value) if value.is_object() => value,
                Err(CallError::Timeout) => return Err(StartError::Deadline),
                Err(CallError::Http(detail)) => return Err(StartError::StartFailed(detail)),
                Err(CallError::Login(detail)) => return Err(StartError::Login(detail)),
                Ok(_) | Err(_) => {
                    return Err(StartError::StartFailed(
                        "The server's `initialize` reply was not a result.".to_owned(),
                    ));
                }
            };
            // The negotiated version rides on every later request of a
            // remote server; a child ignores it.
            let version = crate::http::negotiated_version(&initialize);
            server.inner.peer.negotiated(&version);
            let remote = matches!(server.inner.peer, Peer::Remote(_));
            if remote {
                server.inner.shared.insert(INITIALIZED_ACK);
            }
            server.notify("notifications/initialized", &serde_json::json!({}));
            if remote {
                server.await_initialized(deadline, &stopping)?;
            }
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
                    |error| match error {
                        Some(CallError::Timeout) => Err(StartError::Deadline),
                        Some(CallError::Login(detail)) => Err(StartError::Login(detail)),
                        _ => Err(not_a_list()),
                    },
                )?
            } else {
                Vec::new()
            };
            // The connection timer ends here, with the full tool list in
            // hand: prompt discovery runs after the boundary and can
            // neither extend the line nor write a second one. With no
            // advertised tools capability the known empty list is that
            // same boundary.
            drop(connection.take());
            // `prompts/list` runs only when `initialize` advertises the
            // `prompts` capability as an object, and a list that errors
            // or is not a result leaves the server with no prompts while
            // the start goes on; only a missed deadline fails it
            // (`docs/mcp.md`, "Prompts and resources").
            let advertises_prompts = initialize
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("prompts"))
                .is_some_and(Value::is_object);
            let prompts: Vec<ListedPrompt> = if advertises_prompts {
                list_pages(
                    server,
                    clock,
                    "prompts/list",
                    "prompts",
                    deadline,
                    &stopping,
                    |error| match error {
                        Some(CallError::Timeout) => Err(StartError::Deadline),
                        Some(CallError::Login(detail)) => Err(StartError::Login(detail)),
                        _ => Ok(Vec::new()),
                    },
                )?
            } else {
                Vec::new()
            };
            // Whether the server offers resources to read: the same
            // object test the handshake applies to tools and prompts.
            let resources = initialize
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("resources"))
                .is_some_and(Value::is_object);
            Ok((
                Cached {
                    tools,
                    prompts,
                    resources,
                },
                advertises_tools,
                advertises_prompts,
            ))
        };
        match handshake(&server) {
            Ok((listed, advertises_tools, advertises_prompts)) => Ok(OpenServer {
                server,
                listed,
                advertises_tools,
                advertises_prompts,
            }),
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

    fn await_initialized(&self, deadline: Instant, stopping: &Stopping) -> Result<(), StartError> {
        let inner = &self.inner;
        let bridge = CancelBridge::arm(&inner.shared);
        stopping.subscribe(Arc::downgrade(&(bridge.clone() as Arc<dyn Wake>)));
        let result = loop {
            let view = inner.shared.view(INITIALIZED_ACK, stopping);
            if let Some(outcome) = view.response {
                break match outcome {
                    Outcome::Login(detail) => Err(StartError::Login(detail)),
                    Outcome::Result(_) | Outcome::Http(_) | Outcome::Error { .. } => Ok(()),
                };
            }
            if view.gone {
                break Ok(());
            }
            if view.cancelled {
                break Err(shutting_down());
            }
            if inner.clock.now() >= deadline {
                break Err(StartError::Deadline);
            }
            park(
                inner.clock.as_ref(),
                &inner.shared,
                stopping,
                deadline,
                view.seq,
            );
        };
        drop(bridge);
        let _leftovers = inner.shared.remove(INITIALIZED_ACK);
        result
    }

    /// Calls `method` with `params`, waiting until `timeout` passes on the
    /// clock or `cancel` fires. A late response to a timed-out or cancelled
    /// id is discarded: the slot is removed on every exit path. A call
    /// with no asker never takes an elicitation and is never listed in
    /// its `action_ids`.
    pub(crate) fn call<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
        cancel: &dyn Cancel,
    ) -> Result<Value, CallError> {
        let inner = &self.inner;
        let deadline = deadline(inner.clock.as_ref(), timeout);
        self.request(method, params, deadline, cancel, None)
    }

    /// Calls `method` with `params` as [`Server::call`] does, asking
    /// elicitations through `asker` when it is `Some`: the call registers
    /// before its line is sent, and takes queued elicitations while it
    /// waits. Startup pagination passes `None`.
    pub(crate) fn call_asking<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
        cancel: &dyn Cancel,
        asker: Option<Asker<'_>>,
    ) -> Result<Value, CallError> {
        let inner = &self.inner;
        let deadline = deadline(inner.clock.as_ref(), timeout);
        self.request(method, params, deadline, cancel, asker)
    }

    /// Whether the server's output ended (it exited).
    pub(crate) fn is_gone(&self) -> bool {
        lock(&self.inner.shared.inner).gone
    }

    /// Whether the remote server ended its session.
    pub(crate) fn is_expired(&self) -> bool {
        lock(&self.inner.shared.inner).expired
    }

    /// Stops the server: closes stdin, sends SIGTERM to its process group,
    /// waits on the clock until its output ends or the grace passes, then
    /// kills and reaps the child (`docs/invocation.md`, "Shutdown"). Idempotent:
    /// the child is taken, killed and reaped exactly once, and [`Drop`]
    /// repeats only what is left. `&self` because tools share the
    /// connection while [`Servers`] owns the shutdown.
    pub(crate) fn stop(&self) {
        let inner = &self.inner;
        inner.close_stdin();
        match &inner.peer {
            Peer::Child(child) => {
                // Under the child's lock: a reap takes the child under
                // it, so the group id is unreaped while the SIGTERM runs.
                let child = lock(child);
                if let Some((child, _)) = child.as_ref() {
                    before_signal();
                    match support::group::signal(child.id(), Signal::TERM) {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
            // A remote server takes no signal: dropping the writer ends
            // its pump, which closes its sockets.
            Peer::Remote(_) => {}
        }
        inner.wait_gone();
        self.reap();
    }

    /// Sends one request and waits for its response. With an asker, the
    /// call registers before its line is sent and takes queued
    /// elicitations while it waits, answering each with no shared lock
    /// held.
    fn request<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
        deadline: Instant,
        cancel: &dyn Cancel,
        asker: Option<Asker<'_>>,
    ) -> Result<Value, CallError> {
        let inner = &self.inner;
        let id = inner.shared.next_id();
        inner.shared.insert(id);
        if let Some(asker) = asker {
            inner.shared.register(id, asker.ask.action(), deadline);
        }
        // The slot is removed on every exit path below, so a late response
        // finds no slot and is discarded.
        let line = encode_request(id, method, params);
        inner.send(line);
        let bridge = CancelBridge::arm(&inner.shared);
        cancel.subscribe(Arc::downgrade(&(Arc::clone(&bridge) as Arc<dyn Wake>)));
        // Whether this call handled an elicitation: once it has, a cancel
        // or its own deadline wins over a response delivered meanwhile,
        // or a cancelled or timed-out call would read as a success.
        let mut handled = false;
        let outcome = loop {
            // The test-only pause point, before the view: a test holds a
            // registered call here to force the queue-then-remove order.
            before_pass();
            let view = inner.shared.view(id, cancel);
            if handled {
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
            }
            if let Some(outcome) = view.response {
                break Ok(outcome);
            }
            // An expired session sets both flags: the expiry reads
            // first, so it never reports as a death.
            if view.expired {
                break Err(CallError::Expired);
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
            if let Some(asker) = asker
                && let Some(taken) = inner.shared.take_elicitation()
            {
                let result = crate::elicit::respond(
                    asker,
                    &taken.elicitation.form,
                    taken.action_ids,
                    taken.until,
                );
                // One send attempt: a full or disconnected writer
                // drops it, and the call then ends as a death.
                inner.send(encode_result(&taken.elicitation.id, &result));
                handled = true;
                continue;
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
        // The slot is removed before the abandon: a socket error from the
        // abandoned request then finds no waiter and never marks the
        // server gone. When this was the last asking call, the queued
        // elicitations it left come back here and are declined.
        let leftovers = inner.shared.remove(id);
        for elicitation in leftovers {
            inner.send(encode_result(
                &elicitation.id,
                &serde_json::json!({"action": "decline"}),
            ));
        }
        inner.peer.abandon(id);
        match outcome {
            Ok(Outcome::Result(value)) => Ok(value),
            Ok(Outcome::Error { code, message }) => Err(CallError::JsonRpc { code, message }),
            Ok(Outcome::Http(message)) => Err(CallError::Http(message)),
            Ok(Outcome::Login(message)) => Err(CallError::Login(message)),
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
        match &self.inner.peer {
            Peer::Remote(link) => link.close(),
            Peer::Child(child) => {
                let taken = {
                    before_lock("child");
                    lock(child).take()
                };
                if let Some((mut child, listing)) = taken {
                    // The leader is taken but unreaped, so the group id
                    // cannot have been reused: the SIGKILL reaches this
                    // server's group.
                    match support::group::signal(child.id(), Signal::KILL) {
                        Ok(()) | Err(_) => {}
                    }
                    before_lock("live");
                    // Unlisted under the shared list's lock, before the
                    // wait frees the pid: a group the shutdown kill signals
                    // always has an unreaped leader.
                    support::group::live().unlist(listing);
                    match child.wait() {
                        Ok(_) | Err(_) => {}
                    }
                }
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

    /// Waits until the server's output ends or [`support::group::GRACE`]
    /// passes on the clock.
    fn wait_gone(&self) {
        let until = deadline(self.clock.as_ref(), support::group::GRACE);
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

/// Runs the test hook [`tests::before_pass`] installs, if any: the seam
/// a test pauses on to hold a registered call before its first view, in
/// the pattern of `registry`'s `before_signal`.
#[cfg(test)]
fn before_pass() {
    crate::server::tests::before_pass();
}

/// No hook outside tests: the wait never pauses.
#[cfg(not(test))]
fn before_pass() {}

#[cfg(test)]
#[path = "server_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "server_elicit_tests.rs"]
mod elicit_tests;
