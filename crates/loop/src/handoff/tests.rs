//! Tests for the trigger, the context size and the note request.

use std::collections::BTreeMap;

use contract::events::{Control, HandoffCompleted, Note, Outcome, ToolCallRequested};
use contract::provider::Input;
use contract::shapes::{Question, Tokens};
use contract::{ActionId, events::ContextNudged};
use serde_json::json;

use super::{Carry, HandoffSettings, context_tokens, estimate, note_request_text, trigger_at};

fn settings(enabled: bool, tokens: u64, window_fraction: f64) -> HandoffSettings {
    HandoffSettings {
        enabled,
        tokens,
        window_fraction,
        nudge: true,
    }
}

#[test]
fn the_defaults_are_the_documented_ones() {
    let defaults = HandoffSettings::default();
    assert!(defaults.enabled);
    assert_eq!(defaults.tokens, 400_000);
    assert!((defaults.window_fraction - 0.7).abs() < f64::EPSILON);
    assert!(defaults.nudge);
}

#[test]
fn off_has_no_trigger() {
    assert_eq!(trigger_at(&settings(false, 400_000, 0.7), 1_000_000), None);
}

#[test]
fn the_token_trigger_wins_on_a_big_window() {
    assert_eq!(
        trigger_at(&settings(true, 400_000, 0.7), 1_000_000),
        Some(400_000)
    );
}

#[test]
fn the_window_fraction_wins_on_a_small_window() {
    assert_eq!(
        trigger_at(&settings(true, 400_000, 0.7), 200_000),
        Some(140_000)
    );
}

#[test]
fn the_fraction_of_the_window_is_floored() {
    // 0.7 of 1,001 is 700.7.
    assert_eq!(trigger_at(&settings(true, 400_000, 0.7), 1_001), Some(700));
    // 0.5 of 1,001 is 500.5.
    assert_eq!(trigger_at(&settings(true, 400_000, 0.5), 1_001), Some(500));
}

#[test]
fn a_token_trigger_below_the_fraction_is_kept() {
    assert_eq!(
        trigger_at(&settings(true, 1_000, 0.7), 200_000),
        Some(1_000)
    );
}

#[test]
fn context_tokens_sum_the_prompt_and_the_output() {
    let tokens = Tokens {
        input: 1,
        cache_read: 10,
        cache_write: BTreeMap::from([("5m".to_owned(), 100), ("1h".to_owned(), 1_000)]),
        output: 10_000,
    };
    assert_eq!(context_tokens(&tokens), 11_111);
}

fn call(name: &str, arguments: serde_json::Value) -> Input {
    Input::ToolCall {
        action_id: ActionId("a_1".into()),
        call: ToolCallRequested {
            name: name.into(),
            arguments,
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
        model: "m".into(),
    }
}

#[test]
fn an_input_estimates_a_quarter_of_its_bytes_rounded_up() {
    let user = |text: &str| Input::User {
        text: text.into(),
        images: Vec::new(),
    };
    assert_eq!(estimate(&user("")), 0);
    assert_eq!(estimate(&user("abcd")), 1);
    assert_eq!(estimate(&user("abcde")), 2);
    assert_eq!(
        estimate(&Input::Assistant {
            model: "m".into(),
            text: "abcdefgh".into(),
            provider_item: None,
        }),
        2
    );
    assert_eq!(
        estimate(&Input::Reasoning {
            model: "m".into(),
            text: "abcdefghi".into(),
            provider_item: None,
        }),
        3
    );
    assert_eq!(
        estimate(&Input::ToolResult {
            action_id: ActionId("a_1".into()),
            text: "abcd".into(),
            is_error: false,
            images: Vec::new(),
        }),
        1
    );
}

#[test]
fn a_tool_call_estimates_its_name_and_arguments() {
    // `read` is 4 bytes and `{"a":1}` is 7.
    assert_eq!(estimate(&call("read", json!({"a": 1}))), 3);
}

#[test]
fn the_note_request_is_the_note_section_alone_without_instructions() {
    let text = note_request_text("/log/events.jsonl", None);
    assert!(text.starts_with("Fiber is about to restart your context."));
    assert!(text.contains("The session log is at /log/events.jsonl."));
    assert!(text.ends_with("make no tool calls."));
}

#[test]
fn instructions_follow_the_note_section_after_a_blank_line() {
    let plain = note_request_text("/log", None);
    let text = note_request_text("/log", Some("focus on tests"));
    assert_eq!(
        text,
        format!(
            "{plain}\n\nThe person asked that the next stretch of work focus on:\n\nfocus on tests"
        )
    );
}

fn completed(note: Option<Note>) -> HandoffCompleted {
    HandoffCompleted {
        outcome: Outcome::Completed,
        error: None,
        note,
        tokens_before: 0,
        instructions: None,
    }
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

#[test]
fn a_restart_is_the_carried_input_then_the_note() {
    let mut carry = Carry {
        input: vec![user("one"), user("two")],
        texts: vec![
            (ActionId("a_other".into()), "not it".into()),
            (ActionId("a_note".into()), "first".into()),
            (ActionId("a_note".into()), "second".into()),
        ],
        nudged: true,
        ..Carry::default()
    };
    let done = completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    }));
    let restarted = carry.restart(&done);
    assert_eq!(restarted, [user("one"), user("two"), user("first\nsecond")]);
    // A new context: not nudged, and the window's texts are spent.
    assert!(!carry.nudged);
    assert!(carry.texts.is_empty());
}

