//! An inbox that preserves deliveries skipped by the ownership drain.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvError, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use contract::inbox::Delivery;

pub(crate) struct Queue {
    rx: Receiver<Delivery>,
    held: VecDeque<Delivery>,
}

impl Queue {
    pub(crate) fn new(rx: Receiver<Delivery>) -> Self {
        Self {
            rx,
            held: VecDeque::new(),
        }
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
