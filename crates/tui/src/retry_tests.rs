//! Tests for the wait between connection attempts, on a fake clock.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::Retry;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// The first delay the loop gives.
const HALF: Duration = Duration::from_millis(500);

/// A fake clock and a permit woken by it.
fn permit() -> (Arc<FakeClock>, Arc<Retry>) {
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    let retry = Retry::new(&shared);
    (clock, retry)
}

/// Runs `retry.wait` on its own thread: its result arrives on the receiver.
fn waiting(clock: &Arc<FakeClock>, retry: &Arc<Retry>) -> Receiver<bool> {
    let clock = Arc::clone(clock);
    let retry = Arc::clone(retry);
    let (done, result) = mpsc::channel();
    std::thread::Builder::new()
        .name("retry-wait".to_owned())
        .spawn(move || done.send(retry.wait(clock.as_ref())).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    result
}

/// The wait's result, within [`DEADLINE`].
fn result(rx: &Receiver<bool>) -> bool {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the wait to return: {err}"))
}

/// Waits for the thread to park at `until`, within [`DEADLINE`].
fn parked(clock: &FakeClock, until: std::time::Instant) {
    assert!(
        clock.await_parked(until, DEADLINE),
        "waited {DEADLINE:?} for the wait to park on the clock"
    );
}

#[test]
fn wait_returns_after_the_delay_on_the_fake_clock() {
    let (clock, retry) = permit();
    let rx = waiting(&clock, &retry);
    retry.give(HALF);
    parked(&clock, clock.origin() + HALF);
    clock.advance(HALF);
    assert!(result(&rx));
    assert_eq!(retry.held(), None);
}

#[test]
fn an_advance_short_of_the_delay_keeps_waiting() {
    let (clock, retry) = permit();
    let rx = waiting(&clock, &retry);
    retry.give(HALF);
    let until = clock.origin() + HALF;
    parked(&clock, until);
    let mark = clock.advance_marked(Duration::from_millis(499));
    // Parked again at the same deadline: 499 ms is short of it, and the
    // thread has checked the clock since the advance.
    assert!(
        clock.await_parked_since(&mark, Some(until), DEADLINE),
        "waited {DEADLINE:?} for the wait to park again"
    );
    assert!(rx.try_recv().is_err());
    clock.advance(Duration::from_millis(1));
    assert!(result(&rx));
}

#[test]
fn a_delay_given_before_the_wait_is_used() {
    let (clock, retry) = permit();
    retry.give(HALF);
    let rx = waiting(&clock, &retry);
    parked(&clock, clock.origin() + HALF);
    clock.advance(HALF);
    assert!(result(&rx));
}

#[test]
fn quit_while_waiting_for_a_delay_returns_false() {
    let (clock, retry) = permit();
    let rx = waiting(&clock, &retry);
    retry.quit();
    assert!(!result(&rx));
    assert!(clock.parked().is_empty());
    // Quit is sticky: a later wait returns at once.
    assert!(!result(&waiting(&clock, &retry)));
}

#[test]
fn quit_during_the_timed_wait_returns_false_without_advancing() {
    let (clock, retry) = permit();
    let rx = waiting(&clock, &retry);
    retry.give(HALF);
    parked(&clock, clock.origin() + HALF);
    retry.quit();
    assert!(!result(&rx));
    assert_eq!(clock.now(), clock.origin());
}

/// A stop whose reader is already gone: only the permit's bookkeeping
/// is under test.
fn stop() -> support::stoppable::Stop {
    let (ours, _theirs) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (_read, stop) =
        support::stoppable::reader(ours).unwrap_or_else(|err| panic!("reader: {err}"));
    stop
}

#[test]
fn track_succeeds_until_quit_then_refuses() {
    let (_clock, retry) = permit();
    let (ours, _theirs) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    assert!(
        retry
            .track(&ours, stop())
            .unwrap_or_else(|err| panic!("track: {err}"))
    );
    retry.untrack();
    assert!(retry.lock().watched.is_none());
    retry.quit();
    assert!(
        !retry
            .track(&ours, stop())
            .unwrap_or_else(|err| panic!("track: {err}"))
    );
}

#[test]
fn track_reports_the_clone_failure_and_watches_nothing() {
    let (clock, retry) = permit();
    // Inject the clone failure: exhausting descriptors would depend on
    // the fd limit, which exceeds the loop bound on some hosts.
    let error = match retry.track_with(|| Err(std::io::Error::other("clone failed")), stop()) {
        Err(error) => error,
        Ok(_) => panic!("track reports the clone failure"),
    };
    assert!(!error.to_string().is_empty());
    assert!(retry.lock().watched.is_none());
    // The failure wedges nothing: quit still ends a wait, within DEADLINE.
    let rx = waiting(&clock, &retry);
    retry.quit();
    assert!(!result(&rx));
}

#[test]
fn end_read_stops_the_watched_read_and_leaves_the_permit_open() {
    let (_clock, retry) = permit();
    let (ours, _theirs) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (mut read, stop) = support::stoppable::reader(
        ours.try_clone()
            .unwrap_or_else(|err| panic!("clone: {err}")),
    )
    .unwrap_or_else(|err| panic!("reader: {err}"));
    assert!(
        retry
            .track(&ours, stop)
            .unwrap_or_else(|err| panic!("track: {err}"))
    );
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("retry-end-read".to_owned())
        .spawn(move || {
            let mut buf = [0u8; 1];
            done.send(std::io::Read::read(&mut read, &mut buf).is_err())
                .unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    retry.end_read();
    assert!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the stopped read: {err}")),
        "end_read stops the watched read"
    );
    assert!(retry.lock().watched.is_none());
    assert!(!retry.lock().quit, "the permit stays open");
}

#[test]
fn quit_ends_the_watched_read_and_shuts_the_stream() {
    let (_clock, retry) = permit();
    let (ours, theirs) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (mut read, stop) = support::stoppable::reader(
        ours.try_clone()
            .unwrap_or_else(|err| panic!("clone: {err}")),
    )
    .unwrap_or_else(|err| panic!("reader: {err}"));
    assert!(
        retry
            .track(&ours, stop)
            .unwrap_or_else(|err| panic!("track: {err}"))
    );
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("retry-stop".to_owned())
        .spawn(move || {
            let mut buf = [0u8; 1];
            done.send(std::io::Read::read(&mut read, &mut buf).is_err())
                .unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    retry.quit();
    assert!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the stopped read: {err}")),
        "quit stops the watched read"
    );
    // The shutdown already ran, so the peer's read sees the end at once:
    // nonblocking, a missing shutdown fails as `WouldBlock`, with no wait.
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut buf = [0u8; 1];
    assert_eq!(
        std::io::Read::read(&mut &theirs, &mut buf).ok(),
        Some(0),
        "quit shuts the stream down"
    );
}
