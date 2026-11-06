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
    Answer, AssistantMessageCompleted, Event, FormAnswer, HandoffCompleted, InputItem, Interaction,
    InteractionRequested, InteractionResolved, KeptMessage, MessageOutcome, Note, Outcome,
    ResolvedBy, ReviewerKept, SessionStarted, SteeringApplied, TextCompleted, ToolCallRequested,
    TurnCompleted, TurnOutcome, TurnStarted, Variables, VariablesSource,
};
use contract::provider::Input;
use contract::shapes::{Choice, ContentPart, Failure, Origin, Question, Sender, True};
use contract::{ActionId, CommandId, ErrorCode, RequestId, Seq, SessionId, TurnId};
use serde_json::json;

use super::{PendingAsks, REPLY_CAP, Reviewed, Shown, render_reviewed};

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

fn text_completed(text: &str) -> Event {
    Event::TextCompleted(TextCompleted {
        text: text.into(),
        provider_item: None,
    })
}

fn assistant_completed(outcome: MessageOutcome) -> Event {
    Event::AssistantMessageCompleted(AssistantMessageCompleted {
        outcome,
        error: None,
    })
}

fn turn_completed(outcome: TurnOutcome) -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome,
        error: None,
        questions: None,
    })
}

fn person_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![message(text, driver())],
    })
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    let action = ActionId("a_1".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
        Some(Seq(13)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 3);
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(12, 0)]),
        None,
        Some(Seq(57)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(Vec::new()),
        None,
        Some(Seq(57)),
        &mut PendingAsks::default(),
    );
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(99, 0), kept(12, 3)]),
        None,
        Some(Seq(57)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        None,
        Some(Seq(31)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(31, 0)]),
        None,
        Some(Seq(57)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(13)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        None,
        Some(Seq(31)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_2".into())),
        Some(Seq(32)),
        &mut PendingAsks::default(),
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
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => panic!("a reviewer item is always a user item"),
        })
        .collect()
}

#[test]
fn a_completed_handoff_drops_earlier_calls_and_keeps_persons_word_for_word() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    assert_eq!(reviewed.len(), 4);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
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
            &mut PendingAsks::default(),
        );
    }
    assert_eq!(reviewed.len(), 2);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
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
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    let before = reviewed.clone();
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
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
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert!(reviewed.is_empty());
}

#[test]
fn a_failed_handoff_keeps_calls_and_persons_byte_identical() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    let before = reviewed.clone();
    render_reviewed(
        &mut reviewed,
        &handoff_failed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn a_cancelled_handoff_keeps_calls_and_persons_byte_identical() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    let before = reviewed.clone();
    render_reviewed(
        &mut reviewed,
        &handoff_cancelled(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn reviewer_kept_after_a_completed_handoff_selects_the_kept_persons() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(31, 0)]),
        None,
        Some(Seq(57)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    assert!(reviewed.is_empty());
}

#[test]
fn successive_completed_handoffs_drop_each_cut_before_them() {
    let mut reviewed = Vec::new();
    busy_turn(&mut reviewed);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("keep the tests fast", driver())],
        }),
        None,
        Some(Seq(51)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_3".into())),
        Some(Seq(52)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(60)),
        &mut PendingAsks::default(),
    );
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(13)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    let after = ActionId("a_2".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&after),
        Some(Seq(41)),
        &mut PendingAsks::default(),
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
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        None,
        Some(Seq(13)),
        &mut PendingAsks::default(),
    );
    for n in 0..300u64 {
        render_reviewed(
            &mut reviewed,
            &Event::ToolCallRequested(call("shell")),
            Some(&ActionId(format!("a_{n}"))),
            Some(Seq(14 + n)),
            &mut PendingAsks::default(),
        );
    }
    let before = reviewed.capacity();
    assert_eq!(reviewed.len(), 302);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(400)),
        &mut PendingAsks::default(),
    );
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
        supervised: false,
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
        Some(after.clone()),
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    // A reply-then-person pair, then a text-then-call-then-person run:
    // only the text after the call answers the last message.
    let third = TurnId("t_3".into());
    let fourth = TurnId("t_4".into());
    log.append(
        &Event::TextCompleted(TextCompleted {
            text: "Done.".into(),
            provider_item: None,
        }),
        Some(after),
        None,
    )
    .unwrap();
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![message("say hi", driver())],
        }),
        Some(third.clone()),
        None,
    )
    .unwrap();
    log.append(
        &Event::TextCompleted(TextCompleted {
            text: "Stale.".into(),
            provider_item: None,
        }),
        Some(third.clone()),
        None,
    )
    .unwrap();
    log.append(
        &Event::ToolCallRequested(call("shell")),
        Some(third.clone()),
        Some(ActionId("a_3".into())),
    )
    .unwrap();
    log.append(
        &Event::TextCompleted(TextCompleted {
            text: "Fresh.".into(),
            provider_item: None,
        }),
        Some(third),
        None,
    )
    .unwrap();
    log.append(
        &Event::TurnStarted(TurnStarted {
            input: vec![message("go", driver())],
        }),
        Some(fourth),
        None,
    )
    .unwrap();
    // Before the cut, at it and after it, folding the log to that point
    // reads the same reviewer input as rendering every line to it: the
    // live path and resume share one fold, never two implementations.
    for point in [2, 3, 5, 7, 8, 10, 11, 12] {
        let folded = crate::history::forked(&dir, Seq(point)).unwrap();
        let mut live = Vec::new();
        for line in log::read(&dir).unwrap() {
            if line.seq.is_some_and(|seq| seq.0 > point) {
                continue;
            }
            if let Some(event) = Event::from_envelope(&line).unwrap() {
                render_reviewed(
                    &mut live,
                    &event,
                    line.action_id.as_ref(),
                    line.seq,
                    &mut PendingAsks::default(),
                );
            }
        }
        assert_eq!(folded.reviewed, live, "at seq {point}");
    }
}

