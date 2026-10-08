//! One client connection (`docs/architecture.md`, "The threads"): a thread
//! that reads its commands and answers them, and, once it has subscribed, a
//! thread that writes its events.

mod level;

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use contract::clock::wall_ms;
use contract::commands::{Command, CommandLine};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, Event};
use contract::inbox::{Ack, Answer, Delivery, Message, Rejection};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

#[cfg(test)]
use log::Injector;

use crate::session::{self, Gate};

/// Wakes a connection's writer so it leaves `recv`.
pub(crate) const STOP: &str = "doors.stop";

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
    pub(crate) direct: Option<UnixStream>,
    pub(crate) writer: Option<Box<dyn Write + Send>>,
    pub(crate) subscribed: bool,
    pub(crate) full: bool,
    pub(crate) outbox: Option<level::Outbox>,
    pub(crate) switches: Option<mpsc::Sender<level::Switch>>,
    pub(crate) gone: bool,
    /// The in-process driver's answer and extension: `send` and `inbox_ack`
    /// answer it, not a stream, and a driven `prompt` or `steer` carries it.
    /// One takes it: each command answers at once or through the inbox.
    pub(crate) drive: Option<(Ack, Origin)>,
}

/// Reads `stream` until the client hangs up. `id` is the slot [`Gate`] stored
/// the reader in, with the shutdown that unblocks it. The writer is the
/// socket's other clone.
pub(crate) fn serve(stream: UnixStream, gate: Arc<Gate>, id: u64) {
    let Some(writer) = stream.try_clone().ok() else {
        return;
    };
    serve_connection(stream, Box::new(writer), gate, id);
}

/// Reads `stream` and writes through `writer`. The connection's shutdown was
/// stored with its reader, which is how [`crate::session::Session::close`]
/// reaps a blocked writer. Production builds the reader and the writer from
/// one socket.
pub(crate) fn serve_connection(
    stream: UnixStream,
    writer: Box<dyn Write + Send>,
    gate: Arc<Gate>,
    id: u64,
) {
    let _finish = Finish {
        gate: Arc::clone(&gate),
        id,
    };
    let Some(direct) = stream.try_clone().ok() else {
        return;
    };
    let mut conn = Conn {
        id,
        gate,
        direct: Some(direct),
        writer: Some(writer),
        subscribed: false,
        full: false,
        outbox: None,
        switches: None,
        gone: false,
        drive: None,
    };
    let mut read = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                while buf.last().is_some_and(|byte| is_line_ending(*byte)) {
                    buf.pop();
                }
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
    if let Some(outbox) = conn.outbox.take() {
        outbox.stop(&conn.gate.session_id);
    }
}

/// Tells `injector`'s writer to exit. The line is kept when the queue is
/// full, so a disconnect still reaches a writer that has fallen behind.
#[cfg(test)]
pub(crate) fn stop_writer(injector: &Injector, session: &SessionId) {
    injector.push_kept(control_line(session, STOP, 0));
}

/// A carriage return and a line feed both end a command line.
fn is_line_ending(byte: u8) -> bool {
    byte == b'\n' || byte == b'\r'
}

