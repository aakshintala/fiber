//! The first-turn preamble build (`docs/prompt-cache.md`, "The preamble"
//! and `docs/events.md`, "`preamble_built`"): written once per loop before
//! `turn_started`, with the request's bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use contract::events::CacheLifetime;
use fakes::Scripted;
use serde_json::Value;

use support::{MODEL, Session, delivery, kinds};

#[test]
fn a_new_session_builds_the_preamble_before_its_first_turn() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)[0..4],
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started"
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert_eq!(built.payload["reason"], "start");
    assert_eq!(built.payload["model"], MODEL);
    assert_eq!(built.payload["context_window"], 0);
    assert_eq!(built.payload["tool_choice"], "auto");
    assert_eq!(built.payload["cache_lifetime"], "1h");
    assert!(built.payload.get("effort").is_none());
    assert!(built.payload.get("thinking").is_none());
    assert!(built.payload.get("credential").is_none());
    assert_eq!(built.payload["trigger_at"], 400_000);
    // The event's system prompt is the first request's, and its tool
    // choice and cache lifetime are what the request sends.
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(built.payload["system_prompt"], requests[0].system_prompt);
    assert_eq!(
        built.payload["tool_choice"],
        serde_json::json!(requests[0].tool_choice)
    );
    assert_eq!(
        built.payload["cache_lifetime"],
        serde_json::to_value(requests[0].cache_lifetime).unwrap()
    );
    assert_eq!(built.payload["tools"], Value::Array(vec![]));
    assert!(built.payload.get("replaced").is_none());
    // The opening message follows the preamble build, and renders as the
    // conversation's first message.
    let opening = lines.iter().find(|l| l.kind == "opening_message").unwrap();
    assert_eq!(opening.payload["environment"]["date"], "2023-11-14");
    assert_eq!(opening.payload["environment"]["shell"], "/bin/sh");
    assert_eq!(opening.payload["instruction_files"], Value::Array(vec![]));
    assert_eq!(opening.payload["skills"], Value::Array(vec![]));
    let first = &requests[0].conversation[0];
    assert!(
        matches!(first, contract::provider::Input::User { text } if text.starts_with("This message is from Fiber")),
        "{first:?}"
    );
}

#[test]
fn an_unreadable_instruction_file_is_a_notice_after_the_opening_message() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    // A directory where the workspace file should be cannot be read as
    // one: the file is left out and a notice names it.
    std::fs::create_dir_all(session.workspace.join("AGENTS.md")).unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)[0..5],
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "notice",
            "turn_started",
        ]
    );
    let notice = lines.iter().find(|l| l.kind == "notice").unwrap();
    assert_eq!(notice.payload["code"], "io_failed");
    assert!(
        notice.payload["message"]
            .as_str()
            .unwrap()
            .contains(&session.workspace.join("AGENTS.md").display().to_string()),
        "{}",
        notice.payload["message"]
    );
    let opening = lines.iter().find(|l| l.kind == "opening_message").unwrap();
    assert_eq!(opening.payload["instruction_files"], Value::Array(vec![]));
}

#[test]
fn a_tool_registered_twice_records_what_it_replaced() {
    let first: std::sync::Arc<dyn contract::tool::Tool> =
        std::sync::Arc::new(support::TestTool::reads("get_weather", "Sunny."));
    let second: std::sync::Arc<dyn contract::tool::Tool> =
        std::sync::Arc::new(support::TestTool::reads("get_weather", "Rain."));
    // Both register as `builtin`; a later tool of a taken name replaces
    // the earlier one.
    let mut session = Session::open(
        vec![Scripted::text("Done.")],
        Vec::new(),
        vec![first, second],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let built = session
        .lines()
        .into_iter()
        .find(|line| line.kind == "preamble_built")
        .unwrap();
    assert_eq!(
        built.payload["replaced"],
        serde_json::json!([{"name": "get_weather", "from": "builtin", "to": "builtin"}])
    );
    let tools = built.payload["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "get_weather");
    assert_eq!(tools[0]["registered_by"], "builtin");
    assert_eq!(tools[0]["deferred"], false);
    assert!(tools[0].get("definition").is_some());
}

#[test]
fn a_loop_that_takes_no_turn_writes_no_preamble() {
    let session = Session::new(vec![Scripted::text("Done.")], None);
    let lines: Vec<contract::Envelope> = log::read(&session.dir).unwrap();
    assert_eq!(kinds(&lines), ["session_started"]);
}

#[test]
fn an_unattended_loop_carries_the_unattended_line() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None).answerable(false);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    assert!(
        session.requests()[0]
            .system_prompt
            .contains("Nobody is present"),
        "{}",
        session.requests()[0].system_prompt
    );
}

#[test]
fn the_preamble_is_built_once_per_loop_not_per_turn() {
    let mut session = Session::new(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        None,
    );
    session.inbox.send(delivery("one")).unwrap();
    session.turn();
    session.inbox.send(delivery("two")).unwrap();
    session.turn();
    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines.iter().filter(|l| l.kind == "preamble_built").count(),
        1
    );
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].system_prompt, requests[1].system_prompt);
    for request in &requests {
        assert_eq!(request.tool_choice, "auto");
        assert_eq!(request.cache_lifetime, CacheLifetime::OneHour);
    }
}

#[test]
fn trigger_at_is_absent_when_automatic_handoff_is_off() {
    let mut session =
        Session::new(vec![Scripted::text("Done.")], None).handoff(r#loop::HandoffSettings {
            enabled: false,
            ..r#loop::HandoffSettings::default()
        });
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)
            .into_iter()
            .filter(|kind| !kind.ends_with("_delta"))
            .collect::<Vec<_>>(),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert!(built.payload.get("trigger_at").is_none());
}

#[test]
fn trigger_at_follows_the_configured_tokens() {
    let mut session =
        Session::new(vec![Scripted::text("Done.")], None).handoff(r#loop::HandoffSettings {
            tokens: 1_234,
            ..r#loop::HandoffSettings::default()
        });
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)
            .into_iter()
            .filter(|kind| !kind.ends_with("_delta"))
            .collect::<Vec<_>>(),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert_eq!(built.payload["trigger_at"], 1_234);
}
