#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{ReviewerUse, UsageRecorded};
use contract::provider::CostLookup;
use contract::shapes::Tokens;
use contract::{ActionId, GenerationId, TurnId};
use fakes::clock::FakeClock;

use super::{LOOKUP_AFTER, LateCost, Settled};

/// How long a test waits on the wall clock for the worker to park, call or
/// let go of a lookup.
const WAIT: Duration = Duration::from_secs(5);

/// The channels a blocking lookup says it was called on, and waits on.
type Block = (Mutex<Sender<()>>, Mutex<Receiver<()>>);

/// A lookup returning `cost`, which records each generation it is asked
/// for and says when the worker lets go of it.
struct Lookup {
    cost: Option<f64>,
    calls: Arc<Mutex<Vec<String>>>,
    /// When set, `cost` says it was called, then waits for the test's go.
    block: Option<Block>,
    dropped: Mutex<Sender<()>>,
}

impl CostLookup for Lookup {
    fn cost(&self, generation_id: &GenerationId) -> Option<f64> {
        self.calls.lock().unwrap().push(generation_id.0.clone());
        if let Some((entered, go)) = &self.block {
            entered.lock().unwrap().send(()).unwrap();
            go.lock()
                .unwrap()
                .recv_timeout(WAIT)
                .expect("waited for the test to release the lookup");
        }
        self.cost
    }
}

impl Drop for Lookup {
    fn drop(&mut self) {
        match self.dropped.lock().unwrap().send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// What a test holds of its lookup: the calls it saw and its drop signal.
struct Seen {
    calls: Arc<Mutex<Vec<String>>>,
    dropped: Receiver<()>,
}

impl Seen {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Waits until the worker no longer holds the lookup: it ran and its
    /// result was pushed or dropped, or it was never dispatched and the
    /// queue is gone.
    fn await_dropped(&self) {
        self.dropped
            .recv_timeout(WAIT)
            .expect("waited for the worker to let go of the lookup");
    }
}

fn lookup(cost: Option<f64>) -> (Arc<dyn CostLookup>, Seen) {
    lookup_with(cost, None)
}

fn lookup_with(
    cost: Option<f64>,
    block: Option<(Sender<()>, Receiver<()>)>,
) -> (Arc<dyn CostLookup>, Seen) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (dropped_tx, dropped) = mpsc::channel();
    let lookup = Lookup {
        cost,
        calls: Arc::clone(&calls),
        block: block.map(|(entered, go)| (Mutex::new(entered), Mutex::new(go))),
        dropped: Mutex::new(dropped_tx),
    };
    (Arc::new(lookup), Seen { calls, dropped })
}

/// A first record with every payload field set, so a copy that drops one
/// shows.
fn first(id: &str) -> UsageRecorded {
    UsageRecorded {
        generation_id: GenerationId(id.into()),
        model: "openrouter/z-ai/glm-5.3-flash".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 2,
            cache_write: BTreeMap::from([("5m".to_owned(), 3)]),
            output: 4,
        },
        input_bytes: 812,
        input_media: Some(true),
        web_searches: Some(1),
        cost: Some(0.0001),
        subscription: Some(true),
        extension: Some("ext".into()),
        origin_session_id: None,
        reviewer: None,
    }
}

fn turn() -> Option<TurnId> {
    Some(TurnId("t_1".into()))
}

fn action() -> Option<ActionId> {
    Some(ActionId("a_1".into()))
}

fn clocks() -> (Arc<FakeClock>, Arc<dyn Clock>) {
    let clock = FakeClock::new();
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    (clock, dyn_clock)
}

#[test]
fn a_lookup_runs_only_once_30_seconds_pass_and_settles_a_copy_with_its_cost() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(Some(0.0000072));
    let mut late = LateCost::default();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");

    let mark = clock.advance_marked(LOOKUP_AFTER.saturating_sub(Duration::from_millis(1)));
    assert!(
        clock.await_parked_since(&mark, Some(due), WAIT),
        "a millisecond early, the worker parks at the due again"
    );
    assert!(late.take_settled().is_empty());
    assert!(seen.calls().is_empty());

    let mark = clock.advance_marked(Duration::from_millis(1));
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue once it has pushed"
    );
    assert_eq!(seen.calls(), ["gen-1"]);
    let mut want = first("gen-1");
    want.cost = Some(0.0000072);
    assert_eq!(
        late.take_settled(),
        [Settled {
            record: want,
            turn: turn(),
            action: action(),
        }]
    );
    assert!(late.take_settled().is_empty(), "a take empties the queue");
}

#[test]
fn a_lookup_returning_nothing_settles_nothing() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(None);
    let mut late = LateCost::default();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    let mark = clock.advance_marked(LOOKUP_AFTER);
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue after the call"
    );
    assert_eq!(seen.calls(), ["gen-1"]);
    assert!(late.take_settled().is_empty());
}

#[test]
fn a_stop_before_the_due_means_the_lookup_is_never_called() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(Some(1.0));
    let mut late = LateCost::default();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    late.stop();
    clock.advance(LOOKUP_AFTER);
    assert!(late.take_settled().is_empty());
    // The queue goes with the last holder: the worker, once it has seen
    // the stop, and the loop's own.
    drop(late);
    seen.await_dropped();
    assert!(seen.calls().is_empty());
}

