//! The stall tests' shared wait after the deadline (`docs/testing.md`,
//! "Fakes"): the run either parks for the SIGTERM grace or answers at
//! once, whichever comes first.

use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One sighting round's wait for the grace park or the run's answer.
const SIGHT: Duration = Duration::from_millis(200);

/// Rounds of waiting for the grace park or the answer: 8 rounds of one
/// bounded wait are about 2 s of wall clock, the hang guard for a run that
/// neither parks nor answers.
const ROUNDS: u32 = 8;

/// After a seen grace park, advances past it — or takes the run's answer
/// when the run already left the park and answered instead.
///
/// `await_grace_or_answer` returns `None` for a held grace park, and the
/// caller advances past the grace only then. The run can leave the park
/// and answer between that sighting and the advance: a blind
/// `advance_when_parked` then waits out its bound for a park that never
/// comes back. This step advances only while the park is still held and
/// otherwise returns the arrived answer, as the sighting already does
/// before the park.
///
/// Returns the early answer when the run finished first (before or after
/// the sighting), `None` when it advanced past a still-held grace park.
/// The caller reads the final answer either way, asserting the same
/// deadline error. Panics when the run neither parks nor answers.
#[track_caller]
pub(crate) fn advance_past_grace_or_answer<T>(
    clock: &fakes::clock::FakeClock,
    done: &mpsc::Receiver<T>,
    kill_at: Instant,
    advance: Duration,
    answered: Option<T>,
) -> Option<T> {
    if answered.is_some() {
        return answered;
    }
    for _ in 0..ROUNDS {
        match clock.try_advance_when_parked(Some(kill_at), None, advance, SIGHT) {
            Ok(_) => return None,
            Err(fakes::clock::Miss::NoPark) => match done.try_recv() {
                Ok(answer) => return Some(answer),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            },
            Err(miss) => match done.try_recv() {
                // A park at another instant fails the advance: an answer
                // that arrived instead still ends the wait, as above.
                Ok(answer) => return Some(answer),
                Err(mpsc::TryRecvError::Empty) => panic!("{miss}"),
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            },
        }
    }
    panic!("the stalled run neither parks for the grace nor answers");
}

