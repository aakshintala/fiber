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

    pub(crate) fn try_iter(&mut self) -> Vec<Delivery> {
        self.held.drain(..).chain(self.rx.try_iter()).collect()
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
        let mut jobs = Vec::new();
        for delivery in self.try_iter() {
            if matches!(delivery, Delivery::ExtensionJob(..)) {
                jobs.push(delivery);
            } else {
                self.held.push_back(delivery);
            }
        }
        jobs
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
