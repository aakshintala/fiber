//! Tests for terminal state.

use super::{App, Effect, QUIT_WINDOW};
use crate::keys::{Key, Parser};
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;
use std::time::Duration;

/// Creates an app in `/w`.
fn app() -> App {
    App::new(PathBuf::from("/w"))
}

/// Connects the app to the hub.
fn connect(app: &mut App) {
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: Default::default(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
}

/// Types `text` and presses Enter, returning the effect.
fn send(app: &mut App, text: &str, now: std::time::Instant) -> Effect {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    app.on_key(Key::Enter, now)
}

/// The single line an effect sends.
fn one_line(effect: Effect) -> String {
    match effect {
        Effect::Send(lines) => {
            assert_eq!(lines.len(), 1);
            lines.into_iter().next().unwrap_or_default()
        }
        Effect::None | Effect::Quit => panic!("expected one line"),
    }
}

/// Parses one command line.
fn parse(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_default()
}

/// Starts the app and sends `hi`, returning the `start` id.
fn started(app: &mut App, now: std::time::Instant) -> String {
    connect(app);
    let line = one_line(send(app, "hi", now));
    let value = parse(&line);
    assert_eq!(value.get("command"), Some(&serde_json::json!("start")));
    assert_eq!(
        value
            .get("args")
            .and_then(|args| args.get("workspace"))
            .and_then(serde_json::Value::as_str),
        Some("/w")
    );
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Accepts `start` for `session`, returning the subscribe line.
fn accept_start(app: &mut App, command_id: &str, session: &str) -> String {
    let hello = contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "command_id": command_id,
            "result": {"session_id": session},
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    };
    let lines = app.on_line(Line::Hub(hello));
    assert_eq!(lines.len(), 1);
    lines.into_iter().next().unwrap_or_default()
}

/// Attaches the app to `session`.
fn attach(app: &mut App, now: std::time::Instant, session: &str) {
    let id = started(app, now);
    let sub = accept_start(app, &id, session);
    let value = parse(&sub);
    assert_eq!(value.get("command"), Some(&serde_json::json!("subscribe")));
    assert_eq!(
        value.get("session_id").and_then(serde_json::Value::as_str),
        Some(session)
    );
}

/// One session envelope.
fn session_line(
    session: &str,
    kind: &str,
    payload: serde_json::Value,
    action: Option<&str>,
) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `turn_started` line with one message.
fn turn_started(session: &str, text: &str) -> Line {
    session_line(
        session,
        "turn_started",
        serde_json::json!({"input": [{
            "type": "message",
            "source": "driver",
            "content": [{"type": "text", "text": text}],
        }]}),
        None,
    )
}

/// A `turn_completed` line.
fn turn_completed(session: &str, outcome: &str) -> Line {
    let payload = if outcome == "failed" {
        serde_json::json!({"outcome": outcome, "error": {"code": "io_failed", "message": "boom"}})
    } else {
        serde_json::json!({"outcome": outcome})
    };
    session_line(session, "turn_completed", payload, None)
}

#[test]
fn first_enter_sends_start() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    let id = started(&mut app, now);
    assert!(id.starts_with("c_"));
    assert_eq!(app.draft(), "");
}

#[test]
fn start_accepted_sends_subscribe_full() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    assert!(app.session().is_some());
}

#[test]
fn enter_idle_sends_prompt() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "again", now));
    let value = parse(&line);
    assert_eq!(value.get("command"), Some(&serde_json::json!("prompt")));
    assert_eq!(
        value.get("session_id").and_then(serde_json::Value::as_str),
        Some("s_aaaaaaaaaaaaaaaa")
    );
}

#[test]
fn enter_busy_sends_steer() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    let line = one_line(send(&mut app, "use x", now));
    let value = parse(&line);
    assert_eq!(value.get("command"), Some(&serde_json::json!("steer")));
}

