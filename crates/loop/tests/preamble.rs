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
    assert_eq!(built.payload["credential"], "work");
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
        matches!(first, contract::provider::Input::User { text , ..} if text.starts_with("This message is from Fiber")),
        "{first:?}"
    );
}

#[test]
fn a_workspace_skill_is_logged_in_the_opening_message_listing() {
    let mut session = Session::new(vec![Scripted::text("Done.")], None);
    let skill = session.workspace.join(".agents/skills/review/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(
        &skill,
        "---\nname: review\ndescription: Reviews a diff.\n---\nBody\n",
    )
    .unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let opening = lines.iter().find(|l| l.kind == "opening_message").unwrap();
    let path = skill.canonicalize().unwrap().display().to_string();
    assert_eq!(
        opening.payload["skills"],
        serde_json::json!([{
            "name": "review",
            "description": "Reviews a diff.",
            "path": path,
            "source": "repository",
        }])
    );
    let requests = session.requests();
    let contract::provider::Input::User { text, .. } = &requests[0].conversation[0] else {
        panic!("{:?}", requests[0].conversation[0]);
    };
    assert!(
        text.ends_with(&format!("- review: Reviews a diff. ({path})\n")),
        "{text}"
    );
    // No notice: the skill is well formed.
    assert!(lines.iter().all(|l| l.kind != "notice"));
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
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
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
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert_eq!(built.payload["trigger_at"], 1_234);
}

fn opening_text(session: &Session) -> String {
    let requests = session.requests();
    let [request] = requests.as_slice() else {
        panic!("one request, got {}", requests.len());
    };
    let [contract::provider::Input::User { text, .. }, ..] = request.conversation.as_slice() else {
        panic!("{:?}", request.conversation);
    };
    text.clone()
}

#[test]
fn an_extension_section_is_logged_and_sent_after_the_instruction_files() {
    let held = fakes::TempDir::new("fiber-preamble-sections");
    let a = held.path().join("a.md");
    let b = held.path().join("b.md");
    std::fs::write(&a, "First.\n").unwrap();
    std::fs::write(&b, "Second.\n").unwrap();
    let mut session = Session::sectioned(
        vec![Scripted::text("Done.")],
        vec![("fiber.test/notes".into(), vec![a.clone(), b.clone()], None)],
    );
    std::fs::write(session.workspace.join("AGENTS.md"), "Leaf rules.\n").unwrap();
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let opening = lines.iter().find(|l| l.kind == "opening_message").unwrap();
    assert_eq!(
        opening.payload["extension_sections"],
        serde_json::json!([{
            "extension": "fiber.test/notes",
            "files": [
                {"path": a.display().to_string(), "content": "First.\n"},
                {"path": b.display().to_string(), "content": "Second.\n"},
            ],
        }])
    );
    // No budget: the key is absent, not null.
    assert!(
        opening.payload["extension_sections"][0]
            .get("budget_bytes")
            .is_none()
    );
    let text = opening_text(&session);
    let leaf_at = text.find("Leaf rules.").unwrap();
    let heading = text.find("# From the fiber.test/notes extension").unwrap();
    assert!(leaf_at < heading, "{text}");
    assert!(
        text.contains(&format!("### {}\n\nFirst.\n", a.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("### {}\n\nSecond.\n", b.display())),
        "{text}"
    );
}

#[test]
fn a_section_whose_files_are_all_missing_has_no_entry() {
    let held = fakes::TempDir::new("fiber-preamble-sections");
    let present = held.path().join("present.md");
    std::fs::write(&present, "Here.\n").unwrap();
    let mut session = Session::sectioned(
        vec![Scripted::text("Done.")],
        vec![
            (
                "fiber.test/gone".into(),
                vec![held.path().join("absent.md")],
                None,
            ),
            (
                "fiber.test/notes".into(),
                vec![held.path().join("absent.md"), present.clone()],
                None,
            ),
        ],
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let opening = lines.iter().find(|l| l.kind == "opening_message").unwrap();
    // The missing file sends nothing, and the all-missing extension has
    // no section: only the present file is recorded.
    assert_eq!(
        opening.payload["extension_sections"],
        serde_json::json!([{
            "extension": "fiber.test/notes",
            "files": [{"path": present.display().to_string(), "content": "Here.\n"}],
        }])
    );
    assert!(lines.iter().all(|l| l.kind != "notice"));
}

#[test]
fn over_budget_ends_the_section_with_the_prune_line_and_under_does_not() {
    let held = fakes::TempDir::new("fiber-preamble-sections");
    let file = held.path().join("index.md");
    std::fs::write(&file, "123456").unwrap();
    for (budget, over) in [(Some(5), true), (Some(6), false)] {
        let mut session = Session::sectioned(
            vec![Scripted::text("Done.")],
            vec![("fiber.test/notes".into(), vec![file.clone()], budget)],
        );
        session.inbox.send(delivery("hi")).unwrap();
        session.turn();
        let lines = session.lines();
        assert_eq!(
            kinds(&lines),
            [
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ]
        );
        let text = opening_text(&session);
        if over {
            assert!(
                text.contains(
                    "Fiber: these files are 6 bytes, over their budget of 5 bytes. Prune them."
                ),
                "{text}"
            );
        } else {
            assert!(!text.contains("Prune"), "{text}");
        }
    }
}

#[test]
fn a_five_minute_input_reaches_the_preamble_and_the_requests() {
    let mut session =
        Session::with_cache_lifetime(vec![Scripted::text("Done.")], CacheLifetime::FiveMinutes);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert_eq!(built.payload["cache_lifetime"], "5m");
    // Every request sends the input's lifetime, and `preamble_built`
    // records it.
    let requests = session.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        assert_eq!(request.cache_lifetime, CacheLifetime::FiveMinutes);
    }
}

#[test]
fn a_thinking_level_is_recorded_and_sent_on_every_request() {
    let mut session = Session::with_thinking(
        vec![Scripted::text("Done.")],
        Some(contract::ThinkingLevel::High),
    );
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    assert_eq!(built.payload["thinking"], "high");
    let requests = session.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].thinking, Some(contract::ThinkingLevel::High));
}

