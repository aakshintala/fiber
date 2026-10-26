//! Tests for stopping one session from home's list: Delete on a focused
//! live row asks "Stop <name>?", and Enter sends `close` with `now`
//! (`docs/tui.md`, "Quit").

use super::super::super::{App, Effect};
use super::tests::{accepted, closes, commands, home, rejected};
use super::{Ask, Prompt};
use crate::app::testkit::hello;
use crate::home::{QuitChoice, Spot};
use crate::keys::{Button, Edit, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Pointer, TargetId};
use contract::SessionId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";

/// Links the app: the feed and recent ids, both waiting for their
/// answers.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
    (
        lines[0]["id"].as_str().unwrap_or_default().to_owned(),
        lines[1]["id"].as_str().unwrap_or_default().to_owned(),
    )
}

/// A live `session_status` for `session` named `name`, in `state`, with
/// `clients`, `jobs` and `delegates`.
fn live_named(
    session: &str,
    name: &str,
    state: Value,
    clients: u32,
    jobs: u32,
    delegates: u32,
) -> Line {
    let mut payload = json!({
        "name": name,
        "workspace": "/w",
        "project": "-w",
        "started": 0, "calls": 0, "since": 0,
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

/// A live row named "fix the parser" in `state`.
fn live(session: &str, state: Value) -> Line {
    live_named(session, "fix the parser", state, 0, 0, 0)
}

/// A hub `session_left` for `session`, ending `how`.
fn left(session: &str, how: &str) -> Line {
    Line::Hub(contract::HubLine {
        kind: "session_left".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: json!({"session_id": session, "how": how})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })
}

/// The lines an effect sends, parsed; none for any other effect.
fn sent(effect: Effect) -> Vec<Value> {
    if let Effect::Send(lines) = effect {
        commands(lines)
    } else {
        Vec::new()
    }
}

/// The frame at 80x24, with the targets it drew.
fn drawn(app: &App) -> (String, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

/// The frame's text at 80x24.
fn frame(app: &App) -> String {
    drawn(app).0
}

/// Presses `key`.
fn press(app: &mut App, key: Key) -> Effect {
    app.on_key(key, fakes::clock::FakeClock::new().now())
}

/// Focuses the home row of `session`.
fn focus_row(app: &mut App, session: &str) {
    let key = app
        .home
        .as_ref()
        .and_then(|home| home.sessions.row(&SessionId(session.to_owned())))
        .map(|row| row.key)
        .unwrap_or_else(|| panic!("a row for {session}"));
    app.focus = Some(TargetId::Home(Spot::Entry(key)));
}

/// A linked home with one live row `A`, in `state`, focused.
fn focused(state: Value) -> App {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(A, state));
    focus_row(&mut app, A);
    app
}

/// Delete on the focused row.
fn delete(app: &mut App) -> Effect {
    app.on_edit(Edit::Delete)
}

/// Whether the foot's question is the stop question.
fn stop_open(app: &App) -> bool {
    app.home
        .as_ref()
        .is_some_and(|home| matches!(home.questions.prompt(), Some(Prompt::Stop { .. })))
}

/// Whether the foot's question is the delete question.
fn delete_open(app: &App) -> bool {
    app.home
        .as_ref()
        .is_some_and(|home| matches!(home.questions.prompt(), Some(Prompt::Delete { .. })))
}

/// The stops waiting for their answers.
fn stops(app: &App) -> Vec<String> {
    app.home
        .as_ref()
        .map(|home| {
            home.questions
                .asks
                .values()
                .filter_map(|ask| match ask {
                    Ask::Stop(session) => Some(session.0.clone()),
                    Ask::Delete(_) | Ask::Import { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn delete_on_a_focused_live_row_opens_the_stop_question_and_sends_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    assert!(stop_open(&app));
    assert!(app.quit_open());
    assert!(!app.home_question_open());
    assert!(!delete_open(&app));
    assert!(stops(&app).is_empty());
    let shown = frame(&app);
    assert!(shown.contains("Stop fix the parser?"), "{shown}");
    assert!(shown.contains("Enter"), "{shown}");
    assert!(shown.contains("stop"), "{shown}");
    assert!(shown.contains("Esc"), "{shown}");
    assert!(shown.contains("cancel"), "{shown}");
    assert!(shown.contains("click a choice"), "{shown}");
}

#[test]
fn the_stop_question_names_the_session_by_its_id_without_a_name() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live_named(A, "", json!({"state": "idle"}), 0, 0, 0));
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(app.stop_question(), Some((A.to_owned(), false, false)));
}

#[test]
fn each_working_state_says_working() {
    let states = [
        json!({"state": "streaming"}),
        json!({"state": "tool", "tool": "shell"}),
        json!({"state": "retrying"}),
        json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "shell"}}),
        json!({"state": "jobs"}),
    ];
    for state in states {
        let mut app = focused(state.clone());
        assert_eq!(delete(&mut app), Effect::None);
        assert_eq!(
            app.stop_question(),
            Some(("fix the parser".to_owned(), true, false)),
            "{state}"
        );
        let shown = frame(&app);
        assert!(shown.contains("working"), "{state}: {shown}");
        assert!(!shown.contains("open elsewhere"), "{state}: {shown}");
    }
}

#[test]
fn an_idle_row_with_a_job_or_a_delegate_says_working() {
    for (jobs, delegates) in [(1, 0), (0, 1)] {
        let mut app = home();
        linked(&mut app);
        app.on_line(live_named(
            A,
            "fix the parser",
            json!({"state": "idle"}),
            0,
            jobs,
            delegates,
        ));
        focus_row(&mut app, A);
        assert_eq!(delete(&mut app), Effect::None);
        assert_eq!(
            app.stop_question(),
            Some(("fix the parser".to_owned(), true, false)),
            "{jobs} jobs, {delegates} delegates"
        );
    }
}

#[test]
fn an_idle_row_with_nothing_running_says_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, false))
    );
    let shown = frame(&app);
    assert!(!shown.contains("working"), "{shown}");
    assert!(!shown.contains("elsewhere"), "{shown}");
}