#[test]
fn esc_busy_sends_cancel() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    match app.on_key(Key::Esc, now) {
        Effect::Send(lines) => {
            assert_eq!(lines.len(), 1);
            let value = parse(&lines[0]);
            assert_eq!(value.get("command"), Some(&serde_json::json!("cancel")));
        }
        Effect::None | Effect::Quit => panic!("expected cancel"),
    }
}

#[test]
fn esc_idle_does_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
}

#[test]
fn empty_enter_does_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    connect(&mut app);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    for ch in "   ".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
}

#[test]
fn enter_while_start_pending_does_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    connect(&mut app);
    let line = one_line(send(&mut app, "hi", now));
    assert_eq!(
        parse(&line).get("command"),
        Some(&serde_json::json!("start"))
    );
    for ch in "more".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "more");
}

#[test]
fn enter_before_connect_holds_start_until_hub_hello() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    assert_eq!(send(&mut app, "hi", now), Effect::None);
    assert_eq!(app.draft(), "");
    // A second Enter while `start` is held does nothing.
    assert_eq!(send(&mut app, "more", now), Effect::None);
    assert_eq!(app.draft(), "more");
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: Default::default(),
    };
    let lines = app.on_line(Line::Hub(hello));
    assert_eq!(lines.len(), 1);
    let value = parse(&lines[0]);
    assert_eq!(value["command"], "start");
    assert_eq!(value["args"]["content"][0]["text"], "hi");
}

#[test]
fn a_failed_connect_returns_a_held_start_to_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    assert_eq!(send(&mut app, "hi", now), Effect::None);
    app.connect_failed("Could not reach the hub: gone".to_owned());
    assert_eq!(app.notice(), Some("Could not reach the hub: gone"));
    assert_eq!(app.draft(), "hi");
    // Enter with no hub to send to keeps the draft.
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "hi");
    // Nothing is held any more: a hub that speaks later gets no `start`.
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: Default::default(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    // Back to no session: the next Enter sends `start` again.
    let line = one_line(app.on_key(Key::Enter, now));
    assert_eq!(parse(&line)["command"], "start");
}

#[test]
fn a_schema_mismatch_keeps_a_held_start_back() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    assert_eq!(send(&mut app, "hi", now), Effect::None);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION + 1,
        payload: Default::default(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    assert_eq!(app.draft(), "hi");
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "hi");
}

#[test]
fn after_the_connection_is_lost_enter_and_esc_keep_still() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.disconnected();
    assert_eq!(send(&mut app, "more", now), Effect::None);
    assert_eq!(app.draft(), "more");
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
}

#[test]
fn ctrl_c_clears_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    for ch in "hi".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
    assert_eq!(app.draft(), "");
    assert!(!app.hint());
}

#[test]
fn ctrl_c_twice_within_the_window_quits() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.hint());
    clock.advance(
        QUIT_WINDOW
            .checked_sub(Duration::from_millis(1))
            .unwrap_or(QUIT_WINDOW),
    );
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::Quit);
}

#[test]
fn ctrl_c_at_the_window_edge_rearms() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    clock.advance(QUIT_WINDOW);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert!(app.hint());
}

#[test]
fn ctrl_c_then_another_key_disarms() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::Char('a'), clock.now()), Effect::None);
    assert!(!app.hint());
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
}

#[test]
fn rejection_restores_the_draft_only_when_empty() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "again", now));
    let id = parse(&line)
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let rejected = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "busy turn"}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "again");
    assert!(app.notice().is_some());

    let line = one_line(send(&mut app, "next", now));
    let id = parse(&line)
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    for ch in "typed".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    let rejected = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "busy again"}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert_eq!(app.draft(), "typed");
}

#[test]
fn rejected_cancel_shows_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    let id = match app.on_key(Key::Esc, now) {
        Effect::Send(lines) => parse(&lines[0])
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Effect::None | Effect::Quit => panic!("expected cancel"),
    };
    let rejected = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "stale_request", "message": "no turn"}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(app.notice().is_none());
}

