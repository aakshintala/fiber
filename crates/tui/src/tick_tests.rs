//! Tests for the working line's timer: the gate's pure step, and the
//! thread's wiring to the channel, on a fake clock.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::{Gate, Step, TICK, TickThread, Ticker};
use crate::Input;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// A gate armed at `at`, through `arm`, so the test reads what the loop
/// armed rather than a literal.
fn armed(at: Instant) -> Gate {
    let mut gate = Gate {
        at: None,
        in_flight: false,
        quit: false,
    };
    gate.at = Some(at);
    gate
}

#[test]
fn gate_step_table() {
    let origin = FakeClock::new().origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    // Quit ends, whatever else holds.
    let mut gate = Gate {
        at: Some(at),
        in_flight: true,
        quit: true,
    };
    let mut calls = 0;
    assert_eq!(
        gate.step(|| {
            calls += 1;
            origin
        }),
        Step::Quit
    );
    assert_eq!(calls, 0);
    // Disarmed waits, and asks for no time.
    let mut gate = Gate {
        at: None,
        in_flight: false,
        quit: false,
    };
    let mut calls = 0;
    assert_eq!(
        gate.step(|| {
            calls += 1;
            origin
        }),
        Step::Idle
    );
    assert_eq!(calls, 0);
    // In flight waits, and asks for no time.
    let mut gate = Gate {
        at: Some(at),
        in_flight: true,
        quit: false,
    };
    let mut calls = 0;
    assert_eq!(
        gate.step(|| {
            calls += 1;
            origin
        }),
        Step::Idle
    );
    assert_eq!(calls, 0);
    // Armed: before the deadline waits on it, at and past it sends.
    let before = at.checked_sub(Duration::from_millis(1)).expect("1ms in");
    let mut gate = armed(at);
    assert_eq!(gate.step(|| before), Step::Until(at));
    let mut gate = armed(at);
    assert_eq!(gate.step(|| at), Step::Send);
    // Sending clears the deadline and holds a tick out.
    assert_eq!(gate.at, None);
    assert!(gate.in_flight);
    let mut gate = armed(at);
    let past = at.checked_add(Duration::from_millis(2)).expect("2ms past");
    assert_eq!(gate.step(|| past), Step::Send);
}

/// An arm never touches `in_flight`; only the ack clears it.
#[test]
fn arm_leaves_in_flight_and_ack_clears_it() {
    let origin = FakeClock::new().origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let mut gate = Gate {
        at: Some(at),
        in_flight: true,
        quit: false,
    };
    let later = at.checked_add(TICK).expect("two deadlines past the origin");
    gate.at = Some(later);
    let mut calls = 0;
    assert_eq!(
        gate.step(|| {
            calls += 1;
            origin
        }),
        Step::Idle
    );
    assert_eq!(calls, 0);
    gate.in_flight = false;
    assert_eq!(gate.step(|| later), Step::Send);
}

/// A ticker on the fake clock, and the ticks its thread sends.
fn ticker() -> (Arc<FakeClock>, Arc<Ticker>) {
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    let ticker = Ticker::new(Some(&shared));
    (clock, ticker)
}

