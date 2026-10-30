//! One client connection (`docs/architecture.md`, "The threads"): a thread
//! that reads its commands and answers them, and, once it has subscribed, a
//! thread that writes its events.

mod level;
mod writer;

#[cfg(test)]
pub(crate) use writer::Flight;
#[cfg(test)]
pub(crate) use writer::write_loop;
pub(crate) use writer::{Prelude, STOP, control_line, spawn_writer, write_line};

use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;

use contract::commands::{Command, CommandLine};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, Event, Notice};
use contract::inbox::{Ack, Answer, Delivery, Message, Rejection};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::steering::Duplicate;
use contract::{CommandId, Envelope, ErrorCode};
use support::lock;

use crate::session::{self, Gate};

pub(crate) const MALFORMED: &str =
    "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
pub(crate) const DUPLICATE: &str = "A command with this id was already accepted.";
pub(crate) const ALREADY: &str = "This connection is already subscribed at this level.";
pub(crate) const UNFIT: &str = "The arguments do not fit this command.";
/// A process extension's own command on an ordinary client connection.
const PROCESS_ONLY: &str = "`call` and `ready` ride a process extension's own pipe.";
const PAST: &str = "`from_seq` is past the latest line.";
const REVERSED: &str = "`to_seq` is before `from_seq`.";
pub(crate) const ENDED: &str = "The session ended before answering.";
const CLOSING_PROMPT: &str = "The session is closing and takes no new turn.";
/// A `steer_drop` names nothing still queued: the loop's stale answer
/// reads the same (`docs/invocation.md`, "What each command does").
const STALE_DROP: &str = "That steering message was already applied, or was never queued.";
/// A `cancel` names no running turn.
const NO_TURN: &str = "No turn is running.";
/// A `job_stop` names no running job.
const NO_JOB: &str = "That job is not running.";
/// A `job_input` names a job that was not started with `tty`.
const NOT_TTY: &str = "That job was not started with `tty`.";
/// A `job_input` whose terminal never took the input within the wait.
const NOT_TAKEN: &str = "The job's terminal did not take the input.";
/// A `background` finds no shell call in the foreground.
const NO_CALL: &str = "No shell call is running.";
const HISTORY: usize = 256;

/// A line that is not one of the commands this process answers.
pub(crate) fn not_built(command: &str) -> String {
    format!("`{command}` is not built in this Fiber yet.")
}

pub(crate) struct Conn {
    pub(crate) id: u64,
    pub(crate) gate: Arc<Gate>,
    pub(crate) sink: Sink,
    pub(crate) full: bool,
    pub(crate) gone: bool,
    /// The socket peer's session attribution, when the connection arrived
    /// on the session socket. The in-process driver carries none.
    pub(crate) attribution: Option<Attribution>,
}

/// Where a connection's answers go: `Direct` writes its lines straight to
/// the socket until it subscribes, `Writer` routes them through the writer
/// thread, and the in-process driver answers its host call exactly once,
/// moving `Driven` to `Answered`.
pub(crate) enum Sink {
    Direct {
        direct: UnixStream,
        writer: Box<dyn Write + Send>,
    },
    Writer {
        outbox: level::Outbox,
        switches: mpsc::Sender<level::Switch>,
    },
    Driven {
        /// The host call's answer, taken exactly once by the first of
        /// `send` and `inbox_ack`; a rejection frees the id.
        answer: Ack,
        /// What a driven `prompt` or `steer` carries; a socket's is `Driver`.
        origin: Origin,
    },
    Answered {
        /// What the answered `prompt` or `steer` carried.
        origin: Origin,
    },
}

impl Conn {
    /// Whether the connection has subscribed: every sink but the
    /// not-yet-subscribed `Direct` one.
    pub(crate) fn subscribed(&self) -> bool {
        !matches!(self.sink, Sink::Direct { .. })
    }

    /// The subscribed connection's route to its writer, if it has one.
    pub(crate) fn subscription(&self) -> Option<(level::Outbox, mpsc::Sender<level::Switch>)> {
        if let Sink::Writer { outbox, switches } = &self.sink {
            Some((outbox.clone(), switches.clone()))
        } else {
            None
        }
    }

    /// The subscribed connection's route to its writer's queue, if it has one.
    pub(crate) fn outbox(&self) -> Option<level::Outbox> {
        self.subscription().map(|(outbox, _)| outbox)
    }

    /// The sender a driven `prompt` or `steer` carries; a socket's is `Driver`.
    pub(crate) fn origin(&self) -> Origin {
        match &self.sink {
            Sink::Direct { .. } => Origin::Driver,
            Sink::Writer { .. } => Origin::Driver,
            Sink::Driven { origin, .. } => origin.clone(),
            Sink::Answered { origin } => origin.clone(),
        }
    }
}

/// Reads `stream` until the client hangs up or its ender stops it. `id` is
/// the slot [`Gate`] stored the reader in, with the ender that stops it and
/// shuts the socket. The writer is the socket's other clone. `peer` is the
/// kernel peer the accept thread read, kept distinct from a failed read.
pub(crate) fn serve(
    stream: support::stoppable::Reader,
    gate: Arc<Gate>,
    id: u64,
    peer: Result<support::peer::Pid, support::peer::PeerCredentialError>,
) {
    let Some(writer) = stream.get_ref().try_clone().ok() else {
        return;
    };
    serve_connection(stream, Box::new(writer), gate, id, peer);
}

/// Reads `stream` and writes through `writer`. The connection's ender was
/// stored with its reader, which is how [`crate::session::Session::close`]
/// stops a blocked reader and reaps it. Production builds the reader and
/// the writer from one socket. The caller attribution is walked once here;
/// the target side is read fresh for every command in [`dispatch`].
pub(crate) fn serve_connection(
    stream: support::stoppable::Reader,
    writer: Box<dyn Write + Send>,
    gate: Arc<Gate>,
    id: u64,
    peer: Result<support::peer::Pid, support::peer::PeerCredentialError>,
) {
    let _finish = Finish {
        gate: Arc::clone(&gate),
        id,
    };
    let Some(direct) = stream.get_ref().try_clone().ok() else {
        return;
    };
    let attribution = attribute_peer(peer, read_for(&gate).as_ref());
    let mut conn = Conn {
        id,
        gate,
        sink: Sink::Direct { direct, writer },
        full: false,
        gone: false,
        attribution: Some(attribution),
    };
    let mut read = BufReader::new(stream);
    loop {
        match support::framing::read_line(&mut read, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(buf)) => {
                on_line(&buf, &mut conn);
                if conn.gone {
                    break;
                }
            }
        }
    }
    if conn.full {
        conn.gate.detach();
    }
    if let Some(outbox) = conn.outbox() {
        outbox.cancel_settles();
        outbox.stop(&conn.gate.session_id);
    }
}