#[test]
fn rejected_start_returns_to_connected() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    let id = started(&mut app, now);
    let rejected = contract::HubLine {
        kind: "command_rejected".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": id, "code": "io_failed", "message": "no hub"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    assert!(app.on_line(Line::Hub(rejected)).is_empty());
    assert!(app.notice().is_some());
    let line = one_line(send(&mut app, "retry", now));
    assert_eq!(
        parse(&line).get("command"),
        Some(&serde_json::json!("start"))
    );
}

#[test]
fn unknown_command_ids_are_ignored() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    let rejected = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": "c_unknown", "code": "busy", "message": "busy"}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(app.notice().is_none());
}

#[test]
fn lines_for_another_session_are_ignored() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_bbbbbbbbbbbbbbbb", "hi"));
    assert!(app.lines().is_empty());
    let line = one_line(send(&mut app, "again", clock.now()));
    assert_eq!(
        parse(&line).get("command"),
        Some(&serde_json::json!("prompt"))
    );
}

#[test]
fn schema_mismatch_is_a_notice() {
    let mut app = app();
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION + 1,
        payload: Default::default(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    assert_eq!(
        app.notice(),
        Some(format!(
            "The hub runs schema version {}; this terminal reads {}.",
            contract::SCHEMA_VERSION + 1,
            contract::SCHEMA_VERSION
        ))
        .as_deref()
    );
}

#[test]
fn connection_lost_is_a_notice() {
    let mut app = app();
    connect(&mut app);
    assert!(app.connected());
    app.disconnected();
    assert!(!app.connected());
    assert_eq!(app.notice(), Some("Connection lost."));
}

#[test]
fn a_refused_schema_keeps_its_notice_when_the_hub_hangs_up() {
    let mut app = app();
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION + 1,
        payload: Default::default(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    assert!(!app.connected());
    app.disconnected();
    assert!(
        app.notice()
            .is_some_and(|notice| notice.contains("schema version"))
    );
}

#[test]
fn a_failed_write_loses_the_connection_and_returns_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "again", now));
    assert_eq!(app.draft(), "");
    app.write_failed(&[line]);
    assert!(!app.connected());
    assert_eq!(app.notice(), Some("Connection lost."));
    assert_eq!(app.draft(), "again");
    // Nothing goes out on a lost connection; the draft stays.
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "again");
}

#[test]
fn a_rejection_after_acceptance_is_ignored() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "again", now));
    let id = parse(&line)
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let accepted = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_accepted",
        serde_json::json!({"command_id": id}),
        None,
    );
    assert!(app.on_line(accepted).is_empty());
    let rejected = session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "busy"}),
        None,
    );
    assert!(app.on_line(rejected).is_empty());
    assert!(app.notice().is_none());
    assert_eq!(app.draft(), "");
}

#[test]
fn turn_started_sets_busy_and_completed_clears_it() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    let line = one_line(send(&mut app, "more", now));
    assert_eq!(
        parse(&line).get("command"),
        Some(&serde_json::json!("steer"))
    );
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_applied",
        serde_json::json!({"content": [{"type": "text", "text": "more"}], "source": "driver"}),
        None,
    ));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    let line = one_line(send(&mut app, "next", now));
    assert_eq!(
        parse(&line).get("command"),
        Some(&serde_json::json!("prompt"))
    );
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "next"));
    assert_eq!(
        app.lines(),
        vec![
            "› hi".to_owned(),
            "steer · more".to_owned(),
            "▣ completed".to_owned(),
            "› next".to_owned(),
        ]
    );
}

#[test]
fn failed_turn_closes_with_its_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "failed"));
    assert_eq!(app.lines().last(), Some(&"▣ failed · boom".to_owned()));
}

#[test]
fn deltas_accumulate_and_completed_replaces() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_delta",
        serde_json::json!({"text": "Hel"}),
        Some("a_1"),
    ));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "assistant_message_delta",
        serde_json::json!({"text": "lo"}),
        Some("a_1"),
    ));
    assert!(app.lines().contains(&"Hello".to_owned()));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({"text": "Hi."}),
        Some("a_1"),
    ));
    assert!(app.lines().contains(&"Hi.".to_owned()));
}