/// Bytes of the definitions a probe session sent in full.
fn definition_bytes(tools: Vec<std::sync::Arc<dyn contract::tool::Tool>>) -> u64 {
    let mut session = Session::windowed(vec![Scripted::text("Done.")], tools, 1_000_000);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let built = session
        .lines()
        .into_iter()
        .find(|line| line.kind == "preamble_built")
        .unwrap();
    built
        .payload
        .get("tools")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| serde_json::to_string(&tool["definition"]).unwrap().len() as u64)
        .sum()
}

fn two_tools() -> Vec<std::sync::Arc<dyn contract::tool::Tool>> {
    vec![
        std::sync::Arc::new(support::TestTool::reads("get_weather", "Sunny.")),
        std::sync::Arc::new(support::TestTool::reads("get_news", "Quiet.")),
    ]
}

const TURN_KINDS: [&str; 12] = [
    "session_started",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

/// The turn's event kinds and its notices' payloads, for a window of
/// `window` tokens.
fn tool_turn(window: u64) -> (Vec<String>, Vec<Value>) {
    let mut session = Session::windowed(vec![Scripted::text("Done.")], two_tools(), window);
    session.inbox.send(delivery("hi")).unwrap();
    session.turn();
    let lines = session.lines();
    let kinds = kinds(&lines).into_iter().map(str::to_owned).collect();
    let notices = lines
        .into_iter()
        .filter(|line| line.kind == "notice")
        .map(|line| Value::Object(line.payload))
        .collect();
    (kinds, notices)
}

#[test]
fn definitions_over_ten_percent_of_the_window_write_tool_definitions_large() {
    let bytes = definition_bytes(two_tools());
    // Four bytes a token: a window one token under 2.5 x bytes puts the
    // definitions just over 10%.
    let (kinds, notices) = tool_turn((bytes * 10).div_ceil(4) - 1);
    let mut expected = TURN_KINDS.to_vec();
    expected.insert(3, "notice");
    assert_eq!(kinds, expected);
    assert_eq!(notices.len(), 1);
    let notice = notices.first().unwrap();
    assert_eq!(notice["code"], "tool_definitions_large");
    let text = notice["message"].as_str().unwrap();
    assert!(text.contains("builtin"), "{text}");
    assert!(text.contains("tools.disabled"), "{text}");
}

#[test]
fn definitions_at_exactly_ten_percent_of_the_window_write_no_notice() {
    let bytes = definition_bytes(two_tools());
    let (kinds, notices) = tool_turn((bytes * 10).div_ceil(4));
    assert_eq!(kinds, TURN_KINDS);
    assert!(notices.is_empty());
}