/// Holds the session at `full` on this connection.
fn hold_full(app: &mut App, session: &str) {
    if let Some(home) = app.home.as_mut() {
        home.subs.sent(
            "c_1".to_owned(),
            SessionId(session.to_owned()),
            crate::home::Level::Full,
        );
        home.subs.answered("c_1", true);
    }
}

#[test]
fn clients_equal_to_the_own_full_connection_is_not_elsewhere() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live_named(
        A,
        "fix the parser",
        json!({"state": "idle"}),
        1,
        0,
        0,
    ));
    hold_full(&mut app, A);
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, false))
    );
    let shown = frame(&app);
    assert!(!shown.contains("elsewhere"), "{shown}");
}

#[test]
fn one_client_more_than_the_own_full_connection_is_elsewhere() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live_named(
        A,
        "fix the parser",
        json!({"state": "idle"}),
        2,
        0,
        0,
    ));
    hold_full(&mut app, A);
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, true))
    );
    let shown = frame(&app);
    assert!(shown.contains("open elsewhere"), "{shown}");
    assert!(!shown.contains("working"), "{shown}");
}

#[test]
fn one_client_not_held_full_is_elsewhere() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live_named(
        A,
        "fix the parser",
        json!({"state": "idle"}),
        1,
        0,
        0,
    ));
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, true))
    );
}

#[test]
fn working_and_elsewhere_read_as_one_note() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live_named(
        A,
        "fix the parser",
        json!({"state": "streaming"}),
        1,
        0,
        0,
    ));
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    let shown = frame(&app);
    assert!(shown.contains("working, open elsewhere"), "{shown}");
}

#[test]
fn the_stop_question_freezes_when_it_opens() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    app.on_line(live(A, json!({"state": "streaming"})));
    assert_eq!(
        app.stop_question(),
        Some(("fix the parser".to_owned(), false, false))
    );
}

#[test]
fn an_unreadable_row_opens_nothing() {
    let mut app = home();
    linked(&mut app);
    let mut envelope = live(A, json!({"state": "idle"}));
    if let Line::Session(status) = &mut envelope {
        status.schema_version = contract::SCHEMA_VERSION + 1;
    }
    app.on_line(envelope);
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    assert!(!app.quit_open());
    assert!(!stop_open(&app));
    assert!(!delete_open(&app));
}

#[test]
fn with_the_link_down_delete_opens_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    app.disconnected();
    assert_eq!(delete(&mut app), Effect::None);
    assert!(!app.quit_open());
    assert!(!stop_open(&app));
}

/// One crashed `recent` row for `session`.
fn crashed(session: &str, name: &str) -> Value {
    json!({
        "session_id": session,
        "ts": 0,
        "project": "-w",
        "workspace": "/w",
        "name": name,
        "how": "crashed",
    })
}

#[test]
fn delete_on_an_exited_or_crashed_row_still_asks_to_delete() {
    for how in ["crashed", "exited"] {
        let mut app = home();
        let (_, recent) = linked(&mut app);
        let mut row = crashed(A, "dead work");
        row["how"] = json!(how);
        app.on_line(accepted(&recent, json!({"sessions": [row]})));
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        let targets = crate::view::render(&app, area, &mut buf, None);
        app.drawn(&targets);
        // The exited section is open once its row is focused.
        let key = app
            .home
            .as_ref()
            .and_then(|home| home.sessions.row(&SessionId(A.to_owned())))
            .map(|row| row.key)
            .unwrap_or_default();
        app.focus = Some(TargetId::Home(Spot::Entry(key)));
        assert_eq!(delete(&mut app), Effect::None, "{how}");
        assert!(delete_open(&app), "{how}");
        assert!(!stop_open(&app), "{how}");
        assert!(!app.quit_open(), "{how}");
    }
}

#[test]
fn delete_with_the_box_focused_edits_the_draft() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(A, json!({"state": "idle"})));
    for ch in "abc".chars() {
        assert_eq!(press(&mut app, Key::Char(ch)), Effect::None);
    }
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(app.on_edit(Edit::Left), Effect::None);
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(app.draft(), "ac");
    assert!(!app.quit_open());
}