#[test]
fn each_action_streams_its_own_reply() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    for (action, text) in [("a_1", "one"), ("a_2", "two"), ("a_1", " more")] {
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "assistant_message_delta",
            serde_json::json!({ "text": text }),
            Some(action),
        ));
    }
    assert_eq!(app.lines(), vec!["one more".to_owned(), "two".to_owned()]);
}

#[test]
fn steering_applied_is_a_line() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "steering_applied",
        serde_json::json!({"content": [{"type": "text", "text": "use x"}], "source": "driver"}),
        None,
    ));
    assert!(app.lines().contains(&"steer · use x".to_owned()));
}

#[test]
fn keys_feed_into_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut parser = Parser::default();
    let mut app = app();
    for event in parser.feed(b"hi\x7f!") {
        if let crate::keys::Event::Key(key) = event {
            assert_eq!(app.on_key(key, now), Effect::None);
        }
    }
    assert_eq!(app.draft(), "h!");
}

#[test]
fn the_conversation_gives_up_a_row_each_for_input_hint_and_notice() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.set_size(60, 12);
    assert_eq!(app.conversation_height(), 11);
    app.on_key(Key::CtrlC, now);
    assert_eq!(app.conversation_height(), 10);
    connect(&mut app);
    app.disconnected();
    assert_eq!(app.conversation_height(), 9);
    app.on_key(Key::Char('x'), now);
    assert_eq!(app.conversation_height(), 10);
    app.set_size(60, 1);
    assert_eq!(app.conversation_height(), 0);
}

#[test]
fn command_ids_are_c_and_sixteen_fresh_hex_digits() {
    let first = super::mint();
    let second = super::mint();
    for id in [&first, &second] {
        let hex = id.strip_prefix("c_").unwrap_or_default();
        assert_eq!(hex.len(), 16, "{id}");
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{id}"
        );
    }
    assert_ne!(first, second);
}

const S_A: &str = "s_aaaaaaaaaaaaaaaa";
const S_B: &str = "s_bbbbbbbbbbbbbbbb";

/// A `tool_call_requested` for `action`.
fn call(session: &str, action: &str, name: &str, arguments: serde_json::Value) -> Line {
    session_line(
        session,
        "tool_call_requested",
        serde_json::json!({"name": name, "arguments": arguments}),
        Some(action),
    )
}

/// A standing ask by a global rule on `prefix`.
fn standing_ask(session: &str, action: &str, request: &str, prefix: &str) -> Line {
    session_line(
        session,
        "permission_requested",
        serde_json::json!({
            "request_id": request, "effects": ["executes"], "reversible": true,
            "step": "standing_ask",
            "standing_rule": {"scope": "global", "prefix": prefix},
        }),
        Some(action),
    )
}

/// A review request with `extra` keys (`escalation`, `rule`).
fn review(session: &str, action: &str, request: &str, extra: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "request_id": request, "effects": ["executes"], "reversible": true, "step": "review",
    });
    if let (Some(into), Some(from)) = (payload.as_object_mut(), extra.as_object()) {
        into.extend(from.clone());
    }
    session_line(session, "permission_requested", payload, Some(action))
}

/// A review request offering the rule `npm test`.
fn offering(session: &str, request: &str) -> Line {
    review(
        session,
        "a_9",
        request,
        serde_json::json!({"rule": {"subject": "npm test --watch", "prefix": "npm test"}}),
    )
}

/// A `permission_resolved` for `request`.
fn resolved(session: &str, request: &str) -> Line {
    session_line(
        session,
        "permission_resolved",
        serde_json::json!({"request_id": request, "decision": "deny", "decided_by": "cancel"}),
        Some("a_1"),
    )
}

/// An attached, connected app.
fn attached(now: std::time::Instant) -> App {
    let mut app = app();
    attach(&mut app, now, S_A);
    app
}

/// The panel's lines, or none when it is closed.
fn panel(app: &App) -> Vec<String> {
    app.panel().map(|panel| panel.lines).unwrap_or_default()
}

