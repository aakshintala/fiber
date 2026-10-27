//! Tests of the core's waits through its own seam: the condvar wait
//! sleeps its bound with no wake, the readiness wait ends early on
//! readable bytes and sleeps otherwise, and a consumed wake never
//! spins the next park.

use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;

use super::super::{CheckGate, Limits, RequestBudget, WaitAck, WaitEnd};
use super::*;
use crate::clock::FakeClock;
use crate::deadline::Deadline;

/// How long one test operation may take on the wall clock.
const OP: Duration = Duration::from_secs(10);

fn fake_clock() -> Arc<FakeClock> {
    FakeClock::new()
}

fn bind(clock: &Arc<FakeClock>) -> HttpServer {
    HttpServer::bind(clock.clone(), Limits::default()).expect("the core binds")
}

fn budget(clock: &Arc<FakeClock>) -> RequestBudget {
    RequestBudget::start_on(
        &(clock.clone() as Arc<dyn Clock>),
        Duration::from_secs(10),
        Duration::from_secs(45),
    )
}

/// The next clock-wait acknowledgment.
fn next_acknowledgment(deadline: &Deadline, rx: &mpsc::Receiver<WaitAck>, what: &str) -> WaitAck {
    deadline.recv_or_fail(rx, what)
}

/// A condvar wait with no wake sleeps its bound and counts it: the
/// waiter parks with a millisecond poll and nothing advances the clock
/// or stops the server, so the bound passes at once. The wait runs on
/// a thread under the operation deadline, so a broken bound fails the
/// test instead of hanging it.
#[test]
fn a_condvar_wait_with_no_wake_sleeps_its_bound() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            poll: Duration::from_millis(1),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let budget = budget(&clock);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            let end = server.shared.wait(
                &budget,
                "the test request",
                "to wait out the test",
                &mut seq,
                &mut acked,
            );
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the bounded wait"),
                WaitEnd::Waited
            ),
            "no wake ends the wait at its bound"
        );
    });
    assert_eq!(server.test_wait_entries(), 1, "the wait parks once");
    assert_eq!(server.test_slept_waits(), 1, "the bound's sleep is counted");
    assert!(server.shutdown(), "shutdown stops the server");
}

/// A readiness wait with nothing readable sleeps its bound and counts
/// it: the waiter polls an unwritten stream with a millisecond poll,
/// so the bound passes at once with no advance and no shutdown. The
/// wait runs on a thread under the operation deadline, so a broken
/// bound fails the test instead of hanging it.
#[test]
fn a_readiness_wait_with_nothing_readable_sleeps_its_bound() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            poll: Duration::from_millis(1),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    // Held open and never written: the read end is never readable, so
    // the park always reaches its bound.
    let (unwritten, _held) = UnixStream::pair().expect("the test pipes its stream");
    let budget = budget(&clock);
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            let end = server.shared.wait_ready(
                &budget,
                "the test request",
                "to read the test bytes",
                &mut seq,
                &mut acked,
                Some(unwritten.as_fd()),
                true,
            );
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the bounded wait"),
                WaitEnd::Waited
            ),
            "nothing readable ends the wait at its bound"
        );
    });
    assert_eq!(server.test_wait_entries(), 1, "the wait parks once");
    assert_eq!(server.test_slept_waits(), 1, "the bound's sleep is counted");
    assert!(server.shutdown(), "shutdown stops the server");
}

/// A consumed clock wake does not spin the next park: the first park is
/// held after its check until the clock advances, so it ends on the pipe
/// byte and spends it. The first result rendezvous lets the controller
/// permit the next park only after it has observed that wake. That park has
/// no readable bytes or stale wake and sleeps its millisecond bound. A drain
/// that spent nothing would end the second park at once on the stale byte
/// instead, counting no sleep.
#[test]
fn a_readiness_wait_after_a_wake_parks_instead_of_spinning() {
    let clock = fake_clock();
    let server = bind(&clock);
    // Held open and never written: the read end is never readable, so
    // only a stale pipe byte may end a park early.
    let (unwritten, _held) = UnixStream::pair().expect("the test pipes its stream");
    let budget = budget(&clock);
    let acks = server.test_acknowledgments();
    let gate = server.test_gate();
    gate.arm();
    let (continue_tx, continue_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        let _release = ReleaseOnDrop {
            gate: Arc::clone(&gate),
        };
        let wait_server = &server;
        let wait_budget = &budget;
        let wait_stream = &unwritten;
        scope.spawn(move || {
            let mut seq = 0;
            let mut acked = None;
            for park in 0..2 {
                let end = wait_server.shared.wait_ready(
                    wait_budget,
                    "the test request",
                    "to read the test bytes",
                    &mut seq,
                    &mut acked,
                    Some(wait_stream.as_fd()),
                    true,
                );
                match tx.send(end) {
                    Ok(()) | Err(_) => {}
                }
                if park == 0 {
                    Deadline::after(OP)
                        .recv_or_fail(&continue_rx, "the controller before the next park");
                }
            }
        });
        let ack = next_acknowledgment(
            &Deadline::after(OP),
            &acks,
            "the wake-consuming readiness park",
        );
        assert_eq!(ack.phase, "to read the test bytes");
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(1));
        gate.release();
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the woken wait"),
                WaitEnd::Waited
            ),
            "the wake ends the first park at once"
        );
        continue_tx
            .send(())
            .expect("the waiter is ready for the next park");
        let next = next_acknowledgment(&Deadline::after(OP), &acks, "the next readiness park");
        assert_eq!(next.phase, "to read the test bytes");
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the next wait"),
                WaitEnd::Waited
            ),
            "the next park ends at its bound"
        );
    });
    assert_eq!(server.test_wait_entries(), 2, "the waiter parks twice");
    assert_eq!(
        server.test_slept_waits(),
        1,
        "only the park past the consumed wake sleeps its bound"
    );
    assert_eq!(
        server.test_readied_waits(),
        0,
        "a wake with no bytes readies nothing"
    );
    assert!(server.shutdown(), "shutdown stops the server");
}

