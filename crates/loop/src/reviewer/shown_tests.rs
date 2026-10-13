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
    Event, HandoffCompleted, InputItem, KeptMessage, Note, Outcome, ReviewerKept, SessionStarted,
    SteeringApplied, ToolCallRequested, TurnStarted, Variables, VariablesSource,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{ActionId, CommandId, ErrorCode, Seq, SessionId, TurnId};
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
fn lines_without_a_durable_identity_add_nothing() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        None,
        Some(Seq(40)),
    );
    assert!(reviewed.is_empty());
}

#[test]
fn successive_completed_handoffs_drop_each_cut_before_them() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("keep the tests fast", driver())],
        }),
        None,
        Some(Seq(51)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_3".into())),
        Some(Seq(52)),
    );
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(60)));
    assert!(
        reviewed
            .iter()
            .all(|item| matches!(item.shown, Shown::Person(_))),
        "no call from either cut stays: {reviewed:?}"
    );
    assert_eq!(
        person_text(&reviewed),
        [
            "The person: never push to main",
            "The person: say hi",
            "The person: keep the tests fast"
        ],
    );
}

#[test]
fn a_call_after_the_cut_stays_and_a_call_before_it_goes() {
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
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(13)),
    );
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(40)));
    let after = ActionId("a_2".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&after),
        Some(Seq(41)),
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
                shown: Shown::Call(after),
                input: Input::User {
                    text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
                    images: Vec::new(),
                },
            },
        ],
    );
}

#[test]
fn the_cut_releases_the_calls_allocation() {
    let mut reviewed = Vec::with_capacity(1024);
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
        Some(Seq(13)),
    );
    for n in 0..300u64 {
        render_reviewed(
            &mut reviewed,
            &Event::ToolCallRequested(call("shell")),
            Some(&ActionId(format!("a_{n}"))),
            Some(Seq(14 + n)),
        );
    }
    let before = reviewed.capacity();
    assert_eq!(reviewed.len(), 302);
    render_reviewed(&mut reviewed, &handoff_completed(), None, Some(Seq(400)));
    // Length alone would still hold the peak's pages: the allocation
    // itself must fall with the dropped calls.
    assert_eq!(reviewed.len(), 2);
    assert!(
        reviewed.capacity() < before,
        "capacity {} stays at the {}-call peak {before}",
        reviewed.capacity(),
        300,
    );
}

#[test]
fn the_live_fold_and_the_history_fold_agree_at_each_side_of_the_cut() {
    let root = fakes::TempDir::new("fiber-reviewed-fold");
    let log = log::Log::create(
        root.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let dir = root.path().join("s_1");
    let turn = TurnId("t_1".into());
    let after = TurnId("t_2".into());
    let started = Event::SessionStarted(SessionStarted {
        workspace: "/w".into(),
        variables: Variables {
            path: String::new(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: None,
        rewind: None,
        worktree: None,
    });
    log.append(&started, None, None).unwrap();
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![message("never push to main", driver())],
        }),
        Some(turn.clone()),
        None,
    )
    .unwrap();
    log.append(
        &Event::ToolCallRequested(call("shell")),
        Some(turn.clone()),
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    log.append(&handoff_completed(), Some(turn), None).unwrap();
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![message("now run it", driver())],
        }),
        Some(after.clone()),
        None,
    )
    .unwrap();
    log.append(
        &Event::ToolCallRequested(call("shell")),
        Some(after),
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    // Before the cut, at it and after it, folding the log to that point
    // reads the same reviewer input as rendering every line to it: the
    // live path and resume share one fold, never two implementations.
    for point in [2, 3, 5] {
        let folded = crate::history::forked(&dir, Seq(point)).unwrap();
        let mut live = Vec::new();
        for line in log::read(&dir).unwrap() {
            if line.seq.is_some_and(|seq| seq.0 > point) {
                continue;
            }
            if let Some(event) = Event::from_envelope(&line).unwrap() {
                render_reviewed(&mut live, &event, line.action_id.as_ref(), line.seq);
            }
        }
        assert_eq!(folded.reviewed, live, "at seq {point}");
    }
}
