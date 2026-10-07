//! Tests for terminal state.

use super::{App, Effect, QUIT_WINDOW};
use crate::keys::{Edit, Key, Parser};
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

/// `n` numbered lines joined by line breaks.
fn numbered(n: usize) -> String {
    (1..=n)
        .map(|at| format!("l{at}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text part a prompt or steer line carries.
fn content_text(line: &str) -> String {
    parse(line)
        .pointer("/args/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn enter_sends_the_draft_with_its_tokens_expanded() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_key(Key::Char('a'), now);
    app.on_edit(Edit::Paste(numbered(11)));
    assert_eq!(app.input().rows(80), vec!["> a[Pasted text #1 · 11 lines]"]);
    let line = one_line(app.on_key(Key::Enter, now));
    assert_eq!(content_text(&line), format!("a{}", numbered(11)));
    assert!(app.input().is_empty());
    // The next draft numbers its tokens from 1 again.
    app.on_edit(Edit::Paste(numbered(11)));
    assert_eq!(app.input().rows(80), vec!["> [Pasted text #1 · 11 lines]"]);
}

#[test]
fn shift_enter_and_ctrl_j_insert_line_breaks() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_key(Key::Char('a'), now);
    app.on_edit(Edit::ShiftEnter);
    app.on_key(Key::Char('b'), now);
    app.on_edit(Edit::CtrlJ);
    app.on_key(Key::Char('c'), now);
    assert_eq!(app.draft(), "a\nb\nc");
    let line = one_line(app.on_key(Key::Enter, now));
    assert_eq!(content_text(&line), "a\nb\nc");
}

#[test]
fn editing_keys_move_and_delete_in_the_draft() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    for ch in "one two".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_edit(Edit::WordLeft);
    app.on_edit(Edit::Left);
    app.on_key(Key::Char('!'), now);
    assert_eq!(app.draft(), "one! two");
    app.on_edit(Edit::WordRight);
    app.on_edit(Edit::DeleteWord);
    assert_eq!(app.draft(), "one! ");
    app.on_edit(Edit::LineStart);
    app.on_edit(Edit::Delete);
    app.on_edit(Edit::Right);
    app.on_key(Key::Backspace, now);
    assert_eq!(app.draft(), "e! ");
    app.on_edit(Edit::LineEnd);
    app.on_key(Key::Char('x'), now);
    assert_eq!(app.draft(), "e! x");
}

#[test]
fn up_and_down_move_through_a_draft_of_several_lines() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    for ch in "ab".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_edit(Edit::ShiftEnter);
    app.on_key(Key::Char('c'), now);
    assert_eq!(app.on_key(Key::Up, now), Effect::None);
    app.on_key(Key::Char('!'), now);
    assert_eq!(app.draft(), "a!b\nc");
    app.on_key(Key::Down, now);
    app.on_key(Key::Char('?'), now);
    assert_eq!(app.draft(), "a!b\nc?");
}

#[test]
fn ctrl_c_clears_a_draft_with_tokens() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.on_edit(Edit::Paste(numbered(11)));
    app.on_edit(Edit::Paste(numbered(11)));
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
    assert!(app.input().is_empty());
    assert!(!app.hint());
    app.on_edit(Edit::Paste(numbered(11)));
    assert_eq!(app.input().rows(80), vec!["> [Pasted text #1 · 11 lines]"]);
}

#[test]
fn an_editing_key_disarms_ctrl_c() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.on_key(Key::CtrlC, now);
    assert!(app.hint());
    app.on_edit(Edit::Left);
    assert!(!app.hint());
    assert_eq!(app.on_key(Key::CtrlC, now), Effect::None);
}

#[test]
fn the_input_box_grows_to_a_third_of_the_screen() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.set_size(60, 12);
    assert_eq!(app.input_height(), 1);
    assert_eq!(app.conversation_height(), 11);
    for _ in 0..2 {
        app.on_edit(Edit::ShiftEnter);
    }
    assert_eq!(app.input_height(), 3);
    assert_eq!(app.conversation_height(), 9);
    for _ in 0..5 {
        app.on_edit(Edit::ShiftEnter);
    }
    // 12 / 3 rows at most.
    assert_eq!(app.input_height(), 4);
    assert_eq!(app.conversation_height(), 8);
    app.set_size(60, 14);
    assert_eq!(app.input_height(), 4);
    app.set_size(60, 15);
    assert_eq!(app.input_height(), 5);
    // A screen under 3 rows still shows one.
    app.set_size(60, 2);
    assert_eq!(app.input_height(), 1);
    // Wrapped rows count.
    let mut app = self::app();
    app.set_size(10, 12);
    for _ in 0..9 {
        app.on_key(Key::Char('x'), now);
    }
    assert_eq!(app.input_height(), 2);
}

#[test]
fn the_panel_takes_editing_keys_first_and_a_paste_goes_to_its_feedback() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_key(Key::Char('d'), now);
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "permission_requested",
        serde_json::json!({"request_id": "r_1", "effects": ["executes"],
            "reversible": true, "step": "review"}),
        Some("a_1"),
    ));
    assert!(app.panel().is_some());
    for edit in [
        Edit::Left,
        Edit::WordLeft,
        Edit::LineStart,
        Edit::Delete,
        Edit::DeleteWord,
        Edit::ShiftEnter,
        Edit::CtrlJ,
    ] {
        app.on_edit(edit);
    }
    app.on_edit(Edit::Paste("no\nway".to_owned()));
    assert_eq!(app.draft(), "d");
    let feedback = app
        .panel()
        .and_then(|panel| panel.lines.last().cloned())
        .unwrap_or_default();
    assert_eq!(feedback, "› deny · no way");
}