/// Ends a client connection's reader: stops its stoppable read, then shuts
/// the socket down, so the reader runs its normal cleanup and the client
/// sees EOF. Stored with the reader, which is how
/// [`crate::session::Session::close`] reaps a blocked reader.
pub(crate) fn ender(
    stream: UnixStream,
    stop: support::stoppable::Stop,
) -> Box<dyn Fn() + Send + Sync> {
    Box::new(move || {
        stop.stop();
        #[cfg(test)]
        if crate::session::shutdown_skipped_for_test() {
            return;
        }
        match stream.shutdown(Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
    })
}

struct Finish {
    gate: Arc<Gate>,
    id: u64,
}

impl Drop for Finish {
    fn drop(&mut self) {
        #[cfg(test)]
        crate::session::park_reader_for_test();
        self.gate.conns.finish(self.id);
    }
}

fn on_line(bytes: &[u8], conn: &mut Conn) {
    let line = match crate::drive::classify(bytes) {
        Ok(line) => line,
        Err(id) => {
            reject(conn, id, ErrorCode::Malformed, MALFORMED);
            return;
        }
    };
    if !conn.gate.accepted.reserve(&line.id) {
        reject(conn, Some(line.id), ErrorCode::DuplicateCommand, DUPLICATE);
        return;
    }
    #[cfg(test)]
    conn.gate
        .note(crate::session::tests::Probe::CommandReserved);
    if !conn.subscribed() && line.command != "subscribe" {
        reject(
            conn,
            Some(line.id),
            ErrorCode::NotSubscribed,
            NOT_SUBSCRIBED,
        );
        return;
    }
    let (parsed, name) = match crate::drive::parse(line) {
        Ok(parsed) => parsed,
        Err((id, code, message)) => {
            reject(conn, Some(id), code, &message);
            return;
        }
    };
    dispatch(conn, parsed, &name);
}

pub(crate) fn dispatch(conn: &mut Conn, line: CommandLine, name: &str) {
    let id = line.id;
    // A session-owned connection is refused before its command reaches
    // the inbox. The in-process driver carries no attribution, so its
    // commands are never refused here.
    if let Some(attribution) = conn.attribution.as_ref() {
        let read = read_for(&conn.gate);
        if let Some(refused) = session_command_refusal(
            attribution,
            &line.command,
            &conn.gate.session_id,
            support::peer::Pid(std::process::id()),
            read.as_ref(),
        ) {
            reject(conn, Some(id), refused.code, &refused.message);
            return;
        }
    }
    match line.command {
        Command::Subscribe(args) => {
            // `dispatch` pulls the `Direct` halves out and passes them to
            // `subscribe` as arguments.
            let origin = conn.origin();
            match std::mem::replace(&mut conn.sink, Sink::Answered { origin }) {
                Sink::Direct { direct, writer } => {
                    subscribe(conn, id, args.level, args.credential, direct, writer);
                }
                sink @ (Sink::Writer { .. } | Sink::Driven { .. }) => {
                    conn.sink = sink;
                    level::change(conn, id, args.level, args.credential);
                }
                Sink::Answered { origin } => {
                    conn.sink = Sink::Answered { origin };
                    reject(conn, Some(id), ErrorCode::InvalidArguments, ALREADY);
                }
            }
        }
        Command::Tools => {
            let tools = lock(&conn.gate.tools).clone();
            // Without a log the session is closing and the answer keeps
            // its byte sizes.
            let rate = conn
                .gate
                .log
                .upgrade()
                .map(|_| lock(&conn.gate.rate).rate());
            let tools = tools
                .into_iter()
                .map(|mut info| {
                    info.tokens = rate.as_ref().and_then(|rate| rate.tokens(info.bytes));
                    info
                })
                .collect();
            accept(conn, id, Some(CommandResult::Tools { tools }));
        }
        Command::Commands => {
            let commands = conn.gate.commands();
            accept(conn, id, Some(CommandResult::Commands { commands }));
        }
        Command::Skills => {
            let skills = conn.gate.skills();
            accept(conn, id, Some(CommandResult::Skills { skills }));
        }
        Command::Hooks => {
            let Some(hooks) = conn.gate.hooks() else {
                unknown(conn, id, name);
                return;
            };
            let hooks = hooks.registrations();
            accept(conn, id, Some(CommandResult::Hooks { hooks }));
        }
        Command::Quota(args) => {
            let (quota, notice_at) = conn.gate.quota(args.fetch);
            accept(conn, id, Some(CommandResult::Quota { quota, notice_at }));
        }
        Command::Credentials => {
            let Some(read) = conn.gate.credentials_reader() else {
                unknown(conn, id, name);
                return;
            };
            match read() {
                Some(labels) => accept(conn, id, Some(CommandResult::Credentials(labels))),
                None => reject(conn, Some(id), ErrorCode::Closing, ENDED),
            }
        }
        Command::History(args) => {
            // The log is dropped once read, so this connection does not
            // hold the session lock.
            let Some(log) = conn.gate.log.upgrade() else {
                reject(conn, Some(id), ErrorCode::Closing, ENDED);
                return;
            };
            let read = history(&log, &conn.gate.session_id, &args);
            drop(log);
            match read {
                Ok(lines) => accept(conn, id, Some(CommandResult::History { lines })),
                Err(message) => reject(conn, Some(id), ErrorCode::InvalidArguments, &message),
            }
        }
        Command::Prompt(args) => {
            let images = conn.gate.images();
            let pasting = Arc::clone(&conn.gate.pasting);
            match crate::pasted::content(args.content, images.as_deref(), pasting.as_ref()) {
                Ok(content) => deliver_message(conn, id, content, true, conn.origin()),
                Err((code, message)) => reject(conn, Some(id), code, &message),
            }
        }
        Command::Steer(args) => {
            let images = conn.gate.images();
            let pasting = Arc::clone(&conn.gate.pasting);
            match crate::pasted::content(args.content, images.as_deref(), pasting.as_ref()) {
                Ok(content) => deliver_message(conn, id, content, false, conn.origin()),
                Err((code, message)) => reject(conn, Some(id), code, &message),
            }
        }
        Command::SteerDrop(args) => {
            admit_drop(conn, id, args.command_id);
        }
        Command::Message(args) => crate::message::run(conn, id, args),
        Command::Reply(reply) => crate::reply::route(conn, id, reply),
        Command::Call(_) | Command::Ready => {
            // `call` and `ready` ride the process extension's own pipe:
            // an ordinary client connection is refused (`docs/invocation.md`,
            // "Driver commands"). The supervised pipe admits them through the
            // extensions' own path, never through this dispatch.
            reject(conn, Some(id), ErrorCode::InvalidArguments, PROCESS_ONLY);
        }
        Command::Cancel => cancel(conn, id),
        Command::Shell(args) => shell(conn, id, &args, name),
        Command::ProposeNotes(args) => crate::notes::run(conn, id, args),
        Command::Close(args) => crate::close::run(conn, id, args.now),
        Command::Model(args) => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::Model(args, ack));
        }
        Command::Credential(args) => {
            let ack = inbox_ack(conn, id);
            let ack = conn.gate.with_provider(ack);
            conn.gate.deliver(Delivery::Credential(args, ack));
        }
        Command::Name(args) => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::Name(args, ack));
        }
        Command::AddDirectory(args) => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::AddDirectory(args, ack));
        }
        Command::Handoff(args) => {
            let ack = inbox_ack(conn, id.clone());
            conn.gate.deliver(Delivery::Handoff(id, args, ack));
        }
        Command::Rewind(args) => crate::rewind::run(conn, id, args),
        Command::ChangeWorkspace(args) => {
            let ack = inbox_ack(conn, id.clone());
            conn.gate.deliver(Delivery::ChangeWorkspace(id, args, ack));
        }
        Command::JobStop(args) => {
            let stopped = conn.gate.jobs().is_some_and(|jobs| jobs.stop(&args.job_id));
            answer(conn, id, stopped, NO_JOB);
        }
        Command::JobInput(args) => job_input(conn, id, &args),
        Command::Background => {
            let moving = conn.gate.jobs().is_some_and(|jobs| jobs.background() > 0);
            answer(conn, id, moving, NO_CALL);
        }
        Command::Command(args) => crate::run_command::run(conn, id, &args),
        Command::Reload => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::Reload(ack));
        }
        Command::Import(args) => import(conn, id, &args),
        Command::McpServers => {
            let Some(refresh) = conn.gate.servers_refresh() else {
                unknown(conn, id, name);
                return;
            };
            let tools = lock(&conn.gate.tools).clone();
            match refresh(tools) {
                Ok(rows) => {
                    accept(conn, id, Some(CommandResult::McpServers { servers: rows }));
                }
                Err(failure) => reject(conn, Some(id), failure.code, &failure.message),
            }
        }
        Command::McpReconnect(args) => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::McpReconnect(args, ack));
        }
        Command::McpLogin(args) => {
            // The login waits on the loopback callback, so it runs on
            // its own thread and answers once it ends: accepted at once
            // is the admission, not the answer (`docs/mcp.md`,
            // "Servers, their state and the per-server commands").
            let Some(login) = conn.gate.mcp_login() else {
                unknown(conn, id, name);
                return;
            };
            let ack = inbox_ack(conn, id);
            let server = args.server.clone();
            let gate = Arc::clone(&conn.gate);
            let deliver: Arc<dyn Fn(Delivery) + Send + Sync> =
                Arc::new(move |delivery| gate.deliver(delivery));
            thread::spawn(move || match login(&server, deliver) {
                Ok(()) => ack.0(Ok(None)),
                Err(failure) => ack.0(Err(Rejection {
                    code: failure.code,
                    message: failure.message,
                })),
            });
        }
        Command::OfferedFile(args) => crate::offered::answer(conn, id, &args),
        Command::Unknown => unknown(conn, id, name),
        Command::ArtifactPage(args) => crate::artifact::run(conn, id, args),
    }
}

