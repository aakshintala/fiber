//! The reading side: a session's log as it stands, and watchers that receive
//! each event as it is written. Readers take no lock: the log is append-only,
//! and a reader stops at the last complete line (`docs/events.md`, "Writing").

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use contract::Envelope;

use crate::{EVENTS, Error, io_at};

/// Every durable line in the session directory `dir`, in order, up to the
/// last complete line. A torn tail, a line cut short by a crash, is skipped;
/// a complete line that is not an event line is an error naming it.
pub fn read(dir: &Path) -> Result<Vec<Envelope>, Error> {
    let path = dir.join(EVENTS);
    let bytes = fs::read(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::NotFound(dir.to_owned())
        } else {
            io_at(&path)(e)
        }
    })?;
    bytes
        .get(..complete_len(&bytes))
        .unwrap_or_default()
        .split_inclusive(|b| *b == b'\n')
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_slice(line).map_err(|source| Error::Unreadable {
                path: path.clone(),
                line: i + 1,
                source,
            })
        })
        .collect()
}

/// The length of `bytes` up to and including its last newline: everything
/// before a torn tail.
pub(crate) fn complete_len(bytes: &[u8]) -> usize {
    bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)
}

/// How many events a watcher's queue holds before it falls behind. Picked,
/// not measured.
const CAPACITY: usize = 1024;

/// One watcher's bounded queue, shared by the log that fills it and the
/// watcher that drains it.
#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

#[derive(Default)]
struct State {
    lines: VecDeque<Envelope>,
    /// The queue filled and lines were dropped since the watcher last caught
    /// up from the log.
    lagged: bool,
    /// The log is gone; nothing more will arrive.
    closed: bool,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands the watcher `line` without ever blocking the writer. A watcher
    /// that has fallen behind gets nothing until it catches up: it re-reads
    /// the durable lines it missed from the log, and the ephemeral ones are
    /// lost, which costs nothing (`docs/architecture.md`, "Streaming").
    pub(crate) fn push(&self, line: &Envelope) {
        let mut state = self.lock();
        if state.lagged || state.lines.len() >= CAPACITY {
            state.lagged = true;
        } else {
            state.lines.push_back(line.clone());
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Tells the watcher the log is gone.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_one();
    }
}

/// Receives a session's events, durable and ephemeral, as they are written,
/// from the moment [`crate::Log::watch`] made it. It never slows the writer:
/// a watcher that falls behind re-reads the durable lines it missed from the
/// log by `seq`, and loses the ephemeral ones.
pub struct Watcher {
    queue: Arc<Queue>,
    dir: PathBuf,
    /// The `seq` of the next durable line this watcher has not yet returned.
    next: u64,
    /// Lines re-read from the log, not yet returned.
    backlog: VecDeque<Envelope>,
}

/// What the watcher takes from its queue.
enum Taken {
    Line(Envelope),
    CatchUp,
    Closed,
}

impl Watcher {
    pub(crate) fn new(queue: Arc<Queue>, dir: PathBuf, next: u64) -> Self {
        Self {
            queue,
            dir,
            next,
            backlog: VecDeque::new(),
        }
    }

    /// The next event, waiting for one to be written. `None` once the log is
    /// dropped and every event written before that has been returned.
    pub fn recv(&mut self) -> Result<Option<Envelope>, Error> {
        loop {
            let line = match self.backlog.pop_front() {
                Some(line) => line,
                None => match self.take() {
                    Taken::Line(line) => line,
                    Taken::CatchUp => {
                        // ponytail: re-reads the whole log to find the lines it
                        // missed; a read from an offset by `seq` when logs grow
                        // large enough for a lagging watcher to notice.
                        self.backlog = read(&self.dir)?.into();
                        continue;
                    }
                    Taken::Closed => return Ok(None),
                },
            };
            match line.seq {
                // Already returned, from the queue or from the log.
                Some(seq) if seq.0 < self.next => {}
                Some(seq) => {
                    self.next = seq.0 + 1;
                    return Ok(Some(line));
                }
                None => return Ok(Some(line)),
            }
        }
    }

    /// Waits for a line, for the need to catch up, or for the end. The
    /// lagged flag is cleared before the log is re-read, so a line written
    /// after the re-read is queued, never lost.
    fn take(&self) -> Taken {
        let mut state = self.queue.lock();
        loop {
            if let Some(line) = state.lines.pop_front() {
                return Taken::Line(line);
            }
            if state.lagged {
                state.lagged = false;
                return Taken::CatchUp;
            }
            if state.closed {
                return Taken::Closed;
            }
            state = self
                .queue
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}
