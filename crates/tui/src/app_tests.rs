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
        Effect::None | Effect::Quit | Effect::ListFiles | Effect::Search { .. } => {
            panic!("expected one line")
        }
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

/// Accepts `start` for `session`, returning the subscribe line. The
/// `commands` line sent beside it is checked and dropped.
fn accept_start(app: &mut App, command_id: &str, session: &str) -> String {
    let mut lines = accept_start_lines(app, command_id, session);
    assert_eq!(lines.len(), 2);
    assert_eq!(parse(&lines[1])["command"], "commands");
    lines.swap_remove(0)
}

/// Accepts `start` for `session`, returning every line the app sends.
fn accept_start_lines(app: &mut App, command_id: &str, session: &str) -> Vec<String> {
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
    app.on_line(Line::Hub(hello))
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
        Effect::None | Effect::Quit | Effect::ListFiles | Effect::Search { .. } => {
            panic!("expected cancel")
        }
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
        Effect::None | Effect::Quit | Effect::ListFiles | Effect::Search { .. } => {
            panic!("expected cancel")
        }
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
fn failed_turn_closes_with_its_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = app();
    attach(&mut app, clock.now(), "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "failed"));
    assert_eq!(texts(&app).last(), Some(&"▣ failed · boom".to_owned()));
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

/// The `/` panel's rows for `query`, typed into an empty draft and then
/// cleared.
fn slash_rows(app: &mut App, query: &str, now: std::time::Instant) -> Vec<String> {
    for ch in format!("/{query}").chars() {
        app.on_key(Key::Char(ch), now);
    }
    let rows = app
        .completions()
        .map(|completions| completions.lines)
        .unwrap_or_default();
    app.on_key(Key::CtrlC, now);
    rows
}

/// A session `command_accepted` for `id` answering `commands` with `rows`.
fn commands_answer(id: &str, rows: serde_json::Value) -> Line {
    session_line(
        S_A,
        "command_accepted",
        serde_json::json!({"command_id": id, "result": {"commands": rows}}),
        None,
    )
}

/// Attaches through `start` and returns the `commands` line's id.
fn attached_asking(app: &mut App, now: std::time::Instant) -> String {
    let start = started(app, now);
    let lines = accept_start_lines(app, &start, S_A);
    let commands = parse(&lines[1]);
    assert_eq!(commands["session_id"], S_A);
    assert!(commands.get("args").is_none());
    commands["id"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn attaching_sends_subscribe_then_commands() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    let start = started(&mut app, now);
    let lines = accept_start_lines(&mut app, &start, S_A);
    let commands: Vec<serde_json::Value> = lines.iter().map(|line| parse(line)).collect();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["command"], "subscribe");
    assert_eq!(commands[1]["command"], "commands");
    assert_ne!(commands[0]["id"], commands[1]["id"]);
}

#[test]
fn the_commands_answer_fills_the_list_with_hints_and_tags() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    let id = attached_asking(&mut app, now);
    // Until the answer arrives, the built-ins only.
    assert!(slash_rows(&mut app, "rev", now).is_empty());
    let answer = commands_answer(
        &id,
        serde_json::json!([
            {"name": "review", "description": "Review a diff.", "argument_hint": "[base]",
             "tag": "template"},
            {"name": "tdd", "description": "Test first.", "tag": "skill"},
            {"name": "reload", "description": "Not the built-in.", "tag": "skill"}]),
    );
    assert!(app.on_line(answer).is_empty());
    assert_eq!(
        slash_rows(&mut app, "rev", now),
        ["/review [base]  Review a diff.  template"]
    );
    assert_eq!(
        slash_rows(&mut app, "td", now),
        ["/tdd  Test first.  skill"]
    );
    assert_eq!(
        slash_rows(&mut app, "relo", now),
        ["/reload  Reloads configuration, MCP servers and extensions.  command"]
    );
}

#[test]
fn reloaded_asks_again_and_only_the_latest_answer_fills_the_list() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    let first = attached_asking(&mut app, now);
    let reloaded = session_line(
        S_A,
        "reloaded",
        serde_json::json!({"servers": {"kept": [], "restarted": [], "started": [],
            "stopped": []}, "extensions": []}),
        None,
    );
    let lines = app.on_line(reloaded);
    assert_eq!(lines.len(), 1);
    let again = parse(&lines[0]);
    assert_eq!(again["command"], "commands");
    assert_eq!(again["session_id"], S_A);
    let second = again["id"].as_str().unwrap_or_default().to_owned();
    assert_ne!(first, second);
    let row = |name: &str| serde_json::json!([{"name": name, "description": "d", "tag": "skill"}]);
    // The older answer arrives after the reload: ignored.
    assert!(
        app.on_line(commands_answer(&first, row("older")))
            .is_empty()
    );
    assert!(slash_rows(&mut app, "older", now).is_empty());
    assert!(
        app.on_line(commands_answer(&second, row("latest")))
            .is_empty()
    );
    assert_eq!(slash_rows(&mut app, "latest", now), ["/latest  d  skill"]);
    // Its answer is used once: a repeat of the first id still changes nothing.
    assert!(
        app.on_line(commands_answer(&first, row("older")))
            .is_empty()
    );
    assert_eq!(slash_rows(&mut app, "latest", now), ["/latest  d  skill"]);
}

#[test]
fn the_opening_messages_skills_no_longer_feed_the_list() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attached_asking(&mut app, now);
    let opening = session_line(
        S_A,
        "opening_message",
        serde_json::json!({
            "environment": {"date": "2026-10-06", "os": "macos", "arch": "aarch64",
                "shell": "zsh", "workspace": "/w", "session_log": "/l"},
            "instruction_files": [],
            "skills": [{"name": "tdd", "description": "d", "path": "/s/tdd/SKILL.md",
                "source": "repository"}],
        }),
        None,
    );
    assert!(app.on_line(opening).is_empty());
    assert!(slash_rows(&mut app, "td", now).is_empty());
}

#[test]
fn a_commands_answer_from_another_session_is_ignored() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    let id = attached_asking(&mut app, now);
    let mut answer = commands_answer(
        &id,
        serde_json::json!([{"name": "tdd", "description": "d", "tag": "skill"}]),
    );
    if let Line::Session(envelope) = &mut answer {
        envelope.session_id = contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    }
    assert!(app.on_line(answer).is_empty());
    assert!(slash_rows(&mut app, "td", now).is_empty());
}
