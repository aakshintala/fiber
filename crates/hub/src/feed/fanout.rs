//! The feed's subscriber fan-out: each subscriber's channel and writer
//! thread, the id counter, and the ended writers kept for joining.
//!
//! The owner's lock covers a `Fanout`: it takes no lock of its own, and
//! it never joins a thread, shuts a socket or writes to one. Every method
//! that ends a subscriber drops its sender and hands its shutdown and its
//! writer's handle back, so the caller shuts and joins after the guard
//! drops. Each subscriber carries two shutdowns of its connection: one
//! for the overflow report, one retained with a retired writer, so a
//! later remove or stop still frees a writer whose overflow shutdown has
//! not run yet.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// One line queued for a subscriber.
pub(crate) type Line = Arc<[u8]>;

// debt: 1,024 is picked, not measured. A hub queue budget
// (`docs/performance.md`, once set) and a slow client's measured lag
// would set it.
/// How many lines fit in one subscriber's queue: the try_send that finds
/// this many already queued reports Full, and the hub closes that
/// connection. A dequeued line being written is outside the queue.
pub(crate) const QUEUE: usize = 1_024;

/// How one connection's subscriber shuts down: its Unix halves, then its
/// websocket's TCP shut capability when the connection is bridged (local
/// connections carry none). Cloned before admission, so cleanup never
/// needs a fallible clone at the moment it runs.
pub(crate) struct Closer {
    pub(crate) stream: UnixStream,
    pub(crate) web: Option<Arc<crate::port::Web>>,
}

/// A channel holding [`QUEUE`] lines whose lines a new thread named
/// `name` writes to `writer`, each flushed; the thread ends on the first
/// failed write or once every sender is dropped.
pub(crate) fn spawn_writer(
    writer: Arc<Mutex<UnixStream>>,
    name: &str,
) -> Option<(SyncSender<Line>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::sync_channel::<Line>(QUEUE);
    let handle = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            for line in rx {
                let mut out = support::lock(&writer);
                // Queued lines carry their line feed; the writer takes the line.
                // An empty queue item sends nothing: the snapshot of an empty
                // feed holds no line, and a zero-byte write sends none.
                let stripped: &[u8] = line.strip_suffix(b"\n").unwrap_or(&line);
                if stripped.is_empty() {
                    continue;
                }
                if support::framing::write_line(&mut *out, stripped).is_err() {
                    return;
                }
            }
        })
        .ok()?;
    Some((tx, handle))
}

pub(crate) fn to_line(value: &impl serde::Serialize) -> Line {
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    Arc::from(bytes)
}

struct Subscriber {
    id: u64,
    state: State,
    closer: Closer,
    retired: Closer,
}

/// What a subscriber owns: a pending registration holds only its writer
/// for a later first line, while an active one holds its channel and
/// writer thread. A private enum, not flags, so no state contradicts an
/// optional sender or handle.
enum State {
    Pending {
        writer: Arc<Mutex<UnixStream>>,
    },
    Active {
        tx: SyncSender<Line>,
        writer: JoinHandle<()>,
    },
}

/// What removing one subscriber hands back: the shutdown its join path
/// uses, and its writer's handle when it has one, for the caller to shut
/// and join outside every lock.
pub(crate) struct WriterEnd {
    pub(crate) closer: Closer,
    pub(crate) writer: Option<JoinHandle<()>>,
}

/// What a targeted enqueue reaches: one subscriber, not the registry.
pub(crate) enum Targeted {
    /// The line fits: order with earlier lines is kept.
    Sent,
    /// Its queue is full: unregistered and kept for joining, with the
    /// shutdown that closes the whole connection.
    Full(Closer),
    /// Unknown or already removed: a no-op that resurrects nothing.
    Gone,
    /// Its receiver is gone: unregistered, with its handle for the
    /// caller to join.
    DeadWriter(JoinHandle<()>),
}