/// Copies another harness's configuration into Fiber home, once
/// (`docs/configuration.md`, "Importing"): the same shared import `fiber
/// import` runs, answered at once on the reader thread. With no `source`
/// both sources run, each independent: one source's completed receipt
/// short-circuits only that source, and the first failure rejects, so a
/// retry short-circuits what landed and completes the rest.
fn import(conn: &mut Conn, id: CommandId, args: &contract::commands::ImportArgs) {
    use config::{ImportSource, source_home};
    let sources: &[ImportSource] = match args.source.as_deref() {
        None => &[ImportSource::ClaudeCode, ImportSource::Codex],
        Some("claude-code") => &[ImportSource::ClaudeCode],
        Some("codex") => &[ImportSource::Codex],
        Some(_) => {
            reject(
                conn,
                Some(id),
                ErrorCode::InvalidArguments,
                "`source` must be `claude-code` or `codex`.",
            );
            return;
        }
    };
    let Some(home_env) = std::env::var_os("HOME") else {
        reject(
            conn,
            Some(id),
            ErrorCode::IoFailed,
            "HOME is not set, so the source home cannot be found.",
        );
        return;
    };
    let clock = Arc::clone(&conn.gate.clock);
    // The session process wires the discovery-backed check before the
    // session runs; the socket drive path wires none, so it treats
    // nothing as already read, exactly like the direct test helper.
    let check = conn
        .gate
        .personal_source_check()
        .unwrap_or_else(|| Arc::new(config::no_personal_sources));
    let mut imported = contract::events::ImportedNames::default();
    let mut left_behind = Vec::new();
    for source in sources {
        let report = match config::import_source(
            &conn.gate.home,
            &source_home(std::path::Path::new(&home_env), *source),
            *source,
            check.as_ref(),
            clock.as_ref(),
        ) {
            Ok(report) => report,
            Err(error) => {
                reject(conn, Some(id), error.code(), &error.to_string());
                return;
            }
        };
        imported.mcp_servers.extend(report.imported.mcp_servers);
        imported.skills.extend(report.imported.skills);
        imported.commands.extend(report.imported.commands);
        imported.instructions.extend(report.imported.instructions);
        left_behind.extend(report.left_behind.into_iter().map(|left| {
            contract::events::LeftBehindEntry {
                item: left.item,
                reason: left.reason,
            }
        }));
    }
    accept(
        conn,
        id,
        Some(CommandResult::Import {
            imported,
            left_behind,
        }),
    );
}

