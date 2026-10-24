//! One client connection's writer thread (`docs/architecture.md`, "The
//! threads"): owns the Watcher and the socket stream, and writes the
//! connection's events.

use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::{self, ThreadId};

use contract::events::{CommandRejected, Event};
use contract::{CommandId, Envelope, ErrorCode, SCHEMA_VERSION, SessionId};
use serde_json::{Map, Value};

use super::level::{LEVEL, Switch, Writing, apply};
use crate::session::{Answered, CUT};
use crate::session::{Gate, envelope};

/// A first subscribe's deferred response: the loop's answer to the
/// delivered switch, emitted before anything the seeded watcher holds.
/// `response` is bare acceptance, then a notice on failure; `after` is
/// written next (summary snapshots; empty for full).
pub(crate) struct Prelude {
    pub(crate) response: mpsc::Receiver<Vec<Envelope>>,
    pub(crate) after: Vec<Envelope>,
}

/// Wakes a connection's writer so it leaves `recv`.
pub(crate) const STOP: &str = "doors.stop";

/// Which connection a writer serves, and the reader thread whose reads it
/// answers.
pub(crate) struct Flight {
    pub(crate) gate: Arc<Gate>,
    pub(crate) conn: u64,
    pub(crate) reader: ThreadId,
}

/// The sentence of an `indeterminate` answer the close grace gives.
const CUT_ANSWER: &str = "The session ended before answering; the command may have run.";

pub(crate) fn spawn_writer(
    gate: Arc<Gate>,
    id: u64,
    watcher: log::Watcher,
    stream: Box<dyn Write + Send>,
    summary: bool,
    prelude: Option<Prelude>,
) -> mpsc::Sender<Switch> {
    gate.conns.begin_writer();
    // `spawn_writer` runs on the reader thread: from here on the lines it
    // reads are recorded, and the writer answers each one.
    let reader = thread::current().id();
    gate.conns.note_owed(id, reader);
    // The writer's queue is reachable from the start, so a cut wakes it
    // while it still reads the log, not only past its end.
    let _ = gate.conns.register_wake(id, watcher.injector());
    gate.accepted.track(reader);
    let flight = Flight {
        gate: Arc::clone(&gate),
        conn: id,
        reader,
    };
    let ended = Arc::clone(&gate);
    let failed = Arc::clone(&gate);
    let (tx, rx) = mpsc::channel();
    let (switch_tx, switches) = mpsc::channel();
    match thread::Builder::new()
        .name("writer".to_owned())
        .spawn(move || {
            let _end = WriterEnd {
                gate: ended,
                conn: id,
                reader,
            };
            // The handle is stored before this write can block, so close joins it.
            if rx.recv().is_err() {
                return;
            }
            let mut stream = stream;
            // The loop's answer to the delivered switch, before folded
            // history: bare acceptance, then a notice on failure, then
            // the summary snapshots. A cancelled wait means the reader
            // is gone; ending here lets the join proceed.
            if let Some(prelude) = prelude {
                // Test-only pause point: the writer is parked here, so a
                // test that observes it knows the silence that follows is
                // the wait, not a thread that never ran.
                #[cfg(test)]
                crate::session::tests::note_switch_wait();
                let settled = match prelude.response.recv() {
                    Ok(settled) => settled,
                    Err(_) => return,
                };
                for line in settled.into_iter().chain(prelude.after) {
                    if write_line(stream.as_mut(), &line).is_err() {
                        return;
                    }
                }
            }
            if write_loop(watcher, stream, summary, switches, Some(&flight)) == Ended::Failed {
                // The watcher failed: the reader and socket stay open, so
                // the connection is shut and the client sees EOF after
                // every line before the one that failed.
                failed.conns.shut(id);
            }
        }) {
        Ok(handle) => {
            gate.conns.push_writer(id, handle);
            if let Ok(()) = tx.send(()) {}
        }
        Err(_) => {
            gate.accepted.untrack(reader);
            gate.conns.end_writer();
        }
    }
    switch_tx
}

