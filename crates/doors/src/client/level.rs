//! A subscribed connection changing its level (`docs/invocation.md`,
//! `subscribe`): raising `summary` to `full` folds the stream from the log,
//! and lowering `full` to `summary` stops the stream after every durable
//! line written before the change. The connection keeps its one writer,
//! which the reader switches with a control line in order.

use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::commands::SubscribeLevel;
use contract::events::{CommandAccepted, Event};
use contract::{CommandId, Envelope, ErrorCode, SessionId};
use log::Injector;

use super::{Conn, control_line, reject};

/// The control kind the reader pushes to switch its writer: never written
/// to the client.
pub(crate) const LEVEL: &str = "doors.level";

/// What the writer switches to at its next [`LEVEL`] line.
pub(crate) struct Switch {
    pub(crate) watcher: log::Watcher,
    pub(crate) summary: bool,
    /// `log.count()` read during preparation. Lowering: the old writer
    /// drains durable lines below it first, and the new writer skips them.
    /// Raising: 0.
    pub(crate) cutoff: u64,
    /// Written directly after the switch: the acknowledgement first, then
    /// (summary only) the latest session_status and extensions_loaded.
    pub(crate) prelude: Vec<Envelope>,
}

/// The connection's route to its writer's current queue: every reader and
/// acknowledgement line goes through it, so a line pushed before a switch
/// is written at the old level, and one pushed after at the new level.
#[derive(Clone)]
pub(crate) struct Outbox(Arc<Mutex<Injector>>);

impl Outbox {
    pub(crate) fn new(injector: Injector) -> Self {
        Self(Arc::new(Mutex::new(injector)))
    }

    pub(crate) fn push_kept(&self, line: Envelope) {
        lock(&self.0).push_kept(line);
    }

    /// Under one lock: pushes `control` (kept) into the current queue,
    /// then makes `next` current.
    pub(crate) fn swap(&self, control: Envelope, next: Injector) {
        let mut current = lock(&self.0);
        current.push_kept(control);
        *current = next;
    }

    /// Pushes the writer's STOP line into the current queue
    /// (`super::stop_writer`).
    pub(crate) fn stop(&self, session: &SessionId) {
        self.push_kept(control_line(session, super::STOP, 0));
    }
}

/// What the writer carries between lines.
pub(super) struct Writing {
    pub(super) watcher: log::Watcher,
    pub(super) summary: bool,
    /// Durable lines below this are skipped; 0 until a lowering.
    pub(super) cutoff: u64,
    /// The `seq` of the last durable line written.
    pub(super) written: Option<u64>,
}

/// Whether the drain continues: until `cutoff - 1` is written. Nothing is
/// owed when `cutoff` is 0.
fn drain_more(written: Option<u64>, cutoff: u64) -> bool {
    written.map_or(cutoff > 0, |seq| seq + 1 < cutoff)
}