/// Runs the ticker's thread, sending to `tx`; the thread's end arrives on
/// the second channel.
fn run(ticker: &Arc<Ticker>, clock: Arc<FakeClock>, tx: mpsc::Sender<Input>) -> Receiver<()> {
    let ticker = Arc::clone(ticker);
    let shared: Arc<dyn Clock> = clock;
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("tick-run".to_owned())
        .spawn(move || {
            ticker.run(shared.as_ref(), &tx);
            done.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

/// The next tick, within [`DEADLINE`].
fn tick(rx: &Receiver<Input>) -> Input {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the tick: {err}"))
}

/// Waits for the thread to park at `until`, within [`DEADLINE`].
fn parked(clock: &FakeClock, until: Instant) {
    assert!(
        clock.await_parked(until, DEADLINE),
        "waited {DEADLINE:?} for the ticker to park on the clock"
    );
}

#[test]
fn an_armed_ticker_sends_one_tick_at_its_deadline() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let (tx, rx) = mpsc::channel();
    let _finished = run(&ticker, clock.clone(), tx);
    ticker.arm(Some(at));
    parked(&clock, at);
    // Short of the deadline the thread parks again, and nothing arrives.
    let mark = clock.advance_marked(Duration::from_millis(119));
    assert!(
        clock.await_parked_since(&mark, Some(at), DEADLINE),
        "waited {DEADLINE:?} for the ticker to park again"
    );
    // Nothing sent yet: an empty channel, not a dead thread.
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    clock.advance(Duration::from_millis(1));
    assert!(matches!(tick(&rx), Input::Tick));
}

#[test]
fn a_second_deadline_waits_for_the_first_ticks_ack() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let (tx, rx) = mpsc::channel();
    let _finished = run(&ticker, clock.clone(), tx);
    ticker.arm(Some(at));
    parked(&clock, at);
    clock.advance(TICK);
    assert!(matches!(tick(&rx), Input::Tick));
    // Re-armed in the past, but the first tick is unacked: the "no tick
    // before the ack" half lives in `gate_step_table`'s in-flight row, so
    // no quiet wait proves it here.
    ticker.arm(Some(origin));
    ticker.ack();
    assert!(matches!(tick(&rx), Input::Tick));
}

#[test]
fn rearming_moves_the_deadline() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let first = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let moved = origin
        .checked_add(TICK.checked_mul(2).expect("twice the tick"))
        .expect("two deadlines past the origin");
    let (tx, rx) = mpsc::channel();
    let _finished = run(&ticker, clock.clone(), tx);
    ticker.arm(Some(first));
    ticker.arm(Some(moved));
    parked(&clock, moved);
    // Past the first deadline the thread still parks at the moved one,
    // and nothing arrives.
    let mark = clock.advance_marked(TICK);
    assert!(
        clock.await_parked_since(&mark, Some(moved), DEADLINE),
        "waited {DEADLINE:?} for the ticker to park again"
    );
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn disarming_leaves_the_clock() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let (tx, rx) = mpsc::channel();
    let _finished = run(&ticker, clock.clone(), tx);
    ticker.arm(Some(at));
    parked(&clock, at);
    ticker.arm(None);
    // Disarmed the thread parks on its own condvar, not on the clock, so
    // no park signal proves its silence: that half lives in
    // `gate_step_table`'s disarmed row. Re-arming parks at the new
    // deadline only. This test never advances the clock.
    let later = origin
        .checked_add(TICK.checked_mul(4).expect("four ticks"))
        .expect("four deadlines past the origin");
    ticker.arm(Some(later));
    parked(&clock, later);
    assert!(!clock.parked().contains(&Some(at)));
    drop(rx);
}

#[test]
fn a_deadline_already_past_sends_at_once() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let (tx, rx) = mpsc::channel();
    let _finished = run(&ticker, clock.clone(), tx);
    ticker.arm(Some(origin));
    assert!(matches!(tick(&rx), Input::Tick));
    // The past deadline never parked on the clock.
    assert!(clock.parked().is_empty());
}

#[test]
fn quit_ends_the_thread_without_advancing_time() {
    let (clock, ticker) = ticker();
    let origin = clock.origin();
    let (tx, _rx) = mpsc::channel();
    let finished = run(&ticker, clock.clone(), tx);
    ticker.quit();
    finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the thread to end: {err}"));
    assert_eq!(clock.now(), origin);
}

#[test]
fn dropping_a_tick_thread_ends_its_thread() {
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    let (tx, rx) = mpsc::channel();
    let mut tick = TickThread::idle();
    tick.start(shared, tx);
    drop(tick);
    // The thread's return drops its only sender, closing the channel: a
    // timeout instead would pass a thread that never ends.
    assert!(matches!(
        rx.recv_timeout(DEADLINE),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

#[test]
fn an_idle_tick_thread_records_its_arm() {
    let origin = FakeClock::new().origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let tick = TickThread::idle();
    assert_eq!(tick.armed(), None);
    tick.arm(Some(at));
    assert_eq!(tick.armed(), Some(at));
    tick.arm(None);
    assert_eq!(tick.armed(), None);
}

/// The ticks a started thread sends, within [`DEADLINE`].
#[test]
fn a_started_thread_sends_what_its_ticker_arms() {
    let clock = FakeClock::new();
    let shared: Arc<dyn Clock> = clock.clone();
    let origin = clock.origin();
    let at = origin
        .checked_add(TICK)
        .expect("a deadline past the origin");
    let (tx, rx) = mpsc::channel();
    let mut thread = TickThread::idle();
    thread.start(shared, tx);
    thread.arm(Some(at));
    parked(&clock, at);
    clock.advance(TICK);
    assert!(matches!(tick(&rx), Input::Tick));
    thread.ack();
    thread.stop();
}

/// Stopping a thread that never started only quits its ticker.
#[test]
fn stopping_without_a_thread_returns() {
    let mut tick = TickThread::idle();
    tick.stop();
}
