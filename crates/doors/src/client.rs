//! One client connection (`docs/architecture.md`, "The threads"): a thread
//! that reads its commands and answers them, and, once it has subscribed, a
//! thread that writes its events.

mod level;
mod writer;

#[cfg(test)]
pub(crate) use writer::write_loop;
pub(crate) use writer::{Prelude, STOP, control_line, spawn_writer, write_line};

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::mpsc;
use std::thread;

use contract::commands::{Command, CommandLine};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, Event, Notice};
use contract::inbox::{Ack, Answer, Delivery, Message, Rejection};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode};

use crate::session::{self, Gate};

pub(crate) const MALFORMED: &str =
    "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
pub(crate) const DUPLICATE: &str = "A command with this id was already accepted.";
pub(crate) const ALREADY: &str = "This connection is already subscribed at this level.";
pub(crate) const UNFIT: &str = "The arguments do not fit this command.";
const PAST: &str = "`from_seq` is past the latest line.";
const REVERSED: &str = "`to_seq` is before `from_seq`.";
pub(crate) const ENDED: &str = "The session ended before answering.";
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
/// shuts the socket. The writer is the socket's other clone.
pub(crate) fn serve(stream: support::stoppable::Reader, gate: Arc<Gate>, id: u64) {
    let Some(writer) = stream.get_ref().try_clone().ok() else {
        return;
    };
    serve_connection(stream, Box::new(writer), gate, id);
}

/// Reads `stream` and writes through `writer`. The connection's ender was
/// stored with its reader, which is how [`crate::session::Session::close`]
/// stops a blocked reader and reaps it. Production builds the reader and
/// the writer from one socket.
pub(crate) fn serve_connection(
    stream: support::stoppable::Reader,
    writer: Box<dyn Write + Send>,
    gate: Arc<Gate>,
    id: u64,
) {
    let _finish = Finish {
        gate: Arc::clone(&gate),
        id,
    };
    let Some(direct) = stream.get_ref().try_clone().ok() else {
        return;
    };
    let mut conn = Conn {
        id,
        gate,
        sink: Sink::Direct { direct, writer },
        full: false,
        gone: false,
    };
    let mut read = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                // The kept line feed, and any carriage return a client sends
                // with it, are JSON whitespace: `classify` reads the line
                // with them still on, so there is nothing to strip.
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
            let tools = conn
                .gate
                .tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            // The log is dropped once read, so this connection does not
            // hold the session lock. Without a log the session is closing
            // and the answer keeps its byte sizes.
            let rate = conn.gate.log.upgrade().map(|log| {
                let rate = log.rate();
                drop(log);
                rate
            });
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
        Command::Quota => {
            let (quota, notice_at) = conn.gate.quota();
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
            let read = history(&log, &args);
            drop(log);
            match read {
                Ok(lines) => accept(conn, id, Some(CommandResult::History { lines })),
                Err(message) => reject(conn, Some(id), ErrorCode::InvalidArguments, message),
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
            let ack = inbox_ack(conn, id.clone());
            conn.gate.deliver(Delivery::SteerDrop(args.command_id, ack));
        }
        Command::Message(args) => crate::message::run(conn, id, args),
        Command::Reply(reply) => crate::reply::route(conn, id, reply),
        Command::Cancel => cancel(conn, id),
        Command::Shell(args) => shell(conn, id, &args, name),
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
        Command::Handoff(args) => {
            let ack = inbox_ack(conn, id.clone());
            conn.gate.deliver(Delivery::Handoff(id, args, ack));
        }
        Command::Rewind(args) => crate::rewind::run(conn, id, args),
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
        Command::OfferedFile(args) => crate::offered::answer(conn, id, &args),
        Command::Unknown => unknown(conn, id, name),
        Command::ArtifactPage(args) => crate::artifact::run(conn, id, args),
    }
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
    let ack = inbox_ack(conn, id.clone());
    if prompt {
        let ack = match conn.gate.history.get().cloned().flatten() {
            Some(path) => {
                conn.gate
                    .prompts
                    .register(id.clone(), message.content.clone());
                crate::prompt_history::recorded(ack, path, &conn.gate, id)
            }
            None => ack,
        };
        conn.gate.deliver(Delivery::Prompt(message, ack));
    } else {
        conn.gate.deliver(Delivery::Steer(message, ack));
    }
}

/// The durable lines `args` asks for, at most [`HISTORY`] of them, read and
/// parsed from the log's offset table by `seq`: a line outside the window
/// is never read. Without `from_seq`, the answer is the last lines up to
/// `to_seq`, or up to the latest line, at most [`HISTORY`]; on an empty
/// log, with none.
fn history(
    log: &log::Log,
    args: &contract::commands::HistoryArgs,
) -> Result<Vec<Envelope>, &'static str> {
    let (start, max, to) = match args.from_seq {
        Some(from_seq) => {
            let from = from_seq.0;
            if from >= log.count() {
                return Err(PAST);
            }
            let max = match args.to_seq {
                Some(to) if to < from_seq => return Err(REVERSED),
                Some(to) => usize::try_from(to.0 - from)
                    .map_or(HISTORY, |span| span.saturating_add(1).min(HISTORY)),
                None => HISTORY,
            };
            (from, max, args.to_seq.map_or(u64::MAX, |to| to.0))
        }
        None => {
            let count = log.count();
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
    let lines = log.range(start, max).map_err(|_| UNFIT)?;
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

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