/// The panel's header line.
fn header(app: &App) -> String {
    panel(app).first().cloned().unwrap_or_default()
}

/// Presses `key` `times` times, each doing nothing visible to the hub.
fn press(app: &mut App, key: Key, times: usize, now: std::time::Instant) {
    for _ in 0..times {
        assert_eq!(app.on_key(key.clone(), now), Effect::None);
    }
}

/// The reply an Enter sends, with its `id` taken out.
fn reply(app: &mut App, now: std::time::Instant) -> serde_json::Value {
    let mut value = parse(&one_line(app.on_key(Key::Enter, now)));
    let id = value
        .as_object_mut()
        .and_then(|line| line.remove("id"))
        .unwrap_or_default();
    assert!(id.as_str().is_some_and(|id| id.starts_with("c_")));
    value
}

#[test]
fn a_standing_ask_opens_the_panel_and_enter_allows_once() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(call(
        S_A,
        "a_1",
        "shell",
        serde_json::json!({"command": "echo hi"}),
    ));
    app.on_line(standing_ask(S_A, "a_1", "r_1", "echo hi"));
    assert_eq!(
        panel(&app),
        vec![
            format!("approval · {S_A} · 1 of 1"),
            "asked by a global rule: echo hi".to_owned(),
            r#"shell {"command":"echo hi"}"#.to_owned(),
            "› allow once".to_owned(),
            "  deny · type to add feedback".to_owned(),
        ]
    );
    assert_eq!(app.panel().map(|panel| panel.alert), Some(false));
    assert_eq!(
        reply(&mut app, now),
        serde_json::json!({"command": "reply", "session_id": S_A,
            "args": {"request_id": "r_1", "decision": "allow"}})
    );
    assert!(app.panel().is_none());
    assert!(app.badge().is_none());
}

#[test]
fn a_request_offering_a_rule_shows_both_remembering_rows() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(offering(S_A, "r_1"));
    assert_eq!(
        panel(&app),
        vec![
            format!("approval · {S_A} · 1 of 1"),
            "no rule allows this call".to_owned(),
            "› allow once".to_owned(),
            "  allow for this session: npm test".to_owned(),
            "  always allow in this project: npm test".to_owned(),
            "  deny · type to add feedback".to_owned(),
        ]
    );
}

#[test]
fn each_choice_sends_its_answer() {
    let now = fakes::clock::FakeClock::new().now();
    let cases = [
        (
            1,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "session", "prefix": "npm test"}}),
        ),
        (
            2,
            serde_json::json!({"request_id": "r_1", "decision": "allow",
            "remember": {"scope": "project", "prefix": "npm test"}}),
        ),
        (
            3,
            serde_json::json!({"request_id": "r_1", "decision": "deny"}),
        ),
    ];
    for (downs, args) in cases {
        let mut app = attached(now);
        app.on_line(offering(S_A, "r_1"));
        press(&mut app, Key::Down, downs, now);
        assert_eq!(
            reply(&mut app, now),
            serde_json::json!({"command": "reply", "session_id": S_A, "args": args}),
            "{downs} down"
        );
    }
}

#[test]
fn feedback_goes_with_deny_only_when_it_has_text() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(offering(S_A, "r_1"));
    press(&mut app, Key::Char(' '), 3, now);
    assert_eq!(
        reply(&mut app, now)["args"],
        serde_json::json!({"request_id": "r_1", "decision": "deny"})
    );
    app.on_line(offering(S_A, "r_2"));
    for ch in "use pnpmx".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now), Effect::None);
    }
    assert_eq!(app.on_key(Key::Backspace, now), Effect::None);
    assert_eq!(
        panel(&app).last().map(String::as_str),
        Some("› deny · use pnpm")
    );
    // The draft is untouched while the panel is open.
    assert_eq!(app.draft(), "");
    assert_eq!(
        reply(&mut app, now)["args"],
        serde_json::json!({"request_id": "r_2", "decision": "deny", "feedback": "use pnpm"})
    );
}

