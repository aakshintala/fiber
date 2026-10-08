//! Tests for quitting with sessions working: what counts, the question
//! on the foot, and closing all.

use super::super::{App, Effect};
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use contract::SessionId;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;

/// An app on home at 80x24.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
    });
    app.set_size(80, 24);
    app
}

/// A `hub_hello` this terminal reads.
fn hello() -> Line {
    Line::Hub(contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    })
}

/// Links the app: the feed and recent ids, both waiting for their
/// answers.
fn linked(app: &mut App) -> (String, String) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
    assert_eq!(lines.len(), 2);
    (
        lines[0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("feed id"))
            .to_owned(),
        lines[1]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("recent id"))
            .to_owned(),
    )
}

/// A live `session_status` for `session` in `state`, with `clients`,
/// `jobs` and `delegates`.
fn live(session: &str, state: Value, clients: u32, jobs: u32, delegates: u32) -> Line {
    let mut payload = json!({
        "name": "fix the parser",
        "workspace": "/w",
        "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model",
        "delegates": delegates,
        "jobs": jobs,
        "clients": clients,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// Parses outgoing command lines.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The foot home draws.
fn foot(app: &App) -> String {
    app.home_screen()
        .map(|screen| screen.foot)
        .unwrap_or_default()
}

/// Ctrl+C twice: the quit question opens, or the app quits.
fn ctrl_c(app: &mut App) -> Effect {
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    app.on_key(Key::CtrlC, clock.now())
}

/// One working row, streaming.
fn working(app: &mut App, session: &str) {
    app.on_line(live(session, json!({"state": "streaming"}), 0, 0, 0));
}

/// The working sessions, in list order.
fn working_now(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.sessions
                .live()
                .iter()
                .map(|row| row.id.0.clone())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn quit_with_nothing_working_quits() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    assert_eq!(ctrl_c(&mut app), Effect::Quit);
}

#[test]
fn quit_without_home_quits() {
    let mut app = App::new(PathBuf::from("/w"));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::Quit);
}

#[test]
fn each_working_state_counts() {
    let states = [
        json!({"state": "streaming"}),
        json!({"state": "tool", "tool": "shell"}),
        json!({"state": "retrying"}),
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell"}}),
        json!({"state": "jobs"}),
    ];
    for state in states {
        let mut app = home();
        linked(&mut app);
        app.on_line(live("s_aaaaaaaaaaaaaaaa", state, 0, 0, 0));
        assert_eq!(ctrl_c(&mut app), Effect::None);
        assert_eq!(
            foot(&app),
            "1 session working · enter leave them running · c close all · esc stay"
        );
    }
}

#[test]
fn an_idle_row_with_a_job_or_a_delegate_counts() {
    for (jobs, delegates) in [(1, 0), (0, 2)] {
        let mut app = home();
        linked(&mut app);
        app.on_line(live(
            "s_aaaaaaaaaaaaaaaa",
            json!({"state": "idle"}),
            0,
            jobs,
            delegates,
        ));
        assert_eq!(ctrl_c(&mut app), Effect::None);
        assert_eq!(
            foot(&app),
            "1 session working · enter leave them running · c close all · esc stay"
        );
    }
}

#[test]
fn an_idle_row_with_neither_does_not() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    assert_eq!(ctrl_c(&mut app), Effect::Quit);
}

#[test]
fn left_and_unreadable_rows_never_count() {
    let mut app = home();
    linked(&mut app);
    // A row that left while streaming keeps its waiting text, but never
    // counts as working.
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
        0,
        0,
        0,
    ));
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    // A status this terminal cannot read never counts either.
    let mut envelope = live("s_bbbbbbbbbbbbbbbb", json!({"state": "streaming"}), 0, 0, 0);
    if let Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    assert_eq!(ctrl_c(&mut app), Effect::Quit);
}