#[test]
fn a_stop_while_the_lookup_runs_drops_its_result() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (entered_tx, entered) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    let (lookup, seen) = lookup_with(Some(1.0), Some((entered_tx, go_rx)));
    let mut late = LateCost::default();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    clock.advance(LOOKUP_AFTER);
    entered
        .recv_timeout(WAIT)
        .expect("waited for the lookup to start");
    late.stop();
    go.send(()).unwrap();
    seen.await_dropped();
    assert_eq!(seen.calls(), ["gen-1"]);
    assert!(late.take_settled().is_empty());
}

#[test]
fn two_lookups_run_in_due_order() {
    let (clock, dyn_clock) = clocks();
    let start = clock.now();
    let (first_lookup, first_seen) = lookup(Some(1.0));
    let mut late = LateCost::default();
    late.schedule(first_lookup, first("gen-a"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(start + LOOKUP_AFTER, WAIT));
    let mark = clock.advance_marked(Duration::from_secs(10));
    assert!(clock.await_parked_since(&mark, Some(start + LOOKUP_AFTER), WAIT));
    // Both lookups share one record of calls, so their order shows.
    let (second_lookup, _second_seen) = lookup_with(Some(2.0), None);
    let second_lookup = Arc::new(AlsoRecords {
        inner: second_lookup,
        calls: Arc::clone(&first_seen.calls),
    });
    let mark = clock
        .mark_parked(start + LOOKUP_AFTER, WAIT)
        .expect("the worker parks at the first due");
    late.schedule(second_lookup, first("gen-b"), None, None, &dyn_clock)
        .unwrap();
    assert!(
        clock.await_parked_since(&mark, Some(start + LOOKUP_AFTER), WAIT),
        "woken by the schedule, the worker parks at the earlier due"
    );
    let mark = clock.advance_marked(LOOKUP_AFTER);
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue after both calls"
    );
    assert_eq!(first_seen.calls(), ["gen-a", "gen-b"]);
    let settled: Vec<(String, Option<f64>)> = late
        .take_settled()
        .into_iter()
        .map(|s| (s.record.generation_id.0, s.record.cost))
        .collect();
    assert_eq!(
        settled,
        [
            ("gen-a".to_owned(), Some(1.0)),
            ("gen-b".to_owned(), Some(2.0))
        ]
    );
}

#[test]
fn two_lookups_due_together_both_run_in_scheduling_order() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (first_lookup, seen) = lookup(Some(1.0));
    let (second_lookup, _unused) = lookup(Some(2.0));
    let second_lookup = Arc::new(AlsoRecords {
        inner: second_lookup,
        calls: Arc::clone(&seen.calls),
    });
    let mut late = LateCost::default();
    late.schedule(first_lookup, first("gen-a"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    let mark = clock
        .mark_parked(due, WAIT)
        .expect("the worker parks at the due");
    late.schedule(second_lookup, first("gen-b"), None, None, &dyn_clock)
        .unwrap();
    assert!(
        clock.await_parked_since(&mark, Some(due), WAIT),
        "woken by the schedule, the worker parks at the same due"
    );
    let mark = clock.advance_marked(LOOKUP_AFTER);
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue after both calls"
    );
    assert_eq!(seen.calls(), ["gen-a", "gen-b"]);
    assert_eq!(late.take_settled().len(), 2);
}

/// A lookup that also records its calls in another lookup's record.
struct AlsoRecords {
    inner: Arc<dyn CostLookup>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl CostLookup for AlsoRecords {
    fn cost(&self, generation_id: &GenerationId) -> Option<f64> {
        self.calls.lock().unwrap().push(generation_id.0.clone());
        self.inner.cost(generation_id)
    }
}

#[test]
fn a_generation_scheduled_twice_is_looked_up_once() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(Some(1.0));
    let mut late = LateCost::default();
    late.schedule(
        Arc::clone(&lookup),
        first("gen-1"),
        turn(),
        action(),
        &dyn_clock,
    )
    .unwrap();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    let mark = clock.advance_marked(LOOKUP_AFTER);
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue after the call"
    );
    assert_eq!(seen.calls(), ["gen-1"]);
    assert_eq!(late.take_settled().len(), 1);
}

#[test]
fn dropping_the_queue_stops_the_worker_before_the_due() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(Some(1.0));
    let mut late = LateCost::default();
    late.schedule(lookup, first("gen-1"), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    drop(late);
    clock.advance(LOOKUP_AFTER);
    seen.await_dropped();
    assert!(seen.calls().is_empty());
}

#[test]
fn a_settled_record_keeps_the_reviewer_use() {
    let (clock, dyn_clock) = clocks();
    let due = clock.now() + LOOKUP_AFTER;
    let (lookup, seen) = lookup(Some(0.0000072));
    let mut first = first("gen-9");
    first.reviewer = Some(ReviewerUse::Stage2 {
        action_id: ActionId("a_9".into()),
    });
    let mut late = LateCost::default();
    late.schedule(lookup, first.clone(), turn(), action(), &dyn_clock)
        .unwrap();
    assert!(clock.await_parked(due, WAIT), "the worker parks at the due");
    let mark = clock.advance_marked(LOOKUP_AFTER);
    assert!(
        clock.await_parked_since(&mark, None, WAIT),
        "the worker parks with an empty queue after the call"
    );
    assert_eq!(seen.calls(), ["gen-9"]);
    // The settlement only sets the late cost: the reviewer object rides
    // along unchanged.
    let mut want = first;
    want.cost = Some(0.0000072);
    assert_eq!(
        late.take_settled(),
        [Settled {
            record: want,
            turn: turn(),
            action: action(),
        }]
    );
}