/// Accepted when `done`, else rejected `stale_request` with `message`. For
/// the commands answered at once, on the reader thread, that write no
/// durable event (`docs/architecture.md`, "One inbox").
fn answer(conn: &mut Conn, id: CommandId, done: bool, message: &str) {
    if done {
        accept(conn, id, None);
    } else {
        reject(conn, Some(id), ErrorCode::StaleRequest, message);
    }
}

/// Types into a running job started with `tty` (`docs/architecture.md`,
/// "One inbox"). Accepted once written; rejected `stale_request` when the
/// job is not running and `invalid_arguments` when it was not started with
/// `tty`. Answered at once, on the reader thread, writing no durable event.
fn job_input(conn: &mut Conn, id: CommandId, args: &contract::commands::JobInput) {
    let Some(jobs) = conn.gate.jobs() else {
        reject(conn, Some(id), ErrorCode::StaleRequest, NO_JOB);
        return;
    };
    match jobs.write(&args.job_id, &args.text) {
        Ok(()) => accept(conn, id, None),
        Err(error) => {
            let message = match &error {
                contract::jobs::WriteError::NotRunning | contract::jobs::WriteError::Ended(_) => {
                    NO_JOB.to_owned()
                }
                contract::jobs::WriteError::NotTty => NOT_TTY.to_owned(),
                contract::jobs::WriteError::Io(io) => {
                    // A terminal that never took the input reports the wait; any
                    // other failure reports its cause.
                    if io.kind() == std::io::ErrorKind::TimedOut {
                        NOT_TAKEN.to_owned()
                    } else {
                        format!("Writing to the job's terminal failed: {io}.")
                    }
                }
            };
            reject(conn, Some(id), error.code(), &message);
        }
    }
}

/// Ends the running turn (`docs/architecture.md`, "Cancellation") and stops
/// a running driver `shell`. Accepted when either is running; rejected
/// `stale_request` when neither is. Answered at once, on the reader thread.
/// The loop's wake is sent only when a turn was running.
fn cancel(conn: &mut Conn, id: CommandId) {
    let stopped = conn.gate.stop_running();
    if stopped.turn || stopped.shell {
        accept(conn, id, None);
        if stopped.turn {
            conn.gate.deliver(Delivery::Cancelled);
        }
    } else {
        reject(conn, Some(id), ErrorCode::StaleRequest, NO_TURN);
    }
}

/// Starts a driver `shell` on a thread of its own, so this reader reads on
/// and a `cancel` on this connection reaches it. Its cancel is registered
/// before this returns. The thread gets the shell and its answer only once
/// it has started; if it cannot start, the reader answers `io_failed`.
fn shell(conn: &mut Conn, id: CommandId, args: &contract::commands::Shell, name: &str) {
    let Some(running) = crate::shell::start(&conn.gate, args) else {
        return unknown(conn, id, name);
    };
    let gate = Arc::clone(&conn.gate);
    let (tx, rx) = mpsc::channel::<(crate::shell::Running, Ack)>();
    let spawned = thread::Builder::new()
        .name("shell".to_owned())
        .spawn(move || {
            if let Ok((running, ack)) = rx.recv() {
                running.finish(&gate, ack);
            }
        });
    if spawned.is_err() {
        reject(
            conn,
            Some(id),
            ErrorCode::IoFailed,
            crate::shell::COULD_NOT_START,
        );
        running.abandon(&conn.gate);
        return;
    }
    let ack = inbox_ack(conn, id);
    // The thread waits in `recv`, so the send fails only if it died first:
    // the dropped acknowledgement answers `closing`.
    if let Err(mpsc::SendError((running, ack))) = tx.send((running, ack)) {
        drop(ack);
        running.abandon(&conn.gate);
    }
}