pub(crate) fn shutdown_both(stream: UnixStream) -> Box<dyn Fn() + Send + Sync> {
    Box::new(move || match stream.shutdown(Shutdown::Both) {
        Ok(()) | Err(_) => {}
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
        self.gate.finish(self.id);
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
    if !conn.gate.reserve(&line.id) {
        reject(conn, Some(line.id), ErrorCode::DuplicateCommand, DUPLICATE);
        return;
    }
    if !conn.subscribed && line.command != "subscribe" {
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

pub(crate) fn built(command: &str) -> bool {
    matches!(
        command,
        "subscribe"
            | "prompt"
            | "steer"
            | "steer_drop"
            | "reply"
            | "cancel"
            | "tools"
            | "commands"
            | "skills"
            | "history"
            | "model"
            | "credential"
            | "close"
            | "shell"
            | "job_stop"
            | "background"
            | "handoff"
            | "rewind"
            | "command"
    )
}

pub(crate) fn dispatch(conn: &mut Conn, line: CommandLine, name: &str) {
    let id = line.id;
    match line.command {
        Command::Subscribe(args) => {
            if conn.subscribed {
                level::change(conn, id, args.level);
            } else {
                subscribe(conn, id, args.level);
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
            conn.gate.deliver(Delivery::Credential(args, ack));
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
        Command::Background => {
            let moving = conn.gate.jobs().is_some_and(|jobs| jobs.background() > 0);
            answer(conn, id, moving, NO_CALL);
        }
        Command::Command(args) => crate::run_command::run(conn, id, &args),
        Command::Message(_) | Command::Reload | Command::Name(_) => unknown(conn, id, name),
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
    let running = match crate::shell::start(&conn.gate, args) {
        Ok(running) => running,
        Err(crate::shell::Refused::Unknown) => return unknown(conn, id, name),
        Err(crate::shell::Refused::Rejected { code, message }) => {
            return reject(conn, Some(id), code, &message);
        }
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

fn subscribe(conn: &mut Conn, id: CommandId, level: contract::commands::SubscribeLevel) {
    let summary = matches!(level, contract::commands::SubscribeLevel::Summary);
    // The watcher is registered and its seed queued under the one log lock
    // that `append` takes, so no later line can be queued before an older
    // snapshot. The log is dropped here so this connection does not hold
    // the session lock.
    let (watcher, status, extensions) = {
        let Some(log) = conn.gate.log.upgrade() else {
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
    // The acknowledgement is written here, before the writer starts, so it
    // is the first line the client reads.
    accept(conn, id, None);
    #[cfg(test)]
    if conn.full {
        conn.gate
            .note(crate::session::tests::Probe::SubscribeAcknowledged);
    }
    if conn.gone {
        return;
    }
    if summary {
        for line in [status, extensions].into_iter().flatten() {
            if let Some(direct) = conn.direct.as_mut()
                && write_line(direct, &line).is_err()
            {
                conn.gone = true;
                return;
            }
        }
    }
    conn.subscribed = true;
    conn.outbox = Some(outbox);
    let Some(stream) = conn.writer.take() else {
        return;
    };
    conn.switches = Some(spawn_writer(
        Arc::clone(&conn.gate),
        conn.id,
        watcher,
        stream,
        summary,
    ));
    // The writer is the only writer from here.
    drop(conn.direct.take());
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
    let ack = inbox_ack(conn, id);
    if prompt {
        let ack = match conn.gate.history.get().cloned().flatten() {
            Some(path) => recorded(ack, path, &conn.gate, message.content.clone()),
            None => ack,
        };
        conn.gate.deliver(Delivery::Prompt(message, ack));
    } else {
        conn.gate.deliver(Delivery::Steer(message, ack));
    }
}

/// The durable lines `args` asks for, at most [`HISTORY`] of them, read and
/// parsed from the log's offset table by `seq`: a line outside the window
/// is never read.
fn history(
    log: &log::Log,
    args: &contract::commands::HistoryArgs,
) -> Result<Vec<Envelope>, &'static str> {
    let from = args.from_seq.0;
    if from >= log.count() {
        return Err(PAST);
    }
    let max = match args.to_seq {
        Some(to) if to < args.from_seq => return Err(REVERSED),
        Some(to) => {
            usize::try_from(to.0 - from).map_or(HISTORY, |span| span.saturating_add(1).min(HISTORY))
        }
        None => HISTORY,
    };
    let lines = log.range(from, max).map_err(|_| UNFIT)?;
    let to = args.to_seq.map_or(u64::MAX, |to| to.0);
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
        conn.gate.release(id);
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
    // The driver's acknowledgement goes to the host call, not to any stream.
    if let Some((answer, _)) = conn.drive.take() {
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
        return;
    }
    let line = session::envelope(&conn.gate.session_id, conn.gate.clock.as_ref(), &event);
    if let Some(outbox) = &conn.outbox {
        outbox.push_kept(line);
        return;
    }
    let Some(direct) = conn.direct.as_mut() else {
        return;
    };
    if write_line(direct, &line).is_err() {
        conn.gone = true;
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

pub(crate) fn inbox_ack(conn: &mut Conn, id: CommandId) -> Ack {
    // The driver's acknowledgement goes to the host call: a rejection frees
    // the id as the socket path does.
    if let Some((answer, _)) = conn.drive.take() {
        let gate = Arc::clone(&conn.gate);
        return guard(move |result| {
            if result.is_err() {
                gate.release(&id);
            }
            answer.0(result);
        });
    }
    let Some(outbox) = conn.outbox.clone() else {
        return guard(|_| {});
    };
    let gate = Arc::clone(&conn.gate);
    guard(move |result| {
        let event = match result {
            Ok(result) => Event::CommandAccepted(CommandAccepted {
                command_id: id,
                result,
            }),
            Err(rejection) => {
                gate.release(&id);
                Event::CommandRejected(CommandRejected {
                    command_id: Some(id),
                    code: rejection.code,
                    message: rejection.message,
                })
            }
        };
        outbox.push_kept(session::envelope(
            &gate.session_id,
            gate.clock.as_ref(),
            &event,
        ));
    })
}

/// Wraps a prompt's acknowledgement so an accepted prompt is appended to
/// the prompt history at `path` before `ack` answers, so a client that
/// reads its acceptance finds the line. A wrapper dropped uncalled drops
/// `ack`, which still answers `closing`.
fn recorded(ack: Ack, path: PathBuf, gate: &Arc<Gate>, content: Vec<ContentPart>) -> Ack {
    let gate = Arc::clone(gate);
    Ack(Box::new(move |result| {
        if result.is_ok() {
            // debt: a failed append is dropped unreported, when a session gets a diag log, record a failed history append.
            let ts = wall_ms(gate.clock.wall());
            match crate::prompt_history::append(&path, ts, &gate.session_id, &content) {
                Ok(()) | Err(_) => {}
            }
        }
        (ack.0)(result);
    }))
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

pub(crate) fn spawn_writer(
    gate: Arc<Gate>,
    id: u64,
    watcher: log::Watcher,
    stream: Box<dyn Write + Send>,
    summary: bool,
) -> mpsc::Sender<level::Switch> {
    gate.begin_writer();
    let ended = Arc::clone(&gate);
    let failed = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    let (switch_tx, switches) = mpsc::channel();
    match thread::Builder::new()
        .name("writer".to_owned())
        .spawn(move || {
            let _end = WriterEnd(ended);
            // The handle is stored before this write can block, so close joins it.
            if rx.recv().is_err() {
                return;
            }
            if write_loop(watcher, stream, summary, switches) == Ended::Failed {
                // The watcher failed: the reader and socket stay open, so
                // the connection is shut and the client sees EOF after
                // every line before the one that failed.
                failed.shut(id);
            }
        }) {
        Ok(handle) => {
            gate.push_writer(id, handle);
            if let Ok(()) = tx.send(()) {}
        }
        Err(_) => gate.end_writer(),
    }
    switch_tx
}

struct WriterEnd(Arc<Gate>);

impl Drop for WriterEnd {
    fn drop(&mut self) {
        self.0.end_writer();
    }
}

/// How a writer ended: Failed on a watcher error, else Done. A failed
/// write means the client already went, and the log's end is the session
/// close path, which reaps on its own: neither shuts the connection.
#[derive(PartialEq, Eq)]
pub(crate) enum Ended {
    Done,
    Failed,
}

pub(crate) fn write_loop(
    watcher: log::Watcher,
    mut stream: Box<dyn Write + Send>,
    summary: bool,
    switches: mpsc::Receiver<level::Switch>,
) -> Ended {
    let mut writing = level::Writing {
        watcher,
        summary,
        cutoff: 0,
        written: None,
    };
    loop {
        let line = match writing.watcher.recv() {
            Ok(Some(line)) => line,
            Ok(None) => return Ended::Done,
            Err(_) => return Ended::Failed,
        };
        if line.kind == STOP {
            return Ended::Done;
        }
        if line.kind == level::LEVEL {
            if level::apply(&mut writing, &switches, stream.as_mut()).is_err() {
                return Ended::Failed;
            }
            continue;
        }
        if writing.summary && !summary_line(&line.kind) {
            continue;
        }
        if line.seq.is_some_and(|seq| seq.0 < writing.cutoff) {
            continue;
        }
        if write_line(stream.as_mut(), &line).is_err() {
            return Ended::Done;
        }
        if let Some(seq) = line.seq {
            writing.written = Some(seq.0);
        }
    }
}

fn summary_line(kind: &str) -> bool {
    matches!(
        kind,
        "session_status" | "extensions_loaded" | "command_accepted" | "command_rejected"
    )
}

pub(crate) fn write_line(stream: &mut dyn Write, line: &Envelope) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(line)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

/// A control line for `session`, such as the one that stops its writer.
pub(crate) fn control_line(session: &SessionId, kind: &str, token: u64) -> Envelope {
    let mut payload = Map::new();
    payload.insert("token".to_owned(), Value::from(token));
    Envelope {
        kind: kind.to_owned(),
        session_id: session.clone(),
        ts: 0,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