#[test]
fn a_reply_then_a_person_message_keeps_the_pair() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Delete the 3 stale branches?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        reviewed,
        vec![
            Reviewed {
                shown: Shown::Reply("Delete the 3 stale branches?".into()),
                input: Input::User {
                    text: "The agent: Delete the 3 stale branches?".into(),
                    images: Vec::new(),
                },
            },
            Reviewed {
                shown: Shown::Person(kept(21, 0)),
                input: Input::User {
                    text: "The person: yes".into(),
                    images: Vec::new(),
                },
            },
        ],
    );
}

#[test]
fn a_reply_with_no_person_message_after_it_stays_held() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Delete the 3 stale branches?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 1);
    assert!(
        matches!(reviewed[0].shown, Shown::Reply(_)),
        "an unanswered reply stays held at the tail: {reviewed:?}"
    );
    assert!(
        reviewed
            .iter()
            .all(|item| !matches!(item.shown, Shown::Person(_))),
        "no person message follows the reply: {reviewed:?}"
    );
}

#[test]
fn agent_text_then_a_tool_call_then_a_person_message_carries_no_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Delete the 3 stale branches?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed
            .iter()
            .all(|item| !matches!(item.shown, Shown::Reply(_))),
        "a tool call between the text and the message breaks the link: {reviewed:?}"
    );
    assert!(person_text(&reviewed).contains(&"The person: yes"));
}

#[test]
fn an_unshown_tool_call_between_text_and_message_breaks_the_link() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Delete the 3 stale branches?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    // A hosted call carries no action, so the reviewer never sees it, but
    // it still breaks the link.
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed
            .iter()
            .all(|item| !matches!(item.shown, Shown::Reply(_))),
        "an unshown call breaks the link too: {reviewed:?}"
    );
    assert!(person_text(&reviewed).contains(&"The person: yes"));
}

#[test]
fn text_tool_call_text_person_shows_only_the_second_text() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("first"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("second"),
        None,
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(23)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        reviewed,
        vec![
            Reviewed {
                shown: Shown::Call(ActionId("a_1".into())),
                input: Input::User {
                    text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
                    images: Vec::new(),
                },
            },
            Reviewed {
                shown: Shown::Reply("second".into()),
                input: Input::User {
                    text: "The agent: second".into(),
                    images: Vec::new(),
                },
            },
            Reviewed {
                shown: Shown::Person(kept(23, 0)),
                input: Input::User {
                    text: "The person: yes".into(),
                    images: Vec::new(),
                },
            },
        ],
    );
}

#[test]
fn two_text_parts_join_with_a_newline() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("line one"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("line two"),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Reply("line one\nline two".into()),
            input: Input::User {
                text: "The agent: line one\nline two".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn an_empty_or_whitespace_part_adds_nothing() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed(""),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("   "),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "empty parts start no reply: {reviewed:?}"
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("kept"),
        None,
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("  "),
        None,
        Some(Seq(23)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        reviewed,
        vec![Reviewed {
            shown: Shown::Reply("kept".into()),
            input: Input::User {
                text: "The agent: kept".into(),
                images: Vec::new(),
            },
        }],
    );
}

#[test]
fn two_driver_messages_on_one_line_share_one_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("which?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("A", driver()), message("B", driver())],
        }),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        ["The agent: which?", "The person: A", "The person: B"],
    );
    assert_eq!(reviewed[0].shown, Shown::Reply("which?".into()));
    assert_eq!(reviewed[1].shown, Shown::Person(kept(21, 0)));
    assert_eq!(reviewed[2].shown, Shown::Person(kept(21, 1)));
}