/// Ends a writer: its reader's lines are no longer recorded, since nothing
/// answers them any more.
struct WriterEnd {
    gate: Arc<Gate>,
    conn: u64,
    reader: ThreadId,
}

impl Drop for WriterEnd {
    fn drop(&mut self) {
        self.gate.accepted.untrack(self.reader);
        self.gate.conns.note_writer_done(self.conn);
        self.gate.conns.end_writer();
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

/// Writes the connection's lines until the log ends and, past its end, until
/// everything its reader read is answered.
///
/// Before the end, a `STOP` from the reader returns at once, unless the
/// session is closing: then the writer carries on to the end, so the client
/// gets every line up to `fiber_exited`. Past the end only kept lines
/// arrive: the answers of commands still running, `STOP`, `LEVEL` and `CUT`.
/// The writer ends once it has met `STOP` and nothing its reader read is
/// unanswered, or when the grace cuts it. With no `flight` there is no
/// tracking and no cut: `STOP` ends the writer.
pub(crate) fn write_loop(
    watcher: log::Watcher,
    mut stream: Box<dyn Write + Send>,
    summary: bool,
    switches: mpsc::Receiver<Switch>,
    flight: Option<&Flight>,
) -> Ended {
    let mut writing = Writing {
        watcher,
        summary,
        cutoff: 0,
        written: None,
    };
    let mut stop_seen = false;
    loop {
        // Test-only pause point: the writer saw STOP while the session is
        // stopping and waits here instead of ending, so a test that
        // observes it knows the silence that follows is the wait, not a
        // thread that never ran (`docs/testing.md`, "Waits and timeouts").
        #[cfg(test)]
        if stop_seen && let Some(flight) = flight {
            flight
                .gate
                .note(crate::session::tests::Probe::WriterParkedAtStop);
        }
        let line = match writing.watcher.recv() {
            Ok(Some(line)) => line,
            Ok(None) => {
                match past_end(&mut writing, &switches, stream.as_mut(), flight, stop_seen) {
                    Past::Ended(ended) => return ended,
                    Past::Switched => continue,
                }
            }
            Err(_) => return Ended::Failed,
        };
        // A cut pushed while the writer still reads the log: the writer
        // sees it by the flag when it reaches the end, unless it still
        // owes an answer. Then the grace already decided, so it answers
        // now: `Session::quiesce` cuts while the log is open, and waiting
        // for the end would leave the answer unwritten at `fiber_exited`.
        if line.kind == CUT {
            if let Some(flight) = flight
                && flight.gate.accepted.unanswered(flight.reader) > 0
            {
                return write_cut(&mut writing, stream.as_mut(), flight);
            }
            continue;
        }
        if line.kind == STOP {
            match flight {
                Some(flight) if flight.gate.conns.stopped() => stop_seen = true,
                Some(_) | None => return Ended::Done,
            }
            continue;
        }
        if line.kind == LEVEL {
            if apply(&mut writing, &switches, stream.as_mut(), flight).is_err() {
                return Ended::Failed;
            }
            continue;
        }
        if write_one(&line, &mut writing, stream.as_mut(), flight).is_err() {
            return Ended::Done;
        }
    }
}

/// What the writer does after the log's end.
enum Past {
    Ended(Ended),
    /// A level change moved it to a new watcher: back to the ordinary loop.
    Switched,
}

/// The writer past the log's end (see [`write_loop`]).
fn past_end(
    writing: &mut Writing,
    switches: &mpsc::Receiver<Switch>,
    stream: &mut dyn Write,
    flight: Option<&Flight>,
    mut stop_seen: bool,
) -> Past {
    loop {
        if let Some(flight) = flight {
            // Registered and read under one lock, so a cut pushed into a
            // queue this writer has just left is never missed.
            if flight
                .gate
                .conns
                .register_wake(flight.conn, writing.watcher.injector())
            {
                return Past::Ended(write_cut(writing, stream, flight));
            }
            if stop_seen && flight.gate.accepted.unanswered(flight.reader) == 0 {
                return Past::Ended(Ended::Done);
            }
        }
        let line = writing.watcher.recv_after_end();
        match line.kind.as_str() {
            CUT => {}
            STOP => {
                if flight.is_none() {
                    return Past::Ended(Ended::Done);
                }
                stop_seen = true;
            }
            LEVEL => {
                if apply(writing, switches, stream, flight).is_err() {
                    return Past::Ended(Ended::Failed);
                }
                return Past::Switched;
            }
            _ => {
                if write_one(&line, writing, stream, flight).is_err() {
                    return Past::Ended(Ended::Done);
                }
            }
        }
    }
}

/// The grace passed: writes what is already queued, without waiting, then
/// one `indeterminate` for each command still unanswered, in read order.
/// A command's own answer queued ahead of the cut is written, and that
/// command gets no `indeterminate`.
fn write_cut(writing: &mut Writing, stream: &mut dyn Write, flight: &Flight) -> Ended {
    while let Ok(Some(line)) = writing.watcher.try_recv() {
        if matches!(line.kind.as_str(), STOP | CUT | LEVEL) {
            continue;
        }
        if write_one(&line, writing, stream, Some(flight)).is_err() {
            return Ended::Done;
        }
    }
    for id in flight.gate.accepted.cut(flight.reader) {
        let answer = envelope(
            &flight.gate.session_id,
            flight.gate.clock.as_ref(),
            &Event::CommandRejected(CommandRejected {
                command_id: Some(CommandId(id)),
                code: ErrorCode::Indeterminate,
                message: CUT_ANSWER.into(),
            }),
        );
        if write_line(stream, &answer).is_err() {
            break;
        }
    }
    Ended::Done
}

/// Writes one line the way the connection's level shows it: the summary
/// filter and the lowering's cutoff drop what they drop, and an
/// acknowledgement is marked answered first.
fn write_one(
    line: &Envelope,
    writing: &mut Writing,
    stream: &mut dyn Write,
    flight: Option<&Flight>,
) -> std::io::Result<()> {
    if writing.summary && !summary_line(&line.kind) {
        return Ok(());
    }
    if line.seq.is_some_and(|seq| seq.0 < writing.cutoff) {
        return Ok(());
    }
    write_counted(stream, line, flight)?;
    if let Some(seq) = line.seq {
        writing.written = Some(seq.0);
    }
    Ok(())
}

/// Writes `line`, unless it acknowledges a command whose close grace cut
/// it: that command was answered `indeterminate`, and gets no second
/// answer. An acknowledgement it does write is marked answered.
pub(super) fn write_counted(
    stream: &mut dyn Write,
    line: &Envelope,
    flight: Option<&Flight>,
) -> std::io::Result<()> {
    if let Some(flight) = flight
        && matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
        && let Some(id) = line.payload.get("command_id").and_then(Value::as_str)
    {
        let code = (line.kind == "command_rejected")
            .then(|| line.payload.get("code").and_then(Value::as_str))
            .flatten();
        if flight.gate.accepted.answered(flight.reader, id, code) == Answered::Skip {
            return Ok(());
        }
        // Marked answered only once written: a waiter for owed answers
        // knows the acknowledgement is on the connection, not just decided.
        write_line(stream, line)?;
        flight.gate.accepted.resolve(flight.reader, id, code);
        return Ok(());
    }
    write_line(stream, line)
}

fn summary_line(kind: &str) -> bool {
    matches!(
        kind,
        "session_status" | "extensions_loaded" | "command_accepted" | "command_rejected"
    )
}

pub(crate) fn write_line(stream: &mut dyn Write, line: &Envelope) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(line)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    support::framing::write_line(stream, &bytes)
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
