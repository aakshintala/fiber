//! The manual clock an extension case advances (`docs/testing.md`, "Waits and timeouts").

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};

const WALL_EPOCH: Duration = Duration::from_secs(1_700_000_000);

struct Parked {
    id: u64,
    until: Option<Instant>,
}

#[derive(Default)]
struct State {
    offset: Duration,
    parked: Vec<Parked>,
    /// One id distinguishes parked calls; a case cannot make 2^64 waits
    /// (`docs/testing.md`, "Waits and timeouts").
    next_id: u64,
    wakers: Vec<Weak<dyn Wake>>,
}

/// A monotonic clock whose time moves only when a case advances it.
pub(crate) struct CaseClock {
    origin: Instant,
    state: Mutex<State>,
    changed: Condvar,
}

impl CaseClock {
    /// Builds a clock at an arbitrary monotonic origin and a fixed wall epoch.
    #[expect(
        clippy::disallowed_methods,
        reason = "the case clock reads one origin; later time comes from advances (`docs/testing.md`, \"Values that change every run\")"
    )]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            origin: Instant::now(),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }

    /// The deadlines currently parked in [`Clock::wait_until`].
    #[cfg(test)]
    pub(crate) fn parked(&self) -> Vec<Option<Instant>> {
        lock(&self.state)
            .parked
            .iter()
            .map(|parked| parked.until)
            .collect()
    }

    /// Moves case time only after a waiter is parked at exactly `now + d`.
    /// The bounded wait is on the process clock (`docs/testing.md`,
    /// "Waits and timeouts").
    pub(crate) fn advance_when_parked(&self, d: Duration, within: Duration) -> Result<(), String> {
        self.advance_when_parked_inner(d, within, false)
    }

    /// An event-anchored advance refuses an already parked, shorter wait
    /// that it would skip rather than waiting for the case's whole bound.
    pub(crate) fn advance_when_parked_after_event(
        &self,
        d: Duration,
        within: Duration,
    ) -> Result<(), String> {
        self.advance_when_parked_inner(d, within, true)
    }

    fn advance_when_parked_inner(
        &self,
        d: Duration,
        within: Duration,
        refuse_skipped_wait: bool,
    ) -> Result<(), String> {
        let state = lock(&self.state);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, within, |state| {
                matching_deadline(state, self.origin, d).is_none()
                    && !(refuse_skipped_wait && has_single_shorter_deadline(state, self.origin, d))
            })
            .unwrap_or_else(PoisonError::into_inner);
        if matching_deadline(&state, self.origin, d).is_none() {
            if refuse_skipped_wait && has_single_shorter_deadline(&state, self.origin, d) {
                let parked = state
                    .parked
                    .iter()
                    .filter_map(|parked| parked.until)
                    .collect::<Vec<_>>();
                return Err(format!(
                    "clock advance {d:?} would skip the only parked deadline {parked:?}"
                ));
            }
            let parked = state
                .parked
                .iter()
                .map(|parked| parked.until)
                .collect::<Vec<_>>();
            return Err(format!(
                "no waiter parked at now + {d:?} within {within:?}; parked deadlines: {parked:?}"
            ));
        }
        let wakers = move_by(&mut state, d);
        drop(state);
        self.wake(wakers);
        Ok(())
    }

    /// Moves case time by `d` and wakes every subscribed clock waiter.
    #[cfg(test)]
    pub(crate) fn advance(&self, d: Duration) {
        let wakers = move_by(&mut lock(&self.state), d);
        self.wake(wakers);
    }

    fn wake(&self, wakers: Vec<Arc<dyn Wake>>) {
        self.changed.notify_all();
        for waker in wakers {
            waker.wake();
        }
    }
}

impl Clock for CaseClock {
    fn now(&self) -> Instant {
        now_in(&lock(&self.state), self.origin)
    }

    fn wall(&self) -> SystemTime {
        let offset = lock(&self.state).offset;
        SystemTime::UNIX_EPOCH
            .checked_add(WALL_EPOCH)
            .and_then(|wall| wall.checked_add(offset))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }

    fn sleep(&self, d: Duration) {
        let now = self.now();
        let Some(until) = now.checked_add(d) else {
            return;
        };
        let mut wait = |_| {
            let state = lock(&self.state);
            let _state = self
                .changed
                .wait_while(state, |state| now_in(state, self.origin) < until)
                .unwrap_or_else(PoisonError::into_inner);
        };
        self.wait_until(Some(until), &mut wait);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let (id, bound) = {
            let mut state = lock(&self.state);
            let now = now_in(&state, self.origin);
            if until.is_some_and(|until| until <= now) {
                (None, Some(Duration::ZERO))
            } else {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                state.parked.push(Parked { id, until });
                (Some(id), None)
            }
        };
        if id.is_some() {
            self.changed.notify_all();
        }
        let _leave = id.map(|id| Leave { clock: self, id });
        wait(bound);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.state).wakers.push(waker);
    }
}

fn has_single_shorter_deadline(state: &State, origin: Instant, d: Duration) -> bool {
    let Some(expected) = now_in(state, origin).checked_add(d) else {
        return false;
    };
    let mut deadlines = state.parked.iter().filter_map(|parked| parked.until);
    matches!((deadlines.next(), deadlines.next()), (Some(until), None) if until < expected)
}

fn matching_deadline(state: &State, origin: Instant, d: Duration) -> Option<Instant> {
    let until = now_in(state, origin).checked_add(d)?;
    state
        .parked
        .iter()
        .any(|parked| parked.until == Some(until))
        .then_some(until)
}

fn move_by(state: &mut State, d: Duration) -> Vec<Arc<dyn Wake>> {
    state.offset = state.offset.saturating_add(d);
    let mut wakers = Vec::new();
    state.wakers.retain(|waker| match waker.upgrade() {
        Some(waker) => {
            wakers.push(waker);
            true
        }
        None => false,
    });
    wakers
}

fn now_in(state: &State, origin: Instant) -> Instant {
    origin.checked_add(state.offset).unwrap_or(origin)
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Leave<'a> {
    clock: &'a CaseClock,
    id: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        lock(&self.clock.state)
            .parked
            .retain(|parked| parked.id != self.id);
        self.clock.changed.notify_all();
    }
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