#[test]
fn an_extension_message_after_a_reply_removes_it() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("from elsewhere", extension_sender())],
        }),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "a line with no person message settles the held reply: {reviewed:?}"
    );
}

#[test]
fn a_driver_message_with_no_seq_after_a_reply_removes_it() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("run it", driver())],
        }),
        None,
        None,
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "a message with no durable identity settles the held reply: {reviewed:?}"
    );
}

#[test]
fn a_drivers_steering_message_after_a_reply_keeps_it() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        ["The agent: proposal", "The person: and the linter"],
    );
}

#[test]
fn an_extensions_steering_message_after_a_reply_removes_it() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("an extension steers"),
            sender: extension_sender(),
            changed_by: None,
        }),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "steering from elsewhere settles the held reply: {reviewed:?}"
    );
}

#[test]
fn a_failed_assistant_message_removes_the_held_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &assistant_completed(MessageOutcome::Failed),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "a failed reply never reached the person: {reviewed:?}"
    );
}

#[test]
fn a_completed_assistant_message_keeps_the_held_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &assistant_completed(MessageOutcome::Completed),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 1);
    assert_eq!(reviewed[0].shown, Shown::Reply("proposal".into()));
}

#[test]
fn an_interrupted_turn_removes_the_held_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &turn_completed(TurnOutcome::Interrupted),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "an interrupted turn's text answers nothing: {reviewed:?}"
    );
}

#[test]
fn a_failed_turn_removes_the_held_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &turn_completed(TurnOutcome::Failed),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert!(
        reviewed.is_empty(),
        "a failed turn's text answers nothing: {reviewed:?}"
    );
}

#[test]
fn a_completed_turn_keeps_the_held_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("proposal"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &turn_completed(TurnOutcome::Completed),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 1);
    assert_eq!(reviewed[0].shown, Shown::Reply("proposal".into()));
}

#[test]
fn a_reply_at_one_under_and_at_the_cap_shows_whole() {
    for len in [REPLY_CAP - 1, REPLY_CAP] {
        let mut reviewed = Vec::new();
        let body = "x".repeat(len);
        render_reviewed(
            &mut reviewed,
            &text_completed(&body),
            None,
            Some(Seq(20)),
            &mut PendingAsks::default(),
        );
        assert_eq!(reviewed.len(), 1, "at length {len}");
        assert_eq!(
            reviewed[0].input,
            Input::User {
                text: format!("The agent: {body}"),
                images: Vec::new(),
            },
            "at length {len}"
        );
        assert_eq!(
            reviewed[0].shown,
            Shown::Reply(body),
            "the stored text is never cut, at length {len}"
        );
    }
}

#[test]
fn a_reply_one_over_the_cap_cuts_the_head_and_counts_the_rest() {
    let mut reviewed = Vec::new();
    let raw = "x".repeat(REPLY_CAP + 1);
    render_reviewed(
        &mut reviewed,
        &text_completed(&raw),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 1);
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: format!(
                "The agent: {}\n[Reply cut here; characters not shown: 1]",
                "x".repeat(REPLY_CAP)
            ),
            images: Vec::new(),
        },
    );
    assert_eq!(
        reviewed[0].shown,
        Shown::Reply(raw),
        "the stored text is never cut"
    );
}

#[test]
fn the_cap_counts_characters_never_bytes() {
    for (len, count) in [(REPLY_CAP + 1, 1), (REPLY_CAP + 7, 7)] {
        let mut reviewed = Vec::new();
        render_reviewed(
            &mut reviewed,
            &text_completed(&"é".repeat(len)),
            None,
            Some(Seq(20)),
            &mut PendingAsks::default(),
        );
        assert_eq!(reviewed.len(), 1, "at length {len}");
        assert_eq!(
            reviewed[0].input,
            Input::User {
                text: format!(
                    "The agent: {}\n[Reply cut here; characters not shown: {count}]",
                    "é".repeat(REPLY_CAP)
                ),
                images: Vec::new(),
            },
            "at length {len}"
        );
    }
}

#[test]
fn the_cap_applies_across_parts_without_cutting_the_stored_text() {
    let mut reviewed = Vec::new();
    let first = "y".repeat(7_000);
    let second = "z".repeat(50);
    render_reviewed(
        &mut reviewed,
        &text_completed(&first),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed(&second),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    let raw = format!("{first}\n{second}");
    assert_eq!(raw.chars().count(), 7_000 + 1 + 50);
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: format!(
                "The agent: {}\n[Reply cut here; characters not shown: 1051]",
                "y".repeat(REPLY_CAP)
            ),
            images: Vec::new(),
        },
    );
    assert_eq!(
        reviewed[0].shown,
        Shown::Reply(raw),
        "a later part never extends a cut body"
    );
}