/// Releases an armed check-to-park pause when the test unwinds past
/// it: a waiter held between its check and its park would otherwise
/// outlive the failed test and hang the scope's join, turning a red
/// assertion into a hung suite.
struct ReleaseOnDrop {
    gate: Arc<CheckGate>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// A woken condvar wait counts no sleep: the waiter parks on a
/// minute poll, one fake second ends it well short of its
/// ten-second bound, so the slept counter stays at zero. A park that
/// counted woken waits would read one instead.
#[test]
fn a_woken_condvar_wait_counts_no_sleep() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            // A long poll: only the clock's wake may end the park, so
            // a lost wake sleeps past the test's bound instead of
            // passing on a millisecond poll.
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let budget = budget(&clock);
    let acks = server.test_acknowledgments();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            let end = server.shared.wait(
                &budget,
                "the test request",
                "to wait out the test",
                &mut seq,
                &mut acked,
            );
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the condvar wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the wait parks before the clock moves"
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(1));
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the woken wait"),
                WaitEnd::Waited
            ),
            "the wake ends the wait at once"
        );
    });
    assert_eq!(server.test_wait_entries(), 1, "the wait parks once");
    assert_eq!(server.test_slept_waits(), 0, "a woken wait sleeps nothing");
    assert!(server.shutdown(), "shutdown stops the server");
}

/// A condvar wait ended by shutdown counts no sleep: the waiter parks
/// on a minute poll, shutdown ends it with no clock move, so the
/// slept counter stays at zero. A park that counted stopped waits
/// would read one instead.
#[test]
fn a_shutdown_ended_condvar_wait_counts_no_sleep() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            // A long poll: only shutdown may end the park, so a park
            // that slept its bound would outlive the test's deadline.
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let budget = budget(&clock);
    let acks = server.test_acknowledgments();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            let end = server.shared.wait(
                &budget,
                "the test request",
                "to wait out the test",
                &mut seq,
                &mut acked,
            );
            match tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the condvar wait");
        assert!(
            clock.await_thread_parked(ack.thread, ack.until, Deadline::after(OP)),
            "the wait parks before shutdown lands"
        );
        assert!(server.shutdown(), "shutdown ends the parked wait");
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&rx, "the stopped wait"),
                WaitEnd::Waited
            ),
            "shutdown ends the wait at once"
        );
    });
    assert_eq!(
        server.test_slept_waits(),
        0,
        "a stopped wait sleeps nothing"
    );
}

/// A consumed wake does not end the next readiness wait: the advance
/// lands while the waiter is held between its check and its park, so
/// its first park finds the pipe byte already waiting, ends at once
/// and spends it; the next park with nothing readable sleeps its
/// bound and counts one sleep. A drain that spent nothing would end
/// the next park at once on the stale byte instead, counting nothing.
#[test]
fn a_consumed_wake_does_not_end_the_next_readiness_wait() {
    let clock = fake_clock();
    let server = bind(&clock);
    // Held open and never written: the read end is never readable, so
    // only a stale pipe byte may end a park early.
    let (unwritten, _held) = UnixStream::pair().expect("the test pipes its stream");
    let budget = budget(&clock);
    let acks = server.test_acknowledgments();
    let gate = server.test_gate();
    gate.arm();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        // Dropped when an assertion below fails, before the scope joins
        // the held waiter: the release lets the join end, so a red
        // assertion fails the test instead of hanging it.
        let _release = ReleaseOnDrop {
            gate: Arc::clone(&gate),
        };
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            for _ in 0..2 {
                let end = server.shared.wait_ready(
                    &budget,
                    "the test request",
                    "to read the test bytes",
                    &mut seq,
                    &mut acked,
                    Some(unwritten.as_fd()),
                    true,
                );
                match tx.send(end) {
                    Ok(()) | Err(_) => {}
                }
            }
        });
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the readiness wait");
        assert_eq!(ack.phase, "to read the test bytes");
        #[allow(
            clippy::disallowed_methods,
            reason = "fakes-internal advance inside the crates/fakes path exemption (ruling 6246218049)"
        )]
        clock.advance(Duration::from_secs(1));
        gate.release();
        for _ in 0..2 {
            assert!(
                matches!(
                    Deadline::after(OP).recv_or_fail(&rx, "each readiness wait"),
                    WaitEnd::Waited
                ),
                "each park ends"
            );
        }
    });
    assert_eq!(server.test_wait_entries(), 2, "the waiter parks twice");
    assert_eq!(
        server.test_slept_waits(),
        1,
        "only the park past the consumed wake sleeps its bound"
    );
    assert_eq!(
        server.test_readied_waits(),
        0,
        "a wake with no bytes readies nothing"
    );
    assert!(server.shutdown(), "shutdown stops the server");
}

