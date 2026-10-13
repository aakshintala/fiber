//! The fold [`super::render_reviewed`] builds: which items a line adds and
//! what `reviewer_kept` keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use contract::events::{
    Event, HandoffCompleted, InputItem, KeptMessage, Note, Outcome, ReviewerKept, SteeringApplied,
    ToolCallRequested, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{ActionId, CommandId, ErrorCode, Seq};
use serde_json::json;

use super::{Reviewed, Shown, render_reviewed};

fn driver() -> Sender {
    Sender {
        origin: Origin::Driver,
        command_id: Some(CommandId("c_1".into())),
    }
}

fn extension_sender() -> Sender {
    Sender {
        origin: Origin::Extension {
            extension: "ext".into(),
        },
        command_id: Some(CommandId("c_1".into())),
    }
}

fn text_part(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: text.into() }]
}

fn message(content: &str, sender: Sender) -> InputItem {
    InputItem::Message {
        content: text_part(content),
        sender,
        changed_by: None,
    }
}

fn jobs() -> InputItem {
    InputItem::Jobs {
        job_ids: vec![contract::JobId("j_1".into())],
    }
}

fn call(name: &str) -> ToolCallRequested {
    ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }
}

fn kept(seq: u64, item: u64) -> KeptMessage {
    KeptMessage {
        seq: Seq(seq),
        item,
    }
}

fn kept_event(kept: Vec<KeptMessage>) -> Event {
    Event::ReviewerKept(ReviewerKept { kept, failed: None })
}

#[test]
fn a_person_message_carries_its_line_and_its_index() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![jobs(), message("run the tests", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Person(kept(12, 1)),
            input: Input::User {
                text: "The person: run the tests".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn a_steered_message_is_the_lines_only_item() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(31)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Person(kept(31, 0)),
            input: Input::User {
                text: "The person: and the linter".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn another_senders_message_renders_nothing() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("from elsewhere", extension_sender())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("an extension steers"),
            sender: extension_sender(),
            changed_by: None,
        }),
        None,
        Some(Seq(13)),
    );
    assert!(reviewed.is_empty());
}

#[test]
fn a_call_is_shown_by_its_action() {
    let mut reviewed = Vec::new();
    let action = ActionId("a_7".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
        Some(Seq(40)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Call(action),
            input: Input::User {
                text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn a_person_message_with_no_seq_renders_nothing() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("run the tests", driver())],
        }),
        None,
        None,
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        None,
    );
    assert!(reviewed.is_empty());
}

#[test]
fn reviewer_kept_leaves_the_named_message_word_for_word() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    let action = ActionId("a_1".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
        Some(Seq(13)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![
                jobs(),
                message("say hi", driver()),
                message("from elsewhere", extension_sender()),
            ],
        }),
        None,
        Some(Seq(31)),
    );
    assert_eq!(reviewed.len(), 3);
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(12, 0)]),
        None,
        Some(Seq(57)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Person(kept(12, 0)),
            input: Input::User {
                text: "The person: never push to main".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn reviewer_kept_with_nothing_empties_the_input() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(&mut reviewed, &kept_event(Vec::new()), None, Some(Seq(57)));
    assert!(reviewed.is_empty());
}

#[test]
fn reviewer_kept_naming_nothing_present_keeps_nothing_extra() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(99, 0), kept(12, 3)]),
        None,
        Some(Seq(57)),
    );
    assert!(reviewed.is_empty());
}

#[test]
fn lines_after_reviewer_kept_append_after_the_kept_items() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        None,
        Some(Seq(31)),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(31, 0)]),
        None,
        Some(Seq(57)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("now run it"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(60)),
    );
    assert_eq!(
        reviewed
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>(),
        vec![
            Input::User {
                text: "The person: say hi".into(),
                images: Vec::new(),
            },
            Input::User {
                text: "The person: now run it".into(),
                images: Vec::new(),
            },
        ],
    );
    assert!(matches!(reviewed[0].shown, Shown::Person(_)));
    assert!(matches!(reviewed[1].shown, Shown::Person(_)));
}

#[test]
fn the_reviewer_keeps_an_inner_call() {
    let mut inner = call("fake");
    inner.ran_by = Some(contract::events::RanBy {
        extension: "ext".into(),
        anchor: contract::events::Anchor::OuterCall(ActionId("a_outer".into())),
    });
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(inner),
        Some(&ActionId("a_inner".into())),
        Some(Seq(41)),
    );
    assert_eq!(reviewed.len(), 1);
    assert_eq!(reviewed[0].shown, Shown::Call(ActionId("a_inner".into())));
    assert!(matches!(&reviewed[0].input, Input::User { text, .. } if text.contains("fake")));
}