/// Runs once, on a `Direct` sink: `dispatch` pulls the socket halves out
/// of the sink and passes them in, then `subscribe` leaves `Writer` on
/// success and puts `Direct` back when the acknowledgement cannot be written.
fn subscribe(
    conn: &mut Conn,
    id: CommandId,
    level: contract::commands::SubscribeLevel,
    credential: Option<String>,
    mut direct: UnixStream,
    stream: Box<dyn Write + Send>,
) {
    let summary = matches!(level, contract::commands::SubscribeLevel::Summary);
    // The watcher is registered and its seed queued under the one log lock
    // that `append` takes, so no later line can be queued before an older
    // snapshot. The log is dropped here so this connection does not hold
    // the session lock.
    let (watcher, status, extensions) = {
        let Some(log) = conn.gate.log.upgrade() else {
            // The halves are already out of the sink: put the `Direct`
            // sink back so the rejection reaches the client.
            conn.sink = Sink::Direct {
                direct,
                writer: stream,
            };
            reject(conn, Some(id), ErrorCode::Closing, ENDED);
            return;
        };
        if summary {
            let watcher = log.watch();
            let status = log.latest("session_status");
            let extensions = log.latest("extensions_loaded");
            (watcher, status, extensions)
        } else {
            // A first `full` subscribe folds the stream from the log: over
            // an unreadable page the watcher keeps every line before the
            // one that failed, then the connection closes at the failure.
            let watcher = log.watch_all_seeded();
            // Probe point: the watcher is obtained and its seed queued under
            // the one log lock, before the writer starts.
            #[cfg(test)]
            conn.gate
                .note(crate::session::tests::Probe::SubscribeSeeded);
            (watcher, None, None)
        }
    };
    let last_seq = watcher.last_seq();
    let injector = watcher.injector();
    let outbox = level::Outbox::new(injector.clone());
    // A `full` connection is counted before its acknowledgement, so a
    // client that reads the acknowledgement is already in `clients`. When
    // the write fails, the read loop's cleanup detaches it.
    conn.full = !summary;
    if conn.full {
        // The watcher is registered, so this connection receives the line.
        conn.gate.attach();
    }
    // With a label the switch is submitted only after this preparation
    // succeeded, and the single wire acceptance waits for the loop's
    // answer, which the writer emits before folded history. A refused
    // switch keeps this successful subscription.
    if let Some(label) = credential {
        let (ack, response, cancel) = settle_response(&conn.gate, id, last_seq);
        let snapshots = if summary {
            [status, extensions].into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        outbox.settle(cancel);
        conn.gate.deliver(Delivery::Credential(
            contract::commands::CredentialArgs { label },
            ack,
        ));
        conn.sink = Sink::Writer {
            outbox,
            switches: spawn_writer(
                Arc::clone(&conn.gate),
                conn.id,
                watcher,
                stream,
                summary,
                Some(Prelude {
                    response,
                    after: snapshots,
                }),
            ),
        };
        // The writer is the only writer from here.
        drop(direct);
        return;
    }
    // The acknowledgement is written here, through the halves `dispatch`
    // passed in, before the writer starts, so it is the first line the
    // client reads.
    let accepted = session::envelope(
        &conn.gate.session_id,
        conn.gate.clock.as_ref(),
        &Event::CommandAccepted(CommandAccepted {
            command_id: id,
            result: last_seq.map(|last_seq| CommandResult::Subscribe { last_seq }),
        }),
    );
    if write_line(&mut direct, &accepted).is_err() {
        conn.gone = true;
    }
    #[cfg(test)]
    if conn.full {
        conn.gate
            .note(crate::session::tests::Probe::SubscribeAcknowledged);
    }
    if conn.gone {
        // The acknowledgement never reached the client: put `Direct` back
        // so the socket connection's sink is unchanged.
        conn.sink = Sink::Direct {
            direct,
            writer: stream,
        };
        return;
    }
    if summary {
        for line in [status, extensions].into_iter().flatten() {
            if write_line(&mut direct, &line).is_err() {
                conn.gone = true;
                // As above: the snapshot never reached the client.
                conn.sink = Sink::Direct {
                    direct,
                    writer: stream,
                };
                return;
            }
        }
    }
    conn.sink = Sink::Writer {
        outbox,
        switches: spawn_writer(
            Arc::clone(&conn.gate),
            conn.id,
            watcher,
            stream,
            summary,
            None,
        ),
    };
    // The writer is the only writer from here.
    drop(direct);
}

/// Admits one reader steer into the session's shared steering queue and
/// answers `command_accepted` at once (`docs/architecture.md`, "One
/// inbox"): the row draws while the turn streams, and the loop
/// reconciles the inbox delivery against the queued entry at its next
/// drain, running the unchanged hook pass there. No hook runs here, no
/// hook state is taken, no durable event is written. The forwarded ack is
/// a no-op closure: the at-once answer consumed the single answer. A
/// delivery the inbox refuses after queueing is taken back out, so no
/// row outlives its answer.
fn admit_steer(conn: &mut Conn, id: CommandId, message: Message) {
    let ack = inbox_ack(conn, id.clone());
    let refused = |ack: Ack, code: ErrorCode, text: &str| {
        ack.0(Err(Rejection {
            code,
            message: text.to_owned(),
        }));
    };
    let Some(log) = conn.gate.log.upgrade() else {
        refused(ack, ErrorCode::Closing, ENDED);
        return;
    };
    if let Err(Duplicate) = conn.gate.steering.push(
        id.clone(),
        message.content.clone(),
        message.sender.clone(),
        log.as_ref(),
    ) {
        refused(ack, ErrorCode::DuplicateCommand, DUPLICATE);
        return;
    }
    if conn
        .gate
        .try_deliver(Delivery::Steer(message, Ack(Box::new(|_| {}))))
    {
        ack.0(Ok(None));
    } else {
        conn.gate.steering.take(&id, log.as_ref());
        refused(ack, ErrorCode::Closing, ENDED);
    }
}

/// Takes one queued steer off the shared steering queue for a reader
/// `steer_drop` and answers at once (`docs/architecture.md`, "One
/// inbox"). A drop that misses a live entry is delivered for the loop to
/// answer at its drain; one that names nothing admitted is rejected
/// `stale_request` at once.
fn admit_drop(conn: &mut Conn, id: CommandId, target: CommandId) {
    let ack = inbox_ack(conn, id);
    let Some(log) = conn.gate.log.upgrade() else {
        ack.0(Err(Rejection {
            code: ErrorCode::Closing,
            message: ENDED.to_owned(),
        }));
        return;
    };
    if conn.gate.steering.remove(&target, log.as_ref()) {
        ack.0(Ok(None));
    } else if conn.gate.steering.is_admitted(&target) {
        // Live: the loop answers at its drain; the entry may clear first.
        conn.gate.deliver(Delivery::SteerDrop(target, ack));
    } else {
        ack.0(Err(Rejection {
            code: ErrorCode::StaleRequest,
            message: STALE_DROP.to_owned(),
        }));
    }
}

fn deliver_message(
    conn: &mut Conn,
    id: CommandId,
    content: Vec<ContentPart>,
    prompt: bool,
    origin: Origin,
) {
    let message = Message {
        content,
        sender: Sender {
            origin,
            command_id: Some(id.clone()),
        },
    };
    if prompt {
        let ack = inbox_ack(conn, id.clone());
        let mut prompt = Some((id, message, ack));
        let gate = &conn.gate;
        // A closed admission rejects the prompt with the loop's closing
        // message; an admitted prompt is queued and recorded under the lock.
        let admitted = gate.admission.admit(|| {
            let Some((id, message, ack)) = prompt.take() else {
                unreachable!("admission runs its closure once");
            };
            let ack = match gate.history.get().cloned().flatten() {
                Some(path) => {
                    gate.prompts.register(id.clone(), message.content.clone());
                    crate::prompt_history::recorded(ack, path, gate, id)
                }
                None => ack,
            };
            gate.deliver(Delivery::Prompt(message, ack));
        });
        if admitted.is_none()
            && let Some((_, _, ack)) = prompt.take()
        {
            ack.0(Err(Rejection {
                code: ErrorCode::Closing,
                message: CLOSING_PROMPT.to_owned(),
            }));
        }
    } else {
        admit_steer(conn, id, message);
    }
}

/// The durable lines `args` asks for, at most [`HISTORY`] of them, read and
/// parsed from the log's offset table by `seq`: a line outside the window
/// is never read. Without `from_seq`, the answer is the last lines up to
/// `to_seq`, or up to the latest line, at most [`HISTORY`]; on an empty
/// log, with none.
fn history(
    log: &log::Log,
    own: &contract::SessionId,
    args: &contract::commands::HistoryArgs,
) -> Result<Vec<Envelope>, String> {
    let ancestor = match args.session_id.as_ref().filter(|id| *id != own) {
        None => None,
        Some(id) => {
            if !crate::sessions::valid_id(&id.0) {
                return Err(format!(
                    "{} is not a session id: one is `s_` followed by 16 lowercase hex digits.",
                    id.0
                ));
            }
            let chain = log::history(log.dir()).map_err(|_| UNFIT.to_owned())?;
            let segment = chain
                .into_iter()
                .rev()
                .skip(1)
                .find(|segment| segment.session_id == *id)
                .ok_or_else(|| {
                    format!(
                        "Session {} is neither this session nor one it continues.",
                        id.0
                    )
                })?;
            Some(segment)
        }
    };
    let count = ancestor
        .as_ref()
        .and_then(|segment| segment.to)
        .map_or_else(|| log.count(), |to| to.0.saturating_add(1));
    let (start, max, to) = match args.from_seq {
        Some(from_seq) => {
            let from = from_seq.0;
            if from >= count {
                return Err(PAST.to_owned());
            }
            let max = match args.to_seq {
                Some(to) if to < from_seq => return Err(REVERSED.to_owned()),
                Some(to) => usize::try_from(to.0 - from)
                    .map_or(HISTORY, |span| span.saturating_add(1).min(HISTORY)),
                None => HISTORY,
            };
            (from, max, args.to_seq.map_or(u64::MAX, |to| to.0))
        }
        None => {
            if count == 0 {
                return Ok(vec![]);
            }
            let end = args.to_seq.map_or(count - 1, |to| to.0.min(count - 1));
            let back = u64::try_from(HISTORY - 1).map_or(end, |back| back.min(end));
            let start = end - back;
            let max = usize::try_from(back).map_or(HISTORY, |back| back + 1);
            (start, max, end)
        }
    };
    let lines = match ancestor {
        Some(segment) => segment.range(start, max),
        None => log.range(start, max),
    }
    .map_err(|_| UNFIT.to_owned())?;
    Ok(lines
        .into_iter()
        .filter(|line| line.seq.is_some_and(|seq| seq.0 <= to))
        .collect())
}

pub(crate) fn accept(conn: &mut Conn, id: CommandId, result: Option<CommandResult>) {
    send(
        conn,
        Event::CommandAccepted(CommandAccepted {
            command_id: id,
            result,
        }),
    );
}

/// What a socket connection is, for the session-socket refusals
/// (`docs/permissions.md`, "Sessions on the sockets"): a local connection
/// carries the kernel peer pid and every session-process pid on its walk,
/// and a local connection whose peer credential or walk could not be read
/// carries what is known. An incomplete walk keeps a known peer pid so the
/// direct-parent exception can still be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attribution {
    /// A local connection with its kernel peer pid and matching
    /// session-process pids.
    Local {
        /// The kernel-reported peer pid.
        peer_pid: support::peer::Pid,
        /// Every session-process pid on the peer's strict-ancestor walk,
        /// nearest first.
        session_process_pids: Vec<support::peer::Pid>,
    },
    /// A local connection whose peer credential or walk could not be
    /// read, failing closed.
    Incomplete {
        /// The kernel-reported peer pid, when the credential read held.
        peer_pid: Option<support::peer::Pid>,
        /// The unreadable pid, when the walk failed; `None` when the peer
        /// credential itself could not be read.
        pid: Option<support::peer::Pid>,
        /// The peer-credential read's error text, when the credential
        /// itself could not be read; the refusal names it.
        peer_error: Option<String>,
    },
}

