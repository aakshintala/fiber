//! The clock seam and the clock-parked wait (`docs/testing.md`, "Values that
//! change every run"): monotonic time for deadlines, wall time for `ts` and
//! expiries, and waiting. Callers take `&dyn Clock` or `Arc<dyn Clock>` and
//! never read the process clock themselves (`docs/code-quality.md`, "Lints").

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A source of time. The real clock and a fake both implement it; callers
/// take `Arc<dyn Clock>` or `&dyn Clock` and never read the process clock
/// themselves.
pub trait Clock: Send + Sync {
    /// Monotonic time, for deadlines.
    fn now(&self) -> Instant;

    /// Wall-clock time, for `ts` and expiries a server states.
    fn wall(&self) -> SystemTime;

    /// Pauses the caller for `d` of this clock's time.
    fn sleep(&self, d: Duration);

    /// Blocks the caller once, via `wait`, until it is woken or `until`
    /// passes on this clock.
    ///
    /// `wait` receives the real-time bound to block for. `until` at or
    /// before [`Clock::now`]: `Some(Duration::ZERO)` on both clocks. Later:
    /// `Some(until - now)` on the real clock, `None` (block until woken) on
    /// a fake. `until == None`: `None` on both.
    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>));

    /// Registers a waiter to wake whenever this clock's time moves. The real
    /// clock ignores it.
    fn subscribe(&self, waker: Weak<dyn Wake>);
}

/// Wakes whoever is blocked in [`Clock::wait_until`] when the clock moves.
pub trait Wake: Send + Sync {
    /// Wakes every waiter this value stands for.
    fn wake(&self);
}

/// The operating system's clock: the only production [`Clock`] that reads
/// real time.
pub struct System;

impl Clock for System {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    // A wall-clock sleep; docs/testing.md, "Values that change every run",
    // permits only main's real-clock sleep test.
    #[cfg_attr(false, mutants::skip)]
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        // `saturating_duration_since` is zero when `until` is at or before now,
        // so that branch needs no separate arm.
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

/// Parks the caller on `changed` until `done` holds, handing back the guard
/// it was given. One [`Clock::wait_until`] call: when `done` already holds,
/// the guard goes back untouched; otherwise one condvar wait, bounded by
/// the shorter of the clock's bound and `poll` (`None` waits until
/// notified). For example:
/// `park(clock, Some(deadline), None, &shared.cv, guard, |s| s.seq != seen)`.
///
/// The caller's guard is held from its own check until the condvar wait
/// releases it, so a notify made under the same mutex after that check is
/// never missed. It waits at most once and never loops; the caller re-reads
/// its state and the clock after it returns. `None` only when the clock
/// never invokes the wait; both clocks invoke it exactly once.
pub fn park<'a, T>(
    clock: &dyn Clock,
    until: Option<Instant>,
    poll: Option<Duration>,
    changed: &Condvar,
    guard: MutexGuard<'a, T>,
    mut done: impl FnMut(&mut T) -> bool,
) -> Option<MutexGuard<'a, T>> {
    let mut slot = Some(guard);
    clock.wait_until(until, &mut |bound| {
        let Some(mut guard) = slot.take() else {
            return;
        };
        if done(&mut guard) {
            slot = Some(guard);
            return;
        }
        let guard = match bounded(bound, poll) {
            Some(limit) => {
                changed
                    .wait_timeout(guard, limit)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            }
            None => changed.wait(guard).unwrap_or_else(PoisonError::into_inner),
        };
        slot = Some(guard);
    });
    slot
}

/// The shorter of the clock's bound and the caller's poll cap, either when
/// one is set. A site without polling passes `None` and still blocks until
/// notified.
fn bounded(bound: Option<Duration>, poll: Option<Duration>) -> Option<Duration> {
    match (bound, poll) {
        (Some(bound), Some(poll)) => Some(bound.min(poll)),
        (bound, None) => bound,
        (None, poll) => poll,
    }
}

/// A generation counter with a condvar that is itself a [`Wake`]: sites
/// that own a bare counter park on it instead of threading a guard through
/// [`park`]. A caller reads `seen = generation()` before its own checks;
/// any bump after that read makes [`Parker::park`] return at once.
#[derive(Default)]
pub struct Parker {
    seq: Mutex<u64>,
    changed: Condvar,
}