fn handoff_completed() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Completed,
        error: None,
        note: Some(Note::Actions {
            note: vec![ActionId("a_note".into())],
        }),
        tokens_before: 1000,
        instructions: None,
    })
}

fn handoff_failed() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::ContextOverflow,
            message: "the note did not fit".into(),
            retry_after_ms: None,
            provider: None,
        }),
        note: None,
        tokens_before: 1000,
        instructions: None,
    })
}

fn handoff_cancelled() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Cancelled,
        error: None,
        note: None,
        tokens_before: 1000,
        instructions: None,
    })
}

/// A busy turn's reviewed input: two person messages with a tool call
/// after each.
fn busy_turn(reviewed: &mut Vec<Reviewed>) {
    render_reviewed(
        reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(13)),
    );
    render_reviewed(
        reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        None,
        Some(Seq(31)),
    );
    render_reviewed(
        reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_2".into())),
        Some(Seq(32)),
    );
}

fn person_text(reviewed: &[Reviewed]) -> Vec<&str> {
    reviewed
        .iter()
        .map(|item| match &item.input {
            Input::User { text, .. } => text.as_str(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. } => panic!("a reviewer item is always a user item"),
        })
        .collect()
}

#[test]
fn a_completed_handoff_drops_earlier_calls_and_keeps_persons_word_for_word() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    assert_eq!(reviewed.len(), 4);
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    assert!(
        reviewed
            .iter()
            .all(|item| matches!(item.shown, Shown::Person(_))),
        "no pre-handoff tool call stays in the reviewer's input: {reviewed:?}"
    );
    assert_eq!(
        reviewed,
        vec![
            Reviewed {
                shown: Shown::Person(kept(12, 0)),
                input: Input::User {
                    text: "The person: never push to main".into(),
                    images: Vec::new(),
                },
            },
            Reviewed {
                shown: Shown::Person(kept(31, 0)),
                input: Input::User {
                    text: "The person: say hi".into(),
                    images: Vec::new(),
                },
            },
        ],
    );
}

#[test]
fn a_completed_handoff_on_calls_only_empties_the_input() {
    let mut reviewed = Vec::new();
    for (n, action) in ["a_1", "a_2"].iter().enumerate() {
        render_reviewed(
            &mut reviewed,
            &Event::ToolCallRequested(call("shell")),
            Some(&ActionId((*action).into())),
            Some(Seq(13 + n as u64)),
        );
    }
    assert_eq!(reviewed.len(), 2);
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    assert!(reviewed.is_empty(), "{reviewed:?}");
}

#[test]
fn a_completed_handoff_keeps_every_person_message() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(13)),
    );
    let before = reviewed.clone();
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    assert_eq!(reviewed, before);
    assert_eq!(
        person_text(&reviewed),
        [
            "The person: never push to main",
            "The person: and the linter"
        ],
    );
}

#[test]
fn a_completed_handoff_on_empty_input_stays_empty() {
    let mut reviewed = Vec::new();
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    assert!(reviewed.is_empty());
}

#[test]
fn a_failed_handoff_keeps_calls_and_persons_byte_identical() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    let before = reviewed.clone();
    render_reviewed(&mut reviewed, &handoff_failed(), None, Some(Seq(40)));
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn a_cancelled_handoff_keeps_calls_and_persons_byte_identical() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    let before = reviewed.clone();
    render_reviewed(&mut reviewed, &handoff_cancelled(), None, Some(Seq(40)));
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn reviewer_kept_after_a_completed_handoff_selects_the_kept_persons() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(31, 0)]),
        None,
        Some(Seq(57)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Person(kept(31, 0)),
            input: Input::User {
                text: "The person: say hi".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn a_repaired_call_renders_the_arguments_that_run() {
    let mut repaired = call("shell");
    let mut map = serde_json::Map::new();
    map.insert("city".into(), serde_json::Value::String("Paris".into()));
    repaired.repair = Some(contract::events::ArgumentRepair {
        repaired: map,
        repairs: Vec::new(),
    });
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(repaired),
        Some(&ActionId("a_9".into())),
        Some(Seq(41)),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Call(ActionId("a_9".into())),
            input: Input::User {
                text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn lines_without_a_durable_identity_add_nothing() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("run the tests", driver())],
        }),
        None,
        None,
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        None,
        Some(Seq(40)),
    );
    assert!(reviewed.is_empty());
}