impl Attribution {
    /// The kernel peer pid, when the credential read held.
    pub(crate) fn peer_pid(&self) -> Option<support::peer::Pid> {
        match self {
            Attribution::Local { peer_pid, .. } => Some(*peer_pid),
            Attribution::Incomplete { peer_pid, .. } => *peer_pid,
        }
    }
}

/// The refused command class a `from_session` message names
/// (`docs/permissions.md`, "Sessions on the sockets").
pub(crate) enum FromSessionRefusal<'a> {
    /// An approval answer to a session.
    Approval {
        /// The session the approval is for.
        target: &'a contract::SessionId,
    },
    /// `prompt`, `steer` or `reply` to a session on the caller's walk or
    /// sharing a session process with it.
    SessionCommand {
        /// The refused wire command.
        command: &'a str,
        /// The refused session.
        target: &'a contract::SessionId,
    },
}

impl FromSessionRefusal<'_> {
    /// The wire command the refusal names in its fallback messages.
    fn command(&self) -> &str {
        match self {
            FromSessionRefusal::Approval { .. } => "reply",
            FromSessionRefusal::SessionCommand { command, .. } => command,
        }
    }
}

/// The `from_session` message for a refused command: the exact sentence for
/// the refusal class when the caller's walk passes a session process, the
/// unreadable pid on an incomplete caller walk, otherwise the preserved
/// peer-credential error. No session id of the caller's is ever named:
/// which session it is, Fiber reads from the process tree, never from the
/// arguments.
pub(crate) fn from_session_message(
    attribution: &Attribution,
    refusal: FromSessionRefusal<'_>,
) -> String {
    match attribution {
        Attribution::Local { .. } => match refusal {
            FromSessionRefusal::Approval { target } => format!(
                "Only a person answers an approval. This connection comes from inside a session, so `reply` to session `{}` is refused.",
                target.0
            ),
            FromSessionRefusal::SessionCommand { command, target } => format!(
                "A connection from inside a session cannot send `{command}` to session `{}`.",
                target.0
            ),
        },
        Attribution::Incomplete { pid: Some(pid), .. } => {
            let command = refusal.command();
            format!(
                "The process walk from this connection is incomplete at unreadable pid `{pid}`, so `{command}` is refused."
            )
        }
        Attribution::Incomplete { peer_error, .. } => {
            let command = refusal.command();
            format!(
                "This connection's process could not be identified ({}), so `{command}` is refused.",
                peer_error.as_deref().unwrap_or("unknown error")
            )
        }
    }
}

/// The `from_session` message when the refused target session's own process
/// cannot be read: the target names the session, not the caller.
pub(crate) fn unreadable_target_message(target: &contract::SessionId, command: &str) -> String {
    format!(
        "Session `{}`'s process could not be read, so `{command}` is refused.",
        target.0
    )
}

/// A refused socket command: the `from_session` code with its sentence.
pub(crate) struct Refused {
    pub(crate) code: ErrorCode,
    pub(crate) message: String,
}