#[derive(Default)]
pub(crate) struct Fanout {
    list: Vec<Subscriber>,
    next: u64,
    retired: Vec<(Closer, JoinHandle<()>)>,
    /// Tests only: refuses the next lazy writer spawn in `broadcast`, so
    /// the failed-spawn path runs without an outside seam: thread-spawn
    /// failure has no deterministic outside trigger, since resource
    /// exhaustion cannot be staged per test and no socket outcome fails a
    /// spawn on demand.
    #[cfg(test)]
    fail_spawn: AtomicBool,
    /// Successful writer starts, including retired ones: never decremented.
    #[cfg(test)]
    started: usize,
}

impl Fanout {
    /// Registers a spawned writer pair and queues `snapshot` for it in
    /// order, ignoring a failed send: the channel is fresh, so the one
    /// slot the snapshot takes is always free. Returns the subscriber's
    /// fresh id, the first being 1. The snapshot is queued before the
    /// subscriber can receive any `broadcast`, which holds because the
    /// caller holds its lock across building the snapshot and `push`. The
    /// caller passes exclusive ownership of the sender: no clone of `tx`
    /// may survive outside the `Fanout`, or a kept clone keeps the
    /// writer's channel open after `remove` or `close` drops the
    /// `Fanout`'s copy and the caller's join never returns. Both
    /// shutdowns are the connection's, cloned before admission: `closer`
    /// serves the overflow report, `retired` stays with the writer
    /// however it is unregistered.
    pub(crate) fn push(
        &mut self,
        (tx, writer): (SyncSender<Line>, JoinHandle<()>),
        closer: Closer,
        retired: Closer,
        snapshot: Line,
    ) -> u64 {
        // A writer that already ended is dropped at the next broadcast.
        tx.send(snapshot).unwrap_or(());
        self.next += 1;
        let id = self.next;
        self.list.push(Subscriber {
            id,
            state: State::Active { tx, writer },
            closer,
            retired,
        });
        id
    }

    /// Reserves a pending registration: no channel and no writer thread
    /// until the first broadcast line activates it. Returns the
    /// registration's fresh id, the first being 1.
    pub(crate) fn listen(
        &mut self,
        writer: Arc<Mutex<UnixStream>>,
        closer: Closer,
        retired: Closer,
    ) -> u64 {
        self.next += 1;
        let id = self.next;
        self.list.push(Subscriber {
            id,
            state: State::Pending { writer },
            closer,
            retired,
        });
        id
    }

    /// Unregisters `id`, drops its sender, and hands its retained
    /// shutdown and writer back for the caller to shut and join outside
    /// its lock. A pending registration holds no sender and no writer:
    /// its shutdown comes back unshut, with no handle. `None` for an
    /// unknown id.
    pub(crate) fn remove(&mut self, id: u64) -> Option<WriterEnd> {
        let at = self.list.iter().position(|sub| sub.id == id)?;
        let subscriber = self.list.remove(at);
        let Subscriber {
            id: _,
            state,
            closer,
            retired,
        } = subscriber;
        drop(closer);
        match state {
            State::Active { tx, writer } => {
                drop(tx);
                Some(WriterEnd {
                    closer: retired,
                    writer: Some(writer),
                })
            }
            State::Pending { writer } => {
                drop(writer);
                Some(WriterEnd {
                    closer: retired,
                    writer: None,
                })
            }
        }
    }

