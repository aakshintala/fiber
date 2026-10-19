//! Tests for the shared MCP response slots and wait conditions.

use contract::clock::Clock;
use serde_json::json;

#[test]
fn login_refusal_wins_over_a_later_result_but_is_not_sticky() {
    use super::{NoCancel, Outcome, Shared};
    let shared = Shared::default();
    shared.insert(7);
    shared.insert(8);
    shared.deliver(8, Outcome::Result(json!(8)));
    shared.refuse_login("login");
    shared.deliver(7, Outcome::Result(json!(7)));
    assert_eq!(
        shared.view(7, &NoCancel).response,
        Some(Outcome::Login("login".into()))
    );
    assert_eq!(
        shared.view(8, &NoCancel).response,
        Some(Outcome::Result(json!(8)))
    );
    shared.remove(7);
    shared.remove(8);
    shared.refuse_login("login");
    shared.insert(9);
    shared.deliver(9, Outcome::Result(json!(9)));
    shared.deliver(9, Outcome::Result(json!(10)));
    assert_eq!(
        shared.view(9, &NoCancel).response,
        Some(Outcome::Result(json!(10)))
    );
}

#[test]
fn shared_slots_deliver_remove_and_mark_gone() {
    use super::{Outcome, Shared};
    let shared = Shared::default();
    assert_eq!(shared.next_id(), 1);
    assert_eq!(shared.next_id(), 2);
    shared.insert(7);
    shared.deliver(7, Outcome::Result(json!({})));
    let cancel = fakes::CancelToken::new();
    let seen = shared.view(7, &cancel);
    assert_eq!(seen.response, Some(Outcome::Result(json!({}))));
    assert!(!seen.gone);
    // A late response to a removed id is discarded, never misrouted.
    shared.remove(7);
    shared.deliver(7, Outcome::Result(json!({"late": true})));
    let missing = shared.view(7, &cancel);
    assert_eq!(missing.response, None);
    shared.gone();
    assert!(shared.view(9, &cancel).gone);
}

#[test]
fn the_wait_ends_on_a_new_response_or_cancel() {
    // All four combinations: `||` into `&&` misses the two mixed rows,
    // and `!=` into `==` flips the two uncancelled rows.
    assert!(!super::should_stop(7, 7, false));
    assert!(super::should_stop(8, 7, false));
    assert!(super::should_stop(7, 7, true));
    assert!(super::should_stop(8, 7, true));
}

#[test]
fn deadline_is_now_plus_timeout_and_saturates() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    assert_eq!(
        super::deadline(clock.as_ref(), std::time::Duration::from_secs(5)),
        now.checked_add(std::time::Duration::from_secs(5))
            .expect("deadline"),
    );
    assert_eq!(
        super::deadline(clock.as_ref(), std::time::Duration::MAX),
        now,
        "an overflowing timeout saturates to now",
    );
}

#[test]
fn awaiting_holds_only_for_a_pending_slot() {
    use super::Shared;
    let shared = Shared::default();
    assert!(!shared.awaiting(7), "no slot waits");
    shared.insert(7);
    assert!(shared.awaiting(7), "an empty slot waits");
    shared.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(!shared.awaiting(7), "an answered slot waits no longer");
    shared.remove(7);
    assert!(!shared.awaiting(7), "a removed slot waits no longer");
}

#[test]
fn gone_if_awaiting_marks_only_a_pending_slot() {
    use super::Shared;
    // The forced interleaving at the decision point: insert, remove, then
    // the mark returns false and leaves the server live.
    let shared = Shared::default();
    shared.insert(7);
    assert!(shared.gone_if_awaiting(7), "a pending slot marks");
    let cancel = fakes::CancelToken::new();
    assert!(shared.view(7, &cancel).gone, "the mark wakes the waiter");
    shared.remove(7);
    let live = Shared::default();
    live.insert(7);
    live.remove(7);
    assert!(!live.gone_if_awaiting(7), "a removed slot marks nothing");
    assert!(
        !crate::registry::lock(&live.inner).gone,
        "the server stays live"
    );
    let answered = Shared::default();
    answered.insert(7);
    answered.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(
        !answered.gone_if_awaiting(7),
        "an answered slot marks nothing"
    );
    assert!(
        !crate::registry::lock(&answered.inner).gone,
        "the server stays live"
    );
}

#[test]
fn expire_if_awaiting_sets_expired_and_gone_and_wakes() {
    use super::Shared;
    let shared = Shared::default();
    shared.insert(7);
    assert!(shared.expire_if_awaiting(7), "a pending slot expires");
    let cancel = fakes::CancelToken::new();
    let seen = shared.view(7, &cancel);
    assert!(seen.expired, "the waiter sees the expiry");
    assert!(seen.gone, "an expiry is gone too");
}

