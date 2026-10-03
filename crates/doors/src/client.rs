//! One client connection (`docs/architecture.md`, "The threads"): a thread
//! that reads its commands and answers them, and, once it has subscribed, a
//! thread that writes its events.

use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use contract::commands::{Command, CommandLine, SentPart};
use contract::events::{CommandAccepted, CommandRejected, CommandResult, Event};
use contract::inbox::{Ack, Answer, Delivery, Message, Rejection};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode, SCHEMA_VERSION, SessionId};
use log::Injector;
use serde_json::{Map, Value};

use crate::session::{self, Gate};

/// Wakes a connection's writer so it leaves `recv`.
pub(crate) const STOP: &str = "doors.stop";

const MALFORMED: &str = "A command is one JSON object per line, with a string `id` and `command`.";
const NOT_SUBSCRIBED: &str = "Send `subscribe` first.";
const ALREADY: &str = "This connection is already subscribed.";
const UNFIT: &str = "The arguments do not fit this command.";
const PAST: &str = "`from_seq` is past the latest line.";
const REVERSED: &str = "`to_seq` is before `from_seq`.";
const ENDED: &str = "The session ended before answering.";
const HISTORY: usize = 256;

/// A line that is not one of the commands this process answers.
fn not_built(command: &str) -> String {
    format!("`{command}` is not built in this Fiber yet.")
}

struct Conn {
    id: u64,
    gate: Arc<Gate>,
    direct: Option<UnixStream>,
    writer: Option<Box<dyn Write + Send>>,
    subscribed: bool,
    full: bool,
    injector: Option<Injector>,
    gone: bool,
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
        injector: None,
        gone: false,
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
    if let Some(injector) = conn.injector.take() {
        stop_writer(&injector, &conn.gate.session_id);
    }
}

/// Tells `injector`'s writer to exit. The line is kept when the queue is
/// full, so a disconnect still reaches a writer that has fallen behind.
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
    let line = match classify(bytes) {
        Ok(line) => line,
        Err(id) => {
            reject(conn, id, ErrorCode::Malformed, MALFORMED);
            return;
        }
    };
    if !conn.subscribed && line.command != "subscribe" {
        reject(
            conn,
            Some(line.id),
            ErrorCode::NotSubscribed,
            NOT_SUBSCRIBED,
        );
        return;
    }
    if conn.subscribed && line.command == "subscribe" {
        reject(conn, Some(line.id), ErrorCode::InvalidArguments, ALREADY);
        return;
    }
    if !built(&line.command) {
        unknown(conn, line.id, &line.command);
        return;
    }
    let parsed = match serde_json::from_value::<CommandLine>(line.value) {
        Ok(parsed) => parsed,
        Err(_) => {
            reject(conn, Some(line.id), ErrorCode::InvalidArguments, UNFIT);
            return;
        }
    };
    dispatch(conn, parsed, &line.command);
}

struct Classified {
    id: CommandId,
    command: String,
    value: Value,
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
        .any(|key| key != "id" && key != "command" && key != "args")
    {
        return Err(id);
    }
    let Some(id) = id else {
        return Err(None);
    };
    let Some(Value::String(command)) = map.get("command") else {
        return Err(Some(id));
    };
    match map.get("args") {
        None | Some(Value::Object(_)) => {}
        Some(_) => return Err(Some(id)),
    }
    Ok(Classified {
        id,
        command: command.clone(),
        value,
    })
}

fn built(command: &str) -> bool {
    matches!(
        command,
        "subscribe" | "prompt" | "steer" | "steer_drop" | "reply" | "tools" | "history" | "close"
    )
}

fn dispatch(conn: &mut Conn, line: CommandLine, name: &str) {
    let id = line.id;
    match line.command {
        Command::Subscribe(args) => subscribe(conn, id, args.level),
        Command::Tools => {
            let tools = conn.gate.tools.clone();
            accept(conn, id, Some(CommandResult::Tools { tools }));
        }
        Command::History(args) => match history(&conn.gate.dir, &args) {
            Ok(lines) => accept(conn, id, Some(CommandResult::History { lines })),
            Err(message) => reject(conn, Some(id), ErrorCode::InvalidArguments, message),
        },
        Command::Prompt(args) => match content(args.content) {
            Ok(content) => deliver_message(conn, id, content, true),
            Err(message) => reject(conn, Some(id), ErrorCode::InvalidArguments, &message),
        },
        Command::Steer(args) => match content(args.content) {
            Ok(content) => deliver_message(conn, id, content, false),
            Err(message) => reject(conn, Some(id), ErrorCode::InvalidArguments, &message),
        },
        Command::SteerDrop(args) => {
            let ack = inbox_ack(conn, id.clone());
            conn.gate.deliver(Delivery::SteerDrop(args.command_id, ack));
        }
        Command::Reply(reply) => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::Reply(reply, ack));
        }
        Command::Close => {
            let ack = inbox_ack(conn, id);
            conn.gate.deliver(Delivery::Close(ack));
        }
        Command::Message(_)
        | Command::Cancel
        | Command::JobStop(_)
        | Command::Background
        | Command::Reload
        | Command::Model(_)
        | Command::Credential(_)
        | Command::Name(_)
        | Command::Handoff(_)
        | Command::Rewind(_)
        | Command::Shell(_)
        | Command::Command(_) => unknown(conn, id, name),
    }
}

/// Queues a full subscriber's latest `session_status` after its fold. It is
/// ephemeral, so the fold does not have it and a catch-up cannot recover
/// it: it is pushed kept, like every line doors itself queues.
pub(crate) fn queue_latest(injector: &Injector, status: Option<Envelope>) {
    if let Some(status) = status {
        injector.push_kept(status);
    }
}