/// A reply awaiting its person message, then the message on the next line.
fn pair(reviewed: &mut Vec<Reviewed>, reply: &str, message: &str, at: u64) {
    render_reviewed(
        reviewed,
        &text_completed(reply),
        None,
        Some(Seq(at)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        reviewed,
        &person_turn(message),
        None,
        Some(Seq(at + 1)),
        &mut PendingAsks::default(),
    );
}

#[test]
fn reviewer_kept_keeps_a_pair_and_drops_an_unnamed_pairs_reply() {
    let mut reviewed = Vec::new();
    pair(&mut reviewed, "first?", "one", 20);
    pair(&mut reviewed, "second?", "two", 30);
    assert_eq!(reviewed.len(), 4);
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(31, 0)]),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        ["The agent: second?", "The person: two"],
    );
    assert_eq!(reviewed[0].shown, Shown::Reply("second?".into()));
    assert_eq!(reviewed[1].shown, Shown::Person(kept(31, 0)));
}

#[test]
fn a_kept_second_message_of_a_two_message_line_keeps_the_shared_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("which?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("A", driver()), message("B", driver())],
        }),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(21, 1)]),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        ["The agent: which?", "The person: B"],
    );
}

#[test]
fn a_reply_answers_only_the_run_on_its_own_line() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("which?"),
        None,
        Some(Seq(20)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("A"),
        None,
        Some(Seq(21)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("B"),
        None,
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), 3);
    // Only the later line is named: the reply answered the earlier
    // line, so it goes with it. Keeping the later line must not keep
    // the earlier line's reply.
    render_reviewed(
        &mut reviewed,
        &kept_event(vec![kept(22, 0)]),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(person_text(&reviewed), ["The person: B"]);
    assert_eq!(reviewed[0].shown, Shown::Person(kept(22, 0)));
}

#[test]
fn a_completed_handoff_keeps_the_pairs_and_a_later_yes_carries_no_reply() {
    let mut reviewed = Vec::new();
    pair(&mut reviewed, "first?", "one", 20);
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    pair(&mut reviewed, "second?", "two", 30);
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_2".into())),
        Some(Seq(32)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("unanswered"),
        None,
        Some(Seq(33)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        [
            "The agent: first?",
            "The person: one",
            "The agent: second?",
            "The person: two"
        ],
    );
    // A proposal made before the handoff is cleared: the "yes" after it
    // answers nothing.
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(41)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        [
            "The agent: first?",
            "The person: one",
            "The agent: second?",
            "The person: two",
            "The person: yes"
        ],
    );
}

#[test]
fn a_failed_handoff_with_replies_keeps_everything_byte_identical() {
    let mut reviewed = Vec::new();
    pair(&mut reviewed, "first?", "one", 20);
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("held"),
        None,
        Some(Seq(23)),
        &mut PendingAsks::default(),
    );
    let before = reviewed.clone();
    render_reviewed(
        &mut reviewed,
        &handoff_failed(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn a_cancelled_handoff_with_replies_keeps_everything_byte_identical() {
    let mut reviewed = Vec::new();
    pair(&mut reviewed, "first?", "one", 20);
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(22)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("held"),
        None,
        Some(Seq(23)),
        &mut PendingAsks::default(),
    );
    let before = reviewed.clone();
    render_reviewed(
        &mut reviewed,
        &handoff_cancelled(),
        None,
        Some(Seq(40)),
        &mut PendingAsks::default(),
    );
    assert_eq!(reviewed.len(), before.len());
    assert_eq!(reviewed, before);
}

#[test]
fn successive_completed_handoffs_keep_old_pairs_intact() {
    let mut reviewed = Vec::new();
    pair(&mut reviewed, "first?", "one", 20);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(30)),
        &mut PendingAsks::default(),
    );
    pair(&mut reviewed, "second?", "two", 40);
    render_reviewed(
        &mut reviewed,
        &handoff_completed(),
        None,
        Some(Seq(50)),
        &mut PendingAsks::default(),
    );
    assert_eq!(
        person_text(&reviewed),
        [
            "The agent: first?",
            "The person: one",
            "The agent: second?",
            "The person: two"
        ],
    );
}

