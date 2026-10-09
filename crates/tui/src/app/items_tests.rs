//! Tests for the item fold and the screen swap: opening swaps a fresh
//! screen in, the attached session's lines fold into the stashed screen,
//! and closing swaps back with a `summary` wish the reconciler lowers.

use std::path::PathBuf;
use std::time::Duration;

use contract::clock::Clock;
use contract::{JobId, SessionId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::super::{App, Effect};
use super::retry::RETRY;
use crate::home::{Launch, Level};
use crate::keys::Key;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const DELEGATE_A: &str = "s_dddddddddddddddd";
const DELEGATE_B: &str = "s_eeeeeeeeeeeeeeee";

/// An app on home at 80x24, drawing the default cards.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
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

/// One envelope of the attached session.
fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// One envelope of another session.
fn other_line(session: &str, kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// The `subscribe` lines among `out`.
fn subscribes(out: &[Value]) -> Vec<Value> {
    out.iter()
        .filter(|line| line["command"] == "subscribe")
        .cloned()
        .collect()
}

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
    let lines = commands(app.on_line(hello()));
    assert_eq!(lines.len(), 2);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    let key = app
        .home_screen()
        .map(|screen| {
            screen
                .rows
                .into_iter()
                .map(|(key, _, _)| key)
                .collect::<Vec<_>>()
        })
        .and_then(|keys| keys.into_iter().next())
        .unwrap_or_else(|| panic!("a row"));
    let out = match app.on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id));
}

/// A live `session_status` for `session` in `state`.
fn live(session: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": "fix the parser", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
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

/// A session `command_accepted` for `id` from `session`.
fn session_accepted(session: &str, id: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

/// A session `command_rejected` for `id` from `session` with `code`.
fn session_refused(session: &str, id: &str, code: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            ("code".to_owned(), Value::String(code.to_owned())),
            (
                "message".to_owned(),
                Value::String("no such session".to_owned()),
            ),
        ]
        .into_iter()
        .collect(),
    })
}

/// Folds a Fiber delegate as job `job` with session `delegate`.
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Folds the attached session's `job_completed` for `job`.
fn complete(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": job, "status": "completed"}),
    ));
}

/// Opens `job`'s item view, returning the parsed lines going out.
fn open(app: &mut App, job: &str) -> Vec<Value> {
    match app.open_item(&JobId(job.to_owned())) {
        Effect::Send(lines) => commands(lines),
        Effect::None => Vec::new(),
        Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends or nothing"),
    }
}

/// Acknowledges every `subscribe` in `out` from its session.
fn ack_all(app: &mut App, out: &[Value]) {
    for line in subscribes(out) {
        let id = line["id"].as_str().unwrap_or_else(|| panic!("an id"));
        let session = line["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("a session"));
        app.on_line(session_accepted(session, id));
    }
}

/// The resident conversation's rows as text.
fn shown(app: &App) -> Vec<String> {
    app.lines().iter().map(|line| line.to_string()).collect()
}

/// Drives the clock: the frame time `now` reads from `clock`.
fn tick(app: &mut App, clock: &std::sync::Arc<fakes::clock::FakeClock>) {
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
}

#[test]
fn opening_swaps_in_a_fresh_screen() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    assert!(!shown(&app).is_empty());
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    assert!(app.item_open());
    // A fresh screen: the parent's turn is not shown.
    assert!(shown(&app).is_empty());
    assert_eq!(subscribes(&out).len(), 1);
    assert_eq!(out[0]["args"]["level"], "full");
}

#[test]
fn an_attached_turn_while_open_lands_in_the_parent_and_shows_after_close() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    let before = shown(&app).len();
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "meanwhile"}]}]}),
    ));
    // The delegate's screen is untouched by the attached session's line.
    assert_eq!(shown(&app).len(), before);
    app.close_item();
    assert!(shown(&app).len() > before);
}

#[test]
fn a_resize_while_open_applies_to_both_screens() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    app.set_size(100, 30);
    app.close_item();
    assert_eq!(app.conversation_area().width, 100);
}

#[test]
fn go_home_closes_the_view_first() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    assert!(app.item_open());
    app.go_home();
    assert!(!app.item_open());
}