/// The process reads the refusal paths use: the test seam when set,
/// otherwise the kernel.
pub(crate) fn read_for(_gate: &Gate) -> Arc<dyn support::peer::ProcessRead + Send + Sync> {
    #[cfg(test)]
    if let Some(read) = lock(&_gate.test_read).clone() {
        return read;
    }
    Arc::new(support::peer::KernelProcessRead)
}

/// The attribution of one accepted local connection: the kernel peer pid
/// first, then every session-process pid on its walk. A failed peer
/// credential or walk reads as [`Attribution::Incomplete`], never as
/// remote. The accepted socket peer's own arguments are never read: only
/// strict ancestors are walked.
pub(crate) fn attribute_peer(
    peer: Result<support::peer::Pid, support::peer::PeerCredentialError>,
    read: &dyn support::peer::ProcessRead,
) -> Attribution {
    match peer {
        Err(error) => Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some(error.to_string()),
        },
        Ok(peer_pid) => match support::peer::walk_ancestors(
            peer_pid,
            read,
            contract::session_process::is_session_process,
        ) {
            Ok(walk) => Attribution::Local {
                peer_pid,
                session_process_pids: walk.matched_pids,
            },
            Err(incomplete) => Attribution::Incomplete {
                peer_pid: Some(peer_pid),
                pid: Some(incomplete.pid),
                peer_error: None,
            },
        },
    }
}

/// Whether the parsed socket command answers an approval: a `reply`
/// carrying the approval answer (`docs/invocation.md`, "Replying").
fn is_approval_reply(command: &Command) -> bool {
    matches!(
        command,
        Command::Reply(reply)
            if matches!(
                reply.answer,
                contract::commands::ReplyAnswer::Approval { .. }
            )
    )
}

/// The `from_session` refusal for one socket command, if any
/// (`docs/permissions.md`, "Sessions on the sockets"). Refusal 1 rejects
/// an approval answer from a local connection whose walk passes a session
/// process, or whose walk could not be read, with no exception. Refusal 2
/// rejects `prompt`, `steer` and every `reply` when the target pid is on
/// the caller's walk, or when a session-process pid from this session's
/// parent walk is also on the caller's walk, unless the caller is this
/// session's current kernel parent with a readable ancestry. A `message`
/// is a session message, never a person's words, so it is never refused.
/// An unreadable target pid, parent, or walk fails closed.
pub(crate) fn session_command_refusal(
    attribution: &Attribution,
    command: &Command,
    target: &contract::SessionId,
    target_pid: support::peer::Pid,
    read: &dyn support::peer::ProcessRead,
) -> Option<Refused> {
    let caller_matches: &[support::peer::Pid] = match attribution {
        Attribution::Local {
            session_process_pids,
            ..
        } => {
            if session_process_pids.is_empty() {
                return None;
            }
            session_process_pids
        }
        // An incomplete caller walk fails closed: only the direct-parent
        // exception below still lets it through.
        Attribution::Incomplete { .. } => &[],
    };
    if is_approval_reply(command) {
        return Some(Refused {
            code: ErrorCode::FromSession,
            message: from_session_message(attribution, FromSessionRefusal::Approval { target }),
        });
    }
    let command_name = match command {
        Command::Prompt(_) => "prompt",
        Command::Steer(_) => "steer",
        Command::Reply(_) => "reply",
        Command::Subscribe(_)
        | Command::SteerDrop(_)
        | Command::Message(_)
        | Command::Cancel
        | Command::JobStop(_)
        | Command::JobInput(_)
        | Command::Background
        | Command::Reload
        | Command::Import(_)
        | Command::Tools
        | Command::Hooks
        | Command::McpServers
        | Command::McpLogin(_)
        | Command::McpReconnect(_)
        | Command::Commands
        | Command::Skills
        | Command::Quota(_)
        | Command::Credentials
        | Command::History(_)
        | Command::ArtifactPage(_)
        | Command::Model(_)
        | Command::Credential(_)
        | Command::Name(_)
        | Command::AddDirectory(_)
        | Command::Handoff(_)
        | Command::Rewind(_)
        | Command::ChangeWorkspace(_)
        | Command::Shell(_)
        | Command::ProposeNotes(_)
        | Command::Command(_)
        | Command::Call(_)
        | Command::Ready
        | Command::Close(_)
        | Command::OfferedFile(_)
        | Command::Unknown => return None,
    };
    let refusal = || FromSessionRefusal::SessionCommand {
        command: command_name,
        target,
    };
    // Parent and session-host ancestors come from one kernel walk, so a
    // second parent read cannot authorize against a different process tree.
    let target_walk = match support::peer::walk_ancestors(
        target_pid,
        read,
        contract::session_process::is_session_process,
    ) {
        Ok(walk) => walk,
        Err(_) => {
            return Some(Refused {
                code: ErrorCode::FromSession,
                message: unreadable_target_message(target, command_name),
            });
        }
    };
    // The direct-parent exception: the target's own parent still reaches
    // it, whatever the peer's arguments say, but never on an unreadable
    // target ancestry.
    if target_walk.parent_pid == attribution.peer_pid() {
        return None;
    }
    match attribution {
        Attribution::Incomplete { .. } => Some(Refused {
            code: ErrorCode::FromSession,
            message: from_session_message(attribution, refusal()),
        }),
        Attribution::Local { .. } => {
            if caller_matches.contains(&target_pid)
                || target_walk
                    .matched_pids
                    .iter()
                    .any(|pid| caller_matches.contains(pid))
            {
                Some(Refused {
                    code: ErrorCode::FromSession,
                    message: from_session_message(attribution, refusal()),
                })
            } else {
                None
            }
        }
    }
}

/// A rejection frees the id for a retry, except one that never reserved it:
/// `malformed` (the line's id may belong to an accepted command) and
/// `duplicate_command` (the id is the earlier command's).
pub(crate) fn reject(conn: &mut Conn, id: Option<CommandId>, code: ErrorCode, message: &str) {
    if let Some(id) = &id
        && !matches!(code, ErrorCode::Malformed | ErrorCode::DuplicateCommand)
    {
        conn.gate.accepted.release(id);
    }
    send(
        conn,
        Event::CommandRejected(CommandRejected {
            command_id: id,
            code,
            message: message.to_owned(),
        }),
    );
}

