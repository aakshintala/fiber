//! The reading side: a session's log as it stands, and watchers that receive
//! each event as it is written. Readers take no lock: the log is append-only,
//! and a reader stops at the last complete line (`docs/events.md`, "Writing").

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};

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

// debt: 1,024 is picked, not measured. The queue grows only as lines
// wait in it. The busy-session memory budget (`docs/performance.md`) and a
// slow watcher's measured lag would set it.
/// How many events a watcher's queue holds before it falls behind.
const CAPACITY: usize = 1024;

/// One watcher's bounded queue, shared by the log that fills it and the
/// watcher that drains it. The log holds it weakly, so a dropped watcher's
/// queue is freed at once.
#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

#[derive(Default)]
struct State {
    /// Ordinary lines and kept lines in one order. A kept line stays where
    /// it was pushed, including ahead of a catch-up.
    queue: VecDeque<Envelope>,
    /// The queue filled and lines were dropped since the watcher last caught
    /// up from the log.
    lagged: bool,
    end: End,
}

/// Whether the log will send more.
#[derive(Default)]
enum End {
    #[default]
    Open,
    /// The log is gone.
    Closed,
    /// A write failed and the log stopped.
    Failed { session: String, cause: String },
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
        if state.lagged || state.queue.len() >= CAPACITY {
            state.lagged = true;
        } else {
            state.queue.push_back(line.clone());
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Hands the watcher `line` even when the queue is full or lagged, in
    /// push order with the ordinary lines. [`Queue::push`] would drop it.
    pub(crate) fn push_kept(&self, line: &Envelope) {
        // debt: kept lines are bounded only by the client's own command rate (one acknowledgement per command it sends); a cap on unsent acknowledgements per connection if a flooding client is ever seen.
        let mut state = self.lock();
        state.queue.push_back(line.clone());
        drop(state);
        self.ready.notify_one();
    }

    /// Tells the watcher the log is gone, unless it already knows the log
    /// failed.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        if matches!(state.end, End::Open) {
            state.end = End::Closed;
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Tells the watcher the log stopped on a failed write. It still gets
    /// every line written before, and catches up first if it fell behind.
    pub(crate) fn fail(&self, session: &str, cause: &str) {
        self.lock().end = End::Failed {
            session: session.to_owned(),
            cause: cause.to_owned(),
        };
        self.ready.notify_one();
    }
}

/// Pushes a line into one watcher's queue. Cloning it does not keep the
/// watcher alive: once the watcher is dropped, [`Injector::push`] does
/// nothing.
#[derive(Clone)]
pub struct Injector {
    queue: Weak<Queue>,
}

impl Injector {
    /// Hands `line` to the watcher, without waiting. A watcher that has
    /// fallen behind drops it, as it drops any other line, and a push after
    /// the watcher is dropped is ignored.
    pub fn push(&self, line: Envelope) {
        if let Some(queue) = self.queue.upgrade() {
            queue.push(&line);
        }
    }

    /// Hands `line` to the watcher even when it has fallen behind, in push
    /// order with the ordinary lines. A push after the watcher is dropped is
    /// ignored. A catch-up re-reads the log and does not drop a kept line
    /// still queued.
    pub fn push_kept(&self, line: Envelope) {
        if let Some(queue) = self.queue.upgrade() {
            queue.push_kept(&line);
        }
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
    End(End),
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

    /// A watcher whose first lines are `lines`, then whatever arrives after
    /// it was registered. `next` starts at 0 so a line already queued is
    /// skipped once `lines` has returned it.
    pub(crate) fn starting(queue: Arc<Queue>, dir: PathBuf, lines: Vec<Envelope>) -> Self {
        Self {
            queue,
            dir,
            next: 0,
            backlog: VecDeque::from(lines),
        }
    }

    /// A handle that pushes lines into this watcher's queue and no other.
    /// The handle does not keep the watcher alive.
    pub fn injector(&self) -> Injector {
        Injector {
            queue: Arc::downgrade(&self.queue),
        }
    }

    /// The next event, waiting for one to be written. `None` once the log is
    /// dropped and every event written before that has been returned; an
    /// error, after those events, if the log stopped on a failed write.
    pub fn recv(&mut self) -> Result<Option<Envelope>, Error> {
        loop {
            let line = match self.backlog.pop_front() {
                Some(line) => line,
                None => match self.take() {
                    Taken::Line(line) => line,
                    Taken::CatchUp => {
                        // debt: re-reads the whole log to find the lines it
                        // missed; a read from an offset by `seq` when logs grow
                        // large enough for a lagging watcher to notice.
                        self.backlog = read(&self.dir)?.into();
                        continue;
                    }
                    Taken::End(End::Failed { session, cause }) => {
                        return Err(Error::Poisoned { session, cause });
                    }
                    Taken::End(End::Open | End::Closed) => return Ok(None),
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

    /// Waits for a queued line, for the need to catch up, or for the end,
    /// in that order. Kept lines are in the queue, so they come out before
    /// a catch-up and a catch-up does not drop one still queued. The lagged
    /// flag is cleared before the log is re-read, so a line written after
    /// the re-read is queued, never lost. The end is handed over once and
    /// then reads as closed.
    fn take(&self) -> Taken {
        let mut state = self.queue.lock();
        loop {
            if let Some(line) = state.queue.pop_front() {
                return Taken::Line(line);
            }
            if state.lagged {
                state.lagged = false;
                return Taken::CatchUp;
            }
            match std::mem::replace(&mut state.end, End::Closed) {
                End::Open => state.end = End::Open,
                end @ (End::Closed | End::Failed { .. }) => return Taken::End(end),
            }
            state = self
                .queue
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}