#[test]
fn disconnected_closes_the_view() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    open(&mut app, "j_1");
    assert!(app.item_open());
    app.disconnected();
    assert!(!app.item_open());
}

#[test]
fn a_close_then_items_due_lowers_while_running() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let due = commands(app.items_due(clock.now()));
    let lowered = subscribes(&due);
    assert_eq!(lowered.len(), 1);
    assert_eq!(lowered[0]["session_id"], DELEGATE_A);
    assert_eq!(lowered[0]["args"]["level"], "summary");
}

#[test]
fn a_close_then_items_due_lowers_without_the_card_listed() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .launch
        .panel_cards = vec!["session".to_owned()];
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let due = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&due).len(), 1);
}

#[test]
fn a_close_then_items_due_sends_nothing_once_completed() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    complete(&mut app, "j_1");
    app.close_item();
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn opening_closing_and_reopening_sends_full_summary_full() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    assert_eq!(subscribes(&first).len(), 1);
    ack_all(&mut app, &first);
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    assert_eq!(subscribes(&lowered).len(), 1);
    ack_all(&mut app, &lowered);
    let second = open(&mut app, "j_1");
    let raised = subscribes(&second);
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0]["args"]["level"], "full");
    // The reopened view replays: a `turn_started` after the second `full`
    // acknowledgement shows.
    ack_all(&mut app, &second);
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "again"}]}]}),
    ));
    assert!(!shown(&app).is_empty());
}

#[test]
fn reopening_before_the_lowering_goes_out_sends_the_pair() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    // No `items_due` runs between the close and the reopen: the
    // connection still holds `full`, so the pair goes out in one batch.
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    assert_eq!(pair[0]["args"]["level"], "summary");
    assert_eq!(pair[1]["args"]["level"], "full");
}

#[test]
fn opening_sends_nothing_while_a_subscribe_is_in_flight() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    // The card's `summary` subscribe goes out and stays in flight.
    let card = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    assert_eq!(subscribes(&card).len(), 1);
    let out = open(&mut app, "j_1");
    assert!(app.item_open());
    assert!(subscribes(&out).is_empty());
}

#[test]
fn opening_a_second_item_closes_the_first() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    let parent_rows = shown(&app).len();
    assert!(parent_rows > 0);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    open(&mut app, "j_1");
    open(&mut app, "j_2");
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
    // The first view closed: closing the second restores the parent.
    app.close_item();
    assert_eq!(shown(&app).len(), parent_rows);
}

#[test]
fn a_pair_refused_invalid_arguments_shows_the_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(session_refused(DELEGATE_A, summary_id, "invalid_arguments"));
    app.on_line(session_refused(DELEGATE_A, full_id, "invalid_arguments"));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(
        view.message
            .as_deref()
            .is_some_and(|message| message.contains("Could not open"))
    );
}

#[test]
fn a_pair_refused_session_not_found_retries_while_running() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, summary_id, "session_not_found"));
    app.on_line(session_refused(DELEGATE_A, full_id, "session_not_found"));
    // The retry waits: nothing goes out before its delay.
    assert!(commands(app.items_due(clock.now())).is_empty());
    clock.advance(Duration::from_millis(500));
    tick(&mut app, &clock);
    let resent = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(resent.len(), 1);
    assert_eq!(resent[0]["args"]["level"], "full");
}

#[test]
fn a_refusal_after_completion_sets_no_retry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    let full_id = subscribes(&first)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    complete(&mut app, "j_1");
    app.on_line(session_refused(DELEGATE_A, &full_id, "session_not_found"));
    assert!(commands(app.items_due(clock.now())).is_empty());
    assert!(app.items_wake().is_none());
}

#[test]
fn a_summary_held_then_a_full_refused_shows_the_message() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    ack_all(&mut app, &first);
    app.close_item();
    let second = open(&mut app, "j_1");
    let pair = subscribes(&second);
    assert_eq!(pair.len(), 2);
    let summary_id = pair[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    let full_id = pair[1]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(session_accepted(DELEGATE_A, summary_id));
    app.on_line(session_refused(DELEGATE_A, full_id, "invalid_arguments"));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(
        view.message
            .as_deref()
            .is_some_and(|message| message.contains("Could not open"))
    );
    assert_eq!(
        app.subscribed_level(&SessionId(DELEGATE_A.to_owned())),
        Some(Level::Summary)
    );
}

