//! Tests for terminal state.

use super::{App, Effect, QUIT_WINDOW};
use crate::keys::{Key, Parser};
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;
use std::time::Duration;

/// The app's lines as text.
fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

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
        texts(&app),
        vec![
            " hi ".to_owned(),
            "steer · more".to_owned(),
            "▣ completed".to_owned(),
            " next ".to_owned(),
        ]
    );
}

#[test]
fn failed_turn_says_why_before_it_closes() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "failed"));
    assert_eq!(texts(&app)[1..], ["✗ boom · io_failed", "▣ failed"]);
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
    assert!(texts(&app).contains(&"Hello".to_owned()));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "text_completed",
        serde_json::json!({"text": "Hi."}),
        Some("a_1"),
    ));
    assert!(texts(&app).contains(&"Hi.".to_owned()));
}

#[test]
fn each_action_streams_its_own_reply() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    for (action, text) in [("a_1", "one"), ("a_2", "two"), ("a_1", " more")] {
        app.on_line(session_line(
            "s_aaaaaaaaaaaaaaaa",
            "assistant_message_delta",
            serde_json::json!({ "text": text }),
            Some(action),
        ));
    }
    assert_eq!(
        texts(&app),
        vec![" hi ".to_owned(), "one more".to_owned(), "two".to_owned()]
    );
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
    assert!(texts(&app).contains(&"steer · use x".to_owned()));
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
fn the_conversation_gives_up_a_row_each_for_input_hint_and_steering() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.set_size(60, 12);
    assert_eq!(app.conversation_height(), 11);
    app.on_key(Key::CtrlC, now);
    assert_eq!(app.conversation_height(), 10);
    // A notice floats over the conversation and takes no row.
    connect(&mut app);
    app.disconnected();
    assert_eq!(app.conversation_height(), 10);
    app.on_key(Key::Char('x'), now);
    assert_eq!(app.conversation_height(), 11);
    app.attach(contract::SessionId(S_A.to_owned()));
    app.on_line(steering_queue(S_A, &[("a", Some("c_1")), ("b", None)]));
    assert_eq!(app.conversation_height(), 9);
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

/// A `steering_queue` line for `session`: each row its text and its
/// `steer` id, `None` for Fiber's own message.
fn steering_queue(session: &str, rows: &[(&str, Option<&str>)]) -> Line {
    let messages: Vec<serde_json::Value> = rows
        .iter()
        .map(|(text, id)| {
            let mut message = serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "source": "driver",
            });
            if let (Some(id), Some(object)) = (id, message.as_object_mut()) {
                object.insert("command_id".to_owned(), serde_json::json!(id));
            }
            message
        })
        .collect();
    session_line(
        session,
        "steering_queue",
        serde_json::json!({ "messages": messages }),
        None,
    )
}

/// Every line an effect sends, parsed.
fn sent(effect: Effect) -> Vec<serde_json::Value> {
    match effect {
        Effect::Send(lines) => lines.iter().map(|line| parse(line)).collect(),
        Effect::None | Effect::Quit => Vec::new(),
    }
}

/// A command line's `command` and its `args`.
fn command_of(line: &serde_json::Value) -> (String, serde_json::Value) {
    (
        line.get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        line.get("args").cloned().unwrap_or_default(),
    )
}

/// An app attached to `S_A`, busy, with two queued rows `c_1` and `c_2`.
fn queued(now: std::time::Instant) -> App {
    let mut app = app();
    attach(&mut app, now, S_A);
    app.on_line(turn_started(S_A, "hi"));
    app.on_line(steering_queue(
        S_A,
        &[("first", Some("c_1")), ("second", Some("c_2"))],
    ));
    app
}

#[test]
fn the_latest_steering_queue_wins_and_another_sessions_is_ignored() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = queued(clock.now());
    assert_eq!(app.steering(), ["↳ first", "↳ second"]);
    app.on_line(steering_queue(
        "s_bbbbbbbbbbbbbbbb",
        &[("other", Some("c_9"))],
    ));
    assert_eq!(app.steering(), ["↳ first", "↳ second"]);
    app.on_line(steering_queue(S_A, &[("second", Some("c_2"))]));
    assert_eq!(app.steering(), ["↳ second"]);
    // A turn ending leaves the queue; only a `steering_queue` changes it.
    app.on_line(turn_completed(S_A, "completed"));
    assert_eq!(app.steering(), ["↳ second"]);
    app.on_line(steering_queue(S_A, &[]));
    assert!(app.steering().is_empty());
}