#[test]
fn person_pairs_reuse_rendered_replies_for_each_occurrence() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Which color?"),
        None,
        Some(Seq(10)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![message("Blue", driver()), message("Green", driver())],
        }),
        None,
        Some(Seq(11)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &text_completed("Which number?"),
        None,
        Some(Seq(12)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("Seven"),
        None,
        Some(Seq(13)),
        &mut PendingAsks::default(),
    );

    // docs/handoff.md, "What the model sees after a handoff", item 4: "puts the reply it answered before it" — the pairs are the `reviewer_kept` selection unchanged.
    assert_eq!(
        Reviewed::person_pairs(&reviewed),
        [
            (
                kept(11, 0),
                "The agent: Which color?\nThe person: Blue".into()
            ),
            (
                kept(11, 1),
                "The agent: Which color?\nThe person: Green".into()
            ),
            (
                kept(13, 0),
                "The agent: Which number?\nThe person: Seven".into()
            ),
        ]
    );
}

#[test]
fn person_pairs_keep_the_rendered_text_without_guessing_a_reply() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &person_turn("unprompted"),
        None,
        Some(Seq(14)),
        &mut PendingAsks::default(),
    );
    // docs/handoff.md, "What the model sees after a handoff", item 4: "puts the reply it answered before it" — the pairs are the `reviewer_kept` selection unchanged.
    assert_eq!(
        Reviewed::person_pairs(&reviewed),
        [(kept(14, 0), "The person: unprompted".into())]
    );

    render_reviewed(
        &mut reviewed,
        &text_completed(&"é".repeat(REPLY_CAP + 1)),
        None,
        Some(Seq(15)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("yes"),
        None,
        Some(Seq(16)),
        &mut PendingAsks::default(),
    );
    let Input::User { text: reply, .. } = &reviewed[1].input else {
        panic!("not a rendered reply");
    };
    let Input::User { text: person, .. } = &reviewed[2].input else {
        panic!("not a rendered person");
    };
    // docs/handoff.md, "What the model sees after a handoff", item 4: "puts the reply it answered before it" — the pair reuses the reviewer-rendered reply.
    assert_eq!(
        Reviewed::person_pairs(&reviewed)[1].1,
        format!("{reply}\n{person}"),
        "pair extraction reuses the capped reviewer inputs"
    );
}

#[test]
fn a_reply_does_not_cross_a_different_sequence_person() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &text_completed("Which color?"),
        None,
        Some(Seq(10)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("Blue"),
        None,
        Some(Seq(11)),
        &mut PendingAsks::default(),
    );
    render_reviewed(
        &mut reviewed,
        &person_turn("Green"),
        None,
        Some(Seq(12)),
        &mut PendingAsks::default(),
    );

    // docs/handoff.md, "What the model sees after a handoff", item 4: "puts the reply it answered before it" — the pairs are the `reviewer_kept` selection unchanged.
    assert_eq!(
        Reviewed::person_pairs(&reviewed),
        [
            (
                kept(11, 0),
                "The agent: Which color?\nThe person: Blue".into()
            ),
            (kept(12, 0), "The person: Green".into())
        ]
    );
}

// The person's answers reach the reviewer with their eliciting context
// (`docs/permissions.md`, "What it is shown"): every interaction a person
// resolves renders through one rule, whatever its kind and raiser.

fn person_request_id(tag: &str) -> RequestId {
    RequestId(format!("r_person_{tag}"))
}

fn person_requested(
    tag: &str,
    interaction: Interaction,
    action_ids: Option<Vec<ActionId>>,
    extension: Option<&str>,
) -> InteractionRequested {
    InteractionRequested {
        request_id: person_request_id(tag),
        interaction,
        action_ids,
        extension: extension.map(str::to_owned),
        resumes: false,
    }
}

fn tool_ids() -> Option<Vec<ActionId>> {
    Some(vec![ActionId("a_ask".into())])
}

fn person_choice(label: &str) -> Choice {
    Choice {
        label: label.into(),
        description: None,
    }
}

fn person_question(header: &str, question: &str, options: Vec<Choice>) -> Question {
    Question {
        header: header.into(),
        question: question.into(),
        options,
        multi_select: None,
    }
}

fn person_form() -> Interaction {
    Interaction::Form {
        fields: vec![
            person_question(
                "Deploy",
                "Which pipeline should run?",
                vec![
                    Choice {
                        label: "nightly".into(),
                        description: Some("Recommended".into()),
                    },
                    person_choice("weekly"),
                ],
            ),
            person_question("Region", "Which region holds the run?", Vec::new()),
        ],
    }
}

fn person_form_answer() -> Answer {
    Answer::Form {
        answers: vec![
            FormAnswer::Answered {
                labels: vec!["nightly".into()],
                text: Some("us-east-1".into()),
            },
            FormAnswer::Skipped { skipped: True },
        ],
        note: Some("after lunch".into()),
    }
}