#[test]
fn delete_with_a_chip_focused_does_not_stop() {
    let mut app = focused(json!({"state": "idle"}));
    app.focus = Some(TargetId::Home(Spot::Workspace));
    assert_eq!(delete(&mut app), Effect::None);
    assert!(!app.quit_open());
}

#[test]
fn backspace_on_a_focused_live_row_opens_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(press(&mut app, Key::Backspace), Effect::None);
    assert!(!app.quit_open());
    assert!(!delete_open(&app));
}

#[test]
fn enter_closes_that_session_now() {
    let mut app = home();
    linked(&mut app);
    app.on_line(live(A, json!({"state": "streaming"})));
    app.on_line(live_named(
        B,
        "tidy docs",
        json!({"state": "idle"}),
        0,
        0,
        0,
    ));
    focus_row(&mut app, A);
    assert_eq!(delete(&mut app), Effect::None);
    let closes = closes(sent(press(&mut app, Key::Enter)));
    assert_eq!(closes.len(), 1, "{closes:?}");
    assert_eq!(closes[0]["session_id"], A);
    assert_eq!(closes[0]["args"], json!({"now": true}));
    assert_eq!(stops(&app), vec![A.to_owned()]);
    assert!(!app.quit_open());
    assert!(app.stop_question().is_none());
    assert!(app.session().is_none());
}

#[test]
fn clicking_enter_stop_does_what_enter_does() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    let lines = sent(app.on_click(TargetId::Home(Spot::Quit(QuitChoice::CloseAll))));
    let closes = closes(lines);
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["session_id"], A);
    assert_eq!(closes[0]["args"]["now"], json!(true));
    assert_eq!(stops(&app), vec![A.to_owned()]);
    assert!(!app.quit_open());
}

#[test]
fn the_stop_question_draws_one_target_per_choice() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    let (_, targets) = drawn(&app);
    let on = |choice| {
        targets
            .iter()
            .filter(|target| target.id == TargetId::Home(Spot::Quit(choice)))
            .count()
    };
    assert!(on(QuitChoice::CloseAll) >= 1);
    assert!(on(QuitChoice::Stay) >= 1);
    assert_eq!(on(QuitChoice::Leave), 0);
}

#[test]
fn a_refused_close_becomes_the_rows_note() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    let lines = sent(press(&mut app, Key::Enter));
    let close = lines
        .iter()
        .find(|line| line["command"] == "close")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_default()
        .to_owned();
    assert!(app.on_line(rejected(&close, "busy", "Busy.")).is_empty());
    assert_eq!(app.notice(), Some("Busy."));
    assert!(stops(&app).is_empty());
}

#[test]
fn esc_cancels_and_sends_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(press(&mut app, Key::Esc), Effect::None);
    assert!(!app.quit_open());
    assert!(!stop_open(&app));
    assert!(stops(&app).is_empty());
}

#[test]
fn the_cross_click_cancels_and_sends_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    assert_eq!(
        app.on_click(TargetId::Home(Spot::Quit(QuitChoice::Stay))),
        Effect::None
    );
    assert!(!app.quit_open());
    assert!(stops(&app).is_empty());
}

#[test]
fn a_click_outside_the_frame_cancels_and_sends_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    let (_, targets) = drawn(&app);
    let slab = app
        .frames()
        .slab_of(crate::app::Surface::Quit)
        .unwrap_or_default();
    assert!(!slab.is_empty(), "the stop question notes its frame");
    assert!(slab.y > 0, "the frame leaves the top row free");
    let effect = app.on_mouse(
        &Mouse {
            kind: MouseKind::Press(Button::Left),
            col: 0,
            row: 0,
        },
        &targets,
        &mut Pointer::default(),
        true,
        fakes::clock::FakeClock::new().now(),
    );
    assert_eq!(effect, Effect::None);
    assert!(!app.quit_open());
    assert!(stops(&app).is_empty());
}

#[test]
fn other_keys_and_ctrl_c_change_nothing() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    for key in [
        Key::Char('x'),
        Key::Char('c'),
        Key::Up,
        Key::Down,
        Key::Backspace,
        Key::CtrlC,
    ] {
        assert_eq!(press(&mut app, key), Effect::None);
    }
    assert_eq!(delete(&mut app), Effect::None);
    assert!(stop_open(&app));
    assert_eq!(app.draft(), "");
    assert!(stops(&app).is_empty());
}

#[test]
fn enter_with_the_link_down_sends_nothing_and_keeps_the_question() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    app.disconnected();
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(stop_open(&app));
    assert!(stops(&app).is_empty());
}

#[test]
fn a_session_that_left_while_the_question_showed_gets_no_close() {
    let mut app = focused(json!({"state": "idle"}));
    assert_eq!(delete(&mut app), Effect::None);
    app.on_line(left(A, "exited"));
    assert_eq!(press(&mut app, Key::Enter), Effect::None);
    assert!(!app.quit_open());
    assert!(stops(&app).is_empty());
}
