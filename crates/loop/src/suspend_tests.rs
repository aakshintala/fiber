//! The suspended batch [`super::form_batch`] builds: the calls before the
//! action are cancelled without running, the action runs decided, and the
//! calls after it are judged as in any step.

#![allow(
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::cell::Cell;

use contract::ActionId;
use contract::events::{CallStatus, ToolCallCompleted, ToolCallRequested};
use contract::shapes::ContentPart;

use super::form_batch;
use crate::calls::Decided;

fn call(id: &str) -> (ActionId, ToolCallRequested) {
    (
        ActionId(id.into()),
        ToolCallRequested {
            name: "read".into(),
            arguments: serde_json::json!({"path": "a"}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
    )
}

/// A decided completion with `text`, standing in for either closure's
/// answer: which positions hold which answer is what the tests pin.
fn completed(text: &str) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

fn mark(decided: &Option<Decided>) -> &str {
    match decided {
        Some(Err(done)) => match done.content.as_slice() {
            [ContentPart::Text { text }] if text == "before" => "before",
            [ContentPart::Text { text }] if text == "decided" => "decided",
            _ => "other",
        },
        Some(Ok(_)) => "approved",
        None => "none",
    }
}

fn marks(formed: &[(ActionId, ToolCallRequested, Option<Decided>)]) -> (Vec<String>, Vec<&str>) {
    (
        formed.iter().map(|(id, _, _)| id.0.clone()).collect(),
        formed.iter().map(|(_, _, decided)| mark(decided)).collect(),
    )
}

#[test]
fn without_the_action_every_call_is_before_and_decide_never_runs() {
    let decide_calls = Cell::new(0u32);
    let formed = form_batch(
        vec![call("a_0"), call("a_1")],
        &ActionId("a_9".into()),
        || Err(completed("before")),
        |_| {
            decide_calls.set(decide_calls.get() + 1);
            Err(completed("decided"))
        },
    );
    assert_eq!(decide_calls.get(), 0);
    assert_eq!(
        marks(&formed),
        (
            vec!["a_0".to_owned(), "a_1".to_owned()],
            vec!["before", "before"]
        )
    );
}

#[test]
fn with_the_action_first_it_is_decided_once_and_the_rest_is_judged_later() {
    let decide_calls = Cell::new(0u32);
    let formed = form_batch(
        vec![call("a_0"), call("a_1"), call("a_2")],
        &ActionId("a_0".into()),
        || Err(completed("before")),
        |_| {
            decide_calls.set(decide_calls.get() + 1);
            Err(completed("decided"))
        },
    );
    assert_eq!(decide_calls.get(), 1);
    assert_eq!(
        marks(&formed),
        (
            vec!["a_0".to_owned(), "a_1".to_owned(), "a_2".to_owned()],
            vec!["decided", "none", "none"]
        )
    );
}

#[test]
fn with_the_action_last_the_calls_before_it_are_before() {
    let decide_calls = Cell::new(0u32);
    let formed = form_batch(
        vec![call("a_0"), call("a_1"), call("a_2")],
        &ActionId("a_2".into()),
        || Err(completed("before")),
        |_| {
            decide_calls.set(decide_calls.get() + 1);
            Err(completed("decided"))
        },
    );
    assert_eq!(decide_calls.get(), 1);
    assert_eq!(
        marks(&formed),
        (
            vec!["a_0".to_owned(), "a_1".to_owned(), "a_2".to_owned()],
            vec!["before", "before", "decided"]
        )
    );
}

#[test]
fn a_duplicated_action_is_decided_once_at_its_first_occurrence() {
    let decide_calls = Cell::new(0u32);
    let formed = form_batch(
        vec![call("a_0"), call("a_1"), call("a_1")],
        &ActionId("a_1".into()),
        || Err(completed("before")),
        |_| {
            decide_calls.set(decide_calls.get() + 1);
            Err(completed("decided"))
        },
    );
    assert_eq!(decide_calls.get(), 1);
    assert_eq!(
        marks(&formed),
        (
            vec!["a_0".to_owned(), "a_1".to_owned(), "a_1".to_owned()],
            vec!["before", "decided", "none"]
        )
    );
}
