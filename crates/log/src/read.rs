//! The reading side: a session's log as it stands, and watchers that receive
//! each event as it is written. Readers take no lock: the log is append-only,
//! and a reader stops at the last complete line (`docs/events.md`, "Writing").

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};

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

// ponytail: 1,024 is picked, not measured. std's bounded channel allocates
// every slot up front, 160 bytes each (152 for an `Envelope` on 64-bit), so
// 160 KiB per watcher. The busy-session memory budget
// (`docs/performance.md`) and a slow watcher's measured lag would set it.
/// How many events a watcher's channel holds before it falls behind.
const CAPACITY: usize = 1024;

/// What happens to a watcher once its channel is empty and disconnected.
#[derive(Default)]
pub(crate) enum Next {
    /// The log is gone; the watcher ends.
    #[default]
    End,
    /// The watcher fell behind, and the log carried on in a new channel.
    Rearmed(Receiver<Envelope>),
    /// A write failed and the log stopped.
    Failed {
        /// The session.
        session: String,
        /// The failure.
        cause: String,
    },
}

type Shared = Arc<Mutex<Next>>;

fn set(shared: &Shared, next: Next) -> Next {
    std::mem::replace(
        &mut *shared.lock().unwrap_or_else(PoisonError::into_inner),
        next,
    )
}

/// The log's end of one watcher. Sending never blocks the writer.
pub(crate) struct Feed {
    tx: SyncSender<Envelope>,
    shared: Shared,
}

impl Feed {
    /// Hands the watcher `line`; false once the watcher is gone. A watcher
    /// whose channel is full gets a new one and re-reads the durable lines it
    /// missed from the log; the ephemeral ones are lost, which costs nothing
    /// (`docs/architecture.md`, "Streaming").
    pub(crate) fn push(&mut self, line: &Envelope) -> bool {
        if Arc::strong_count(&self.shared) == 1 {
            return false;
        }
        match self.tx.try_send(line.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                let (tx, rx) = sync_channel(CAPACITY);
                set(&self.shared, Next::Rearmed(rx));
                self.tx = tx;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Ends the watcher with the failure that stopped the log, once it has
    /// every line it was sent.
    pub(crate) fn fail(self, session: &str, cause: &str) {
        set(
            &self.shared,
            Next::Failed {
                session: session.to_owned(),
                cause: cause.to_owned(),
            },
        );
    }
}

/// Receives a session's events, durable and ephemeral, as they are written,
/// from the moment [`crate::Log::watch`] made it. It never slows the writer:
/// a watcher that falls behind re-reads the durable lines it missed from the
/// log by `seq`, and loses the ephemeral ones.
pub struct Watcher {
    rx: Receiver<Envelope>,
    shared: Shared,
    dir: PathBuf,
    /// The `seq` of the next durable line this watcher has not yet returned.
    next: u64,
    /// Lines re-read from the log, not yet returned.
    backlog: VecDeque<Envelope>,
}

/// A watcher and the log's end of it, for a log whose next `seq` is `next`.
pub(crate) fn watcher(dir: PathBuf, next: u64) -> (Feed, Watcher) {
    let (tx, rx) = sync_channel(CAPACITY);
    let shared = Shared::default();
    let feed = Feed {
        tx,
        shared: Arc::clone(&shared),
    };
    let watcher = Watcher {
        rx,
        shared,
        dir,
        next,
        backlog: VecDeque::new(),
    };
    (feed, watcher)
}

impl Watcher {
    /// The next event, waiting for one to be written. `None` once the log is
    /// dropped and every event written before that has been returned; an
    /// error if the log stopped on a failed write.
    pub fn recv(&mut self) -> Result<Option<Envelope>, Error> {
        loop {
            let line = match self.backlog.pop_front() {
                Some(line) => line,
                None => match self.rx.recv() {
                    Ok(line) => line,
                    Err(RecvError) => match set(&self.shared, Next::End) {
                        Next::Rearmed(rx) => {
                            // ponytail: re-reads the whole log to find the
                            // lines it missed; a read from an offset by `seq`
                            // when logs grow large enough to notice.
                            self.rx = rx;
                            // What the new channel holds so far is stale:
                            // its durable lines are in the log, read next,
                            // and its ephemeral ones are obsolete by then.
                            while self.rx.try_recv().is_ok() {}
                            self.backlog = read(&self.dir)?.into();
                            continue;
                        }
                        Next::End => return Ok(None),
                        Next::Failed { session, cause } => {
                            return Err(Error::Poisoned { session, cause });
                        }
                    },
                },
            };
            match line.seq {
                // Already returned, from the channel or from the log.
                Some(seq) if seq.0 < self.next => {}
                Some(seq) => {
                    self.next = seq.0 + 1;
                    return Ok(Some(line));
                }
                None => return Ok(Some(line)),
            }
        }
    }
}