#[test]
fn a_restart_lists_the_jobs_still_running_in_start_order() {
    let mut carry = Carry {
        jobs: vec![
            ("j_2".into(), "second job".into()),
            ("j_1".into(), "first job".into()),
        ],
        texts: vec![(ActionId("a_note".into()), "note".into())],
        ..Carry::default()
    };
    let done = completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    }));
    let restarted = carry.restart(&done);
    assert_eq!(restarted.len(), 2);
    assert_eq!(restarted[0], user("note"));
    let Input::User { text, .. } = &restarted[1] else {
        panic!("{restarted:?}");
    };
    assert!(
        text.ends_with("\n\n- j_2: second job\n- j_1: first job"),
        "{text}"
    );
}

#[test]
fn a_restart_without_jobs_has_no_jobs_line() {
    let mut carry = Carry {
        texts: vec![(ActionId("a_note".into()), "note".into())],
        ..Carry::default()
    };
    let restarted = carry.restart(&completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    })));
    assert_eq!(restarted, [user("note")]);
}

#[test]
fn a_restart_forgets_the_artifacts_of_results_left_behind() {
    let mut carry = Carry {
        texts: vec![(ActionId("a_note".into()), "note".into())],
        artifacts: vec![(ActionId("a_old".into()), "artifacts/a_old.txt".into())],
        ..Carry::default()
    };
    carry.restart(&completed(Some(Note::Actions {
        note: vec![ActionId("a_note".into())],
    })));
    // Memory follows the context: the old result is not in the new one.
    assert!(carry.artifacts.is_empty());
}

#[test]
fn a_hooks_note_text_is_the_note() {
    let mut carry = Carry::default();
    let restarted = carry.restart(&completed(Some(Note::Hook {
        note_text: "from a hook".into(),
        extension: "ext".into(),
    })));
    assert_eq!(restarted, [user("from a hook")]);
}

#[test]
fn the_nudge_names_its_numbers_and_the_log() {
    let carry = Carry {
        session_log: "/log/events.jsonl".into(),
        ..Carry::default()
    };
    let text = carry.nudge_text(&ContextNudged {
        tokens: 266_700,
        trigger_at: 400_000,
    });
    assert!(text.contains("holds 266700 tokens"), "{text}");
    assert!(text.contains("At 400000 tokens"), "{text}");
    assert!(text.contains("/log/events.jsonl"), "{text}");
}

fn question(text: &str) -> Question {
    Question {
        header: "h".into(),
        question: text.into(),
        options: Vec::new(),
        multi_select: None,
    }
}

fn asking(texts: &[&str]) -> Control {
    Control {
        handoff: None,
        questions: Some(texts.iter().map(|text| question(text)).collect()),
    }
}

/// A carry whose last reply requested `a_1`, `a_2` and `a_3`, in that order.
fn three_calls() -> (Carry, [ActionId; 3]) {
    let mut carry = Carry::default();
    let ids = [1, 2, 3].map(|n| ActionId(format!("a_{n}")));
    for id in &ids {
        carry.call_requested(id, &call("ask_user", json!({})));
    }
    (carry, ids)
}

#[test]
fn the_questions_asked_follow_call_order_not_completion_order() {
    let (mut carry, [first, second, third]) = three_calls();
    carry.call_completed(&third, &user("r3"), None, Some(&asking(&["c"])));
    carry.call_completed(&second, &user("r2"), None, None);
    carry.call_completed(&first, &user("r1"), None, Some(&asking(&["a", "b"])));
    assert_eq!(carry.asked(), [question("a"), question("b"), question("c")]);
    assert!(carry.noted().is_empty());
}

#[test]
fn a_handoff_note_alone_asks_nothing() {
    let (mut carry, [first, second, _]) = three_calls();
    let note = Control {
        handoff: Some("the note".into()),
        questions: None,
    };
    carry.call_completed(&first, &user("r1"), None, Some(&note));
    carry.call_completed(&second, &user("r2"), None, None);
    assert!(carry.asked().is_empty());
    assert_eq!(carry.noted(), [first]);
    assert_eq!(carry.step[0].note.as_deref(), Some("the note"));
}