#[test]
fn alt_arrows_select_rows_stashing_and_restoring_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    for ch in "draft".chars() {
        app.on_key(Key::Char(ch), now);
    }
    assert_eq!(app.on_key(Key::AltUp, now), Effect::None);
    assert_eq!(app.draft(), "second");
    assert_eq!(app.steering(), ["↳ first", "▸ second"]);
    app.on_key(Key::AltUp, now);
    assert_eq!(app.draft(), "first");
    app.on_key(Key::AltDown, now);
    app.on_key(Key::AltDown, now);
    assert_eq!(app.draft(), "draft");
    assert_eq!(app.steering(), ["↳ first", "↳ second"]);
}

#[test]
fn enter_on_a_selected_row_drops_it_then_steers_the_edit() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    for ch in "draft".chars() {
        app.on_key(Key::Char(ch), now);
    }
    app.on_key(Key::AltUp, now);
    app.on_key(Key::Backspace, now);
    app.on_key(Key::Char('D'), now);
    let lines = sent(app.on_key(Key::Enter, now));
    assert_eq!(lines.len(), 2);
    assert_eq!(
        command_of(&lines[0]),
        (
            "steer_drop".to_owned(),
            serde_json::json!({"command_id": "c_2"})
        )
    );
    assert_eq!(
        command_of(&lines[1]),
        (
            "steer".to_owned(),
            serde_json::json!({"content": [{"type": "text", "text": "seconD"}]})
        )
    );
    assert_eq!(
        lines[1]
            .get("session_id")
            .and_then(serde_json::Value::as_str),
        Some(S_A)
    );
    assert_ne!(lines[0].get("id"), lines[1].get("id"));
    // The stash is back, and the selection is gone.
    assert_eq!(app.draft(), "draft");
    assert_eq!(app.steering(), ["↳ first", "↳ second"]);

    // Another client dropped the row first: the rejected `steer_drop`
    // changes nothing and shows nothing.
    let rejected = |line: &serde_json::Value| {
        session_line(
            S_A,
            "command_rejected",
            serde_json::json!({
                "command_id": line.get("id"),
                "code": "stale_request",
                "message": "gone",
            }),
            None,
        )
    };
    app.on_line(rejected(&lines[0]));
    assert_eq!(app.draft(), "draft");
    assert!(app.notice().is_none());
    // A rejected `steer` puts its edited text back into an empty draft.
    app.on_key(Key::CtrlC, now);
    app.on_line(rejected(&lines[1]));
    assert_eq!(app.draft(), "seconD");
}

#[test]
fn alt_x_drops_the_selected_row_or_every_row() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    let lines = sent(app.on_key(Key::AltX, now));
    let drops: Vec<_> = lines.iter().map(command_of).collect();
    assert_eq!(
        drops,
        [
            (
                "steer_drop".to_owned(),
                serde_json::json!({"command_id": "c_1"})
            ),
            (
                "steer_drop".to_owned(),
                serde_json::json!({"command_id": "c_2"})
            ),
        ]
    );
    app.on_key(Key::AltUp, now);
    app.on_key(Key::AltUp, now);
    let lines = sent(app.on_key(Key::AltX, now));
    assert_eq!(lines.len(), 1);
    assert_eq!(
        command_of(&lines[0]).1,
        serde_json::json!({"command_id": "c_1"})
    );
    // The row leaving the queue restores the draft.
    app.on_line(steering_queue(S_A, &[("second", Some("c_2"))]));
    assert_eq!(app.draft(), "");
    // An empty queue sends nothing.
    app.on_line(steering_queue(S_A, &[]));
    assert_eq!(app.on_key(Key::AltX, now), Effect::None);
}