impl Parker {
    /// A parker no thread has bumped.
    pub fn new() -> Self {
        Self {
            seq: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    /// Counts one wake and wakes every parked thread.
    pub fn bump(&self) {
        let mut seq = crate::lock(&self.seq);
        *seq = seq.wrapping_add(1);
        self.changed.notify_all();
    }

    /// How many bumps have landed.
    pub fn generation(&self) -> u64 {
        *crate::lock(&self.seq)
    }

    /// Parks on the parker's own counter until a bump past `seen`, `until`
    /// passes on `clock`, or the clock wakes it: [`park`] over the counter.
    pub fn park(&self, clock: &dyn Clock, until: Option<Instant>, seen: u64) {
        self.park_polled(clock, until, None, seen);
    }

    /// [`park`](Parker::park) with a real-time poll cap: the wait ends at
    /// most `poll` past its start even with no bump and no clock move, so
    /// a missed exit is re-checked instead of slept through.
    pub fn park_polled(
        &self,
        clock: &dyn Clock,
        until: Option<Instant>,
        poll: Option<Duration>,
        seen: u64,
    ) {
        let _guard = park(
            clock,
            until,
            poll,
            &self.changed,
            crate::lock(&self.seq),
            |seq| *seq != seen,
        );
    }
}

impl Wake for Parker {
    fn wake(&self) {
        self.bump();
    }
}

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

/// Which parks an `advance_when_parked` wait watches: one named thread, or
/// an unnamed wait for exactly `Count` distinct threads.
#[derive(Clone, Copy)]
enum Select {
    Thread(ThreadId),
    Count(usize),
}

/// A test-only pause: the signal that the wait point was reached, the
/// release the test sends, and whether it waits for a look with a held park.
#[cfg(test)]
type AdvancePause = (
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
    bool,
);

struct State {
    offset: Duration,
    parked: Vec<Parked>,
    /// The latest park id of each thread that ever parked on this clock.
    /// Written in `wait_until` under the lock that pushes the park; a
    /// `Manual` lives for one test, so this holds at most one entry
    /// per test thread. `Leave` never touches it.
    latest: Vec<(ThreadId, u64)>,
    /// Park ids are one per park; wrapping needs 2^64 parks, which a test
    /// process cannot reach, so a later park's id compares greater with `>`.
    next_id: u64,
    /// `next_id` at the last move, including a zero-length move.
    fresh_from: u64,
    wakers: Vec<Weak<dyn Wake>>,
    /// Parks and moves during an [`Manual::advance_when_parked`] wait,
    /// in the order they take the state lock. The first failing entry wins,
    /// even if a park leaves before the helper looks. Cleared on wait exit.
    observed: Vec<Observation>,
    /// Whether an [`Manual::advance_when_parked`] wait is recording into
    /// `observed`: a second wait while one is active fails rather than
    /// share the log.
    watching: bool,
}

/// Which thread last parked at the instant of an advance, from
/// [`Manual::advance_marked`]: each thread's latest park id, whether
/// it is still parked or has left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark(Vec<(ThreadId, u64)>);

impl Mark {
    /// The threads in this mark, one entry per thread it holds.
    pub fn threads(&self) -> Vec<ThreadId> {
        self.0.iter().map(|(thread, _)| *thread).collect()
    }
}

/// What a soft [`Manual::try_advance_when_parked`] wait found instead of
/// its named park: no fresh park arrived within the pause, or a watched
/// park sits at another instant. A driver that is the clock's only mover
/// acts on either; every louder failure still panics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    /// No fresh selected park arrived within the pause.
    NoPark,
    /// A watched fresh selected park sits at another instant.
    Mismatch {
        /// The park that failed the wait, even when it has since left.
        watched: (ThreadId, Option<Instant>),
        /// Every parked thread with its instant at the look, in park order,
        /// possibly empty when the mismatching park already left.
        parked: Vec<(ThreadId, Option<Instant>)>,
    },
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::NoPark => write!(f, "no fresh park arrived within the pause"),
            Miss::Mismatch { watched, parked } => {
                write!(f, "watched park {:?} at {:?}", watched.0, watched.1)?;
                write!(f, "; parked threads: [")?;
                for (index, (thread, until)) in parked.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{thread:?} at {until:?}")?;
                }
                write!(f, "]")
            }
        }
    }
}

/// How [`Manual::sleep`] moves time, chosen when the clock is built.
#[derive(Clone, Copy)]
pub enum Sleep {
    /// [`Manual::sleep`] advances the clock at once, like a test helper.
    Advances,
    /// [`Manual::sleep`] parks until an advance moves the clock past it.
    Waits,
}

