//! The feed's first scan settling, and its live snapshot: a hub that has
//! not finished its first scan of `run/` answers a listing after it, once
//! every session that scan followed has sent its first status or ended.
//! A session that never answers holds the listing for at most one
//! [`RUN_SCAN`] past the scan.

#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use contract::clock::Wake;
use contract::events::SessionStatus;

use super::{Entry, Feed, PoisonError, RUN_SCAN, lock};

#[cfg(test)]
const PAUSE_DEADLINE: Duration = Duration::from_secs(10);

/// Settles session `id` when dropped: the caller has taken its line.
pub(super) struct Settle<'a>(pub(super) &'a Feed, pub(super) &'a str);

impl Drop for Settle<'_> {
    fn drop(&mut self) {
        let removed = lock(&self.0.state).awaited.remove(self.1);
        if removed {
            self.0.tick.wake();
        }
    }
}

impl Feed {
    /// Marks the first scan finished.
    pub(super) fn scanned(&self) {
        let first = !std::mem::replace(&mut lock(&self.state).scanned, true);
        if first {
            self.tick.wake();
        }
    }

    /// Returns once the first scan has settled, or the feed has stopped:
    /// before the scan the listing waits with no bound, and after it for
    /// at most [`RUN_SCAN`] on the clock for the followed sessions' first
    /// statuses.
    pub(crate) fn settled(&self) {
        loop {
            {
                let state = lock(&self.state);
                if state.stopped || state.scanned {
                    break;
                }
            }
            #[cfg(test)]
            self.pause_before_wait();
            self.wait_for(None);
        }
        let until = self.clock.now().checked_add(RUN_SCAN);
        loop {
            {
                let state = lock(&self.state);
                if state.stopped || state.awaited.is_empty() {
                    return;
                }
            }
            if until.is_none_or(|until| self.clock.now() >= until) {
                return;
            }
            #[cfg(test)]
            self.pause_before_wait();
            self.wait_for(until);
        }
    }

    /// Parks until woken or `until` passes on the clock.
    fn wait_for(&self, until: Option<Instant>) {
        let guard = lock(&self.tick.held);
        let mut slot = Some(guard);
        self.clock.wait_until(until, &mut |bound| {
            let Some(held) = slot.take() else {
                return;
            };
            slot = Some(match bound {
                Some(limit) => {
                    self.tick
                        .moved
                        .wait_timeout(held, limit)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self
                    .tick
                    .moved
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner),
            });
        });
    }

    #[cfg(test)]
    fn pause_before_wait(&self) {
        let Some(pause) = lock(&self.settle_pause).take() else {
            return;
        };
        pause.arrived.send(()).unwrap_or(());
        assert!(
            pause.release.recv_timeout(PAUSE_DEADLINE).is_ok(),
            "the settle wait is released"
        );
    }

    /// Every running session's latest status, in the feed's order, copied
    /// under the lock.
    pub(crate) fn live(&self) -> Vec<(String, SessionStatus)> {
        lock(&self.state)
            .entries
            .iter()
            .filter_map(|(id, entry)| match entry {
                Entry::Running(status) => Some((id.clone(), status.payload.clone())),
                Entry::Left(..) => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "feed_settle_tests.rs"]
mod tests;