    /// Queues `line` for every subscriber without blocking. A pending
    /// registration starts its writer on this first line, under this same
    /// guard, at most once; a spawn that fails unregisters it and hands
    /// its shutdown back, closing the connection. One whose queue is full
    /// has stalled: it is unregistered, its writer kept for joining, and
    /// its shutdown handed back for the caller to run outside its lock,
    /// keeping the rest in order. One whose writer already ended is not a
    /// cap violation: it is unregistered and kept for joining, with no
    /// shutdown handed back.
    pub(crate) fn broadcast(&mut self, line: &Line) -> Vec<Closer> {
        let mut closers = Vec::new();
        // Phase 1: activate every pending registration, in order. The
        // transition holds this guard, so concurrent first lines cannot
        // spawn twice; a failed spawn unregisters at once.
        let mut failed = Vec::new();
        for subscriber in self.list.iter_mut() {
            let State::Pending { writer } = &subscriber.state else {
                continue;
            };
            let writer = Arc::clone(writer);
            // Tests refuse the next lazy spawn through
            // `fail_next_spawn`: no outside seam fails a thread spawn
            // on demand.
            #[cfg(test)]
            if self.fail_spawn.swap(false, Ordering::SeqCst) {
                failed.push(subscriber.id);
                continue;
            }
            // The lazy path serves attention only; the feed stays eager.
            match spawn_writer(writer, "hub-attention-out") {
                Some((tx, handle)) => {
                    subscriber.state = State::Active { tx, writer: handle };
                    #[cfg(test)]
                    {
                        self.started += 1;
                    }
                }
                None => failed.push(subscriber.id),
            }
        }
        for id in failed {
            if let Some(at) = self.list.iter().position(|sub| sub.id == id) {
                let subscriber = self.list.remove(at);
                closers.push(subscriber.closer);
                drop(subscriber.retired);
            }
        }
        // Phase 2: enqueue to every active subscriber.
        let mut live = Vec::with_capacity(self.list.len());
        for subscriber in std::mem::take(&mut self.list) {
            let Subscriber {
                id,
                state,
                closer,
                retired,
            } = subscriber;
            let (tx, writer) = match state {
                State::Active { tx, writer } => (tx, writer),
                // Phase 1 activates or removes every pending registration
                // under this same guard, so none reaches here; requeueing
                // keeps the registration if one ever did.
                State::Pending { writer } => {
                    live.push(Subscriber {
                        id,
                        state: State::Pending { writer },
                        closer,
                        retired,
                    });
                    continue;
                }
            };
            match tx.try_send(Arc::clone(line)) {
                Ok(()) => live.push(Subscriber {
                    id,
                    state: State::Active { tx, writer },
                    closer,
                    retired,
                }),
                Err(TrySendError::Full(_)) => {
                    closers.push(closer);
                    self.retired.push((retired, writer));
                }
                Err(TrySendError::Disconnected(_)) => {
                    drop(closer);
                    self.retired.push((retired, writer));
                }
            }
        }
        self.list = live;
        closers
    }

    /// Queues `line` for exactly the subscriber `id` without blocking:
    /// [`Targeted::Sent`] when it fits; [`Targeted::Full`] with its
    /// shutdown, unregistered and kept for joining, when its queue is
    /// full; [`Targeted::Gone`] for an unknown or already-removed id,
    /// which resurrects nothing; [`Targeted::DeadWriter`] with its handle
    /// for a disconnected receiver, for the caller to join.
    pub(crate) fn send_to(&mut self, id: u64, line: &Line) -> Targeted {
        let Some(at) = self.list.iter().position(|subscriber| subscriber.id == id) else {
            return Targeted::Gone;
        };
        // Probe without holding the borrow. A pending registration holds
        // no channel: like an unknown id, a no-op that resurrects
        // nothing. Only the feed sends targeted lines, and the feed stays
        // eager.
        let send = match self.list.get(at) {
            Some(Subscriber {
                state: State::Active { tx, .. },
                ..
            }) => Some(tx.try_send(Arc::clone(line))),
            _ => None,
        };
        match send {
            Some(Ok(())) => Targeted::Sent,
            Some(Err(TrySendError::Full(_))) => {
                let subscriber = self.list.remove(at);
                let Subscriber {
                    id: _,
                    state,
                    closer,
                    retired,
                } = subscriber;
                let State::Active { tx: _, writer } = state else {
                    drop(closer);
                    drop(retired);
                    return Targeted::Gone;
                };
                self.retired.push((retired, writer));
                Targeted::Full(closer)
            }
            Some(Err(TrySendError::Disconnected(_))) => {
                let subscriber = self.list.remove(at);
                let Subscriber {
                    id: _,
                    state,
                    closer,
                    retired,
                } = subscriber;
                let State::Active { tx: _, writer } = state else {
                    drop(closer);
                    drop(retired);
                    return Targeted::Gone;
                };
                drop(closer);
                drop(retired);
                Targeted::DeadWriter(writer)
            }
            // A pending registration, or, defensively, a vanished index:
            // a no-op that resurrects nothing.
            None => Targeted::Gone,
        }
    }