/// Stages `requested`, then resolves it as `by` with `answer`: every kind
/// and raiser shape exercises the same render path.
fn person_input_rendered(
    requested: InteractionRequested,
    by: ResolvedBy,
    answer: Answer,
    seq: u64,
) -> Vec<Reviewed> {
    let mut reviewed = Vec::new();
    let mut asks = PendingAsks::default();
    let id = requested.request_id.clone();
    render_reviewed(
        &mut reviewed,
        &Event::InteractionRequested(requested),
        None,
        Some(Seq(seq)),
        &mut asks,
    );
    render_reviewed(
        &mut reviewed,
        &Event::InteractionResolved(InteractionResolved {
            request_id: id,
            by,
            answer,
        }),
        None,
        Some(Seq(seq + 1)),
        &mut asks,
    );
    reviewed
}

fn person_input_texts(reviewed: &[Reviewed]) -> Vec<String> {
    reviewed
        .iter()
        .map(|item| match &item.input {
            Input::User { text, .. } => text.clone(),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => {
                panic!("reviewer input holds user text, got {:?}", item.input)
            }
        })
        .collect()
}

/// Every interaction kind in every raiser shape reaches the reviewer
/// through the one person rule: the person's answer as a `Person` item
/// with its eliciting context as the `Reply` before it. A `fiber`
/// resolution renders nothing in every shape.
#[test]
fn person_input_every_kind_and_raiser_pairs() {
    struct Case {
        interaction: Interaction,
        answer: Answer,
        body: &'static str,
        prompt_words: &'static [&'static str],
    }
    let cases = [
        Case {
            interaction: person_form(),
            answer: person_form_answer(),
            body: "The person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\"",
            prompt_words: &[
                "Which pipeline should run?",
                "Which region holds the run?",
                "nightly",
                "weekly",
                "Recommended",
            ],
        },
        Case {
            interaction: Interaction::Confirm {
                prompt: "ELICIT-CONFIRM shimmer-cove".into(),
            },
            answer: Answer::Confirmed { confirmed: true },
            body: "The person: yes",
            prompt_words: &["ELICIT-CONFIRM shimmer-cove"],
        },
        Case {
            interaction: Interaction::Select {
                prompt: "ELICIT-SELECT glitter-valley".into(),
                options: vec![person_choice("cherry"), person_choice("plum")],
            },
            answer: Answer::Labels {
                labels: vec!["cherry".into()],
            },
            body: "The person: cherry",
            prompt_words: &["ELICIT-SELECT glitter-valley", "cherry", "plum"],
        },
        Case {
            interaction: Interaction::MultiSelect {
                prompt: "ELICIT-MULTI ember-field".into(),
                options: vec![person_choice("fig"), person_choice("plum")],
            },
            answer: Answer::Labels {
                labels: vec!["fig".into(), "plum".into()],
            },
            body: "The person: fig, plum",
            prompt_words: &["ELICIT-MULTI ember-field", "fig", "plum"],
        },
        Case {
            interaction: Interaction::TextInput {
                prompt: "ELICIT-TEXT stone-orchard".into(),
            },
            answer: Answer::Text {
                text: "fig-harvest".into(),
            },
            body: "The person: \"fig-harvest\"",
            prompt_words: &["ELICIT-TEXT stone-orchard", "fig-harvest"],
        },
    ];
    let agent_questions = "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?";
    let extension_channel =
        "Fiber: A question from the garden-helper extension. Its words are not shown.";
    let elicitation_channel =
        "Fiber: A question from a server elicitation. Its words are not shown.";
    let bare_channel = "Fiber: A question with no recorded asker. Its words are not shown.";
    struct Raiser {
        interaction: Interaction,
        action_ids: Option<Vec<ActionId>>,
        extension: Option<&'static str>,
        reply: &'static str,
    }
    let mut seq = 100;
    for (kind_at, case) in cases.iter().enumerate() {
        // One raiser shape with its expected reply: a tool-raised form
        // shows the agent's questions as asked; every other shape shows
        // fiber's own channel description, never the submitted prompt
        // words.
        let raisers = [
            Raiser {
                interaction: case.interaction.clone(),
                action_ids: tool_ids(),
                extension: None,
                reply: if kind_at == 0 {
                    agent_questions
                } else {
                    elicitation_channel
                },
            },
            Raiser {
                interaction: case.interaction.clone(),
                action_ids: None,
                extension: Some("garden-helper"),
                reply: extension_channel,
            },
            Raiser {
                interaction: case.interaction.clone(),
                action_ids: None,
                extension: None,
                reply: bare_channel,
            },
        ];
        for (raiser_at, raiser) in raisers.into_iter().enumerate() {
            seq += 10;
            let tag = format!("k{kind_at}r{raiser_at}s{seq}");
            let reviewed = person_input_rendered(
                person_requested(
                    &tag,
                    raiser.interaction,
                    raiser.action_ids,
                    raiser.extension,
                ),
                ResolvedBy::Person,
                case.answer.clone(),
                seq,
            );
            assert_eq!(reviewed.len(), 2, "kind {kind_at} raiser {raiser_at}");
            assert!(
                matches!(reviewed[0].shown, Shown::Reply(_)),
                "kind {kind_at} raiser {raiser_at}"
            );
            assert_eq!(
                reviewed[1].shown,
                Shown::Person(KeptMessage {
                    seq: Seq(seq + 1),
                    item: 0,
                }),
                "kind {kind_at} raiser {raiser_at}"
            );
            let texts = person_input_texts(&reviewed);
            assert_eq!(texts[0], raiser.reply, "kind {kind_at} raiser {raiser_at}");
            assert_eq!(texts[1], case.body, "kind {kind_at} raiser {raiser_at}");
            if raiser_at > 0 || kind_at > 0 {
                for words in case.prompt_words {
                    assert!(
                        !texts[0].contains(words),
                        "kind {kind_at} raiser {raiser_at}: reply leaks {words:?}"
                    );
                }
            }
            // A `fiber` resolution is no person's input: nothing renders.
            let dropped = person_input_rendered(
                person_requested(&tag, case.interaction.clone(), tool_ids(), None),
                ResolvedBy::Fiber,
                case.answer.clone(),
                seq + 4,
            );
            assert!(
                dropped.is_empty(),
                "kind {kind_at}: a fiber resolve renders nothing"
            );
        }
    }
    // The fail-closed side: a form with empty `action_ids` is no
    // agent-raised form, so its question words stay out.
    seq += 10;
    let reviewed = person_input_rendered(
        person_requested("empty", person_form(), Some(Vec::new()), None),
        ResolvedBy::Person,
        person_form_answer(),
        seq,
    );
    assert_eq!(reviewed.len(), 2);
    let texts = person_input_texts(&reviewed);
    assert_eq!(texts[0], bare_channel);
    assert_eq!(
        texts[1],
        "The person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\""
    );
}