/// After the deadline advance, waits for the grace park at `kill_at` or the
/// run's answer on `done`, whichever comes first.
///
/// The old tests assumed the runner ALWAYS parks for the SIGTERM grace
/// after the deadline advance. When the fixture dies on SIGTERM (or the
/// child is reaped before the supervisor re-parks), the run finishes with
/// no grace park and a blind `await_parked(kill_at, WITHIN)` waits the full
/// bound for a park that never comes. An answer meanwhile ends this at
/// once instead of burning the wait.
///
/// Returns the early answer when the run finished first, `None` when it
/// parked for the grace. The caller advances past the grace only on `None`
/// and reads the final answer either way, asserting the same deadline
/// error. Panics when the run neither parks nor answers.
#[track_caller]
pub(crate) fn await_grace_or_answer<T>(
    clock: &fakes::clock::FakeClock,
    done: &mpsc::Receiver<T>,
    kill_at: Instant,
) -> Option<T> {
    for _ in 0..ROUNDS {
        if clock.await_parked(kill_at, fakes::Deadline::after(SIGHT)) {
            return match done.try_recv() {
                Ok(done) => Some(done),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("the stalled run returns")
                }
            };
        }
        match done.try_recv() {
            Ok(done) => return Some(done),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("the stalled run returns")
            }
        }
    }
    panic!("the stalled run neither parks for the grace nor answers");
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test code"
    )]

    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use contract::clock::Clock as _;
    use fakes::Deadline;
    use fakes::clock::FakeClock;

    use super::{advance_past_grace_or_answer, await_grace_or_answer};

    const TEST_GUARD: Duration = Duration::from_secs(4);

    #[track_caller]
    fn park(clock: &Arc<FakeClock>, until: Instant) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (release_tx, release_rx) = mpsc::channel();
        let (exited_tx, exited_rx) = mpsc::channel();
        let parked_clock = Arc::clone(clock);
        thread::spawn(move || {
            parked_clock.wait_until(Some(until), &mut |_bound| {
                Deadline::after(TEST_GUARD)
                    .recv(&release_rx)
                    .expect("waited for the test to release the parked thread");
            });
            let _sent = exited_tx.send(());
        });
        assert!(
            clock.await_parked(until, fakes::Deadline::after(TEST_GUARD)),
            "waited {TEST_GUARD:?} for the thread to park at the grace"
        );
        (release_tx, exited_rx)
    }

    #[track_caller]
    fn release(release_tx: mpsc::Sender<()>, exited_rx: mpsc::Receiver<()>, wait: &Deadline) {
        release_tx
            .send(())
            .expect("the parked thread is waiting for release");
        wait.recv(&exited_rx)
            .expect("waited for the parked thread to exit");
    }

    #[test]
    fn answer_before_the_park_returns_the_answer() {
        let clock = FakeClock::new();
        let until = clock.origin() + Duration::from_secs(30);
        let (answer_tx, answer_rx) = mpsc::channel();
        answer_tx.send("answer").expect("the answer is queued");

        assert_eq!(
            await_grace_or_answer(&clock, &answer_rx, until),
            Some("answer")
        );
    }

    #[test]
    fn park_before_the_answer_returns_the_park() {
        let clock = FakeClock::new();
        let until = clock.origin() + Duration::from_secs(30);
        let (release_tx, exited_rx) = park(&clock, until);
        let (_answer_tx, answer_rx) = mpsc::channel::<()>();

        let outcome = await_grace_or_answer(&clock, &answer_rx, until);
        release(release_tx, exited_rx, &Deadline::after(TEST_GUARD));

        assert_eq!(outcome, None);
    }

    #[test]
    fn answer_wins_when_park_and_answer_are_both_present() {
        let clock = FakeClock::new();
        let until = clock.origin() + Duration::from_secs(30);
        let (release_tx, exited_rx) = park(&clock, until);
        let (answer_tx, answer_rx) = mpsc::channel();
        answer_tx.send("answer").expect("the answer is queued");

        let outcome = await_grace_or_answer(&clock, &answer_rx, until);
        release(release_tx, exited_rx, &Deadline::after(TEST_GUARD));

        assert_eq!(outcome, Some("answer"));
    }

    #[track_caller]
    fn run_that_answers_after_release(
        clock: &Arc<FakeClock>,
        kill_at: Instant,
    ) -> (
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
        mpsc::Receiver<&'static str>,
    ) {
        let (answer_tx, answer_rx) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (left_tx, left_rx) = mpsc::channel::<()>();
        let worker = Arc::clone(clock);
        thread::spawn(move || {
            worker.wait_until(Some(kill_at), &mut |_bound| {
                Deadline::after(TEST_GUARD)
                    .recv(&go_rx)
                    .expect("waited for the test to release the parked run");
            });
            // The park entry drops before the answer is sent: the run
            // already left the grace park when the post-park step runs.
            answer_tx
                .send("answer")
                .expect("the test waits for the answer");
            left_tx
                .send(())
                .expect("the test waits for the run to leave");
        });
        (go_tx, left_rx, answer_rx)
    }

    #[test]
    fn answer_after_the_sighting_returns_the_answer_without_advancing() {
        let clock = Arc::new(FakeClock::new());
        let kill_at = clock.now() + Duration::from_secs(30);
        let advance = Duration::from_secs(5);
        let (go_tx, left_rx, answer_rx) = run_that_answers_after_release(&clock, kill_at);
        // The sighting sees the held grace park and no answer yet.
        assert_eq!(await_grace_or_answer(&clock, &answer_rx, kill_at), None);
        go_tx
            .send(())
            .expect("the run is parked waiting for release");
        Deadline::after(TEST_GUARD)
            .recv(&left_rx)
            .expect("waited for the run to leave the park and answer");
        let before = clock.now();
        let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
        assert_eq!(outcome, Some("answer"));
        assert_eq!(clock.now(), before, "the answer arrives with no clock move");
    }

    #[test]
    fn held_park_advances_and_returns_no_answer() {
        let clock = Arc::new(FakeClock::new());
        let kill_at = clock.now() + Duration::from_secs(30);
        let advance = Duration::from_secs(5);
        let (release_tx, exited_rx) = park(&clock, kill_at);
        let (_answer_tx, answer_rx) = mpsc::channel::<()>();

        let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
        release(release_tx, exited_rx, &Deadline::after(TEST_GUARD));

        assert_eq!(outcome, None);
        assert_eq!(
            clock.now(),
            clock.origin() + advance,
            "the held park moves the clock past the grace"
        );
    }

    #[test]
    fn sighted_answer_returns_without_advancing() {
        let clock = FakeClock::new();
        let kill_at = clock.origin() + Duration::from_secs(30);
        let advance = Duration::from_secs(5);
        let before = clock.now();
        let (_answer_tx, answer_rx) = mpsc::channel::<&str>();

        let outcome =
            advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, Some("answer"));

        assert_eq!(outcome, Some("answer"));
        assert_eq!(clock.now(), before, "the sighted answer moves no clock");
    }

    #[test]
    fn wrong_park_with_an_answer_returns_the_answer() {
        let clock = Arc::new(FakeClock::new());
        let kill_at = clock.now() + Duration::from_secs(30);
        let elsewhere = clock.now() + Duration::from_secs(10);
        let advance = Duration::from_secs(5);
        let (release_tx, exited_rx) = park(&clock, elsewhere);
        let (answer_tx, answer_rx) = mpsc::channel();
        answer_tx.send("answer").expect("the answer is queued");

        let outcome = advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
        release(release_tx, exited_rx, &Deadline::after(TEST_GUARD));

        assert_eq!(outcome, Some("answer"));
    }

    #[test]
    #[should_panic(expected = "watched park")]
    fn wrong_park_without_an_answer_panics() {
        let clock = Arc::new(FakeClock::new());
        let kill_at = clock.now() + Duration::from_secs(30);
        let elsewhere = clock.now() + Duration::from_secs(10);
        let advance = Duration::from_secs(5);
        let (release_tx, _exited_rx) = park(&clock, elsewhere);
        // The panic unwinds past this test: release the parked thread on
        // the way out so it never waits out its bound.
        struct ReleaseOnDrop(Option<mpsc::Sender<()>>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                if let Some(release) = self.0.take() {
                    let _sent = release.send(());
                }
            }
        }
        let _release = ReleaseOnDrop(Some(release_tx));
        let (_answer_tx, answer_rx) = mpsc::channel::<()>();

        advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    }

    #[test]
    #[should_panic(expected = "the stalled run neither parks for the grace nor answers")]
    fn post_park_neither_park_nor_answer_hits_the_hang_guard() {
        let clock = FakeClock::new();
        let kill_at = clock.origin() + Duration::from_secs(30);
        let advance = Duration::from_secs(5);
        let (_answer_tx, answer_rx) = mpsc::channel::<()>();

        advance_past_grace_or_answer(&clock, &answer_rx, kill_at, advance, None);
    }

    #[test]
    #[should_panic(expected = "the stalled run neither parks for the grace nor answers")]
    fn neither_park_nor_answer_hits_the_hang_guard() {
        let clock = FakeClock::new();
        let until = clock.origin() + Duration::from_secs(30);
        let (_answer_tx, answer_rx) = mpsc::channel::<()>();

        await_grace_or_answer(&clock, &answer_rx, until);
    }
}