#[test]
fn typing_or_backspace_moves_the_cursor_to_deny() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(offering(S_A, "r_1"));
    app.on_key(Key::Char('x'), now);
    assert!(panel(&app).last().is_some_and(|row| row.starts_with('›')));
    press(&mut app, Key::Up, 3, now);
    assert_eq!(panel(&app).get(2).map(String::as_str), Some("› allow once"));
    app.on_key(Key::Backspace, now);
    assert!(panel(&app).last().is_some_and(|row| row.starts_with('›')));
    assert_eq!(
        panel(&app).last().map(String::as_str),
        Some("› deny · type to add feedback")
    );
}

#[test]
fn up_and_down_stop_at_the_ends() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "echo hi"));
    // A standing ask offers no rule: one Down reaches deny, and more stay.
    press(&mut app, Key::Down, 5, now);
    assert_eq!(reply(&mut app, now)["args"]["decision"], "deny");
    app.on_line(offering(S_A, "r_2"));
    press(&mut app, Key::Down, 5, now);
    press(&mut app, Key::Up, 2, now);
    assert_eq!(reply(&mut app, now)["args"]["remember"]["scope"], "session");
    app.on_line(offering(S_A, "r_3"));
    press(&mut app, Key::Down, 1, now);
    press(&mut app, Key::Up, 5, now);
    assert_eq!(
        reply(&mut app, now)["args"],
        serde_json::json!({"request_id": "r_3", "decision": "allow"})
    );
}

#[test]
fn the_panel_says_why_it_asked_and_tints_an_escalation() {
    let now = fakes::clock::FakeClock::new().now();
    let cases = [
        (
            serde_json::json!({"escalation": {"cause": "consecutive_blocks", "reason": "rm -rf"}}),
            "the reviewer escalated: rm -rf",
        ),
        (
            serde_json::json!({"escalation": {"cause": "session_blocks", "reason": "too many"}}),
            "the reviewer escalated: too many",
        ),
        (
            serde_json::json!({"escalation": {"cause": "reviewer_failed",
                "error": {"code": "io_failed", "message": "no model"}}}),
            "the reviewer failed: no model",
        ),
    ];
    for (extra, why) in cases {
        let mut app = attached(now);
        app.on_line(review(S_A, "a_1", "r_1", extra));
        assert_eq!(panel(&app).get(1).map(String::as_str), Some(why));
        assert_eq!(app.panel().map(|panel| panel.alert), Some(true), "{why}");
    }
    let mut app = attached(now);
    app.on_line(review(S_A, "a_1", "r_1", serde_json::json!({})));
    assert_eq!(app.panel().map(|panel| panel.alert), Some(false));
    let mut app = attached(now);
    app.on_line(session_line(
        S_A,
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": [], "reversible": true,
            "step": "standing_ask", "standing_rule": {"scope": "project", "prefix": "git push"}}),
        Some("a_1"),
    ));
    assert_eq!(
        panel(&app).get(1).map(String::as_str),
        Some("asked by a project rule: git push")
    );
}

#[test]
fn an_irreversible_call_says_so_in_the_header() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(session_line(
        S_A,
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"], "reversible": false,
            "step": "review"}),
        Some("a_1"),
    ));
    assert_eq!(
        header(&app),
        format!("approval · {S_A} · 1 of 1 · irreversible")
    );
}

#[test]
fn a_raw_string_argument_shows_as_typed() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(call(S_A, "a_1", "shell", serde_json::json!("not {json")));
    app.on_line(standing_ask(S_A, "a_1", "r_1", "echo hi"));
    assert_eq!(
        panel(&app).get(2).map(String::as_str),
        Some("shell not {json")
    );
}