fn send(conn: &mut Conn, event: Event) {
    // The driver's acknowledgement goes to the host call, not to any
    // stream: the first of `send` and `inbox_ack` takes it and moves the
    // sink to `Answered`. A send on an answered connection writes nowhere:
    // every `dispatch` arm answers a driven connection once, so the single
    // answer is never lost.
    let origin = conn.origin();
    match std::mem::replace(&mut conn.sink, Sink::Answered { origin }) {
        Sink::Direct { mut direct, writer } => {
            let line = session::envelope(&conn.gate.session_id, conn.gate.clock.as_ref(), &event);
            if write_line(&mut direct, &line).is_err() {
                conn.gone = true;
            }
            conn.sink = Sink::Direct { direct, writer };
        }
        Sink::Writer { outbox, switches } => {
            let line = session::envelope(&conn.gate.session_id, conn.gate.clock.as_ref(), &event);
            outbox.push_kept(line);
            conn.sink = Sink::Writer { outbox, switches };
        }
        Sink::Driven { answer, origin } => {
            conn.sink = Sink::Answered { origin };
            if let Event::CommandAccepted(accepted) = event {
                answer.0(Ok(accepted.result));
            } else if let Event::CommandRejected(rejected) = event {
                answer.0(Err(Rejection {
                    code: rejected.code,
                    message: rejected.message,
                }));
            } else {
                // `send` carries only acknowledgements; anything else answers `closing`.
                answer.0(Err(Rejection {
                    code: ErrorCode::Closing,
                    message: ENDED.to_owned(),
                }));
            }
        }
        Sink::Answered { origin } => {
            conn.sink = Sink::Answered { origin };
        }
    }
}

fn unknown(conn: &mut Conn, id: CommandId, command: &str) {
    reject(
        conn,
        Some(id),
        ErrorCode::UnknownCommand,
        &not_built(command),
    );
}

/// A label-bearing subscribe's deferred response envelopes: acceptance
/// with the opening boundary at full, then a notice on failure.
type Settled = Vec<Envelope>;

/// Cancels a label-bearing subscribe's deferred response when its reader
/// leaves before the loop answers: taking the sender closes the channel,
/// so the writer's wait ends instead of hanging the join that follows.
/// The connection's outbox holds one per label-bearing subscribe; the
/// reader's cleanup cancels them all before the writer is joined.
pub(super) struct SettleCancel {
    sender: Arc<Mutex<Option<mpsc::Sender<Settled>>>>,
}

impl Drop for SettleCancel {
    fn drop(&mut self) {
        drop(lock(&self.sender).take());
    }
}

/// Builds the deferred response for a label-bearing subscribe: the Ack
/// the loop answers once the delivered switch admits or refuses, the
/// receiver the writer emits before history, and the cancel the reader
/// holds until it leaves. The loop's answer becomes subscribe acceptance
/// with the opening boundary at full, then a connection-local notice on
/// failure; the subscribe id is never released and no rejection is sent
/// for it. The sender is taken exactly once, by the answer or by the
/// cancel, so a late answer after teardown is dropped and a teardown
/// after the answer changes nothing. The callback only publishes
/// envelopes; it never blocks the loop on socket output.
fn settle_response(
    gate: &Arc<Gate>,
    id: CommandId,
    last_seq: Option<contract::Seq>,
) -> (Ack, mpsc::Receiver<Settled>, SettleCancel) {
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Some(tx)));
    let cancel = SettleCancel {
        sender: Arc::clone(&shared),
    };
    let gate = Arc::clone(gate);
    let ack = guard(move |result: Answer| {
        let mut settled = vec![session::envelope(
            &gate.session_id,
            gate.clock.as_ref(),
            &Event::CommandAccepted(CommandAccepted {
                command_id: id,
                result: last_seq.map(|last_seq| CommandResult::Subscribe { last_seq }),
            }),
        )];
        if let Err(rejection) = result {
            settled.push(session::envelope(
                &gate.session_id,
                gate.clock.as_ref(),
                &Event::Notice(Notice {
                    code: rejection.code,
                    message: rejection.message,
                    extension: None,
                }),
            ));
        }
        if let Some(tx) = lock(&shared).take() {
            tx.send(settled).unwrap_or(());
        }
    });
    (ack, rx, cancel)
}

pub(crate) fn inbox_ack(conn: &mut Conn, id: CommandId) -> Ack {
    // The driver's acknowledgement goes to the host call, as the socket
    // path's does to its outbox: the first of `inbox_ack` and `send` takes
    // it and moves the sink to `Answered`; a rejection frees the id.
    let origin = conn.origin();
    match std::mem::replace(&mut conn.sink, Sink::Answered { origin }) {
        sink @ (Sink::Direct { .. } | Sink::Answered { .. }) => {
            conn.sink = sink;
            guard(|_| {})
        }
        Sink::Writer { outbox, switches } => {
            let queued = outbox.clone();
            let gate = Arc::clone(&conn.gate);
            conn.sink = Sink::Writer { outbox, switches };
            guard(move |result| {
                let event = match result {
                    Ok(result) => Event::CommandAccepted(CommandAccepted {
                        command_id: id,
                        result,
                    }),
                    Err(rejection) => {
                        gate.accepted.release(&id);
                        Event::CommandRejected(CommandRejected {
                            command_id: Some(id),
                            code: rejection.code,
                            message: rejection.message,
                        })
                    }
                };
                queued.push_kept(session::envelope(
                    &gate.session_id,
                    gate.clock.as_ref(),
                    &event,
                ));
            })
        }
        Sink::Driven { answer, origin } => {
            conn.sink = Sink::Answered { origin };
            let gate = Arc::clone(&conn.gate);
            guard(move |result| {
                if result.is_err() {
                    gate.accepted.release(&id);
                }
                answer.0(result);
            })
        }
    }
}

/// Answers `closing` when the loop drops the acknowledgement uncalled.
fn guard(answer: impl FnOnce(Answer) + Send + 'static) -> Ack {
    let mut once = Once {
        answer: Some(Box::new(answer)),
    };
    Ack(Box::new(move |result| {
        if let Some(answer) = once.answer.take() {
            answer(result);
        }
    }))
}

struct Once {
    answer: Option<Box<dyn FnOnce(Answer) + Send>>,
}

impl Drop for Once {
    fn drop(&mut self) {
        if let Some(answer) = self.answer.take() {
            answer(Err(Rejection {
                code: ErrorCode::Closing,
                message: ENDED.to_owned(),
            }));
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
