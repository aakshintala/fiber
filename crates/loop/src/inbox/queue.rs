//! An inbox that preserves deliveries skipped by the ownership drain.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvError, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use contract::inbox::Delivery;

pub(crate) struct Queue {
    rx: Receiver<Delivery>,
    held: VecDeque<Delivery>,
    /// Active admission batches, oldest first. Their unhandled deliveries
    /// remain visible to reconnect collection, but not to nested busy drains.
    pending: Vec<Weak<Mutex<VecDeque<Delivery>>>>,
}

/// An admission batch whose remainder stays visible to the queue collector.
/// It is not an Iterator: callers take one delivery for admission at a time,
/// rather than collecting an untracked vector of waiting commands.
pub(crate) struct Pending {
    deliveries: Arc<Mutex<VecDeque<Delivery>>>,
}

impl Pending {
    pub(crate) fn next(&mut self) -> Option<Delivery> {
        support::lock(&self.deliveries).pop_front()
    }
}

impl Queue {
    pub(crate) fn new(rx: Receiver<Delivery>) -> Self {
        Self {
            rx,
            held: VecDeque::new(),
            pending: Vec::new(),
        }
    }

    /// Registers a snapshot before any of its deliveries is admitted.
    pub(crate) fn pending(&mut self, deliveries: impl IntoIterator<Item = Delivery>) -> Pending {
        let deliveries = Arc::new(Mutex::new(deliveries.into_iter().collect()));
        self.pending.retain(|batch| batch.strong_count() > 0);
        self.pending.push(Arc::downgrade(&deliveries));
        Pending { deliveries }
    }

    pub(crate) fn try_recv(&mut self) -> Result<Delivery, TryRecvError> {
        self.held.pop_front().map_or_else(|| self.rx.try_recv(), Ok)
    }

    pub(crate) fn try_iter(&mut self) -> std::vec::IntoIter<Delivery> {
        self.held
            .drain(..)
            .chain(self.rx.try_iter())
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub(crate) fn recv(&mut self) -> Result<Delivery, RecvError> {
        self.held.pop_front().map_or_else(|| self.rx.recv(), Ok)
    }

    pub(crate) fn recv_timeout(
        &mut self,
        duration: Duration,
    ) -> Result<Delivery, RecvTimeoutError> {
        #[allow(
            clippy::disallowed_methods,
            reason = "the caller supplies the injected clock's wait bound"
        )]
        self.held
            .pop_front()
            .map_or_else(|| self.rx.recv_timeout(duration), Ok)
    }

    /// Collects duplicates up to a session-ending delivery, retaining
    /// everything else in order. A move to the current folder is rejected
    /// without ending the session, so duplicates behind it still join.
    pub(crate) fn take_reconnects(
        &mut self,
        server: &str,
        workspace: &std::path::Path,
        home: Option<&std::path::Path>,
    ) -> Vec<contract::inbox::Ack> {
        let mut joined = Vec::new();
        let mut stopped = false;
        let mut collect = |waiting: &mut VecDeque<Delivery>| {
            let mut retained = VecDeque::new();
            while let Some(delivery) = waiting.pop_front() {
                if !stopped {
                    stopped = matches!(&delivery, Delivery::Close(..) | Delivery::Rewind(..));
                    if let Delivery::ChangeWorkspace(_, args, _) = &delivery {
                        stopped = !crate::rewind::workspace::resolve(&args.path, workspace, home)
                            .is_ok_and(|target| {
                                crate::rewind::workspace::is_same_folder(&target, workspace)
                            });
                    }
                    if let Delivery::McpReconnect(args, _) = &delivery
                        && args.server == server
                    {
                        let Delivery::McpReconnect(_, ack) = delivery else {
                            unreachable!("the matching delivery is a reconnect");
                        };
                        joined.push(ack);
                        continue;
                    }
                }
                retained.push_back(delivery);
            }
            *waiting = retained;
        };
        self.pending.retain(|batch| batch.strong_count() > 0);
        for batch in self.pending.iter().filter_map(Weak::upgrade) {
            collect(&mut support::lock(&batch));
        }
        self.held.extend(self.rx.try_iter());
        collect(&mut self.held);
        joined
    }

    pub(crate) fn take_extension_jobs(&mut self) -> Vec<Delivery> {
        self.take_where(|delivery| matches!(delivery, Delivery::ExtensionJob(..)))
    }

    /// Every delivery waiting now that `keep` selects, holding back
    /// anything else in arrival order for the next drain.
    fn take_where(&mut self, keep: fn(&Delivery) -> bool) -> Vec<Delivery> {
        let mut kept = Vec::new();
        for delivery in self.try_iter() {
            if keep(&delivery) {
                kept.push(delivery);
            } else {
                self.held.push_back(delivery);
            }
        }
        kept
    }

    /// Every session message waiting now, holding back anything else in
    /// arrival order for the next drain: only a message waits for its
    /// take to be answered, so only a message is taken early.
    pub(crate) fn take_messages(&mut self) -> Vec<Delivery> {
        self.take_where(|delivery| matches!(delivery, Delivery::Message(..)))
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
