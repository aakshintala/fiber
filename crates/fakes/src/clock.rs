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

#[derive(Clone, Copy)]
enum Observation {
    Park(ThreadId, Option<Instant>),
    Moved,
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
    /// `next_id` at the last move, including a zero-length move.
    fresh_from: u64,
    wakers: Vec<Weak<dyn Wake>>,
    /// Parks and moves during an [`FakeClock::advance_when_parked`] wait,
    /// in the order they take the state lock. The first failing entry wins,
    /// even if a park leaves before the helper looks. Cleared on wait exit.
    observed: Vec<Observation>,
    /// Whether an [`FakeClock::advance_when_parked`] wait is recording into
    /// `observed` (0 or 1): a second wait while one is active fails rather
    /// than share the log.
    watching: usize,
}

/// Which thread last parked at the instant of an advance, from
/// [`FakeClock::advance_marked`]: each thread's latest park id, whether
/// it is still parked or has left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark(Vec<(ThreadId, u64)>);

/// A clock a test drives. `Arc<FakeClock>` coerces to `Arc<dyn Clock>`.
/// [`FakeClock::new`] reads the process clock once, as an arbitrary origin
/// it never reads again. [`FakeClock::advance`], [`FakeClock::advance_marked`],
/// [`FakeClock::advance_when_parked`] and [`Clock::sleep`] are the only things
/// that move it.
pub struct FakeClock {
    origin: Instant,
    state: Mutex<State>,
    parked_cv: Condvar,
    #[cfg(test)]
    await_any_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    mark_parked_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    advance_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    advance_pass_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
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
                fresh_from: 0,
                wakers: Vec::new(),
                observed: Vec::new(),
                watching: 0,
            }),
            parked_cv: Condvar::new(),
            #[cfg(test)]
            await_any_pause: Mutex::new(None),
            #[cfg(test)]
            mark_parked_pause: Mutex::new(None),
            #[cfg(test)]
            advance_pause: Mutex::new(None),
            #[cfg(test)]
            advance_pass_pause: Mutex::new(None),
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
            let moved = state.offset.saturating_add(d);
            self.move_time(&mut state, moved);
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

    fn move_time(&self, state: &mut State, moved: Duration) {
        state.offset = moved;
        state.fresh_from = state.next_id;
        if state.watching > 0 {
            state.observed.push(Observation::Moved);
        }
        self.parked_cv.notify_all();
    }

    /// The only way a test outside `fakes` advances to a named park
    /// (`docs/testing.md`, "Fakes"): waits for `expected`, then advances by
    /// `advance`, returning each thread's latest park as in
    /// [`FakeClock::advance_marked`]. `None` expects an unbounded park. A
    /// finite advance stops at the named instant or short of it, never past it.
    ///
    /// Only fresh parks count: every move records the next park-id boundary
    /// under the moving lock, including zero-length moves, moves through
    /// [`FakeClock::advance_marked`], [`FakeClock::advance`] and [`Clock::sleep`],
    /// and this helper's own move. Parks made before the last move are stale:
    /// they neither match, mismatch nor count toward several threads. Before
    /// the first move every park is fresh. With `thread` named, only that
    /// thread's fresh parks are watched, and any at `expected` matches. Without
    /// a name, all fresh parks are watched and must belong to one distinct
    /// thread; nested parks on one thread count as one. Several fresh threads
    /// fail with "several threads hold a park and no thread was named".
    ///
    /// Lock order decides the failure. During the wait, arrivals and moves by
    /// other paths are logged in the order they take the clock's lock. Each
    /// look takes the first failing entry: a watched arrival at another instant
    /// is a mismatch (including a finite park where `None` was named), and a
    /// move fails with "time moved during the wait by another driver". The
    /// helper then moves no time of its own. After the log, held fresh watched
    /// parks at other instants are mismatches. Only then are several threads,
    /// a match, and the deadline considered. A later match never hides an
    /// earlier failure, even if the mismatching park has left.
    ///
    /// Failures panic at the caller's line, naming every parked thread and its
    /// instant. Validation rejects a finite instant at or before `now()`, an
    /// overshoot, an unrepresentable advance, or a second active wait (the
    /// first keeps its observations). Validation, mismatch and expiry move
    /// no time; each wait closes on failure. The other-driver failure closes
    /// the wait without moving time itself.
    ///
    /// `deadline` bounds waiting for a park, not acquiring the clock's mutex
    /// or calling [`Wake::wake`]. A match already present when examined is
    /// satisfied even after the deadline; without one, expiry fails at once.
    /// The lock that sees the park moves time before releasing the lock and
    /// waking subscribers. A waker panic propagates after time has moved and
    /// is not one of the failures above.
    ///
    /// There is no atomic end-and-advance protocol: a fresh retirement park
    /// made after the code under test reports its result is legitimate and
    /// may advance. Tests assert on that result, not on no advance after end.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    pub fn advance_when_parked(
        &self,
        expected: Option<Instant>,
        thread: Option<ThreadId>,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
    ) -> Mark {
        {
            let state = lock(&self.state);
            if let Err(kind) = checked_move(&state, self.origin, expected, advance) {
                let report = report(&state, expected, thread, advance, kind, None);
                drop(state);
                panic!("{report}");
            }
        }
        let mut state = lock(&self.state);
        if state.watching > 0 {
            let report = report(
                &state,
                expected,
                thread,
                advance,
                "another advance_when_parked wait is already active: overlapping waits share one observation log",
                None,
            );
            drop(state);
            panic!("{report}");
        }
        state.watching += 1;
        state.observed.clear();
        loop {
            #[cfg(test)]
            {
                state = self.advance_pass_checked(state);
            }
            // A watched mismatch fails before any match is considered: a
            // convenient later park never hides the first wrong one.
            if let Some(found) = watched_mismatch(&state, expected, thread) {
                let (kind, park) = match found {
                    Observation::Moved => ("time moved during the wait by another driver", None),
                    Observation::Park(id, until) => {
                        let kind = if expected.is_none() {
                            "a watched thread parked at a finite instant where None was named"
                        } else if until.is_none() {
                            "a watched park arrived without a deadline where a finite instant was named"
                        } else {
                            "a watched park arrived at a different instant first"
                        };
                        (kind, Some((id, until)))
                    }
                };
                let report = report(&state, expected, thread, advance, kind, park);
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            if thread.is_none() && several_threads_parked(&state) {
                let report = report(
                    &state,
                    expected,
                    thread,
                    advance,
                    "several threads hold a park and no thread was named: name the thread",
                    None,
                );
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            if matched(&state, expected, thread) {
                match checked_move(&state, self.origin, expected, advance) {
                    Ok(moved) => {
                        let mark = Mark(state.latest.clone());
                        let wakers = state
                            .wakers
                            .iter()
                            .filter_map(Weak::upgrade)
                            .collect::<Vec<_>>();
                        self.move_time(&mut state, moved);
                        leave_wait(&mut state);
                        drop(state);
                        for waker in wakers {
                            waker.wake();
                        }
                        return mark;
                    }
                    Err(kind) => {
                        let report = report(&state, expected, thread, advance, kind, None);
                        leave_wait(&mut state);
                        drop(state);
                        panic!("{report}");
                    }
                }
            }
            let wait_for = deadline.left();
            if wait_for.is_zero() {
                let report = report(
                    &state,
                    expected,
                    thread,
                    advance,
                    "the deadline passed before the named park arrived",
                    None,
                );
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            #[cfg(test)]
            self.advance_wait_checked();
            let (guard, _) = self
                .parked_cv
                .wait_timeout(state, wait_for)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
    }

    #[cfg(test)]
    fn pause_next_advance_pass(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .advance_pass_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    /// Pauses once before a decision, with the wait registered but no state
    /// lock held. No outside seam can hold the helper between two events
    /// that each wake it. The test owns the release sender: sending or
    /// dropping it releases the pause, including when the test panics.
    #[cfg(test)]
    fn advance_pass_checked<'a>(&'a self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        let pause = self
            .advance_pass_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((entered, release)) = pause {
            drop(state);
            let _sent = entered.send(());
            let _released = release.recv();
            lock(&self.state)
        } else {
            state
        }
    }

    #[cfg(test)]
    fn pause_next_advance_wait(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (checked_tx, checked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .advance_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((checked_tx, release_rx));
        (checked_rx, release_tx)
    }

    /// The `advance_when_parked` wait point, called only while the wait
    /// predicate holds: no watched match and no watched mismatch yet. Fires
    /// once per arming; the pause mutex is released before the test signal
    /// and release wait. A wrong park already held never reaches it, so a
    /// test that arms it proves the helper waited before the park arrived.
    #[cfg(test)]
    fn advance_wait_checked(&self) {
        let pause = self
            .advance_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((checked, release)) = pause {
            let _sent = checked.send(());
            crate::deadline::Deadline::after(Duration::from_secs(5))
                .recv(&release)
                .expect("waited for the test to release the advance_when_parked wait");
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

    #[cfg(test)]
    fn pause_next_await_any_predicate(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (checked_tx, checked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .await_any_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((checked_tx, release_rx));
        (checked_rx, release_tx)
    }

    #[cfg(test)]
    #[track_caller]
    fn await_any_predicate_checked(&self) {
        let pause = self
            .await_any_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((checked, release)) = pause {
            let _sent = checked.send(());
            crate::deadline::Deadline::after(Duration::from_secs(5))
                .recv(&release)
                .expect("waited for the test to release the await_any predicate");
        }
    }

    #[cfg(test)]
    fn pause_next_mark_parked_predicate(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (checked_tx, checked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .mark_parked_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((checked_tx, release_rx));
        (checked_rx, release_tx)
    }

    /// The `mark_parked` pause point, called only while its wait predicate is
    /// true. Fires once per arming; the pause mutex is released before the
    /// test signal and release wait.
    #[cfg(test)]
    #[track_caller]
    fn mark_parked_predicate_checked(&self) {
        let pause = self
            .mark_parked_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((checked, release)) = pause {
            let _sent = checked.send(());
            crate::deadline::Deadline::after(Duration::from_secs(5))
                .recv(&release)
                .expect("waited for the test to release the mark_parked predicate");
        }
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
            .wait_timeout_while(state, within, |state| {
                let waiting = parked_count(state, until) == 0;
                #[cfg(test)]
                if waiting {
                    self.mark_parked_predicate_checked();
                }
                waiting
            })
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
            .wait_timeout_while(state, within, |state| {
                let waiting = !parked_after(state, mark);
                #[cfg(test)]
                self.await_any_predicate_checked();
                waiting
            })
            .unwrap_or_else(PoisonError::into_inner);
        parked_after(&guard, mark)
    }

    /// Waits, at most `within` of real time, until `thread` is parked in
    /// [`Clock::wait_until`] with this `until`. True once it is; false at
    /// the deadline. The bound is the condvar's timeout, so this reads no
    /// process clock.
    pub fn await_thread_parked(
        &self,
        thread: ThreadId,
        until: Option<Instant>,
        within: Duration,
    ) -> bool {
        let state = lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_thread(state, thread, until))
            .unwrap_or_else(PoisonError::into_inner);
        parked_thread(&guard, thread, until)
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
                // An active advance_when_parked wait sees every arrival, even
                // one that leaves before the helper diagnoses it.
                if state.watching > 0 {
                    state.observed.push(Observation::Park(thread, until));
                }
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

/// The process clock tests use (`docs/testing.md`, "Values that change
/// every run"): the operating system's clock in [`support::clock`].
pub use support::clock::System as SystemClock;

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

fn parked_thread(state: &State, thread: ThreadId, until: Option<Instant>) -> bool {
    state
        .parked
        .iter()
        .any(|parked| parked.thread == thread && parked.until == until)
}

fn parked_unbounded(state: &State) -> bool {
    state.parked.iter().any(|parked| parked.until.is_none())
}

/// The offset after `advance`, or why [`FakeClock::advance_when_parked`] must
/// fail without moving time. Checked before waiting and again under the lock
/// that would move time, so neither a named park already passed nor an
/// unrepresentable advance ever mutates the clock.
fn checked_move(
    state: &State,
    origin: Instant,
    expected: Option<Instant>,
    advance: Duration,
) -> Result<Duration, &'static str> {
    let now = now_in(state, origin);
    if let Some(until) = expected {
        // The named park already passed: waiting could only expire.
        if until <= now {
            return Err("the named park already passed: it is at or before now");
        }
        // A finite park stops at its instant or short of it, never past it.
        if now
            .checked_add(advance)
            .is_some_and(|target| target > until)
        {
            return Err("the advance would pass the named instant");
        }
    }
    let Some(moved) = state.offset.checked_add(advance) else {
        return Err("the advance overflows the clock");
    };
    // removed: a combined check has no portable single-overflow case because
    // Instant and SystemTime have platform-specific ceilings.
    if origin.checked_add(moved).is_none() {
        return Err("the advance overflows the clock's monotonic instant");
    }
    if wall_checked(moved).is_none() {
        return Err("the advance overflows the clock's wall time");
    }
    Ok(moved)
}

/// A park made at or after the last move's boundary is fresh.
fn fresh(state: &State, parked: &Parked) -> bool {
    parked.id >= state.fresh_from
}

/// First failure in lock order, then a held fresh watched mismatch. A move
/// fails regardless of the selected thread; arrivals watch only that thread
/// when named. Logged arrivals need no freshness filter: any later move is
/// itself a failure, so neither it nor a later match hides an earlier mismatch.
fn watched_mismatch(
    state: &State,
    expected: Option<Instant>,
    thread: Option<ThreadId>,
) -> Option<Observation> {
    let selected = |id: ThreadId| thread.is_none() || thread == Some(id);
    state
        .observed
        .iter()
        .copied()
        .chain(
            state
                .parked
                .iter()
                .filter(|parked| fresh(state, parked))
                .map(|parked| Observation::Park(parked.thread, parked.until)),
        )
        .find(|entry| match entry {
            Observation::Moved => true,
            Observation::Park(id, until) => selected(*id) && *until != expected,
        })
}

/// A selected fresh park matches; unnamed parks must belong to exactly one
/// distinct thread, even when that thread holds nested parks.
fn matched(state: &State, expected: Option<Instant>, thread: Option<ThreadId>) -> bool {
    let mut held = state.parked.iter().filter(|parked| fresh(state, parked));
    match thread {
        Some(selected) => held.any(|parked| parked.thread == selected && parked.until == expected),
        None => match held.next() {
            Some(first) => {
                let same_thread = held.clone().all(|parked| parked.thread == first.thread);
                let at_expected = std::iter::once(first)
                    .chain(held)
                    .any(|parked| parked.until == expected);
                same_thread && at_expected
            }
            None => false,
        },
    }
}

/// Whether fresh parks belong to two or more distinct threads. Nested parks
/// on one thread count as one; diagnostics still list every held park.
fn several_threads_parked(state: &State) -> bool {
    let mut held = state.parked.iter().filter(|parked| fresh(state, parked));
    held.next()
        .is_some_and(|first| held.any(|parked| parked.thread != first.thread))
}

/// Closes an [`FakeClock::advance_when_parked`] wait: the observation log
/// never outlives the last wait it belongs to.
fn leave_wait(state: &mut State) {
    state.watching -= 1;
    if state.watching == 0 {
        state.observed.clear();
    }
}

/// The caller diagnostic for an [`FakeClock::advance_when_parked`] failure:
/// the failure kind, the expected instant, the named thread when one was
/// named, the requested advance, the watched park that failed the wait when
/// one did, and every thread parked now with its instant (`None` for an
/// unbounded park, `[]` when nobody is parked).
fn report(
    state: &State,
    expected: Option<Instant>,
    thread: Option<ThreadId>,
    advance: Duration,
    kind: &str,
    found: Option<(ThreadId, Option<Instant>)>,
) -> String {
    let mut message = format!(
        "advance_when_parked failed: {kind}; expected park {expected:?}, thread {thread:?}, advance {advance:?}"
    );
    if let Some((id, until)) = found {
        message.push_str(&format!("; watched park {id:?} at {until:?}"));
    }
    message.push_str("; parked threads: [");
    let mut first = true;
    for parked in &state.parked {
        if !first {
            message.push_str(", ");
        }
        first = false;
        message.push_str(&format!("{:?} at {:?}", parked.thread, parked.until));
    }
    message.push(']');
    message
}

/// `wall()` without the saturating fallback, so an unrepresentable advance is
/// rejected instead of clamped.
fn wall_checked(offset: Duration) -> Option<SystemTime> {
    UNIX_EPOCH
        .checked_add(WALL_EPOCH)
        .and_then(|epoch| epoch.checked_add(offset))
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