#[test]
fn the_attached_busy_session_counts_once() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.phase = crate::app::Phase::Attached {
        session: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        busy: true,
    };
    assert_eq!(working_now(&app), ["s_aaaaaaaaaaaaaaaa"]);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(
        app.hint_text(),
        "1 session working · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn the_attached_busy_session_counts_without_the_feed() {
    let mut app = home();
    linked(&mut app);
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.phase = crate::app::Phase::Attached {
        session: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        busy: true,
    };
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(
        app.hint_text(),
        "1 session working · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn the_prompt_names_sessions_working() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "2 sessions working · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn clients_two_held_full_is_open_elsewhere() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
        2,
        0,
        0,
    ));
    // This connection holds the session at `full`: one of the two.
    let session = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    if let Some(home) = app.home.as_mut() {
        home.subs
            .sent("c_1".to_owned(), session, crate::home::Level::Full);
        home.subs.answered("c_1", true);
    }
    assert_eq!(ctrl_c(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "1 session working, 1 also open elsewhere · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn clients_one_held_full_is_not() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
        1,
        0,
        0,
    ));
    let session = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    if let Some(home) = app.home.as_mut() {
        home.subs
            .sent("c_1".to_owned(), session, crate::home::Level::Full);
        home.subs.answered("c_1", true);
    }
    assert_eq!(ctrl_c(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "1 session working · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn clients_one_not_held_full_is() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "streaming"}),
        1,
        0,
        0,
    ));
    assert_eq!(ctrl_c(&mut app), Effect::None);
    assert_eq!(
        foot(&app),
        "1 session working, 1 also open elsewhere · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn enter_leaves_them_running() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::Quit);
}

#[test]
fn c_subscribes_where_needed_and_closes_each_now() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    // The second session this connection already holds at `full`: no
    // subscribe goes out for it.
    let held = SessionId("s_bbbbbbbbbbbbbbbb".to_owned());
    if let Some(home) = app.home.as_mut() {
        home.subs
            .sent("c_1".to_owned(), held, crate::home::Level::Full);
        home.subs.answered("c_1", true);
    }
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(lines) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[0]["args"]["level"], "summary");
    assert_eq!(lines[1]["command"], "close");
    assert_eq!(lines[1]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[1]["args"], json!({"now": true}));
    assert_eq!(lines[2]["command"], "close");
    assert_eq!(lines[2]["session_id"], "s_bbbbbbbbbbbbbbbb");
    assert_eq!(lines[2]["args"], json!({"now": true}));
    assert_ne!(lines[0]["id"], lines[1]["id"]);
    assert_ne!(lines[1]["id"], lines[2]["id"]);
}

#[test]
fn esc_stays_and_shows_no_hint() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert!(!app.hint());
    assert_eq!(
        foot(&app),
        "↓ the session list · F1 the key map · Ctrl+C twice to quit"
    );
}

#[test]
fn other_keys_and_ctrl_c_do_nothing_while_it_is_open() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    let question = foot(&app);
    assert_eq!(app.on_key(Key::Char('x'), clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(foot(&app), question);
    assert!(app.input().expand().is_empty());
}

#[test]
fn slash_quit_goes_through_the_same_prompt() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    let clock = fakes::clock::FakeClock::new();
    for ch in "/quit".chars() {
        assert_eq!(app.on_key(Key::Char(ch), clock.now()), Effect::None);
    }
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(
        foot(&app),
        "1 session working · enter leave them running · c close all · esc stay"
    );
}

#[test]
fn hint_text_is_the_prompt_while_open_and_the_quit_hint_otherwise() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(app.hint_text(), crate::app::QUIT_HINT);
    assert!(!app.hint());
    assert_eq!(ctrl_c(&mut app), Effect::None);
    assert!(app.hint());
    assert_eq!(
        app.hint_text(),
        "1 session working · enter leave them running · c close all · esc stay"
    );
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Esc, clock.now()), Effect::None);
    assert_eq!(app.hint_text(), crate::app::QUIT_HINT);
    assert!(!app.hint());
}

#[test]
fn a_session_that_leaves_while_the_question_shows_gets_no_close() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    assert_eq!(
        foot(&app),
        "1 session working · enter leave them running · c close all · esc stay"
    );
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(lines) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["command"], "subscribe");
    assert_eq!(lines[0]["session_id"], "s_bbbbbbbbbbbbbbbb");
    assert_eq!(lines[1]["command"], "close");
    assert_eq!(lines[1]["session_id"], "s_bbbbbbbbbbbbbbbb");
}

#[test]
fn a_session_that_goes_idle_gets_no_close() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(lines) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["session_id"], "s_bbbbbbbbbbbbbbbb");
}

