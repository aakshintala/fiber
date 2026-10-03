//! Step 7's prompt sections and transcript (`docs/permissions.md`, "The
//! reviewer"): what the reviewer is shown and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use contract::events::{
    Event, InputItem, ReasoningCompleted, SteeringApplied, TextCompleted, ToolCallArgumentsDelta,
    ToolCallCompleted, ToolCallRequested, TurnStarted,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{ActionId, CommandId};
use serde_json::{Map, Value, json};

use super::{First, Second, read_first, read_second, render_reviewed, sections};

const PROMPT: &str = include_str!("../prompt/reviewer.md");

fn sender(origin: Origin) -> Sender {
    Sender {
        origin,
        command_id: CommandId("c_1".into()),
    }
}

fn driver() -> Sender {
    sender(Origin::Driver)
}

fn extension() -> Sender {
    sender(Origin::Extension {
        extension: "ext".into(),
    })
}

fn session() -> Sender {
    sender(Origin::Session {
        from_session_id: contract::SessionId("s_other".into()),
    })
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

fn call(name: &str) -> ToolCallRequested {
    ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
    }
}

#[test]
fn sections_run_heading_to_heading_with_blank_ends_removed() {
    let split = sections();
    assert!(
        split.shared.starts_with("## shared\n"),
        "{:?}",
        split.shared
    );
    assert!(
        split.first.starts_with("## first-pass\n"),
        "{:?}",
        split.first
    );
    assert!(
        split.second.starts_with("## second-pass\n"),
        "{:?}",
        split.second
    );
    assert!(!split.shared.ends_with('\n'));
    assert!(!split.first.ends_with('\n'));
    assert!(!split.second.ends_with('\n'));
    assert_eq!(
        format!("{}\n\n{}\n\n{}\n", split.shared, split.first, split.second),
        PROMPT,
    );
}

#[test]
fn only_the_persons_messages_reach_the_reviewer() {
    let mut reviewed = Vec::new();
    render_reviewed(
        &mut reviewed,
        &Event::TurnStarted(TurnStarted {
            input: vec![
                message("run the tests", driver()),
                message("do it faster", extension()),
                message("from elsewhere", session()),
            ],
        }),
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
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("an extension steers"),
            sender: extension(),
            changed_by: None,
        }),
        None,
    );
    assert_eq!(
        reviewed
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>(),
        vec![
            Input::User {
                text: "The person: run the tests".into(),
            },
            Input::User {
                text: "The person: and the linter".into(),
            },
        ],
    );
    assert!(reviewed.iter().all(|item| item.action.is_none()));
}

#[test]
fn the_models_prose_reasoning_and_results_are_left_out() {
    let mut reviewed = Vec::new();
    let action = ActionId("a_1".into());
    for event in [
        Event::TextCompleted(TextCompleted {
            text: "I will run the tests.".into(),
            provider_item: None,
        }),
        Event::ReasoningCompleted(ReasoningCompleted {
            text: "Force-pushes are risky.".into(),
            provider_item: None,
        }),
        Event::ToolCallArgumentsDelta(ToolCallArgumentsDelta {
            index: 0,
            name: Some("shell".into()),
            text: "{}".into(),
        }),
        Event::ToolCallCompleted(ToolCallCompleted {
            status: contract::events::CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: text_part("all green"),
            details: None,
            artifact: None,
            changes: None,
            control: None,
            changed_by: None,
        }),
    ] {
        render_reviewed(&mut reviewed, &event, Some(&action));
    }
    assert!(reviewed.is_empty());
}

#[test]
fn a_call_renders_with_the_arguments_that_run() {
    let mut repaired = call("shell");
    let mut map = Map::new();
    map.insert("city".into(), Value::String("Paris".into()));
    repaired.repair = Some(contract::events::ArgumentRepair {
        repaired: map,
        repairs: Vec::new(),
    });
    let mut reviewed = Vec::new();
    let action = ActionId("a_7".into());
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(call("shell")),
        Some(&action),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(repaired),
        Some(&ActionId("a_8".into())),
    );
    assert_eq!(reviewed.len(), 2);
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
        },
    );
    // The repaired arguments render: the same call renders identically under
    // review and later in history.
    assert_eq!(reviewed[0].input, reviewed[1].input);
    assert_eq!(reviewed[0].action, Some(action));
}

#[test]
fn the_first_stage_reads_one_token() {
    for text in [
        "check",
        "check\n",
        "  CHECK  ",
        "Allow.",
        "`allow`",
        "\"check\"",
        "'CHECK'.",
    ] {
        let verdict = read_first(text);
        let expected = clean_is_allow(text);
        match (verdict, expected) {
            (First::Allow, true) | (First::Check, false) => {}
            (First::Allow, false) | (First::Check, true) | (First::Unreadable(_), _) => {
                panic!("{text:?} read wrong")
            }
        }
    }
    for text in ["", "maybe", "check please", "allow, I guess", "che ck"] {
        assert!(matches!(read_first(text), First::Unreadable(_)), "{text:?}");
    }
}

/// Whether `text` in the list above means allow.
fn clean_is_allow(text: &str) -> bool {
    !text.to_ascii_lowercase().contains("check")
}

#[test]
fn the_second_stage_reads_a_verdict_and_a_reason() {
    match read_second("allow") {
        Second::Allow { reason: None } => {}
        Second::Allow { reason: Some(_) } | Second::Block { .. } | Second::Unreadable(_) => {
            panic!("read wrong")
        }
    }
    match read_second("allow looks routine") {
        Second::Allow {
            reason: Some(reason),
        } => assert_eq!(reason, "looks routine"),
        Second::Allow { reason: None } | Second::Block { .. } | Second::Unreadable(_) => {
            panic!("read wrong")
        }
    }
    match read_second("`BLOCK` - force-pushes to main") {
        Second::Block { reason } => assert_eq!(reason, "force-pushes to main"),
        Second::Allow { .. } | Second::Unreadable(_) => panic!("read wrong"),
    }
    for text in ["", "maybe", "block", "block: ", "allowing this"] {
        assert!(
            matches!(read_second(text), Second::Unreadable(_)),
            "{text:?}"
        );
    }
}