#[test]
fn leaving_with_full_held_lowers_after() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.leave();
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
    assert_eq!(due[0]["args"]["level"], "summary");
}

#[test]
fn leaving_with_full_in_flight_lowers_once_on_its_acknowledgement() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    app.leave();
    // While the `full` is in flight the lowering waits and asks no wake.
    assert!(commands(app.items_due(clock.now())).is_empty());
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["args"]["level"], "summary");
    ack_all(&mut app, &due);
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn a_refused_lowering_past_leaving_drops_with_no_retry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    app.leave();
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = commands(app.items_due(clock.now()));
    let lowering = subscribes(&due);
    assert_eq!(lowering.len(), 1);
    let lowering_id = lowering[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_refused(
        DELEGATE_A,
        &lowering_id,
        "session_not_found",
    ));
    assert!(commands(app.items_due(clock.now())).is_empty());
}

#[test]
fn closing_while_full_is_in_flight_lowers_after_home_on_its_acknowledgement() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let full_id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    // Esc closes the view while the `full` is in flight.
    app.on_key(Key::Esc, clock.now());
    assert!(!app.item_open());
    app.leave();
    app.on_line(session_accepted(DELEGATE_A, &full_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
}

#[test]
fn opening_another_item_then_leaving_lowers_both_once_acknowledged() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    let first = open(&mut app, "j_1");
    let first_id = subscribes(&first)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    // Opening the second closes the first while its `full` is in flight.
    let second = open(&mut app, "j_2");
    let second_id = subscribes(&second)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    assert_eq!(subscribes(&second).len(), 1);
    app.leave();
    // While either `full` is in flight its lowering waits.
    assert!(commands(app.items_due(clock.now())).is_empty());
    app.on_line(session_accepted(DELEGATE_A, &first_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_A);
    app.on_line(session_accepted(DELEGATE_B, &second_id));
    let due = subscribes(&commands(app.items_due(clock.now())));
    assert_eq!(due.len(), 1);
    assert_eq!(due[0]["session_id"], DELEGATE_B);
    assert_eq!(due[0]["args"]["level"], "summary");
}

/// Opens `job`'s delegate view with its `full` acknowledged.
fn opened_item(app: &mut App, job: &str) {
    let out = open(app, job);
    ack_all(app, &out);
}

#[test]
fn the_delegate_turn_draws_in_the_view_and_leaves_the_parent_idle() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "review it"}]}]}),
    ));
    app.on_line(other_line(
        DELEGATE_A,
        "text_completed",
        json!({"text": "looks good"}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("looks good")));
    // A delegate line never changes the attached busy flag.
    assert!(matches!(
        app.phase,
        super::super::Phase::Attached { busy: false, .. }
    ));
}

#[test]
fn enter_sends_exactly_one_steer_naming_the_delegate() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("Enter sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
    assert_eq!(
        out[0]["args"],
        json!({"content": [{"type": "text", "text": "focus on the tests"}]})
    );
    assert!(app.draft.is_empty());
}

#[test]
fn a_rejected_steer_shows_its_notice_and_puts_the_draft_back() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("Enter sends"),
    };
    let id = out[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_refused(DELEGATE_A, &id, "busy"));
    assert_eq!(app.draft(), "focus on the tests");
}

#[test]
fn a_slash_close_in_the_view_closes_the_parent_and_the_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("/close");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("/close sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "close");
    assert_eq!(out[0]["session_id"], SESSION);
    assert_eq!(out[0]["args"], json!({"now": true}));
    assert!(!app.item_open());
}

#[test]
fn enter_on_a_completed_delegate_keeps_the_draft() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    app.draft.set("one more thing");
    let effect = app.on_key(Key::Enter, clock.now());
    assert_eq!(effect, Effect::None);
    assert_eq!(app.draft(), "one more thing");
}

/// Folds a plain job as `job`, with no delegate.
fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// One `job_started` at `ts` from tool call `action`.
fn job_started(app: &mut App, job: &str, description: &str, ts: u64, action: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "job_started".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(action.to_owned())),
        seq: None,
        payload: json!({"job_id": job, "description": description,
            "output_path": "/tmp/out"})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
}