#[test]
fn a_newly_working_session_shows_in_the_question_and_gets_a_close() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    assert_eq!(ctrl_c(&mut app), Effect::None);
    app.on_line(live(
        "s_bbbbbbbbbbbbbbbb",
        json!({"state": "streaming"}),
        0,
        0,
        0,
    ));
    assert_eq!(
        foot(&app),
        "2 sessions working · enter leave them running · c close all · esc stay"
    );
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(lines) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    let lines = commands(lines);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[1]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(lines[3]["session_id"], "s_bbbbbbbbbbbbbbbb");
}

#[test]
fn c_with_nothing_working_left_quits() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Char('c'), clock.now()), Effect::Quit);
}

#[test]
fn c_with_the_link_down_quits_and_closes_nothing() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    app.disconnected();
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::Char('c'), clock.now()), Effect::Quit);
}

#[test]
fn an_unsent_close_keeps_its_session_out_of_closing() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(lines) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    // The first session's `close` is never written: it keeps its resume
    // line, while the written one does not.
    let close = lines
        .iter()
        .find(|line| line.contains("s_aaaaaaaaaaaaaaaa") && line.contains("close"))
        .cloned()
        .unwrap_or_else(|| panic!("a close line"));
    app.write_failed(std::slice::from_ref(&close));
    let closing: Vec<String> = app
        .home
        .as_ref()
        .map(|home| {
            home.closing
                .iter()
                .map(|(session, _)| session.0.clone())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(closing, ["s_bbbbbbbbbbbbbbbb"]);
}

#[test]
fn exit_lines_name_each_live_session() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(
        app.exit_lines(),
        [
            "s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa",
            "s_bbbbbbbbbbbbbbbb  fiber resume s_bbbbbbbbbbbbbbbb",
        ]
    );
}

#[test]
fn closing_sessions_get_no_line() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(_) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    assert!(app.exit_lines().is_empty());
}

#[test]
fn left_rows_get_no_line() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    working(&mut app, "s_bbbbbbbbbbbbbbbb");
    app.on_line(left("s_aaaaaaaaaaaaaaaa", "exited"));
    assert_eq!(
        app.exit_lines(),
        ["s_bbbbbbbbbbbbbbbb  fiber resume s_bbbbbbbbbbbbbbbb"]
    );
}

#[test]
fn the_attached_session_gets_a_line_without_the_feed() {
    let mut app = home();
    linked(&mut app);
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert_eq!(
        app.exit_lines(),
        ["s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa"]
    );
}

#[test]
fn no_home_no_lines() {
    let app = App::new(PathBuf::from("/w"));
    assert!(app.exit_lines().is_empty());
}

#[test]
fn the_attached_idle_session_without_a_row_does_not_count() {
    let mut app = home();
    linked(&mut app);
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::Quit);
}

#[test]
fn the_attached_session_with_a_row_gets_one_line() {
    let mut app = home();
    linked(&mut app);
    working(&mut app, "s_aaaaaaaaaaaaaaaa");
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    assert_eq!(
        app.exit_lines(),
        ["s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa"]
    );
}

#[test]
fn a_closing_attached_session_gets_no_line() {
    let mut app = home();
    linked(&mut app);
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.phase = crate::app::Phase::Attached {
        session: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        busy: true,
    };
    assert_eq!(ctrl_c(&mut app), Effect::None);
    let clock = fakes::clock::FakeClock::new();
    let Effect::Exit(_) = app.on_key(Key::Char('c'), clock.now()) else {
        panic!("`c` closes all");
    };
    assert!(app.exit_lines().is_empty());
}

#[test]
fn an_attached_busy_session_with_an_idle_row_is_not_counted_twice() {
    let mut app = home();
    linked(&mut app);
    // The attached session's feed row is idle, with no jobs: nothing
    // works, so quitting asks nothing.
    app.on_line(live(
        "s_aaaaaaaaaaaaaaaa",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    app.attach(SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.phase = crate::app::Phase::Attached {
        session: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        busy: true,
    };
    let clock = fakes::clock::FakeClock::new();
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::None);
    assert_eq!(app.on_key(Key::CtrlC, clock.now()), Effect::Quit);
}