/// A clock a test drives. `Arc<Manual>` coerces to `Arc<dyn Clock>`.
/// [`Manual::new`] reads the process clock once, as an arbitrary origin
/// it never reads again. [`Manual::advance`], [`Manual::advance_marked`],
/// [`Manual::advance_when_parked`],
/// [`Manual::advance_when_parked_threads`],
/// [`Manual::advance_when_parked_then`],
/// [`Manual::try_advance_when_parked`] and [`Clock::sleep`] are the only
/// things that move it.
pub struct Manual {
    origin: Instant,
    state: Mutex<State>,
    parked_cv: Condvar,
    sleep: Sleep,
    #[cfg(test)]
    await_any_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    mark_parked_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    advance_pause: Mutex<Option<AdvancePause>>,
    #[cfg(test)]
    advance_pass_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    /// Test-only pause: the signal [`Manual::sleep`] fires after it parks in
    /// [`Sleep::Waits`] mode (`docs/testing.md`, "Waits and timeouts").
    /// One-shot: the predicate takes and sends it once.
    #[cfg(test)]
    sleep_pause: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Manual {
    /// Builds a clock at an arbitrary monotonic origin and a fixed wall
    /// epoch, whose [`Manual::sleep`] advances time at once.
    #[expect(
        clippy::disallowed_methods,
        reason = "the manual clock reads one origin; later time moves only through its advance methods"
    )]
    pub fn new() -> Arc<Self> {
        // The origin is an arbitrary Instant. Nothing compares it to a later
        // reading of the process clock.
        let origin = Instant::now();
        Self::build(origin, Sleep::Advances)
    }

    /// Builds a clock at an arbitrary monotonic origin and a fixed wall
    /// epoch, whose [`Manual::sleep`] parks until an advance moves past it.
    #[expect(
        clippy::disallowed_methods,
        reason = "the manual clock reads one origin; later time moves only through its advance methods"
    )]
    pub fn with_sleep(sleep: Sleep) -> Arc<Self> {
        // The origin is an arbitrary Instant. Nothing compares it to a later
        // reading of the process clock.
        let origin = Instant::now();
        Self::build(origin, sleep)
    }

    fn build(origin: Instant, sleep: Sleep) -> Arc<Self> {
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
                watching: false,
            }),
            parked_cv: Condvar::new(),
            sleep,
            #[cfg(test)]
            await_any_pause: Mutex::new(None),
            #[cfg(test)]
            mark_parked_pause: Mutex::new(None),
            #[cfg(test)]
            advance_pause: Mutex::new(None),
            #[cfg(test)]
            advance_pass_pause: Mutex::new(None),
            #[cfg(test)]
            sleep_pause: Mutex::new(None),
        })
    }

    /// The monotonic instant `now()` returned at construction.
    pub fn origin(&self) -> Instant {
        self.origin
    }

    /// Installs the [`Manual::sleep`] pause seam and returns its fire
    /// signal. The test calls this before spawning the sleeper.
    #[cfg(test)]
    pub(crate) fn install_sleep_pause(&self) -> std::sync::mpsc::Receiver<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        *crate::lock(&self.sleep_pause) = Some(tx);
        rx
    }

    /// The deadlines currently parked in [`Clock::wait_until`], as
    /// millisecond offsets from the clock's current now, with the now
    /// offset from the clock origin. Past deadlines saturate to zero.
    pub fn parked_offsets(&self) -> (Duration, Vec<Option<Duration>>) {
        let state = crate::lock(&self.state);
        let now = now_in(&state, self.origin);
        let offset = state.offset;
        let parked = state
            .parked
            .iter()
            .map(|parked| {
                parked
                    .until
                    .map(|until| until.saturating_duration_since(now))
            })
            .collect();
        (offset, parked)
    }

    /// Moves clock time only after a waiter is parked at exactly `now + d`.
    /// The bounded wait is on the process clock (`docs/testing.md`,
    /// "Waits and timeouts").
    pub fn advance_when_parked_in(&self, d: Duration, within: Duration) -> Result<(), String> {
        // An unrepresentable target errors before waiting or moving: without
        // this, the move below would saturate while `now_in` and `wall`
        // fall back to the origin and the epoch, reporting a move that left
        // the clock where it was.
        let unrepresentable = {
            let state = crate::lock(&self.state);
            state.offset.checked_add(d).is_none_or(|moved| {
                self.origin.checked_add(moved).is_none() || wall_checked(moved).is_none()
            })
        };
        if unrepresentable {
            return Err(format!(
                "the advance overflows the clock: now + {d:?} is unrepresentable"
            ));
        }
        let state = crate::lock(&self.state);
        let (mut state, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| {
                matching_deadline(state, self.origin, d).is_none()
            })
            .unwrap_or_else(PoisonError::into_inner);
        if matching_deadline(&state, self.origin, d).is_none() {
            let parked = state
                .parked
                .iter()
                .map(|parked| parked.until)
                .collect::<Vec<_>>();
            return Err(format!(
                "no waiter parked at now + {d:?} within {within:?}; parked deadlines: {parked:?}"
            ));
        }
        // Defensive: the pre-wait check accepted this advance, and the case
        // driver is the clock's only mover, so this cannot fail; a concurrent
        // move past the bound errors instead of misreporting.
        let Some(moved) = state.offset.checked_add(d) else {
            return Err(format!(
                "the advance overflows the clock: now + {d:?} is unrepresentable"
            ));
        };
        let wakers = state
            .wakers
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        self.move_time(&mut state, moved);
        drop(state);
        for waker in wakers {
            waker.wake();
        }
        Ok(())
    }

    /// Moves `now()` and `wall()` forward by `d`, then wakes every live
    /// subscriber. The clock's lock is released before any [`Wake::wake`].
    pub fn advance(&self, d: Duration) {
        self.advance_marked(d);
    }

    /// [`Manual::advance`], returning each thread's latest park at the
    /// instant `now()` moved. A thread that left its park before the
    /// advance still matches its next park in
    /// [`Manual::await_parked_since`]. The snapshot is taken under the
    /// lock that moves it.
    pub fn advance_marked(&self, d: Duration) -> Mark {
        let (mark, wakers) = {
            let mut state = crate::lock(&self.state);
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
        if state.watching {
            state.observed.push(Observation::Moved);
        }
        self.parked_cv.notify_all();
    }

    /// The only way a test outside `fakes` advances to a named park
    /// (`docs/testing.md`, "Fakes"): waits for `expected`, then advances by
    /// `advance`, returning each thread's latest park as in
    /// [`Manual::advance_marked`]. `None` expects an unbounded park. A
    /// finite advance stops at the named instant or short of it, never past it.
    ///
    /// Only fresh parks count: every move records the next park-id boundary
    /// under the moving lock, including zero-length moves, moves through
    /// [`Manual::advance_marked`], [`Manual::advance`] and [`Clock::sleep`],
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
    pub fn advance_when_parked(
        &self,
        expected: Option<Instant>,
        thread: Option<ThreadId>,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
    ) -> Mark {
        let select = match thread {
            Some(id) => Select::Thread(id),
            None => Select::Count(1),
        };
        self.advance_when_parked_strict(expected, select, advance, deadline, &mut || {})
    }

    /// Waits for exactly `count` threads to hold a fresh park at `expected`,
    /// then advances by `advance`, returning each thread's latest park as in
    /// [`Manual::advance_marked`]. `None` expects unbounded parks.
    ///
    /// Only fresh parks count: parks made before the last move are stale and
    /// count for nothing, and nested parks on one thread count once. A park
    /// at any other instant is the existing watched mismatch. More parked
    /// threads than `count` fail at once, naming every parked thread and
    /// instant; fewer wait to the deadline. A count of zero fails before
    /// waiting. Overshoot, other-driver move, second active wait and
    /// "deadline bounds only the wait for a park" are exactly as for
    /// [`Manual::advance_when_parked`].
    ///
    /// Failures panic at the caller's line, naming every parked thread and
    /// its instant, and move no time.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    pub fn advance_when_parked_threads(
        &self,
        expected: Option<Instant>,
        count: usize,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
    ) -> Mark {
        if count == 0 {
            let state = crate::lock(&self.state);
            let report = report(
                &state,
                expected,
                Select::Count(count),
                advance,
                "the thread count must be at least 1",
                None,
            );
            drop(state);
            panic!("{report}");
        }
        self.advance_when_parked_strict(
            expected,
            Select::Count(count),
            advance,
            deadline,
            &mut || {},
        )
    }

    /// [`Manual::advance_when_parked`], running `hook` after the park is
    /// observed and before time moves.
    ///
    /// `hook` runs at most once: only after the park matched and the move
    /// validated, immediately before the move, while the clock's state lock
    /// is held. It therefore never runs for a validation failure, a
    /// mismatch, an other-driver move, a second active wait or an expiry.
    /// It must not call any [`Manual`] method, nor take a lock another
    /// thread holds while calling the clock: the mutex is not reentrant, and
    /// either deadlocks. Wakers run after the lock drops. Returns
    /// the same [`Mark`] the other waits return, taken under the move's
    /// lock.
    ///
    /// A hook panic unwinds to the caller with its payload and leaves time
    /// unmoved; no subscribed waker is called.
    #[track_caller]
    pub fn advance_when_parked_then(
        &self,
        expected: Option<Instant>,
        thread: Option<ThreadId>,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
        hook: impl FnOnce(),
    ) -> Mark {
        let select = match thread {
            Some(id) => Select::Thread(id),
            None => Select::Count(1),
        };
        let mut hook = Some(hook);
        self.advance_when_parked_strict(expected, select, advance, deadline, &mut || {
            if let Some(hook) = hook.take() {
                hook();
            }
        })
    }

    /// The strict form of the loop behind every `advance_when_parked` wait:
    /// the soft path never returns `Err`, so the miss becomes the wait's
    /// panic at the caller's line.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    fn advance_when_parked_strict(
        &self,
        expected: Option<Instant>,
        select: Select,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
        hook: &mut dyn FnMut(),
    ) -> Mark {
        match self.advance_when_parked_core(expected, select, advance, deadline, false, hook) {
            Ok(mark) => mark,
            Err(miss) => panic!("{miss}"),
        }
    }

    /// The soft form of [`Manual::advance_when_parked`] for a poll driver
    /// that is the clock's only mover: waits up to `pause` of real time for
    /// a fresh park at `expected`, then advances by `advance` exactly as the
    /// strict helper does, returning the same [`Mark`]. `None` expects an
    /// unbounded park; `thread` watches one thread, or all fresh parks
    /// without a name.
    ///
    /// Only two mild failures become values: no fresh selected park within
    /// `pause` returns `Miss::NoPark`, and a watched park at another instant
    /// returns `Miss::Mismatch`, holding the watched park and every parked
    /// thread at the look. A match present when examined wins over expiry.
    /// Every louder failure still panics at the caller's line, naming every
    /// parked thread and its instant: validation, an overshoot, a second
    /// active wait, another driver's move, several fresh threads with no
    /// thread named, and a zero `pause`. A `Miss` moves no time and closes
    /// the wait.
    ///
    /// `pause` bounds one wait for a fresh park, as the condvar's timeout,
    /// not a deadline: the driver's own loop holds one `Deadline` and fails
    /// at it. A very long `pause` never overflows: the bound is read through
    /// a saturating remaining-time computation.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    pub fn try_advance_when_parked(
        &self,
        expected: Option<Instant>,
        thread: Option<ThreadId>,
        advance: Duration,
        pause: Duration,
    ) -> Result<Mark, Miss> {
        let select = match thread {
            Some(id) => Select::Thread(id),
            None => Select::Count(1),
        };
        if pause.is_zero() {
            let state = crate::lock(&self.state);
            let report = report(
                &state,
                expected,
                select,
                advance,
                "the pause must be non-zero",
                None,
            );
            drop(state);
            panic!("{report}");
        }
        let deadline = crate::deadline::Deadline::after(pause);
        self.advance_when_parked_core(expected, select, advance, &deadline, true, &mut || {})
    }

    /// The loop behind every `advance_when_parked` wait: `select` says which
    /// parks are watched, and `hook` runs under the lock after the park
    /// matched and the move validated, immediately before the move. With
    /// `soft` set, the two mild failures become values: no fresh park by the
    /// deadline returns `Miss::NoPark`, and a watched park at another
    /// instant returns `Miss::Mismatch`; every louder failure still panics.
    #[track_caller]
    #[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
    fn advance_when_parked_core(
        &self,
        expected: Option<Instant>,
        select: Select,
        advance: Duration,
        deadline: &crate::deadline::Deadline,
        soft: bool,
        hook: &mut dyn FnMut(),
    ) -> Result<Mark, Miss> {
        {
            let state = crate::lock(&self.state);
            if let Err(kind) = checked_move(&state, self.origin, expected, advance) {
                let report = report(&state, expected, select, advance, kind, None);
                drop(state);
                panic!("{report}");
            }
        }
        let mut state = crate::lock(&self.state);
        if state.watching {
            let report = report(
                &state,
                expected,
                select,
                advance,
                "another advance_when_parked wait is already active: overlapping waits share one observation log",
                None,
            );
            drop(state);
            panic!("{report}");
        }
        state.watching = true;
        state.observed.clear();
        loop {
            #[cfg(test)]
            {
                state = self.advance_pass_checked(state);
            }
            // A watched mismatch fails before any match is considered: a
            // convenient later park never hides the first wrong one.
            if let Some(found) = watched_mismatch(&state, expected, select) {
                if soft && let Observation::Park(id, until) = found {
                    let parked = state
                        .parked
                        .iter()
                        .map(|parked| (parked.thread, parked.until))
                        .collect();
                    leave_wait(&mut state);
                    drop(state);
                    return Err(Miss::Mismatch {
                        watched: (id, until),
                        parked,
                    });
                }
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
                let report = report(&state, expected, select, advance, kind, park);
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            if several_threads_parked(&state, select) {
                let kind = select_message(select, SelectOutcome::TooMany);
                let report = report(&state, expected, select, advance, &kind, None);
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            if matched(&state, expected, select) {
                match checked_move(&state, self.origin, expected, advance) {
                    Ok(moved) => {
                        hook();
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
                        return Ok(mark);
                    }
                    Err(kind) => {
                        let report = report(&state, expected, select, advance, kind, None);
                        leave_wait(&mut state);
                        drop(state);
                        panic!("{report}");
                    }
                }
            }
            let wait_for = deadline.left();
            if wait_for.is_zero() {
                if soft {
                    leave_wait(&mut state);
                    drop(state);
                    return Err(Miss::NoPark);
                }
                let kind = select_message(select, SelectOutcome::Expired);
                let report = report(&state, expected, select, advance, &kind, None);
                leave_wait(&mut state);
                drop(state);
                panic!("{report}");
            }
            #[cfg(test)]
            self.advance_wait_checked(&state);
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
        *crate::lock(&self.advance_pass_pause) = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    /// Pauses once before a decision, with the wait registered but no state
    /// lock held. No outside seam can hold the helper between two events
    /// that each wake it. The test owns the release sender: sending or
    /// dropping it releases the pause, including when the test panics.
    #[cfg(test)]
    fn advance_pass_checked<'a>(&'a self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        let pause = crate::lock(&self.advance_pass_pause).take();
        if let Some((entered, release)) = pause {
            drop(state);
            let _sent = entered.send(());
            let _released = release.recv();
            crate::lock(&self.state)
        } else {
            state
        }
    }

    #[cfg(test)]
    fn pause_next_advance_wait(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        self.arm_advance_wait(false)
    }

    /// [`Self::pause_next_advance_wait`], firing only on a look with a fresh
    /// park held. Arm it before a park that a test waits on: looks before
    /// that park pass it by, so it never holds the state lock while the
    /// parker needs that lock.
    #[cfg(test)]
    fn pause_next_advance_wait_when_held(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        self.arm_advance_wait(true)
    }

    #[cfg(test)]
    fn arm_advance_wait(
        &self,
        when_held: bool,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (checked_tx, checked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *crate::lock(&self.advance_pause) = Some((checked_tx, release_rx, when_held));
        (checked_rx, release_tx)
    }

    /// The `advance_when_parked` wait point, called only while the wait
    /// predicate holds: no watched match and no watched mismatch yet, with
    /// the state lock held. Fires once per arming; the pause mutex is
    /// released before the test signal and release wait. A wrong park already
    /// held never reaches it, so a test that arms it proves the helper waited
    /// before the park arrived. A `when_held` arming also passes every look
    /// with no fresh park held.
    #[cfg(test)]
    fn advance_wait_checked(&self, state: &State) {
        let held = state.parked.iter().any(|parked| fresh(state, parked));
        let pause =
            crate::lock(&self.advance_pause).take_if(|(_, _, when_held)| held || !*when_held);
        if let Some((checked, release, _)) = pause {
            let _sent = checked.send(());
            crate::deadline::Deadline::after(Duration::from_secs(5))
                .recv(&release)
                .expect("waited for the test to release the advance_when_parked wait");
        }
    }

    /// The `until` of each thread now inside [`Clock::wait_until`].
    pub fn parked(&self) -> Vec<Option<Instant>> {
        crate::lock(&self.state)
            .parked
            .iter()
            .map(|parked| parked.until)
            .collect()
    }

    /// Waits, at most what remains of `deadline`, until a thread is parked in
    /// [`Clock::wait_until`] with this `until`. True once it is; false at
    /// the deadline. The bound is the condvar's timeout, so this reads no
    /// process clock.
    pub fn await_parked(&self, until: Instant, deadline: crate::deadline::Deadline) -> bool {
        self.await_parked_count(until, 1, deadline)
    }

    /// Waits, at most what remains of `deadline`, until at least `count` threads
    /// are parked in [`Clock::wait_until`] with this `until`. True once they
    /// are; false at the deadline. The bound is the condvar's timeout, so
    /// this reads no process clock.
    pub fn await_parked_count(
        &self,
        until: Instant,
        count: usize,
        deadline: crate::deadline::Deadline,
    ) -> bool {
        let within = deadline.left();
        let state = crate::lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| parked_count(state, until) < count)
            .unwrap_or_else(PoisonError::into_inner);
        parked_count(&guard, until) >= count
    }

    /// Waits, at most what remains of `deadline`, until a thread in `mark` is
    /// parked again, in a later park, with this `until` (`None`: no
    /// deadline). True once it is; false at the deadline. A thread that
    /// left its park before the advance matches its next park.
    pub fn await_parked_since(
        &self,
        mark: &Mark,
        until: Option<Instant>,
        deadline: crate::deadline::Deadline,
    ) -> bool {
        let within = deadline.left();
        let state = crate::lock(&self.state);
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
        *crate::lock(&self.await_any_pause) = Some((checked_tx, release_rx));
        (checked_rx, release_tx)
    }

    #[cfg(test)]
    #[track_caller]
    fn await_any_predicate_checked(&self) {
        let pause = crate::lock(&self.await_any_pause).take();
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
        *crate::lock(&self.mark_parked_pause) = Some((checked_tx, release_rx));
        (checked_rx, release_tx)
    }

    /// The `mark_parked` pause point, called only while its wait predicate is
    /// true. Fires once per arming; the pause mutex is released before the
    /// test signal and release wait.
    #[cfg(test)]
    #[track_caller]
    fn mark_parked_predicate_checked(&self) {
        let pause = crate::lock(&self.mark_parked_pause).take();
        if let Some((checked, release)) = pause {
            let _sent = checked.send(());
            crate::deadline::Deadline::after(Duration::from_secs(5))
                .recv(&release)
                .expect("waited for the test to release the mark_parked predicate");
        }
    }

    /// Waits, at most what remains of `deadline`, until a thread is parked in
    /// [`Clock::wait_until`] with this `until`, and returns a [`Mark`] of the
    /// threads parked at `until`, taken under the lock that saw the park.
    /// `None` at the deadline. A later park by one of them matches
    /// [`Manual::await_parked_since`], whatever woke it.
    pub fn mark_parked(&self, until: Instant, deadline: crate::deadline::Deadline) -> Option<Mark> {
        let within = deadline.left();
        let state = crate::lock(&self.state);
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

    /// Waits, at most what remains of `deadline`, until a thread in `mark` is
    /// parked again, in a later park, with any deadline.
    pub fn await_any_parked_since(&self, mark: &Mark, deadline: crate::deadline::Deadline) -> bool {
        let within = deadline.left();
        let state = crate::lock(&self.state);
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

    /// Waits, at most what remains of `deadline`, until `thread` is parked in
    /// [`Clock::wait_until`] with this `until`. True once it is; false at
    /// the deadline. The bound is the condvar's timeout, so this reads no
    /// process clock.
    pub fn await_thread_parked(
        &self,
        thread: ThreadId,
        until: Option<Instant>,
        deadline: crate::deadline::Deadline,
    ) -> bool {
        let within = deadline.left();
        let state = crate::lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_thread(state, thread, until))
            .unwrap_or_else(PoisonError::into_inner);
        parked_thread(&guard, thread, until)
    }

    /// [`Manual::await_parked`] for a park with no deadline.
    pub fn await_parked_unbounded(&self, deadline: crate::deadline::Deadline) -> bool {
        let within = deadline.left();
        let state = crate::lock(&self.state);
        let (guard, _) = self
            .parked_cv
            .wait_timeout_while(state, within, |state| !parked_unbounded(state))
            .unwrap_or_else(PoisonError::into_inner);
        parked_unbounded(&guard)
    }
}

impl Clock for Manual {
    fn now(&self) -> Instant {
        now_in(&crate::lock(&self.state), self.origin)
    }

    fn wall(&self) -> SystemTime {
        let offset = crate::lock(&self.state).offset;
        UNIX_EPOCH
            .checked_add(WALL_EPOCH)
            .and_then(|epoch| epoch.checked_add(offset))
            .unwrap_or(UNIX_EPOCH)
    }

    fn sleep(&self, d: Duration) {
        match self.sleep {
            Sleep::Advances => self.advance(d),
            Sleep::Waits => {
                let now = self.now();
                let Some(until) = now.checked_add(d) else {
                    return;
                };
                let mut wait = |_| {
                    let state = crate::lock(&self.state);
                    let _state = self
                        .parked_cv
                        .wait_while(state, |state| {
                            let pending = now_in(state, self.origin) < until;
                            #[cfg(test)]
                            if pending && let Some(tx) = crate::lock(&self.sleep_pause).take() {
                                let _sent = tx.send(());
                            }
                            pending
                        })
                        .unwrap_or_else(PoisonError::into_inner);
                };
                self.wait_until(Some(until), &mut wait);
            }
        }
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let id = {
            let mut state = crate::lock(&self.state);
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
                if state.watching {
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

    /// Registers `waker` to wake on every move of this clock. One entry
    /// per allocation: a waker already listed keeps its place and wakes
    /// once per move, and dead entries are dropped first.
    fn subscribe(&self, waker: Weak<dyn Wake>) {
        let mut state = crate::lock(&self.state);
        state.wakers.retain(|listed| listed.strong_count() > 0);
        if !state
            .wakers
            .iter()
            .any(|listed| Weak::ptr_eq(listed, &waker))
        {
            state.wakers.push(waker);
        }
    }
}

/// Drops a thread's parked entry when [`Clock::wait_until`] returns, a panic
/// in `wait` included.
struct Leave<'a> {
    clock: &'a Manual,
    id: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        crate::lock(&self.clock.state)
            .parked
            .retain(|parked| parked.id != self.id);
    }
}

fn now_in(state: &State, origin: Instant) -> Instant {
    origin.checked_add(state.offset).unwrap_or(origin)
}

fn matching_deadline(state: &State, origin: Instant, d: Duration) -> Option<Instant> {
    let until = now_in(state, origin).checked_add(d)?;
    state
        .parked
        .iter()
        .any(|parked| parked.until == Some(until))
        .then_some(until)
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

/// The offset after `advance`, or why [`Manual::advance_when_parked`] must
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
    select: Select,
) -> Option<Observation> {
    let selected = |id: ThreadId| match select {
        Select::Thread(named) => named == id,
        Select::Count(_) => true,
    };
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

/// A selected fresh park matches; a counted wait matches exactly `n`
/// distinct fresh threads at the expected instant, even when one of those
/// threads holds nested parks. An unnamed wait is a count of one.
fn matched(state: &State, expected: Option<Instant>, select: Select) -> bool {
    let mut held = state.parked.iter().filter(|parked| fresh(state, parked));
    match select {
        Select::Thread(selected) => {
            held.any(|parked| parked.thread == selected && parked.until == expected)
        }
        Select::Count(n) => distinct_fresh_threads(state, Some(expected)).len() == n,
    }
}

/// Distinct threads holding a fresh park: at `until` when one filters,
/// at any instant otherwise. Nested parks on one thread count once;
/// diagnostics still list every held park.
fn distinct_fresh_threads(state: &State, until: Option<Option<Instant>>) -> Vec<ThreadId> {
    let mut seen: Vec<ThreadId> = Vec::new();
    for parked in state
        .parked
        .iter()
        .filter(|parked| fresh(state, parked) && until.is_none_or(|until| parked.until == until))
    {
        if !seen.contains(&parked.thread) {
            seen.push(parked.thread);
        }
    }
    seen
}

/// Whether fresh parks belong to more distinct threads than the wait allows:
/// more than one for a named or unnamed wait, more than `n` for a counted
/// one. Nested parks on one thread count as one; diagnostics still list
/// every held park.
fn several_threads_parked(state: &State, select: Select) -> bool {
    let n = match select {
        Select::Thread(_) => return false,
        Select::Count(n) => n,
    };
    distinct_fresh_threads(state, None).len() > n
}

/// Which message a `Select` contributes to an `advance_when_parked` failure.
enum SelectOutcome {
    /// More parked threads than the wait allows.
    TooMany,
    /// The deadline passed before the awaited parks arrived.
    Expired,
    /// The report's thread-count suffix (empty for a single awaited park).
    CountSuffix,
}

/// The message for a `Select` that failed with `outcome`: a counted wait
/// names its count, any other wait names the single awaited park.
fn select_message(select: Select, outcome: SelectOutcome) -> String {
    match select {
        Select::Count(n) if n != 1 => match outcome {
            SelectOutcome::TooMany => format!("more than {n} threads hold a park"),
            SelectOutcome::Expired => {
                format!("the deadline passed before {n} threads parked at the named instant")
            }
            SelectOutcome::CountSuffix => format!(", threads {n}"),
        },
        Select::Thread(_) | Select::Count(_) => match outcome {
            SelectOutcome::TooMany => {
                "several threads hold a park and no thread was named: name the thread".to_string()
            }
            SelectOutcome::Expired => {
                "the deadline passed before the named park arrived".to_string()
            }
            SelectOutcome::CountSuffix => String::new(),
        },
    }
}

/// Closes an [`Manual::advance_when_parked`] wait: the observation log
/// never outlives the last wait it belongs to.
fn leave_wait(state: &mut State) {
    state.watching = false;
    state.observed.clear();
}

/// The caller diagnostic for an [`Manual::advance_when_parked`] failure:
/// the failure kind, the expected instant, the named thread when one was
/// named, the requested advance, the watched park that failed the wait when
/// one did, and every thread parked now with its instant (`None` for an
/// unbounded park, `[]` when nobody is parked).
fn report(
    state: &State,
    expected: Option<Instant>,
    select: Select,
    advance: Duration,
    kind: &str,
    found: Option<(ThreadId, Option<Instant>)>,
) -> String {
    let thread = match select {
        Select::Thread(id) => Some(id),
        Select::Count(_) => None,
    };
    let mut message = format!(
        "advance_when_parked failed: {kind}; expected park {expected:?}, thread {thread:?}"
    );
    message.push_str(&select_message(select, SelectOutcome::CountSuffix));
    message.push_str(&format!(", advance {advance:?}"));
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

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
