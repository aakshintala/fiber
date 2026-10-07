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
    Event, InputItem, KeptMessage, ReviewerKept, SteeringApplied, ToolCallRequested, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{ActionId, CommandId, Seq};
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