/// Reader side: a `subscribe` on a subscribed connection. A same-level
/// request is rejected and nothing else happens. Otherwise preparation
/// runs first: on failure the outbox, `conn.full` and the `clients` count
/// are untouched, and the rejection reaches the client at the old level.
pub(super) fn change(conn: &mut Conn, id: CommandId, level: SubscribeLevel) {
    let summary = matches!(level, SubscribeLevel::Summary);
    if summary == !conn.full {
        reject(conn, Some(id), ErrorCode::InvalidArguments, super::ALREADY);
        return;
    }
    // The new watcher is registered before `latest` is read (summary),
    // or seeded under the one log lock (full). A line written in between
    // is queued and may also be in `latest`; the latest wins. The log is
    // dropped here so this connection does not hold the session lock.
    let prepared = {
        let Some(log) = conn.gate.log.upgrade() else {
            reject(conn, Some(id), ErrorCode::Closing, super::ENDED);
            return;
        };
        if summary {
            let watcher = log.watch();
            let cutoff = log.count();
            let status = log.latest("session_status");
            let extensions = log.latest("extensions_loaded");
            drop(log);
            let injector = watcher.injector();
            let ack = crate::session::envelope(
                &conn.gate.session_id,
                conn.gate.clock.as_ref(),
                &Event::CommandAccepted(CommandAccepted {
                    command_id: id,
                    result: None,
                }),
            );
            let mut prelude = vec![ack];
            prelude.extend([status, extensions].into_iter().flatten());
            (watcher, injector, cutoff, prelude)
        } else {
            // Raising folds the stream from the log as a first `full`
            // subscribe does: over an unreadable page the new watcher
            // keeps every line before the one that failed, then the
            // connection closes at the failure.
            let watcher = log.watch_all_seeded();
            drop(log);
            let injector = watcher.injector();
            let ack = crate::session::envelope(
                &conn.gate.session_id,
                conn.gate.clock.as_ref(),
                &Event::CommandAccepted(CommandAccepted {
                    command_id: id,
                    result: None,
                }),
            );
            (watcher, injector, 0, vec![ack])
        }
    };
    let (watcher, injector, cutoff, prelude) = prepared;
    // The count changes before the writer can see the switch: the new
    // full watcher is registered, so an upgraded connection receives its
    // own `clients` line after the fold, and a downgrade's `clients` line
    // is queued ahead of its control line.
    conn.full = !summary;
    if summary {
        conn.gate.detach();
    } else {
        conn.gate.attach();
    }
    // The seed lines are in the new queue before the control line is
    // published (they were, by registration), so a pending
    // acknowledgement cannot precede them.
    let switch = Switch {
        watcher,
        summary,
        cutoff,
        prelude,
    };
    if conn
        .switches
        .as_ref()
        .is_none_or(|switches| switches.send(switch).is_err())
    {
        // The writer is dead: a dead writer does not always mean the
        // client has gone, so the connection is marked gone and the
        // reader runs its normal disconnect cleanup, which detaches when
        // `conn.full` already matches the count.
        conn.gone = true;
        return;
    }
    let Some(outbox) = conn.outbox.clone() else {
        conn.gone = true;
        return;
    };
    outbox.swap(control_line(&conn.gate.session_id, LEVEL, 0), injector);
}

/// Writer side, on a [`LEVEL`] line: takes the next [`Switch`] with
/// `try_recv` (never `recv`); on a lowering drains `writing.watcher`
/// through `switch.cutoff`; writes the prelude; installs the new watcher,
/// level and cutoff. With no switch pending it changes nothing. A failed
/// write, or the old watcher ending during the drain, is returned as an
/// error.
pub(super) fn apply(
    writing: &mut Writing,
    switches: &mpsc::Receiver<Switch>,
    stream: &mut dyn Write,
) -> std::io::Result<()> {
    let switch = match switches.try_recv() {
        Ok(switch) => switch,
        Err(_) => return Ok(()),
    };
    if switch.summary {
        // The queue lagged: the control line came out before the
        // catch-up. Every durable line below `cutoff` was either pushed
        // into the old queue or dropped from it (which set the lag flag)
        // before the control line, so reading on returns each of them
        // exactly once. Anything else is skipped, never written.
        while drain_more(writing.written, switch.cutoff) {
            match writing.watcher.recv() {
                Ok(Some(line)) => {
                    if line.seq.is_some_and(|seq| seq.0 < switch.cutoff) {
                        super::write_line(stream, &line)?;
                        writing.written = line.seq.map(|seq| seq.0);
                    }
                }
                Ok(None) | Err(_) => {
                    return Err(std::io::Error::other(
                        "the old watcher ended during the drain",
                    ));
                }
            }
        }
    }
    for line in &switch.prelude {
        super::write_line(stream, line)?;
    }
    writing.watcher = switch.watcher;
    writing.summary = switch.summary;
    writing.cutoff = switch.cutoff;
    Ok(())
}

fn lock(mutex: &Mutex<Injector>) -> MutexGuard<'_, Injector> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "level_tests.rs"]
mod tests;
