//! Tests for terminal state.

use super::{App, Effect, QUIT_WINDOW};
use crate::keys::{Edit, Key, Parser};
use crate::link::Line;
use contract::clock::Clock;
use std::path::PathBuf;
use std::time::Duration;

/// The app's lines as text.
fn texts(app: &App) -> Vec<String> {
    app.lines().iter().map(ToString::to_string).collect()
}

impl App {
    /// The draft's text, its tokens expanded.
    pub(crate) fn draft(&self) -> String {
        self.draft.expand()
    }
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
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => {
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
/// `commands` line and the first `prompt` sent after it are checked and
/// dropped.
fn accept_start(app: &mut App, command_id: &str, session: &str) -> String {
    let mut lines = accept_start_lines(app, command_id, session);
    assert_eq!(lines.len(), 3);
    assert_eq!(parse(&lines[1])["command"], "commands");
    assert_eq!(parse(&lines[2])["command"], "prompt");
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
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => {
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
    assert!(value["args"].get("content").is_none());
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
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_) => {
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
            "00:00".to_owned(),
            "steer · more".to_owned(),
            "▣ completed".to_owned(),
            " next ".to_owned(),
            "00:00".to_owned(),
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
    assert_eq!(texts(&app)[2..], ["✗ boom · io_failed", "▣ failed"]);
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
        vec![
            " hi ".to_owned(),
            "00:00".to_owned(),
            "one more".to_owned(),
            "two".to_owned()
        ]
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
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0]["command"], "subscribe");
    assert_eq!(commands[1]["command"], "commands");
    assert_eq!(commands[2]["command"], "prompt");
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

/// Every line an effect sends, parsed.
fn sent(effect: Effect) -> Vec<serde_json::Value> {
    match effect {
        Effect::Send(lines) => lines.iter().map(|line| parse(line)).collect(),
        Effect::None
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. } => Vec::new(),
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
fn slash_name_with_no_session_clears_the_draft_with_a_notice() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = app();
    connect(&mut app);
    assert_eq!(send(&mut app, "/name x", now), Effect::None);
    assert_eq!(app.notice(), Some("No session on screen."));
    assert_eq!(app.draft(), "");
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

/// The `args` of a command line.
fn args(line: &str) -> serde_json::Value {
    parse(line).get("args").cloned().unwrap_or_default()
}

#[test]
fn one_bang_sends_shell_with_send_true() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "!ls", now));
    let value = parse(&line);
    assert_eq!(value["command"], "shell");
    assert_eq!(value["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(
        args(&line),
        serde_json::json!({"command": "ls", "send": true})
    );
    assert!(app.input().is_empty());
}

#[test]
fn two_bangs_send_shell_with_send_false() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "!!git status", now));
    assert_eq!(
        args(&line),
        serde_json::json!({"command": "git status", "send": false})
    );
}

#[test]
fn a_bang_alone_is_a_prompt() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let line = one_line(send(&mut app, "!", now));
    assert_eq!(parse(&line)["command"], "prompt");
    assert_eq!(content_text(&line), "!");
}

#[test]
fn a_bang_command_expands_its_tokens() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_key(Key::Char('!'), now);
    app.on_edit(Edit::Paste(numbered(11)));
    let line = one_line(app.on_key(Key::Enter, now));
    assert_eq!(args(&line)["command"], numbered(11).as_str());
    assert_eq!(args(&line)["send"], true);
}

#[test]
fn a_bang_command_before_a_session_says_start_one_first() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    connect(&mut app);
    assert_eq!(send(&mut app, "!!ls", now), Effect::None);
    assert_eq!(app.notice(), Some("Start a session first."));
    assert_eq!(app.draft(), "!!ls");
    // While `start` waits for its answer, the same.
    let mut app = self::app();
    started(&mut app, now);
    assert_eq!(send(&mut app, "!ls", now), Effect::None);
    assert_eq!(app.notice(), Some("Start a session first."));
    assert_eq!(app.draft(), "!ls");
}

#[test]
fn a_bang_command_during_a_turn_is_shell_not_steer() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", "hi"));
    let line = one_line(send(&mut app, "!!ls", now));
    assert_eq!(parse(&line)["command"], "shell");
    let line = one_line(send(&mut app, "!ls", now));
    assert_eq!(parse(&line)["command"], "shell");
}

