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
pub(crate) fn await_grace_or_answer<T>(
    clock: &fakes::clock::FakeClock,
    done: &mpsc::Receiver<T>,
    kill_at: Instant,
) -> Option<T> {
    for _ in 0..ROUNDS {
        if clock.await_parked(kill_at, SIGHT) {
            return None;
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