#[test]
fn expire_if_awaiting_marks_nothing_once_gone_or_unawaited() {
    use super::Shared;
    // After `gone` the expiry leaves `expired` false: a death stays a death.
    let dead = Shared::default();
    dead.insert(7);
    assert!(dead.gone_if_awaiting(7), "the death marks");
    assert!(
        !dead.expire_if_awaiting(7),
        "an expiry past a death marks nothing"
    );
    assert!(
        !crate::registry::lock(&dead.inner).expired,
        "expired stays false past a death"
    );
    let removed = Shared::default();
    removed.insert(7);
    removed.remove(7);
    assert!(
        !removed.expire_if_awaiting(7),
        "a removed slot expires nothing"
    );
    assert!(
        !crate::registry::lock(&removed.inner).gone,
        "the server stays live"
    );
    let answered = Shared::default();
    answered.insert(7);
    answered.deliver(7, super::Outcome::Http("gone".to_owned()));
    assert!(
        !answered.expire_if_awaiting(7),
        "an answered slot expires nothing"
    );
}

/// One queued elicitation for the queue rows: its form never matters here.
fn elicitation(id: &str) -> super::Elicitation {
    let form = crate::elicit::read(
        r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m","requestedSchema":{"type":"object","properties":{"a":{"type":"boolean"}}}}}"#,
    )
    .expect("the form reads");
    super::Elicitation {
        id: serde_json::json!(id),
        form,
    }
}

#[test]
fn queue_elicitation_refuses_without_an_asking_call() {
    use super::Shared;
    // Without the refusal the reader would strand it: nobody takes it.
    let shared = Shared::default();
    let back = shared
        .queue_elicitation(elicitation("el-1"))
        .expect_err("no asking call refuses the queue");
    assert_eq!(back.id, serde_json::json!("el-1"), "it is given back");
}

#[test]
fn take_lists_every_call_in_id_order_with_the_earliest_deadline() {
    use super::Shared;
    use contract::ActionId;
    let clock = fakes::clock::FakeClock::new();
    let shared = Shared::default();
    // Registered out of id order: the take still lists id order, and the
    // earlier deadline wins even though its call registered second.
    shared.insert(5);
    shared.register(
        5,
        ActionId("a_5".to_owned()),
        clock.now() + std::time::Duration::from_secs(60),
    );
    shared.insert(3);
    shared.register(
        3,
        ActionId("a_3".to_owned()),
        clock.now() + std::time::Duration::from_secs(30),
    );
    shared
        .queue_elicitation(elicitation("el-1"))
        .expect("an asking call queues");
    let taken = shared.take_elicitation().expect("the queued take");
    assert_eq!(
        taken.action_ids,
        vec![ActionId("a_3".to_owned()), ActionId("a_5".to_owned())],
        "request-id order, not registration order",
    );
    assert_eq!(
        taken.until,
        clock.now() + std::time::Duration::from_secs(30),
        "the earliest deadline",
    );
    assert!(
        shared.take_elicitation().is_none(),
        "a second take finds nothing",
    );
}

#[test]
fn remove_returns_the_queue_only_with_the_last_asking_call() {
    use super::Shared;
    use contract::ActionId;
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now() + std::time::Duration::from_secs(30);
    let shared = Shared::default();
    shared.insert(5);
    shared.register(5, ActionId("a_5".to_owned()), deadline);
    shared.insert(3);
    shared.register(3, ActionId("a_3".to_owned()), deadline);
    shared
        .queue_elicitation(elicitation("el-1"))
        .expect("an asking call queues");
    assert!(
        shared.remove(5).is_empty(),
        "a non-last removal returns nothing",
    );
    let leftovers = shared.remove(3);
    assert_eq!(leftovers.len(), 1, "the last removal returns the queue");
    assert_eq!(leftovers[0].id, serde_json::json!("el-1"));
    // An id never registered, as an `initialize` is, with one asking
    // call left returns nothing and leaves the queue queued.
    let held = Shared::default();
    held.insert(7);
    held.register(7, ActionId("a_7".to_owned()), deadline);
    held.queue_elicitation(elicitation("el-2"))
        .expect("an asking call queues");
    assert!(
        held.remove(99).is_empty(),
        "an unregistered removal returns nothing",
    );
    assert!(held.take_elicitation().is_some(), "the queue stays queued",);
}

#[test]
fn queueing_wakes_a_waiter() {
    use super::Shared;
    use contract::ActionId;
    let clock = fakes::clock::FakeClock::new();
    let shared = Shared::default();
    let before = crate::registry::lock(&shared.inner).seq;
    shared.insert(7);
    shared.register(
        7,
        ActionId("a_7".to_owned()),
        clock.now() + std::time::Duration::from_secs(30),
    );
    shared
        .queue_elicitation(elicitation("el-1"))
        .expect("an asking call queues");
    assert!(
        crate::registry::lock(&shared.inner).seq != before,
        "the sequence moves so every parked call wakes",
    );
}