/// Folds a `shell` call asking for a pseudo-terminal on action `a_1`, so
/// the next `job_started` on that action takes a grid.
fn mark_tty(app: &mut App) {
    app.on_line(session_line(
        "tool_call_requested",
        json!({"name": "shell", "arguments": {"tty": true}}),
    ));
}

/// Feeds `text` as `job`'s delta.
fn job_text(app: &mut App, job: &str, text: &str) {
    app.on_line(session_line(
        "job_delta",
        json!({"job_id": job, "text": text}),
    ));
}

/// Renders `app` at `width` by `height`: the screen's rows and the click
/// targets.
fn rendered(app: &App, width: u16, height: u16) -> (Vec<String>, Vec<crate::mouse::Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    let rows: Vec<String> = crate::view::text(&buf).lines().map(str::to_owned).collect();
    (rows, targets)
}

/// The wall time of `clock`, in milliseconds.
fn wall_ms(clock: &std::sync::Arc<fakes::clock::FakeClock>) -> u64 {
    contract::clock::wall_ms(clock.wall())
}

#[test]
fn running_tty_job_shows_its_cleared_and_redrawn_screen() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    mark_tty(&mut app);
    job_started(
        &mut app,
        "j_9",
        "run the editor",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(
        &mut app,
        "j_9",
        "stale output\nmore stale\x1b[2J\x1b[H$ edit file.txt\n",
    );
    let _ = open(&mut app, "j_9");
    assert!(app.item_open());
    let (rows, _) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("$ edit file.txt")));
    assert!(!rows.iter().any(|row| row.contains("stale")));
    insta::assert_snapshot!("running_tty_job_screen", rows.join("\n"));
}

#[test]
fn ordinary_job_shows_the_tail_of_its_output() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    job_started(
        &mut app,
        "j_2",
        "build the workspace",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(&mut app, "j_2", "compiling one\ncompiling two\ndone\n");
    let _ = open(&mut app, "j_2");
    assert!(app.item_open());
    let (rows, _) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("done")));
    insta::assert_snapshot!("ordinary_job_tail", rows.join("\n"));
}

#[test]
fn completed_job_keeps_its_output_and_loses_its_stop() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    job_started(
        &mut app,
        "j_2",
        "build the workspace",
        wall_ms(&clock).saturating_sub(11_000),
        "a_1",
    );
    job_text(&mut app, "j_2", "done\n");
    let _ = open(&mut app, "j_2");
    assert!(app.item_open());
    let started = wall_ms(&clock).saturating_sub(11_000);
    app.on_line(Line::Session(contract::Envelope {
        kind: "job_completed".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: started.saturating_add(5_000),
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"job_id": "j_2", "status": "completed"})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("completed")));
    assert!(rows.iter().any(|row| row.contains("5s")));
    assert!(rows.iter().any(|row| row.contains("done")));
    assert!(
        targets
            .iter()
            .all(|target| target.id != crate::mouse::TargetId::Item(crate::app::items::Spot::Stop)),
        "a completed job draws no stop target"
    );
    insta::assert_snapshot!("completed_job_body", rows.join("\n"));
}

#[test]
fn enter_in_a_job_view_keeps_the_draft_with_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    let _ = open(&mut app, "j_1");
    app.draft.set("do it");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "do it");
    assert_eq!(app.notice(), Some("A job takes no input here."));
}

#[test]
fn enter_in_an_other_harness_delegate_view_keeps_the_draft_with_its_notice() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let _ = open(&mut app, "j_1");
    app.draft.set("steer this");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "steer this");
    assert_eq!(app.notice(), Some("This delegate takes no steering here."));
}

#[test]
fn a_resumed_job_opens_from_its_row_again() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out = open(&mut app, "j_1");
    ack_all(&mut app, &out);
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    ack_all(&mut app, &lowered);
    complete(&mut app, "j_1");
    // Completed: the row opens nothing.
    assert_eq!(app.open_serial(serial), Effect::None);
    // The same job id runs again: the row is clickable and opens.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let resumed = match app.open_serial(serial) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("resuming opens"),
    };
    assert_eq!(subscribes(&resumed).len(), 1);
    assert_eq!(resumed[0]["args"]["level"], "full");
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(view.running);
}

