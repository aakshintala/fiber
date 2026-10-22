//! The timers `host.after` and `host.every` set (`docs/extensions.md`,
//! "Host calls"): when each fires next on the extension's clock, and the
//! ids whose Lua functions the extension's thread still frees. Assigning
//! and ending a call id stays with the lifecycle part in `hub.rs`; this
//! table only lists timers.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::hub::Hub;

/// A timer `host.after` or `host.every` set: when its callback fires next,
/// on the extension's clock (`docs/extensions.md`, "Host calls").
#[derive(Debug)]
pub(crate) struct Timer {
    /// Assigned in set order from 0.
    pub(crate) id: u64,
    /// The period an `every` reschedules with; `None` for an `after`.
    pub(crate) every: Option<Duration>,
    /// When the callback fires next.
    pub(crate) due: Instant,
    /// The firing's timeout, from its start.
    pub(crate) timeout: Duration,
    /// `:cancel()` stopped it; a mid-firing cancel finishes the firing.
    pub(crate) cancelled: bool,
    /// A firing is running; one firing at a time.
    pub(crate) firing: bool,
}

/// The timers `host.after` and `host.every` set, by id. Only this table
/// creates or mutates a [`Timer`], and it never hands out `&mut Timer`.
#[derive(Default)]
pub(crate) struct Timers {
    by_id: HashMap<u64, Timer>,
    cleanup: Vec<u64>,
    next: u64,
}

impl Timers {
    /// Registers a timer `host.after` or `host.every` set: `every` holds
    /// its period for rescheduling, `None` for a one-shot `after`.
    /// Returns its id, assigned in set order from 0.
    pub(super) fn add(&mut self, every: Option<Duration>, due: Instant, timeout: Duration) -> u64 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.by_id.insert(
            id,
            Timer {
                id,
                every,
                due,
                timeout,
                cancelled: false,
                firing: false,
            },
        );
        id
    }

    /// Stops a timer: a due firing never starts, and a mid-firing `every`
    /// does not fire again. Unknown or finished ids are a no-op.
    pub(super) fn cancel(&mut self, id: u64) {
        // A listed timer is never both cancelled and idle: an idle cancel
        // removes it, and a cancelled firing leaves at its end.
        let remove = match self.by_id.get_mut(&id) {
            Some(timer) => {
                timer.cancelled = true;
                !timer.firing
            }
            None => false,
        };
        if remove {
            self.by_id.remove(&id);
            self.cleanup.push(id);
        }
    }

    /// Starts the earliest due timer, when one waits: due, neither
    /// cancelled nor firing. Marks it firing and returns its id and
    /// timeout. One firing at a time.
    pub(super) fn fire(&mut self, now: Instant) -> Option<(u64, Duration)> {
        let timer = self
            .by_id
            .values_mut()
            .filter(|timer| !timer.cancelled && !timer.firing && timer.due <= now)
            .min_by_key(|timer| (timer.due, timer.id))?;
        timer.firing = true;
        Some((timer.id, timer.timeout))
    }

    /// Ends a timer firing at `now`: an `every` that was not cancelled
    /// fires again `ms` after its end; anything else leaves, freeing its
    /// Lua function on the extension's thread.
    pub(super) fn end_firing(&mut self, id: u64, now: Instant) {
        let reschedule = match self.by_id.get_mut(&id) {
            Some(timer) if !timer.cancelled => timer.every,
            Some(_) | None => None,
        };
        match reschedule {
            Some(every) => {
                if let Some(timer) = self.by_id.get_mut(&id) {
                    timer.due = now.checked_add(every).unwrap_or(now);
                    timer.firing = false;
                }
            }
            None => {
                self.by_id.remove(&id);
                self.cleanup.push(id);
            }
        }
    }

    /// The earliest a waiting timer fires, for the thread's next wake.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.by_id
            .values()
            .filter(|timer| !timer.cancelled && !timer.firing)
            .map(|timer| timer.due)
            .min()
    }

    /// The timer ids whose Lua functions the extension's thread still frees.
    pub(super) fn take_cleanup(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.cleanup)
    }

    /// The timers by id, for tests outside this module to read.
    #[cfg(test)]
    pub(crate) fn listed(&self) -> &HashMap<u64, Timer> {
        &self.by_id
    }
}

impl Hub {
    /// Registers a timer `host.after` or `host.every` set: `every` holds
    /// its period for rescheduling, `None` for a one-shot `after`.
    /// Returns its id, assigned in set order from 0.
    pub(crate) fn add_timer(
        &self,
        every: Option<Duration>,
        due: Instant,
        timeout: Duration,
    ) -> u64 {
        let mut shared = self.lock();
        let id = shared.timers.add(every, due, timeout);
        drop(shared);
        self.notify();
        id
    }

    /// Stops a timer: a due firing never starts, and a mid-firing `every`
    /// does not fire again. Unknown or finished ids are a no-op.
    pub(crate) fn cancel_timer(&self, id: u64) {
        let mut shared = self.lock();
        shared.timers.cancel(id);
        drop(shared);
        self.notify();
    }

    /// The timer ids whose Lua functions the extension's thread frees.
    pub(super) fn take_timer_cleanup(&self) -> Vec<u64> {
        self.lock().timers.take_cleanup()
    }

    /// The delegate `run` handles whose entries the extension's thread
    /// clears from the VM, beside the timer functions.
    pub(super) fn take_delegate_cleanup(&self) -> Vec<u64> {
        std::mem::take(&mut self.lock().delegate_cleanup)
    }
}

#[cfg(test)]
#[path = "timers_tests.rs"]
mod tests;