    /// Removes and returns the retired writers that have ended; the rest
    /// stay, joined at `close`. Returning an empty `Vec` leaves ended
    /// writers' handles in `retired` until `close`, which joins them all,
    /// so no consumer sees a difference.
    #[cfg_attr(false, mutants::skip)]
    pub(crate) fn finished(&mut self) -> Vec<JoinHandle<()>> {
        let (done, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retired)
            .into_iter()
            .partition(|(_, writer)| writer.is_finished());
        self.retired = live;
        done.into_iter().map(|(_, writer)| writer).collect()
    }

    /// Removes every retired writer, including ones still running, and
    /// returns its retained shutdown with its handle for close-before-join.
    pub(crate) fn take_retired(&mut self) -> Vec<WriterEnd> {
        std::mem::take(&mut self.retired)
            .into_iter()
            .map(|(closer, writer)| WriterEnd {
                closer,
                writer: Some(writer),
            })
            .collect()
    }

    /// Drops every subscriber's sender, then hands every writer back with
    /// its retained shutdown (registered first, then retired). Pending
    /// registrations own no thread and shut nothing: they are dropped,
    /// not shut. No sender is alive when it returns, so joining the
    /// result cannot wait on this code.
    pub(crate) fn close(self) -> Vec<WriterEnd> {
        let mut ends: Vec<WriterEnd> = Vec::new();
        for subscriber in self.list {
            let Subscriber {
                id: _,
                state,
                closer,
                retired,
            } = subscriber;
            match state {
                State::Active { tx, writer } => {
                    drop(tx);
                    drop(closer);
                    ends.push(WriterEnd {
                        closer: retired,
                        writer: Some(writer),
                    });
                }
                State::Pending { writer } => {
                    drop(writer);
                    drop(closer);
                    drop(retired);
                }
            }
        }
        ends.extend(self.retired.into_iter().map(|(closer, writer)| WriterEnd {
            closer,
            writer: Some(writer),
        }));
        ends
    }

    /// Tests only: refuses the next lazy writer spawn in `broadcast`, so
    /// the failed-spawn path runs without an outside seam: thread-spawn
    /// failure has no deterministic outside trigger, since resource
    /// exhaustion cannot be staged per test and no socket outcome fails a
    /// spawn on demand.
    #[cfg(test)]
    pub(crate) fn fail_next_spawn(&self) {
        self.fail_spawn.store(true, Ordering::SeqCst);
    }

    /// Successful writer starts, including retired ones: never
    /// decremented. Test-only: no outside socket outcome distinguishes an
    /// idle parked thread from no thread.
    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.started
    }
}

#[cfg(test)]
#[path = "fanout_tests.rs"]
mod tests;

/// Shuts every closer down: its Unix write half, then its read half, then
/// its websocket when bridged, each error ignored independently, so a
/// failed first shutdown never skips the second. The caller holds no
/// registry or writer guard: shutting a half unblocks a writer holding
/// the writer mutex. The shutdown function is a seam for tests to pin
/// the order; production passes the socket's own shutdown.
pub(crate) fn close_connections(
    closers: Vec<Closer>,
    mut shutdown: impl FnMut(&UnixStream, Shutdown) -> std::io::Result<()>,
) {
    for closer in &closers {
        shutdown(&closer.stream, Shutdown::Write).unwrap_or(());
        shutdown(&closer.stream, Shutdown::Read).unwrap_or(());
        if let Some(web) = &closer.web {
            web.shut();
        }
    }
}