#[test]
fn esc_with_a_row_selected_clears_it_and_does_not_cancel() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    app.on_key(Key::AltUp, now);
    assert_eq!(app.on_key(Key::Esc, now), Effect::None);
    assert_eq!(app.draft(), "");
    assert_eq!(app.steering(), ["↳ first", "↳ second"]);
    // With nothing selected, Esc cancels again.
    let lines = sent(app.on_key(Key::Esc, now));
    assert_eq!(command_of(&lines[0]).0, "cancel");
}

#[test]
fn select_and_drop_steering_by_index() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    app.on_line(steering_queue(
        S_A,
        &[("first", Some("c_1")), ("fiber's", None)],
    ));
    app.select_steering(1);
    assert_eq!(app.draft(), "");
    app.select_steering(0);
    assert_eq!(app.draft(), "first");
    assert_eq!(app.drop_steering(1), Effect::None);
    assert_eq!(app.drop_steering(5), Effect::None);
    let lines = sent(app.drop_steering(0));
    assert_eq!(lines.len(), 1);
    assert_eq!(
        command_of(&lines[0]),
        (
            "steer_drop".to_owned(),
            serde_json::json!({"command_id": "c_1"})
        )
    );
}

#[test]
fn steering_keys_send_nothing_without_a_connection() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = queued(now);
    app.disconnected();
    assert_eq!(app.on_key(Key::AltX, now), Effect::None);
    app.on_key(Key::AltUp, now);
    assert_eq!(app.on_key(Key::Enter, now), Effect::None);
    assert_eq!(app.draft(), "second");
    assert_eq!(app.steering(), ["↳ first", "▸ second"]);
}

#[test]
fn slash_name_sends_name_and_clears_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, S_A);
    let lines = sent(send(&mut app, "/name  Fix the parser ", now));
    assert_eq!(lines.len(), 1);
    assert_eq!(
        command_of(&lines[0]),
        (
            "name".to_owned(),
            serde_json::json!({"text": "Fix the parser"})
        )
    );
    assert_eq!(
        lines[0]
            .get("session_id")
            .and_then(serde_json::Value::as_str),
        Some(S_A)
    );
    assert_eq!(app.draft(), "");
    // Alone it clears the name; during a turn it is still `name`.
    app.on_line(turn_started(S_A, "hi"));
    let lines = sent(send(&mut app, "/name", now));
    assert_eq!(
        command_of(&lines[0]),
        ("name".to_owned(), serde_json::json!({"text": ""}))
    );
    // A word that only starts with it is a prompt like any other.
    let lines = sent(send(&mut app, "/named", now));
    assert_eq!(command_of(&lines[0]).0, "steer");
}

#[test]
fn a_rejected_name_is_a_notice_and_returns_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    attach(&mut app, now, S_A);
    let lines = sent(send(&mut app, "/name x", now));
    let rejected = session_line(
        S_A,
        "command_rejected",
        serde_json::json!({"command_id": lines[0].get("id"), "code": "closing",
            "message": "The session is closing."}),
        None,
    );
    app.on_line(rejected);
    assert_eq!(app.notice(), Some("The session is closing."));
    assert_eq!(app.draft(), "/name x");
}

#[test]
fn slash_name_with_no_session_keeps_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    connect(&mut app);
    assert_eq!(send(&mut app, "/name x", now), Effect::None);
    assert_eq!(app.draft(), "/name x");
}

#[test]
fn session_named_sets_the_name_and_null_clears_it() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), S_A);
    assert_eq!(app.name(), None);
    let named = |session: &str, name: serde_json::Value| {
        session_line(
            session,
            "session_named",
            serde_json::json!({"name": name, "by": "person"}),
            None,
        )
    };
    app.on_line(named(S_A, serde_json::json!("Fix the parser")));
    assert_eq!(app.name(), Some("Fix the parser"));
    app.on_line(named("s_bbbbbbbbbbbbbbbb", serde_json::json!("Other")));
    assert_eq!(app.name(), Some("Fix the parser"));
    app.on_line(session_line(
        S_A,
        "session_named",
        serde_json::json!({"name": "Parser work", "by": "model"}),
        None,
    ));
    assert_eq!(app.name(), Some("Parser work"));
    app.on_line(named(S_A, serde_json::Value::Null));
    assert_eq!(app.name(), None);
}
