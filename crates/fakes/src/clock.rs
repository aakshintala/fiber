//! A clock whose time moves only when a test says so (`docs/testing.md`,
//! "Fakes" and "Values that change every run").

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use contract::clock::{Clock, Wake};

/// `wall()` at construction: 2023-11-14T22:13:20Z, so a log line's `ts` is
/// `1700000000000`.
const WALL_EPOCH: Duration = Duration::from_secs(1_700_000_000);

struct Parked {
    id: u64,
    thread: ThreadId,
    until: Option<Instant>,
}

struct State {
    offset: Duration,
    parked: Vec<Parked>,
    /// The latest park id of each thread that ever parked on this clock.
    /// Written in `wait_until` under the lock that pushes the park; a
    /// `FakeClock` lives for one test, so this holds at most one entry
    /// per test thread. `Leave` never touches it.
    latest: Vec<(ThreadId, u64)>,
    /// Park ids are one per park; wrapping needs 2^64 parks, which a test
    /// process cannot reach, so a later park's id compares greater with `>`.
    next_id: u64,
    wakers: Vec<Weak<dyn Wake>>,
}

/// Which thread last parked at the instant of an advance, from
/// [`FakeClock::advance_marked`]: each thread's latest park id, whether
/// it is still parked or has left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark(Vec<(ThreadId, u64)>);

/// A clock a test drives. `Arc<FakeClock>` coerces to `Arc<dyn Clock>`.
/// [`FakeClock::new`] reads the process clock once, as an arbitrary origin
/// it never reads again. [`FakeClock::advance`] and [`Clock::sleep`] are the
/// only things that move it.
pub struct FakeClock {
    origin: Instant,
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
            origin,
            state: Mutex::new(State {
                offset: Duration::ZERO,
                parked: Vec::new(),
                latest: Vec::new(),
                next_id: 0,
                wakers: Vec::new(),
            }),
            parked_cv: Condvar::new(),
        })
    }

    /// The monotonic instant `now()` returned at construction.
    pub fn origin(&self) -> Instant {
        self.origin
    }

    /// Moves `now()` and `wall()` forward by `d`, then wakes every live
    /// subscriber. The clock's lock is released before any [`Wake::wake`].
    pub fn advance(&self, d: Duration) {
        self.advance_marked(d);
    }

    /// [`FakeClock::advance`], returning each thread's latest park at the
    /// instant `now()` moved. A thread that left its park before the
    /// advance still matches its next park in
    /// [`FakeClock::await_parked_since`]. The snapshot is taken under the
    /// lock that moves it.
    pub fn advance_marked(&self, d: Duration) -> Mark {
        let (mark, wakers) = {
            let mut state = lock(&self.state);
            state.offset = state.offset.saturating_add(d);
            let mark = Mark(state.latest.clone());
            let wakers = state
                .wakers
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            (mark, wakers)
        };
        for waker in wakers {
            waker.wake();
        }
        mark
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
        self.await_parked_count(until, 1, within)
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

    /// Waits, at most `within` of real time, until a thread in `mark` is
    /// parked again, in a later park, with this `until` (`None`: no
    /// deadline). True once it is; false at the deadline. A thread that
    /// left its park before the advance matches its next park.
    pub fn await_parked_since(
        &self,
        mark: &Mark,
        until: Option<Instant>,
        within: Duration,
    ) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_since(state, mark, until))
            .unwrap_or_else(PoisonError::into_inner);
        parked_since(&guard, mark, until)
    }

    /// Waits, at most `within` of real time, until a thread is parked in
    /// [`Clock::wait_until`] with this `until`, and returns a [`Mark`] of the
    /// threads parked at `until`, taken under the lock that saw the park.
    /// `None` at the deadline. A later park by one of them matches
    /// [`FakeClock::await_parked_since`], whatever woke it.
    pub fn mark_parked(&self, until: Instant, within: Duration) -> Option<Mark> {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| parked_count(state, until) == 0)
            .unwrap_or_else(PoisonError::into_inner);
        (parked_count(&guard, until) > 0).then(|| {
            Mark(
                guard
                    .parked
                    .iter()
                    .filter(|parked| parked.until == Some(until))
                    .map(|parked| (parked.thread, parked.id))
                    .collect(),
            )
        })
    }

    /// Waits, at most `within` of real time, until a thread in `mark` is
    /// parked again, in a later park, with any deadline.
    pub fn await_any_parked_since(&self, mark: &Mark, within: Duration) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_after(state, mark))
            .unwrap_or_else(PoisonError::into_inner);
        parked_after(&guard, mark)
    }

    /// [`FakeClock::await_parked`] for a park with no deadline.
    pub fn await_parked_unbounded(&self, within: Duration) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_unbounded(state))
            .unwrap_or_else(PoisonError::into_inner);
        parked_unbounded(&guard)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        now_in(&lock(&self.state), self.origin)
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
            let now = now_in(&state, self.origin);
            if until.is_some_and(|until| until <= now) {
                None
            } else {
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1);
                let thread = std::thread::current().id();
                state.parked.push(Parked { id, thread, until });
                record_latest(&mut state, thread, id);
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

fn now_in(state: &State, origin: Instant) -> Instant {
    origin.checked_add(state.offset).unwrap_or(origin)
}

/// Records `id` as `thread`'s latest park, replacing its earlier entry.
fn record_latest(state: &mut State, thread: ThreadId, id: u64) {
    if let Some(entry) = state.latest.iter_mut().find(|(t, _)| *t == thread) {
        entry.1 = id;
    } else {
        state.latest.push((thread, id));
    }
}

fn parked_count(state: &State, until: Instant) -> usize {
    state
        .parked
        .iter()
        .filter(|parked| parked.until == Some(until))
        .count()
}

fn parked_since(state: &State, mark: &Mark, until: Option<Instant>) -> bool {
    state.parked.iter().any(|parked| {
        parked.until == until
            && mark
                .0
                .iter()
                .any(|(thread, id0)| *thread == parked.thread && parked.id > *id0)
    })
}

fn parked_after(state: &State, mark: &Mark) -> bool {
    state.parked.iter().any(|parked| {
        mark.0
            .iter()
            .any(|(thread, id0)| *thread == parked.thread && parked.id > *id0)
    })
}

fn parked_unbounded(state: &State) -> bool {
    state.parked.iter().any(|parked| parked.until.is_none())
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
