//! The summary quote is the first usable content after the cut.

#![allow(clippy::unwrap_used, reason = "test fixtures must parse")]

use contract::{Envelope, SessionId};
use serde_json::{Value, json};

use super::excerpt;

fn line(kind: &str, payload: Value) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_quote".into()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().unwrap().clone(),
    }
}

fn text(text: &str) -> Envelope {
    line("text_completed", json!({"text": text}))
}

fn summary() -> Envelope {
    line(
        "turn_started",
        json!({"input":[{"type":"rewind","command_id":"any-id"}]}),
    )
}

#[test]
fn a_message_quotes_its_text_parts_in_order_before_later_text() {
    let message = line(
        "turn_started",
        json!({"input":[{
        "type":"message", "command_id":"c", "source":"driver",
        "content":[{"type":"text","text":"first"}, {"type":"text","text":"second"}]
    }, {"type":"message", "command_id":"d", "source":"driver", "content":[{"type":"text","text":"later message"}]}]}),
    );
    assert_eq!(
        excerpt(&[message, text("later reply")]).unwrap(),
        Some("first\nsecond".into())
    );
}

#[test]
fn jobs_alone_leave_the_quote_to_the_reply() {
    assert_eq!(
        excerpt(&[
            line(
                "turn_started",
                json!({"input":[{"type":"jobs","job_ids":["j_1"]}]})
            ),
            text("reply"),
            text("later")
        ])
        .unwrap(),
        Some("reply".into())
    );
}

#[test]
fn a_tool_call_names_the_tool_before_later_text() {
    assert_eq!(
        excerpt(&[
            line(
                "tool_call_requested",
                json!({"name":"read_file","arguments":{}})
            ),
            text("later")
        ])
        .unwrap(),
        Some("A call to the `read_file` tool.".into())
    );
}

#[test]
fn reasoning_is_not_quoted() {
    assert_eq!(
        excerpt(&[
            line("reasoning_completed", json!({"text":"private thought"})),
            text("public reply")
        ])
        .unwrap(),
        Some("public reply".into())
    );
}

#[test]
fn a_completed_summary_is_skipped_with_its_start_and_end() {
    assert_eq!(
        excerpt(&[
            summary(),
            text("earlier summary"),
            line("turn_completed", json!({"outcome":"completed"})),
            text("real reply")
        ])
        .unwrap(),
        Some("real reply".into())
    );
    assert_eq!(
        excerpt(&[summary(), text("still summarising")]).unwrap(),
        None
    );
}

#[test]
fn a_crashed_summary_stops_being_skipped_at_the_process_start() {
    assert_eq!(
        excerpt(&[
            summary(),
            text("crashed summary"),
            line("fiber_started", json!({"version":"0.0.1","resumed":true})),
            text("after crash")
        ])
        .unwrap(),
        Some("after crash".into())
    );
}

#[test]
fn summary_rendering_preserves_the_persons_input_and_last_step() {
    let mut conversation = Vec::new();
    let mut carry = crate::handoff::Carry::default();
    let mut had = std::collections::BTreeMap::new();
    let action = contract::ActionId("a_real".into());
    let before = [
        line(
            "turn_started",
            json!({"input":[{"type":"message", "source":"driver", "content":[{"type":"text","text":"real prompt"}]}]}),
        ),
        line(
            "tool_call_requested",
            json!({"name":"read_file", "arguments":{"path":"real"}}),
        ),
    ];
    for line in before {
        let event = contract::events::Event::from_envelope(&line)
            .unwrap()
            .unwrap();
        crate::conversation::render(
            &mut conversation,
            &event,
            Some(&action),
            "model",
            &mut had,
            &mut carry,
            None,
        );
    }
    let saved = carry.clone();
    let context = conversation.clone();
    let action = contract::ActionId("a_summary".into());
    for line in [
        summary(),
        line("assistant_message_started", json!({})),
        text("discard"),
        line(
            "tool_call_requested",
            json!({"name":"write_file", "arguments":{}}),
        ),
        line(
            "tool_call_completed",
            json!({"status":"cancelled", "content":[{"type":"text","text":"Cancelled before it ran."}]}),
        ),
        line("turn_completed", json!({"outcome":"completed"})),
    ] {
        let event = contract::events::Event::from_envelope(&line)
            .unwrap()
            .unwrap();
        crate::conversation::render(
            &mut conversation,
            &event,
            Some(&action),
            "model",
            &mut had,
            &mut carry,
            None,
        );
    }
    assert_eq!(carry, saved);
    assert_eq!(conversation, context);
}

#[test]
fn no_qualifying_line_has_no_quote() {
    assert_eq!(excerpt(&[]).unwrap(), None);
    assert_eq!(
        excerpt(&[
            line("step_started", json!({})),
            line("reasoning_completed", json!({"text":"thought"}))
        ])
        .unwrap(),
        None
    );
    assert_eq!(excerpt(&[line("future_kind", json!({}))]).unwrap(), None);
}

#[test]
fn unreadable_excerpt_lines_fail_instead_of_quoting_later_text() {
    for prefix in [vec![], vec![summary()]] {
        let mut lines = prefix;
        lines.push(line("text_completed", json!({"text":7})));
        lines.push(text("later reply"));
        assert!(matches!(excerpt(&lines), Err(crate::Error::Unreadable(_))));
    }
}

#[test]
fn blank_reply_text_and_empty_messages_are_skipped() {
    assert_eq!(
        excerpt(&[text(" \n\t"), text("usable")]).unwrap(),
        Some("usable".into())
    );
    let message = line(
        "turn_started",
        json!({"input":[{"type":"message", "command_id":"c", "source":"driver", "content":[]}]}),
    );
    assert_eq!(
        excerpt(&[message, text("usable")]).unwrap(),
        Some("usable".into())
    );
}