#[test]
fn requests_from_two_sessions_share_one_queue() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(call(S_B, "a_1", "read", serde_json::json!({"path": "b"})));
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    app.on_line(standing_ask(S_B, "a_1", "r_1", "two"));
    app.on_line(standing_ask(S_A, "a_2", "r_2", "three"));
    // Another session's other lines stay out.
    app.on_line(turn_started(S_B, "hi"));
    assert!(app.lines().is_empty());
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    assert_eq!(reply(&mut app, now)["session_id"], S_A);
    assert_eq!(header(&app), format!("approval · {S_B} · 1 of 2"));
    assert_eq!(
        panel(&app).get(2).map(String::as_str),
        Some(r#"read {"path":"b"}"#)
    );
    let answer = reply(&mut app, now);
    assert_eq!(answer["session_id"], S_B);
    assert_eq!(answer["args"]["request_id"], "r_1");
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_repeated_request_adds_nothing() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
}

/// An app with three requests, the panel on the first.
fn three(now: std::time::Instant) -> App {
    let mut app = attached(now);
    for n in 1..=3 {
        app.on_line(standing_ask(
            S_A,
            &format!("a_{n}"),
            &format!("r_{n}"),
            &format!("p{n}"),
        ));
    }
    app
}

#[test]
fn esc_steps_through_the_queue_then_closes() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = three(now);
    app.on_line(turn_started(S_A, "hi"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    // Esc never cancels while the panel is open.
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(header(&app), format!("approval · {S_A} · 2 of 3"));
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(header(&app), format!("approval · {S_A} · 3 of 3"));
    assert!(app.badge().is_none());
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert!(app.panel().is_none());
    assert_eq!(
        app.badge().as_deref(),
        Some("! 3 waiting · /approvals or ⌥A")
    );
    // With the panel closed, Esc cancels a busy turn again.
    let line = one_line(app.on_key(Key::Esc, now));
    assert_eq!(parse(&line)["command"], "cancel");
}

#[test]
fn a_request_after_one_put_aside_waits_behind_the_badge() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    app.on_key(Key::Esc, now);
    app.on_line(standing_ask(S_A, "a_2", "r_2", "two"));
    assert!(app.panel().is_none());
    assert_eq!(
        app.badge().as_deref(),
        Some("! 2 waiting · /approvals or ⌥A")
    );
    // Once the one put aside resolves, a new request opens at itself.
    app.on_line(resolved(S_A, "r_1"));
    app.on_line(resolved(S_A, "r_2"));
    assert!(app.badge().is_none());
    app.on_line(standing_ask(S_A, "a_3", "r_3", "three"));
    assert_eq!(
        panel(&app).get(1).map(String::as_str),
        Some("asked by a global rule: three")
    );
}

#[test]
fn a_request_while_the_panel_is_open_joins_the_queue() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    app.on_line(standing_ask(S_A, "a_2", "r_2", "two"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 2"));
    assert_eq!(
        panel(&app).get(1).map(String::as_str),
        Some("asked by a global rule: one")
    );
}

#[test]
fn slash_approvals_reopens_at_the_first_request() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = three(now);
    for ch in "x".chars() {
        app.on_key(Key::Char(ch), now);
    }
    press(&mut app, Key::Esc, 3, now);
    assert!(app.panel().is_none());
    assert_eq!(send(&mut app, "/approvals ", now), Effect::None);
    assert_eq!(app.draft(), "");
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    // Feedback typed for a request stays with it while it waits.
    assert_eq!(panel(&app).last().map(String::as_str), Some("› deny · x"));
}

#[test]
fn slash_approvals_with_nothing_waiting_says_so() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    assert_eq!(send(&mut app, "/approvals", now), Effect::None);
    assert_eq!(app.draft(), "");
    assert_eq!(app.notice(), Some("No requests waiting."));
    assert!(app.panel().is_none());
}

#[test]
fn alt_a_opens_and_moves_to_the_next_wrapping() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    press(&mut app, Key::AltA, 1, now);
    assert_eq!(app.notice(), Some("No requests waiting."));
    let mut app = three(now);
    press(&mut app, Key::Esc, 3, now);
    press(&mut app, Key::AltA, 1, now);
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    press(&mut app, Key::AltA, 1, now);
    assert_eq!(header(&app), format!("approval · {S_A} · 2 of 3"));
    press(&mut app, Key::AltA, 2, now);
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
}

