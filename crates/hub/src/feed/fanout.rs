//! The feed's subscriber fan-out: each subscriber's channel and writer
//! thread, the id counter, and the ended writers kept for joining.
//!
//! The owner's lock covers a `Fanout`: it takes no lock of its own, and
//! it never joins a thread. Every method that ends a subscriber drops
//! its sender and hands the writer's handle back, so the caller joins it
//! after the guard drops.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// One line queued for a subscriber.
pub(crate) type Line = Arc<[u8]>;

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
}

#[derive(Default)]
pub(crate) struct Fanout {
    list: Vec<Subscriber>,
    next: u64,
    retired: Vec<JoinHandle<()>>,
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
    /// and the caller's join never returns.
    pub(crate) fn push(
        &mut self,
        (tx, writer): (Sender<Line>, JoinHandle<()>),
        snapshot: &[Line],
    ) -> u64 {
        for line in snapshot {
            // A writer that already ended is dropped at the next broadcast.
            tx.send(Arc::clone(line)).unwrap_or(());
        }
        self.next += 1;
        let id = self.next;
        self.list.push(Subscriber { id, tx, writer });
        id
    }

    /// Unregisters `id`, drops its sender, and returns its writer for
    /// the caller to join outside its lock. `None` for an unknown id.
    pub(crate) fn remove(&mut self, id: u64) -> Option<JoinHandle<()>> {
        let at = self.list.iter().position(|sub| sub.id == id)?;
        let subscriber = self.list.remove(at);
        drop(subscriber.tx);
        Some(subscriber.writer)
    }

    /// Queues `line` for every subscriber; one whose send fails is
    /// unregistered and its writer moved to `retired`, keeping the rest
    /// in order.
    pub(crate) fn broadcast(&mut self, line: &Line) {
        let (live, dead): (Vec<_>, Vec<_>) = std::mem::take(&mut self.list)
            .into_iter()
            .partition(|sub| sub.tx.send(Arc::clone(line)).is_ok());
        self.list = live;
        self.retired.extend(dead.into_iter().map(|sub| sub.writer));
    }

    /// Removes and returns the retired writers that have ended; the rest
    /// stay, joined at `close`. Returning an empty `Vec` leaves ended
    /// writers' handles in `retired` until `close`, which joins them all,
    /// so no consumer sees a difference.
    #[cfg_attr(false, mutants::skip)]
    pub(crate) fn finished(&mut self) -> Vec<JoinHandle<()>> {
        let (done, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retired)
            .into_iter()
            .partition(JoinHandle::is_finished);
        self.retired = live;
        done
    }

    /// Drops every subscriber's sender, then returns every writer
    /// (registered first, then retired). No sender is alive when it
    /// returns, so joining the result cannot wait on this code.
    pub(crate) fn close(self) -> Vec<JoinHandle<()>> {
        let mut writers: Vec<JoinHandle<()>> = Vec::new();
        for subscriber in self.list {
            drop(subscriber.tx);
            writers.push(subscriber.writer);
        }
        writers.extend(self.retired);
        writers
    }

    /// Registered ids in registration order.
    #[cfg(test)]
    pub(crate) fn ids(&self) -> Vec<u64> {
        self.list.iter().map(|sub| sub.id).collect()
    }
}
