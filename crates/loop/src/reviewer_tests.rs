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
use contract::{ActionId, CommandId, Seq};
use serde_json::{Map, Value, json};

use super::shown::Shown;
use super::{First, Second, read_first, read_second, render_reviewed, sections, system_prompt};

const PROMPT: &str = include_str!("../prompt/reviewer.md");

fn sender(origin: Origin) -> Sender {
    Sender {
        origin,
        command_id: Some(CommandId("c_1".into())),
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
        ran_by: None,
        provider_item: None,
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
    assert!(
        split.handoff.starts_with("## handoff\n"),
        "{:?}",
        split.handoff
    );
    assert!(
        split.handoff_reask.starts_with("## handoff-reask\n"),
        "{:?}",
        split.handoff_reask
    );
    assert!(!split.shared.ends_with('\n'));
    assert!(!split.first.ends_with('\n'));
    assert!(!split.handoff.ends_with('\n'));
    assert!(!split.handoff_reask.ends_with('\n'));
    assert!(!split.second.ends_with('\n'));
    assert_eq!(
        format!(
            "{}\n\n{}\n\n{}\n\n{}\n\n{}\n",
            split.shared, split.first, split.handoff, split.handoff_reask, split.second
        ),
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
        Some(Seq(11)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("and the linter"),
            sender: driver(),
            changed_by: None,
        }),
        None,
        Some(Seq(12)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::SteeringApplied(SteeringApplied {
            content: text_part("an extension steers"),
            sender: extension(),
            changed_by: None,
        }),
        None,
        Some(Seq(13)),
    );
    assert_eq!(
        reviewed
            .iter()
            .map(|item| item.input.clone())
            .collect::<Vec<_>>(),
        vec![
            Input::User {
                text: "The person: run the tests".into(),
                images: Vec::new(),
            },
            Input::User {
                text: "The person: and the linter".into(),
                images: Vec::new(),
            },
        ],
    );
    assert!(
        reviewed
            .iter()
            .all(|item| matches!(item.shown, Shown::Person(_)))
    );
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
            provider_item: None,
        }),
    ] {
        render_reviewed(&mut reviewed, &event, Some(&action), Some(Seq(20)));
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
        Some(Seq(21)),
    );
    render_reviewed(
        &mut reviewed,
        &Event::ToolCallRequested(repaired),
        Some(&ActionId("a_8".into())),
        Some(Seq(22)),
    );
    assert_eq!(reviewed.len(), 2);
    assert_eq!(
        reviewed[0].input,
        Input::User {
            text: r#"Tool call: {"tool":"shell","arguments":{"city":"Paris"}}"#.into(),
            images: Vec::new(),
        },
    );
    // The repaired arguments render: the same call renders identically under
    // review and later in history.
    assert_eq!(reviewed[0].input, reviewed[1].input);
    assert!(matches!(&reviewed[0].shown, Shown::Call(call) if call == &action));
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
        let allow = !text.to_ascii_lowercase().contains("check");
        match (read_first(text), allow) {
            (Ok(First::Allow), true) | (Ok(First::Check), false) => {}
            (Ok(_), _) | (Err(_), _) => panic!("{text:?} read wrong"),
        }
    }
    for text in [
        "",
        "maybe",
        "check please",
        "allow, I guess",
        "che ck",
        "all",
    ] {
        assert!(read_first(text).is_err(), "{text:?}");
    }
}

#[test]
fn the_second_stage_reads_a_verdict_and_a_reason() {
    match read_second("allow") {
        Ok(Second::Allow { reason: None }) => {}
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("allow looks routine") {
        Ok(Second::Allow {
            reason: Some(reason),
        }) => assert_eq!(reason, "looks routine"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("`BLOCK` - force-pushes to main") {
        Ok(Second::Block { reason }) => assert_eq!(reason, "force-pushes to main"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    // The verdict word may carry its separator.
    match read_second("allow: looks routine") {
        Ok(Second::Allow {
            reason: Some(reason),
        }) => assert_eq!(reason, "looks routine"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    match read_second("block: force-pushes to main") {
        Ok(Second::Block { reason }) => assert_eq!(reason, "force-pushes to main"),
        Ok(_) | Err(_) => panic!("read wrong"),
    }
    for text in ["", "maybe", "allowing this", ": reason"] {
        assert!(read_second(text).is_err(), "{text:?}");
    }
    // A `block` with no reason is unreadable, with its own message.
    for text in ["block", "block:", "block: "] {
        assert_eq!(
            read_second(text),
            Err("a `block` needs a reason in one sentence, but got none".to_owned()),
            "{text:?}"
        );
    }
}

#[test]
fn the_system_prompt_carries_the_notes_after_the_shared_instructions() {
    let shared = "## shared\n\nBe careful.";
    assert_eq!(system_prompt(shared, ""), shared);
    assert_eq!(system_prompt(shared, "  \n "), shared);
    let notes = "## Notes that hold everywhere\n\nOur org is acme.";
    assert_eq!(system_prompt(shared, notes), format!("{shared}\n\n{notes}"));
}