#[test]
fn up_down_and_alt_a_do_nothing_to_the_draft_when_closed() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_key(Key::Char('a'), now);
    press(&mut app, Key::Up, 1, now);
    press(&mut app, Key::Down, 1, now);
    assert_eq!(app.draft(), "a");
    assert!(app.panel().is_none());
}

/// The id of the reply an Enter sends.
fn reply_id(app: &mut App, now: std::time::Instant) -> String {
    parse(&one_line(app.on_key(Key::Enter, now)))["id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn a_rejected_reply_puts_its_request_back() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = three(now);
    press(&mut app, Key::AltA, 1, now);
    let id = reply_id(&mut app, now);
    assert_eq!(header(&app), format!("approval · {S_A} · 2 of 2"));
    app.on_line(session_line(
        S_A,
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
        None,
    ));
    assert_eq!(app.notice(), Some("gone"));
    assert_eq!(header(&app), format!("approval · {S_A} · 3 of 3"));
    // Back where it was: second in the queue.
    press(&mut app, Key::AltA, 1, now);
    assert_eq!(
        panel(&app).get(1).map(String::as_str),
        Some("asked by a global rule: p1")
    );
    press(&mut app, Key::AltA, 1, now);
    assert_eq!(
        panel(&app).get(1).map(String::as_str),
        Some("asked by a global rule: p2")
    );
}

#[test]
fn a_rejected_reply_reopens_a_closed_panel() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    let id = reply_id(&mut app, now);
    assert!(app.panel().is_none());
    app.on_line(session_line(
        S_A,
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
        None,
    ));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_resolved_request_leaves_the_queue() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = three(now);
    // Requests never seen change nothing.
    app.on_line(resolved(S_A, "r_9"));
    app.on_line(resolved(S_B, "r_1"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 3"));
    let id = reply_id(&mut app, now);
    // An answered request resolving leaves the shown one in place.
    app.on_line(resolved(S_A, "r_1"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 2"));
    // A rejection of a reply whose request resolved puts nothing back.
    app.on_line(session_line(
        S_A,
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
        None,
    ));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 2"));
    // The shown request resolving elsewhere moves the panel on.
    app.on_line(resolved(S_A, "r_2"));
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
    app.on_line(resolved(S_A, "r_3"));
    assert!(app.panel().is_none());
    assert!(app.badge().is_none());
}

#[test]
fn a_reply_with_the_link_down_sends_nothing_and_keeps_the_request() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    app.disconnected();
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
}

#[test]
fn a_failed_reply_write_keeps_the_request() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    let line = one_line(app.on_key(Key::Enter, now));
    assert!(app.panel().is_none());
    app.write_failed(&[line]);
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
    assert_eq!(app.draft(), "");
}

#[test]
fn the_conversation_gives_up_rows_for_the_panel_and_the_badge() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.set_size(60, 20);
    assert_eq!(app.conversation_height(), 19);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    // Header, why, allow once and deny replace the input line.
    assert_eq!(app.conversation_height(), 16);
    app.on_line(call(
        S_A,
        "a_2",
        "shell",
        serde_json::json!("w ".repeat(35)),
    ));
    app.on_line(standing_ask(S_A, "a_2", "r_2", "two"));
    app.on_key(Key::AltA, now);
    // The call wraps onto two rows.
    assert_eq!(app.conversation_height(), 14);
    press(&mut app, Key::Esc, 1, now);
    assert!(app.panel().is_none());
    assert_eq!(app.conversation_height(), 18);
}

#[test]
fn a_reopened_request_is_no_longer_put_aside() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = attached(now);
    app.on_line(standing_ask(S_A, "a_1", "r_1", "one"));
    press(&mut app, Key::Esc, 1, now);
    press(&mut app, Key::AltA, 1, now);
    let id = reply_id(&mut app, now);
    assert!(app.panel().is_none());
    app.on_line(session_line(
        S_A,
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "stale_request", "message": "gone"}),
        None,
    ));
    // Nothing waits put aside, so the request back opens the panel.
    assert_eq!(header(&app), format!("approval · {S_A} · 1 of 1"));
}