/// Shutdown reports an accept thread held past its bound: the gate
/// holds the accept wait between its check and its park, shutdown
/// waits out its bound and returns false, and releasing the gate lets
/// the stopped accept end. A shutdown that reported success regardless
/// would return true instead.
#[test]
fn shutdown_reports_an_accept_thread_held_past_its_bound() {
    let clock = fake_clock();
    let server = bind(&clock);
    let acks = server.test_acknowledgments();
    let gate = server.test_gate();
    gate.arm();
    server.serve_persistent(|_conn| {});
    thread::scope(|_scope| {
        // Dropped when an assertion below fails, before the scope ends:
        // the held accept would otherwise outlive the failed test and
        // hang the suite's join, so a red assertion fails the test
        // instead of hanging it.
        let _release = ReleaseOnDrop {
            gate: Arc::clone(&gate),
        };
        let deadline = Deadline::after(OP);
        let ack = next_acknowledgment(&deadline, &acks, "the accept's clock wait");
        assert_eq!(ack.phase, "to accept the connection");
        assert!(
            !server.shutdown(),
            "shutdown reports the held accept instead of hanging"
        );
        gate.release();
        assert!(server.shutdown(), "shutdown reaps the released accept");
    });
}

/// Releases a held park-gap waiter when the test unwinds past it: a
/// waiter held between its park becoming visible and its condvar wait
/// would otherwise outlive the failed test and hang the scope's join,
/// turning a red assertion into a hung suite.
struct ReleaseGapOnDrop {
    release: mpsc::Sender<()>,
}

impl Drop for ReleaseGapOnDrop {
    fn drop(&mut self) {
        match self.release.send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// Shutdown ends a condvar wait held between its park becoming visible
/// and its condvar wait: the waiter signals from the hold with the wake
/// mutex held, shutdown signals before that mutex and blocks on it, and
/// the release lets the notify land after the waiter parks. A shutdown
/// that notified without the mutex would miss the held waiter, which
/// would then sleep its guard instead of ending within the operation
/// bound.
#[test]
fn a_shutdown_in_the_park_gap_ends_the_wait() {
    let clock = fake_clock();
    let server = HttpServer::bind(
        clock.clone() as Arc<dyn Clock>,
        Limits {
            request: Duration::from_secs(10),
            // A long poll: only shutdown may end the park, so a missed
            // notify sleeps the guard instead of passing on a
            // millisecond poll.
            poll: Duration::from_secs(60),
            ..Limits::default()
        },
    )
    .expect("the core binds");
    let budget = budget(&clock);
    let (wait_tx, wait_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    thread::scope(|scope| {
        let (reached_rx, release_tx) = server.test_park_gap();
        // Dropped when an assertion below fails, before the scope joins
        // the held waiter: the release lets the join end, so a red
        // assertion fails the test instead of hanging it.
        let _release_gap = ReleaseGapOnDrop {
            release: release_tx.clone(),
        };
        let wake_rx = server.test_shutdown_before_wake_lock();
        scope.spawn(|| {
            let mut seq = 0;
            let mut acked = None;
            let end = server.shared.wait(
                &budget,
                "the test request",
                "to wait out the test",
                &mut seq,
                &mut acked,
            );
            match wait_tx.send(end) {
                Ok(()) | Err(_) => {}
            }
        });
        Deadline::after(OP).recv_or_fail(&reached_rx, "the waiter in the park gap");
        scope.spawn(|| {
            let joined = server.shutdown();
            match stop_tx.send(joined) {
                Ok(()) | Err(_) => {}
            }
        });
        // Shutdown reached the wake mutex it blocks on while the waiter
        // is held, so releasing the gap lets its notify land after the
        // waiter parks.
        Deadline::after(OP).recv_or_fail(&wake_rx, "shutdown before the wake mutex");
        release_tx
            .send(())
            .expect("the waiter is held in the park gap");
        assert!(
            Deadline::after(OP).recv_or_fail(&stop_rx, "the stopped shutdown"),
            "shutdown stops the server"
        );
        assert!(
            matches!(
                Deadline::after(OP).recv_or_fail(&wait_rx, "the stopped wait"),
                WaitEnd::Waited
            ),
            "shutdown ends the wait at once"
        );
    });
    assert_eq!(server.test_wait_entries(), 1, "the wait parks once");
    assert_eq!(
        server.test_slept_waits(),
        0,
        "a stopped wait sleeps nothing"
    );
}