fn person_harbor_form() -> Interaction {
    Interaction::Form {
        fields: vec![person_question(
            "Harbor",
            "Which harbor holds the boats?",
            vec![person_choice("stone"), person_choice("glass")],
        )],
    }
}

fn person_harbor_answer() -> Answer {
    Answer::Form {
        answers: vec![FormAnswer::Answered {
            labels: vec!["glass".into()],
            text: None,
        }],
        note: None,
    }
}

/// Concurrent pending forms pair by `request_id`, not by position: each
/// resolve answers with its own eliciting reply, in either order.
#[test]
fn person_input_two_forms_resolve_reversed() {
    let mut reviewed = Vec::new();
    let mut asks = PendingAsks::default();
    for (seq, event) in [
        (
            200,
            Event::InteractionRequested(person_requested("first", person_form(), tool_ids(), None)),
        ),
        (
            201,
            Event::InteractionRequested(person_requested(
                "second",
                person_harbor_form(),
                tool_ids(),
                None,
            )),
        ),
        (
            202,
            Event::InteractionResolved(InteractionResolved {
                request_id: person_request_id("second"),
                by: ResolvedBy::Person,
                answer: person_harbor_answer(),
            }),
        ),
        (
            203,
            Event::InteractionResolved(InteractionResolved {
                request_id: person_request_id("first"),
                by: ResolvedBy::Person,
                answer: person_form_answer(),
            }),
        ),
    ] {
        render_reviewed(&mut reviewed, &event, None, Some(Seq(seq)), &mut asks);
    }
    // Each resolve appends its own question reply immediately before its
    // person item, so the second request resolved first still pairs exact.
    assert_eq!(reviewed.len(), 4);
    assert!(matches!(reviewed[0].shown, Shown::Reply(_)));
    assert!(matches!(reviewed[2].shown, Shown::Reply(_)));
    assert_eq!(
        reviewed[1].shown,
        Shown::Person(KeptMessage {
            seq: Seq(202),
            item: 0,
        })
    );
    assert_eq!(
        reviewed[3].shown,
        Shown::Person(KeptMessage {
            seq: Seq(203),
            item: 0,
        })
    );
    let texts = person_input_texts(&reviewed);
    assert_eq!(
        texts[0],
        "The agent: Which harbor holds the boats?\n- stone\n- glass"
    );
    assert_eq!(texts[1], "The person: Harbor: glass");
    assert_eq!(
        texts[2],
        "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?"
    );
    assert_eq!(
        texts[3],
        "The person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\""
    );
    assert_eq!(
        Reviewed::person_pairs(&reviewed),
        [
            (
                KeptMessage {
                    seq: Seq(202),
                    item: 0,
                },
                "The agent: Which harbor holds the boats?\n- stone\n- glass\nThe person: Harbor: glass"
                    .into()
            ),
            (
                KeptMessage {
                    seq: Seq(203),
                    item: 0,
                },
                "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?\nThe person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\"".into()
            ),
        ]
    );
}