fn subscribe(conn: &mut Conn, id: CommandId, level: contract::commands::SubscribeLevel) {
    let summary = matches!(level, contract::commands::SubscribeLevel::Summary);
    // The watcher is registered before `latest` is read. A line written in
    // between is queued and may also be in `latest`; the latest wins. The
    // log is dropped here so this connection does not hold the session lock.
    let (watcher, status, extensions) = {
        let Some(log) = conn.gate.log.upgrade() else {
            reject(conn, Some(id), ErrorCode::Closing, ENDED);
            return;
        };
        let watcher = if summary {
            log.watch()
        } else {
            match log.watch_all() {
                Ok(watcher) => watcher,
                Err(_) => {
                    reject(conn, Some(id), ErrorCode::InvalidArguments, UNFIT);
                    return;
                }
            }
        };
        let status = log.latest("session_status");
        let extensions = if summary {
            log.latest("extensions_loaded")
        } else {
            None
        };
        (watcher, status, extensions)
    };
    let injector = watcher.injector();
    // The acknowledgement is written here, before the writer starts, so it
    // is the first line the client reads.
    accept(conn, id, None);
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
    } else {
        queue_latest(&injector, status);
    }
    conn.subscribed = true;
    conn.full = !summary;
    conn.injector = Some(injector);
    if conn.full {
        // The watcher is registered, so this connection receives the line.
        conn.gate.attach();
    }
    let Some(stream) = conn.writer.take() else {
        return;
    };
    spawn_writer(Arc::clone(&conn.gate), conn.id, watcher, stream, summary);
    // The writer is the only writer from here.
    drop(conn.direct.take());
}

fn deliver_message(conn: &mut Conn, id: CommandId, content: Vec<ContentPart>, prompt: bool) {
    let message = Message {
        content,
        sender: Sender {
            origin: Origin::Driver,
            command_id: id.clone(),
        },
    };
    let ack = inbox_ack(conn, id);
    if prompt {
        conn.gate.deliver(Delivery::Prompt(message, ack));
    } else {
        conn.gate.deliver(Delivery::Steer(message, ack));
    }
}

fn content(parts: Vec<SentPart>) -> Result<Vec<ContentPart>, String> {
    let mut out = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        match part {
            SentPart::Text { text } => out.push(ContentPart::Text { text }),
            SentPart::Image { .. } => {
                let number = index + 1;
                return Err(format!(
                    "Image {number} cannot be read: this Fiber processes no images yet."
                ));
            }
        }
    }
    Ok(out)
}

fn history(
    dir: &std::path::Path,
    args: &contract::commands::HistoryArgs,
) -> Result<Vec<Envelope>, &'static str> {
    let lines = match log::read(dir) {
        Ok(lines) => lines,
        Err(_) => return Err(UNFIT),
    };
    let latest = lines.iter().rev().find_map(|line| line.seq);
    let Some(latest) = latest else {
        return Err(PAST);
    };
    if args.from_seq > latest {
        return Err(PAST);
    }
    if args.to_seq.is_some_and(|to| to < args.from_seq) {
        return Err(REVERSED);
    }
    let to = args.to_seq.unwrap_or(latest);
    Ok(lines
        .into_iter()
        .filter(|line| {
            line.seq
                .is_some_and(|seq| seq >= args.from_seq && seq <= to)
        })
        .take(HISTORY)
        .collect())
}

fn accept(conn: &mut Conn, id: CommandId, result: Option<CommandResult>) {
    send(
        conn,
        Event::CommandAccepted(CommandAccepted {
            command_id: id,
            result,
        }),
    );
}

fn reject(conn: &mut Conn, id: Option<CommandId>, code: ErrorCode, message: &str) {
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
    let line = session::envelope(&conn.gate.session_id, conn.gate.clock.as_ref(), &event);
    if let Some(injector) = &conn.injector {
        injector.push_kept(line);
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

fn inbox_ack(conn: &Conn, id: CommandId) -> Ack {
    let Some(injector) = conn.injector.clone() else {
        return guard(|_| {});
    };
    let gate = Arc::clone(&conn.gate);
    guard(move |result| {
        let event = match result {
            Ok(result) => Event::CommandAccepted(CommandAccepted {
                command_id: id,
                result,
            }),
            Err(rejection) => Event::CommandRejected(CommandRejected {
                command_id: Some(id),
                code: rejection.code,
                message: rejection.message,
            }),
        };
        injector.push_kept(session::envelope(
            &gate.session_id,
            gate.clock.as_ref(),
            &event,
        ));
    })
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
) {
    gate.begin_writer();
    let ended = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    match thread::Builder::new()
        .name("writer".to_owned())
        .spawn(move || {
            let _end = WriterEnd(ended);
            // The handle is stored before this write can block, so close joins it.
            if rx.recv().is_err() {
                return;
            }
            write_loop(watcher, stream, summary);
        }) {
        Ok(handle) => {
            gate.push_writer(id, handle);
            if let Ok(()) = tx.send(()) {}
        }
        Err(_) => gate.end_writer(),
    }
}

struct WriterEnd(Arc<Gate>);

impl Drop for WriterEnd {
    fn drop(&mut self) {
        self.0.end_writer();
    }
}

pub(crate) fn write_loop(
    mut watcher: log::Watcher,
    mut stream: Box<dyn Write + Send>,
    summary: bool,
) {
    loop {
        let line = match watcher.recv() {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => return,
        };
        if line.kind == STOP {
            return;
        }
        if summary && !summary_line(&line.kind) {
            continue;
        }
        if write_line(stream.as_mut(), &line).is_err() {
            return;
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