#[test]
fn an_open_view_runs_and_steers_again_after_its_job_restarts() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    assert!(!app.item_view().unwrap_or_else(|| panic!("a view")).running);
    // The same job id runs again: the open view returns to running.
    start_delegate(&mut app, "j_1", DELEGATE_A);
    assert!(app.item_view().unwrap_or_else(|| panic!("a view")).running);
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("Enter sends"),
    };
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["command"], "steer");
    assert_eq!(out[0]["session_id"], DELEGATE_A);
}

#[test]
fn opening_a_completed_delegate_swaps_but_sends_nothing() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    assert!(open(&mut app, "j_1").is_empty());
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(view.has_transcript);
    assert!(!view.running);
}

#[test]
fn an_accepted_delegate_steer_closes_its_pending_entry() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.draft.set("focus on the tests");
    let out = match app.on_key(Key::Enter, clock.now()) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("Enter sends"),
    };
    let id = out[0]["id"].as_str().unwrap_or_else(|| panic!("an id"));
    assert!(app.pending.contains_key(id));
    app.on_line(session_accepted(DELEGATE_A, id));
    assert!(!app.pending.contains_key(id));
}

#[test]
fn a_cancelled_item_keeps_the_cancelled_status_glyph() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(session_line(
        "job_completed",
        json!({"job_id": "j_1", "status": "cancelled"}),
    ));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.word, "cancelled");
    assert_eq!(view.glyph, "■");
}

#[test]
fn the_delegate_call_count_counts_its_tool_calls() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.calls, Some(2));
}

#[test]
fn reopening_restarts_the_call_count_for_the_replay() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(2)
    );
    app.close_item();
    let lowered = commands(app.items_due(clock.now()));
    ack_all(&mut app, &lowered);
    opened_item(&mut app, "j_1");
    // The fresh screen replays the transcript: the same two
    // completions count once, not on top of the first viewing.
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.calls, Some(2));
}

#[test]
fn paging_follows_the_open_delegate() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    // The shown screen is the delegate's: history pages for it.
    assert_eq!(
        app.paging_session(),
        Some(&SessionId(DELEGATE_A.to_owned()))
    );
    app.on_line(other_line(
        DELEGATE_A,
        "turn_started",
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "deep"}]}]}),
    ));
    assert!(shown(&app).iter().any(|row| row.contains("deep")));
    assert!(app.needs().is_empty());
    app.close_item();
    assert_eq!(app.paging_session(), Some(&SessionId(SESSION.to_owned())));
}

#[test]
fn clicking_a_delegate_row_opens_that_delegate() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    // The second row of the second delegate names its serial.
    let serial = app
        .serial_of_job(&JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out = match app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))) {
        Effect::Send(lines) => commands(lines),
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_)
        | Effect::ReadImage(_) => panic!("opening sends"),
    };
    // No card subscribe went out here, so the click sends the view's
    // `full` subscribe at once.
    let full = subscribes(&out);
    assert_eq!(full.len(), 1);
    assert_eq!(full[0]["args"]["level"], "full");
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
}

#[test]
fn clicking_the_first_drawn_row_with_the_card_scrolled_opens_the_second_delegate() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    app.set_size(160, 40);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    start_delegate(&mut app, "j_3", "s_ffffffffffffffff");
    start_delegate(&mut app, "j_4", "s_0000000000000000");
    app.panel_state.scroll_delegates(false);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    let area = ratatui::layout::Rect::new(0, 0, rect.width, rect.height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let mut targets = Vec::new();
    crate::view::panel::draw(&app, area, &mut buf, &mut targets);
    let first = targets
        .iter()
        .filter_map(|target| {
            if let crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial)) = target.id {
                Some(serial)
            } else {
                None
            }
        })
        .next()
        .unwrap_or_else(|| panic!("a delegate target"));
    assert_eq!(first, 2);
    app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(first)));
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert_eq!(view.description, "task j_2");
}

