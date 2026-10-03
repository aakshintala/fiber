//! A clock whose time moves only when a test says so (`docs/testing.md`,
//! "Fakes" and "Values that change every run").

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use contract::clock::{Clock, Wake};

/// `wall()` at construction: 2023-11-14T22:13:20Z, so a log line's `ts` is
/// `1700000000000`.
const WALL_EPOCH: Duration = Duration::from_secs(1_700_000_000);

struct Parked {
    id: u64,
    until: Option<Instant>,
}

struct State {
    origin: Instant,
    offset: Duration,
    parked: Vec<Parked>,
    next_id: u64,
    wakers: Vec<Weak<dyn Wake>>,
}

/// A clock a test drives. `Arc<FakeClock>` coerces to `Arc<dyn Clock>`.
/// [`FakeClock::new`] reads the process clock once, as an arbitrary origin
/// it never reads again. [`FakeClock::advance`] and [`Clock::sleep`] are the
/// only things that move it.
pub struct FakeClock {
    state: Mutex<State>,
    parked_cv: Condvar,
}

impl FakeClock {
    /// `now()` is [`FakeClock::origin`]. `wall()` is the Unix epoch plus
    /// 1_700_000_000 seconds.
    #[expect(
        clippy::disallowed_methods,
        reason = "the fake clock's origin is one Instant::now; time after that is contract::clock"
    )]
    pub fn new() -> Arc<Self> {
        // The origin is an arbitrary Instant. Nothing compares it to a later
        // reading of the process clock.
        let origin = Instant::now();
        Arc::new(Self {
            state: Mutex::new(State {
                origin,
                offset: Duration::ZERO,
                parked: Vec::new(),
                next_id: 0,
                wakers: Vec::new(),
            }),
            parked_cv: Condvar::new(),
        })
    }

    /// The monotonic instant `now()` returned at construction.
    pub fn origin(&self) -> Instant {
        lock(&self.state).origin
    }

    /// Moves `now()` and `wall()` forward by `d`, then wakes every live
    /// subscriber. The clock's lock is released before any [`Wake::wake`].
    pub fn advance(&self, d: Duration) {
        let wakers = {
            let mut state = lock(&self.state);
            state.offset = state.offset.saturating_add(d);
            state.wakers.retain(|waker| waker.strong_count() > 0);
            state
                .wakers
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// The `until` of each thread now inside [`Clock::wait_until`].
    pub fn parked(&self) -> Vec<Option<Instant>> {
        lock(&self.state)
            .parked
            .iter()
            .map(|parked| parked.until)
            .collect()
    }

    /// Waits, at most `within` of real time, until a thread is parked in
    /// [`Clock::wait_until`] with this `until`. True once it is; false at
    /// the deadline. The bound is the condvar's timeout, so this reads no
    /// process clock.
    pub fn await_parked(&self, until: Instant, within: Duration) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_at(state, until))
            .unwrap_or_else(PoisonError::into_inner);
        parked_at(&guard, until)
    }

    /// Waits, at most `within` of real time, until at least `count` threads
    /// are parked in [`Clock::wait_until`] with this `until`. True once they
    /// are; false at the deadline. The bound is the condvar's timeout, so
    /// this reads no process clock.
    pub fn await_parked_count(&self, until: Instant, count: usize, within: Duration) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| parked_count(state, until) < count)
            .unwrap_or_else(PoisonError::into_inner);
        parked_count(&guard, until) >= count
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        let state = lock(&self.state);
        state
            .origin
            .checked_add(state.offset)
            .unwrap_or(state.origin)
    }

    fn wall(&self) -> SystemTime {
        let offset = lock(&self.state).offset;
        UNIX_EPOCH
            .checked_add(WALL_EPOCH)
            .and_then(|epoch| epoch.checked_add(offset))
            .unwrap_or(UNIX_EPOCH)
    }

    fn sleep(&self, d: Duration) {
        self.advance(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let id = {
            let mut state = lock(&self.state);
            let now = state
                .origin
                .checked_add(state.offset)
                .unwrap_or(state.origin);
            if until.is_some_and(|until| until <= now) {
                None
            } else {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                state.parked.push(Parked { id, until });
                self.parked_cv.notify_all();
                Some(id)
            }
        };
        let bound = if id.is_some() {
            None
        } else {
            Some(Duration::ZERO)
        };
        let _leave = id.map(|id| Leave { clock: self, id });
        wait(bound);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        lock(&self.state).wakers.push(waker);
    }
}

/// Drops a thread's parked entry when [`Clock::wait_until`] returns, a panic
/// in `wait` included.
struct Leave<'a> {
    clock: &'a FakeClock,
    id: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        lock(&self.clock.state)
            .parked
            .retain(|parked| parked.id != self.id);
    }
}

fn parked_at(state: &State, until: Instant) -> bool {
    parked_count(state, until) > 0
}

fn parked_count(state: &State, until: Instant) -> usize {
    state
        .parked
        .iter()
        .filter(|parked| parked.until == Some(until))
        .count()
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