/// A tool call between the agent's text and the person's answer breaks
/// the link: the staged eliciting context answers, never stale model
/// prose from before the call.
#[test]
fn person_input_answer_with_stale_proposal_before_call() {
    let mut reviewed = Vec::new();
    let mut asks = PendingAsks::default();
    render_reviewed(
        &mut reviewed,
        &text_completed("a stale proposal the person never answered"),
        None,
        Some(Seq(300)),
        &mut asks,
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&ActionId("a_1".into())),
        Some(Seq(301)),
        &mut asks,
    );
    render_reviewed(
        &mut reviewed,
        &Event::InteractionRequested(person_requested("stale", person_form(), tool_ids(), None)),
        None,
        Some(Seq(302)),
        &mut asks,
    );
    render_reviewed(
        &mut reviewed,
        &Event::InteractionResolved(InteractionResolved {
            request_id: person_request_id("stale"),
            by: ResolvedBy::Person,
            answer: person_form_answer(),
        }),
        None,
        Some(Seq(303)),
        &mut asks,
    );
    assert_eq!(reviewed.len(), 3);
    assert!(matches!(reviewed[0].shown, Shown::Call(_)));
    assert!(matches!(reviewed[1].shown, Shown::Reply(_)));
    assert_eq!(
        reviewed[2].shown,
        Shown::Person(KeptMessage {
            seq: Seq(303),
            item: 0,
        })
    );
    let texts = person_input_texts(&reviewed);
    assert!(
        texts.iter().all(|text| !text.contains("stale proposal")),
        "{texts:?}"
    );
    assert_eq!(
        texts[1],
        "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?"
    );
    assert_eq!(
        texts[2],
        "The person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\""
    );
}

/// A person's decline renders the single `declined` line with the staged
/// eliciting content as its reply.
#[test]
fn person_input_person_decline_wording() {
    let reviewed = person_input_rendered(
        person_requested("decline", person_form(), tool_ids(), None),
        ResolvedBy::Person,
        Answer::Declined { declined: True },
        400,
    );
    assert_eq!(reviewed.len(), 2);
    assert!(matches!(reviewed[0].shown, Shown::Reply(_)));
    assert_eq!(
        reviewed[1].shown,
        Shown::Person(KeptMessage {
            seq: Seq(401),
            item: 0,
        })
    );
    let texts = person_input_texts(&reviewed);
    assert_eq!(
        texts[0],
        "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?"
    );
    assert_eq!(texts[1], "The person: declined");
}

/// A resolve with no staged request renders nothing, whoever resolves it,
/// with or without a line of its own.
#[test]
fn person_input_resolve_without_request_ignored() {
    for (by, seq) in [
        (ResolvedBy::Person, Some(Seq(500))),
        (ResolvedBy::Person, None),
        (ResolvedBy::Fiber, Some(Seq(501))),
    ] {
        let mut reviewed = Vec::new();
        let mut asks = PendingAsks::default();
        render_reviewed(
            &mut reviewed,
            &Event::InteractionResolved(InteractionResolved {
                request_id: person_request_id("never-raised"),
                by,
                answer: person_form_answer(),
            }),
            None,
            seq,
            &mut asks,
        );
        assert!(reviewed.is_empty(), "resolve without request renders");
    }
}

/// The kept pair carries the eliciting reply with the answer, so the
/// selection that follows judges the answer against what it answers.
#[test]
fn person_input_kept_answer_carries_question_and_answer() {
    let reviewed = person_input_rendered(
        person_requested("kept", person_form(), tool_ids(), None),
        ResolvedBy::Person,
        person_form_answer(),
        600,
    );
    assert_eq!(
        Reviewed::person_pairs(&reviewed),
        [(
            KeptMessage {
                seq: Seq(601),
                item: 0,
            },
            "The agent: Which pipeline should run?\n- nightly (Recommended)\n- weekly\nWhich region holds the run?\nThe person: Deploy: nightly, \"us-east-1\"\nRegion: skipped\nnote: \"after lunch\"".into()
        )]
    );
}