/// A `command_accepted` on the session stream for `id` with `result`.
fn accepted(id: &str, result: Option<serde_json::Value>) -> Line {
    let mut payload = serde_json::json!({"command_id": id});
    if let (Some(result), Some(object)) = (result, payload.as_object_mut()) {
        object.insert("result".to_owned(), result);
    }
    session_line("s_aaaaaaaaaaaaaaaa", "command_accepted", payload, None)
}

/// The id of a command line.
fn id_of(line: &str) -> String {
    parse(line)["id"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn a_show_only_result_is_one_item() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let id = id_of(&one_line(send(&mut app, "!!git status", now)));
    let result = serde_json::json!({"output": "clean\n",
        "process": {"exit_code": 0, "timed_out": false}});
    app.on_line(accepted(&id, Some(result.clone())));
    assert_eq!(texts(&app), vec!["! git status", "clean"]);
    // A repeat of the answer shows nothing more.
    app.on_line(accepted(&id, Some(result)));
    assert_eq!(app.lines().len(), 2);
}

#[test]
fn a_sent_command_shows_once_from_its_event() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let id = id_of(&one_line(send(&mut app, "!ls", now)));
    // Even an answer carrying a result shows nothing for `send` true.
    app.on_line(accepted(
        &id,
        Some(serde_json::json!({"output": "a",
            "process": {"exit_code": 0, "timed_out": false}})),
    ));
    assert!(app.lines().is_empty());
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "shell_command",
        serde_json::json!({"command": "ls", "output": "a\n",
            "process": {"exit_code": 2, "timed_out": false}}),
        None,
    ));
    assert_eq!(texts(&app), vec!["! ls", "a", "exit 2"]);
}

#[test]
fn a_prompt_answer_with_a_shell_shaped_result_shows_nothing() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let id = id_of(&one_line(send(&mut app, "!!ls", now)));
    let line = one_line(send(&mut app, "hello", now));
    app.on_line(accepted(
        &id_of(&line),
        Some(serde_json::json!({"output": "a",
            "process": {"exit_code": 0, "timed_out": false}})),
    ));
    assert!(app.lines().is_empty());
    app.on_line(accepted(&id, None));
    assert!(app.lines().is_empty());
}

#[test]
fn a_rejected_bang_command_returns_to_an_empty_draft() {
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    let id = id_of(&one_line(send(&mut app, "!!ls", now)));
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "command_rejected",
        serde_json::json!({"command_id": id, "code": "busy", "message": "no"}),
        None,
    ));
    assert_eq!(app.notice(), Some("no"));
    assert_eq!(app.draft(), "!!ls");
}

#[test]
fn shell_output_marks_new_lines_while_scrolled_up() {
    // `shell_command` changes no card, so only the shell item marks the
    // scroll: `&=` would leave it unmarked.
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.set_size(60, 12);
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    for n in 0..30 {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &format!("hi {n}")));
        app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    }
    app.on_key(Key::PageUp, now);
    assert!(app.top().is_some());
    assert!(!app.has_new());
    app.on_line(session_line(
        "s_aaaaaaaaaaaaaaaa",
        "shell_command",
        serde_json::json!({"command": "ls", "output": "a\n",
            "process": {"exit_code": 2, "timed_out": false}}),
        None,
    ));
    assert!(app.has_new());
}

#[test]
fn an_answered_shell_command_marks_new_lines_while_scrolled_up() {
    // `command_accepted` changes no card either: the answered shell item
    // alone marks the scroll.
    let now = fakes::clock::FakeClock::new().now();
    let mut app = app();
    app.set_size(60, 12);
    attach(&mut app, now, "s_aaaaaaaaaaaaaaaa");
    for n in 0..30 {
        app.on_line(turn_started("s_aaaaaaaaaaaaaaaa", &format!("hi {n}")));
        app.on_line(turn_completed("s_aaaaaaaaaaaaaaaa", "completed"));
    }
    let id = id_of(&one_line(send(&mut app, "!!ls", now)));
    app.on_key(Key::PageUp, now);
    assert!(app.top().is_some());
    assert!(!app.has_new());
    app.on_line(accepted(
        &id,
        Some(serde_json::json!({"output": "a",
            "process": {"exit_code": 0, "timed_out": false}})),
    ));
    assert!(app.has_new());
}