#[test]
fn a_press_and_release_on_different_serials_opens_nothing() {
    use crate::app::panel::Spot as PanelSpot;
    use crate::keys::{Button, Mouse, MouseKind};
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    start_delegate(&mut app, "j_2", DELEGATE_B);
    let cell = ratatui::layout::Rect::new(4, 8, 10, 1);
    let before = vec![crate::mouse::Target {
        id: crate::mouse::TargetId::Panel(PanelSpot::Delegate(1)),
        rect: cell,
    }];
    let after = vec![crate::mouse::Target {
        id: crate::mouse::TargetId::Panel(PanelSpot::Delegate(2)),
        rect: cell,
    }];
    let mut pointer = crate::mouse::Pointer::default();
    // A press on delegate A, A's `job_completed`, a redraw that puts B
    // on that row, and a release on it: the ids differ, so no click.
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col: 4,
                row: 8
            },
            &before,
            false
        ),
        None
    );
    complete(&mut app, "j_1");
    assert_eq!(
        pointer.on_mouse(
            &Mouse {
                kind: MouseKind::Release,
                col: 4,
                row: 8
            },
            &after,
            false
        ),
        None
    );
    assert!(!app.item_open());
}

#[test]
fn a_click_on_a_completed_job_serial_opens_nothing() {
    use crate::app::panel::Spot as PanelSpot;
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    complete(&mut app, "j_1");
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))),
        Effect::None
    );
    assert!(!app.item_open());
}

#[test]
fn another_harness_delegate_opens_without_a_subscribe() {
    use crate::app::panel::Spot as PanelSpot;
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial))),
        Effect::None
    );
    let view = app.item_view().unwrap_or_else(|| panic!("a view"));
    assert!(!view.has_transcript);
    assert_eq!(view.output_path, "/tmp/other.out");
    // Steering goes nowhere: Enter keeps the draft with the notice.
    app.draft.set("hello?");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "hello?");
}

#[test]
fn reopening_the_same_item_keeps_its_replayed_transcript() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let first = open(&mut app, "j_1");
    assert_eq!(subscribes(&first).len(), 1);
    ack_all(&mut app, &first);
    app.on_line(other_line(DELEGATE_A, "tool_call_completed", json!({})));
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(1)
    );
    assert!(open(&mut app, "j_1").is_empty());
    assert_eq!(
        app.item_view().unwrap_or_else(|| panic!("a view")).calls,
        Some(1)
    );
}

#[test]
fn stop_on_a_completed_item_sends_nothing() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    complete(&mut app, "j_1");
    assert_eq!(app.item_click(super::Spot::Stop), Effect::None);
}

#[test]
fn an_empty_draft_enter_in_the_view_sends_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    assert!(app.draft.is_empty());
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
}

#[test]
fn with_the_link_down_enter_and_stop_do_nothing() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    opened_item(&mut app, "j_1");
    app.connect_failed("down".to_owned());
    app.draft.set("steer me");
    assert_eq!(app.on_key(Key::Enter, clock.now()), Effect::None);
    assert_eq!(app.draft(), "steer me");
    assert_eq!(app.item_click(super::Spot::Stop), Effect::None);
}

#[test]
fn open_serial_with_no_job_opens_nothing() {
    let mut app = home();
    opened(&mut app);
    assert_eq!(app.open_serial(999), Effect::None);
    assert!(!app.item_open());
}

#[test]
fn paging_stays_on_the_parent_without_a_fiber_transcript() {
    let mut app = home();
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE_A,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    use crate::app::panel::Spot as PanelSpot;
    app.on_click(crate::mouse::TargetId::Panel(PanelSpot::Delegate(serial)));
    assert_eq!(app.paging_session(), Some(&SessionId(SESSION.to_owned())));
}

#[test]
fn take_wake_prefers_the_earlier_retry_over_the_status_tick() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    tick(&mut app, &clock);
    start_delegate(&mut app, "j_1", DELEGATE_A);
    let out = open(&mut app, "j_1");
    let id = subscribes(&out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("an id"));
    let refused_at = clock.now();
    tick(&mut app, &clock);
    app.on_line(session_refused(DELEGATE_A, &id, "session_not_found"));
    // Under reduced motion nothing else ticks: the draw asks the next
    // whole second for the running status row, and the retry 500 ms out
    // is earlier.
    app.set_reduced_motion(true);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let _ = crate::view::render(&app, area, &mut buf, None);
    assert_eq!(app.take_wake(), Some(refused_at + RETRY));
}
