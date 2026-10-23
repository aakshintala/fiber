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
/// the sighting), `None` when it advanced past a held park (the grace
/// park, or the later drain park the run re-parked at).
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
    use contract::clock::Clock as _;
    use support::group::DRAIN;

    if answered.is_some() {
        return answered;
    }
    let mut waited = kill_at;
    let mut step = advance;
    // Rounds of waiting with no park to show for them: the follow below
    // retries at once and takes no round of its own, so a mismatch on the
    // final round still follows instead of falling out to the hang guard.
    let mut rounds = 0;
    while rounds < ROUNDS {
        match clock.try_advance_when_parked(Some(waited), None, step, SIGHT) {
            Ok(_) => return None,
            Err(miss) => match done.try_recv() {
                // A park at another instant fails the advance, while no
                // park at all just waits for the next round: an answer
                // that arrived instead still ends the wait either way.
                Ok(answer) => return Some(answer),
                Err(mpsc::TryRecvError::Empty) => match miss {
                    // The run left the grace park and re-parked later, at
                    // the drain bound it parks at after the stop: follow
                    // the later park at once instead of failing the look
                    // for a park that never comes back. A park beyond
                    // that bound is a grace regression and still panics.
                    // `waited` only moves on a follow, so it still
                    // naming `kill_at` means no follow has happened yet:
                    // the follow stays once-only.
                    fakes::clock::Miss::Mismatch {
                        watched: (_, Some(w)),
                        ..
                    } if waited == kill_at && w > waited && w <= kill_at + DRAIN => {
                        waited = w;
                        step = w.saturating_duration_since(clock.now());
                        continue;
                    }
                    fakes::clock::Miss::Mismatch { .. } => {
                        panic!("{miss}")
                    }
                    fakes::clock::Miss::NoPark => {
                        rounds += 1;
                    }
                },
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
    await_grace_or_answer_within(clock, done, kill_at, ROUNDS, SIGHT)
}

/// Waits for the grace park or the run's answer with the given bound:
/// `rounds` rounds of one bounded `sight` wait each. Panics when the run
/// neither parks nor answers.
#[track_caller]
fn await_grace_or_answer_within<T>(
    clock: &fakes::clock::FakeClock,
    done: &mpsc::Receiver<T>,
    kill_at: Instant,
    rounds: u32,
    sight: Duration,
) -> Option<T> {
    for _ in 0..rounds {
        if clock.await_parked(kill_at, fakes::Deadline::after(sight)) {
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
#[path = "stall_tests.rs"]
mod tests;
