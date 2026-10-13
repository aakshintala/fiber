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

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// One line queued for a subscriber.
pub(crate) type Line = Arc<[u8]>;

/// How one connection's subscriber shuts down: its Unix halves, then its
/// websocket's TCP shut capability when the connection is bridged (local
/// connections carry none). Cloned before admission, so cleanup never
/// needs a fallible clone at the moment it runs.
pub(crate) struct Closer {
    pub(crate) stream: UnixStream,
    pub(crate) web: Option<Arc<crate::port::Web>>,
}

/// A channel whose lines a new thread named `name` writes to `writer`, each flushed;
/// the thread ends on the first failed write or once every sender is dropped.
pub(crate) fn spawn_writer(
    writer: Arc<Mutex<UnixStream>>,
    name: &str,
) -> Option<(Sender<Line>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel::<Line>();
    let handle = thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            for line in rx {
                let mut out = support::lock(&writer);
                if out.write_all(&line).and_then(|()| out.flush()).is_err() {
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
    tx: Sender<Line>,
    writer: JoinHandle<()>,
    closer: Closer,
    retired: Closer,
}

/// What removing one subscriber hands back: the shutdown its join path
/// uses, and its writer's handle when it has one, for the caller to shut
/// and join outside every lock.
pub(crate) struct WriterEnd {
    pub(crate) closer: Closer,
    pub(crate) writer: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub(crate) struct Fanout {
    list: Vec<Subscriber>,
    next: u64,
    retired: Vec<(Closer, JoinHandle<()>)>,
}

impl Fanout {
    /// Registers a spawned writer pair and queues `snapshot` for it in
    /// order, ignoring a failed send. Returns the subscriber's fresh id,
    /// the first being 1. The snapshot is queued before the subscriber
    /// can receive any `broadcast`, which holds because the caller holds
    /// its lock across building the snapshot and `push`. The caller
    /// passes exclusive ownership of the sender: no clone of `tx` may
    /// survive outside the `Fanout`, or a kept clone keeps the writer's
    /// channel open after `remove` or `close` drops the `Fanout`'s copy
    /// and the caller's join never returns. Both shutdowns are the
    /// connection's, cloned before admission: `closer` serves the
    /// overflow report, `retired` stays with the writer however it is
    /// unregistered.
    pub(crate) fn push(
        &mut self,
        (tx, writer): (Sender<Line>, JoinHandle<()>),
        closer: Closer,
        retired: Closer,
        snapshot: &[Line],
    ) -> u64 {
        for line in snapshot {
            // A writer that already ended is dropped at the next broadcast.
            tx.send(Arc::clone(line)).unwrap_or(());
        }
        self.next += 1;
        let id = self.next;
        self.list.push(Subscriber {
            id,
            tx,
            writer,
            closer,
            retired,
        });
        id
    }

    /// Unregisters `id`, drops its sender, and hands its retained
    /// shutdown and writer back for the caller to shut and join outside
    /// its lock. `None` for an unknown id.
    pub(crate) fn remove(&mut self, id: u64) -> Option<WriterEnd> {
        let at = self.list.iter().position(|sub| sub.id == id)?;
        let subscriber = self.list.remove(at);
        drop(subscriber.tx);
        drop(subscriber.closer);
        Some(WriterEnd {
            closer: subscriber.retired,
            writer: Some(subscriber.writer),
        })
    }

    /// Queues `line` for every subscriber; one whose send fails has
    /// disconnected, so it is unregistered, its writer kept for joining,
    /// and its shutdown handed back for the caller to run outside its
    /// lock, keeping the rest in order.
    pub(crate) fn broadcast(&mut self, line: &Line) -> Vec<Closer> {
        let mut closers = Vec::new();
        let (live, dead): (Vec<_>, Vec<_>) = std::mem::take(&mut self.list)
            .into_iter()
            .partition(|sub| sub.tx.send(Arc::clone(line)).is_ok());
        self.list = live;
        for subscriber in dead {
            closers.push(subscriber.closer);
            self.retired.push((subscriber.retired, subscriber.writer));
        }
        closers
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

    /// Drops every subscriber's sender, then hands every writer back with
    /// its retained shutdown (registered first, then retired). No sender
    /// is alive when it returns, so joining the result cannot wait on this
    /// code.
    pub(crate) fn close(self) -> Vec<WriterEnd> {
        let mut ends: Vec<WriterEnd> = Vec::new();
        for subscriber in self.list {
            drop(subscriber.tx);
            drop(subscriber.closer);
            ends.push(WriterEnd {
                closer: subscriber.retired,
                writer: Some(subscriber.writer),
            });
        }
        ends.extend(self.retired.into_iter().map(|(closer, writer)| WriterEnd {
            closer,
            writer: Some(writer),
        }));
        ends
    }

    /// Registered ids in registration order.
    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<u64> {
        self.list.iter().map(|sub| sub.id).collect()
    }

    /// How many registered writers have ended. Test-only, until Task 3's
    /// EOF and ownership assertions replace it: no outside seam reaches a
    /// registered writer's state without joining it.
    #[cfg(test)]
    pub(crate) fn active_finished(&self) -> usize {
        self.list
            .iter()
            .filter(|sub| sub.writer.is_finished())
            .count()
    }
}

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
