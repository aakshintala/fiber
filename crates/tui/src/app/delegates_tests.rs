//! Tests for the Delegates card's wheel: over the card it scrolls the
//! card by one delegate, anywhere else on the panel it scrolls the panel.
//! Tests for its `summary` subscriptions: the parent's `session_status`
//! subscribes each running Fiber delegate once, and a delegate's own
//! status is stored, never a home row.

use super::super::{App, Effect};
use crate::home::{Launch, Spot as HomeSpot};
use crate::keys::{Mouse, MouseKind};
use crate::link::Line;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const DELEGATE: &str = "s_dddddddddddddddd";

/// An app with home state, attached at 160 by 40, drawing `names`.
fn panel_app(names: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: names.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(160, 40);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds Fiber delegates `j_1` to `j_<n>`.
fn fold_delegates(app: &mut App, n: usize) {
    for k in 1..=n {
        let job = format!("j_{k}");
        app.on_line(session_line(
            "job_started",
            serde_json::json!({"job_id": job, "description": format!("task {k}"),
                "output_path": "/tmp/out"}),
        ));
        app.on_line(session_line(
            "delegate_started",
            serde_json::json!({"job_id": job,
                "delegate_session_id": format!("s_{job:0>16}"),
                "harness": "fiber", "model": "test/model", "workspace": "/w"}),
        ));
    }
}

/// Folds delegate job `job`'s `job_completed`.
fn complete(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_completed",
        serde_json::json!({"job_id": job, "status": "completed"}),
    ));
}

/// Folds a widget of sixty lines.
fn big_widget(app: &mut App) {
    let lines: Vec<String> = (0..60).map(|n| format!("line {n:02}")).collect();
    app.on_line(session_line(
        "extension_ui",
        serde_json::json!({"extension": "plan", "widget": "tasks", "lines": lines}),
    ));
}

/// The panel rect.
fn panel(app: &App) -> Rect {
    app.chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"))
}

/// A wheel report over the panel at screen row `row`.
fn wheel(app: &mut App, up: bool, row: u16) {
    let kind = if up {
        MouseKind::WheelUp
    } else {
        MouseKind::WheelDown
    };
    let col = panel(app).x.saturating_add(4);
    app.on_wheel(&Mouse { kind, col, row });
}

/// The screen row of the panel's first drawn row.
fn first_row(app: &App) -> u16 {
    panel(app).y.saturating_add(1)
}

#[test]
fn the_wheel_over_the_card_scrolls_it_by_one_delegate_and_clamps() {
    let mut app = panel_app(&["delegates"]);
    fold_delegates(&mut app, 5);
    let row = first_row(&app);
    for expected in [1, 2, 2] {
        wheel(&mut app, false, row);
        assert_eq!(app.panel_state().delegate_scroll(), expected);
    }
    wheel(&mut app, true, row);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    wheel(&mut app, true, row);
    wheel(&mut app, true, row);
    assert_eq!(app.panel_state().delegate_scroll(), 0);
    assert_eq!(app.panel_state().scroll(), 0);
}

#[test]
fn with_three_delegates_the_wheel_over_the_card_does_nothing() {
    let mut app = panel_app(&["delegates"]);
    fold_delegates(&mut app, 3);
    big_widget(&mut app);
    let row = first_row(&app);
    wheel(&mut app, false, row);
    assert_eq!(app.panel_state().delegate_scroll(), 0);
    assert_eq!(app.panel_state().scroll(), 0);
}

#[test]
fn with_four_delegates_one_step_down_moves_one() {
    let mut app = panel_app(&["delegates"]);
    fold_delegates(&mut app, 4);
    let row = first_row(&app);
    wheel(&mut app, false, row);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    wheel(&mut app, false, row);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
}

#[test]
fn the_wheel_over_another_card_scrolls_the_panel_not_the_card() {
    let mut app = panel_app(&["delegates"]);
    fold_delegates(&mut app, 4);
    big_widget(&mut app);
    // The card's eight rows with its edges, a blank row, then the
    // widget's title row.
    let last = first_row(&app).saturating_add(7);
    wheel(&mut app, false, last);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    wheel(&mut app, false, last.saturating_add(1));
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    assert_eq!(app.panel_state().scroll(), 3);
}

#[test]
fn the_wheel_on_the_panels_blank_top_row_scrolls_the_panel() {
    let mut app = panel_app(&["delegates"]);
    // Five, so a step the top row took for the card would show.
    fold_delegates(&mut app, 5);
    big_widget(&mut app);
    let top = panel(&app).y;
    wheel(&mut app, false, top.saturating_add(1));
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    assert_eq!(app.panel_state().scroll(), 0);
    wheel(&mut app, false, top);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    assert_eq!(app.panel_state().scroll(), 3);
}

#[test]
fn the_wheel_finds_the_card_with_the_panel_scrolled() {
    let mut app = panel_app(&["plan/tasks", "delegates"]);
    big_widget(&mut app);
    fold_delegates(&mut app, 4);
    let first = first_row(&app);
    for _ in 0..30 {
        wheel(&mut app, false, first);
    }
    let skip = app.panel_state().scroll();
    assert!(skip > 0);
    assert_eq!(app.panel_state().delegate_scroll(), 0);
    // The widget's 61 text rows with its edges, and a blank row, come
    // before the card.
    let card = first.saturating_add(u16::try_from(64 - skip).unwrap_or(u16::MAX));
    wheel(&mut app, false, card.saturating_sub(1));
    assert_eq!(app.panel_state().delegate_scroll(), 0);
    assert_eq!(app.panel_state().scroll(), skip);
    wheel(&mut app, false, card);
    assert_eq!(app.panel_state().delegate_scroll(), 1);
    assert_eq!(app.panel_state().scroll(), skip);
}

#[test]
fn a_stored_offset_past_the_end_clamps_before_moving() {
    let mut app = panel_app(&["delegates"]);
    fold_delegates(&mut app, 6);
    let row = first_row(&app);
    for _ in 0..3 {
        wheel(&mut app, false, row);
    }
    assert_eq!(app.panel_state().delegate_scroll(), 3);
    complete(&mut app, "j_5");
    complete(&mut app, "j_6");
    wheel(&mut app, true, row);
    assert_eq!(app.panel_state().delegate_scroll(), 0);
}

/// The default card list, with the Delegates card.
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app on home at 80x24, drawing the default cards.
fn home() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
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

/// Parses command lines going out.
fn commands(lines: Vec<String>) -> Vec<Value> {
    lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect()
}

/// Links the app: the feed and recent ids.
fn linked(app: &mut App) -> (String, String) {
    let lines = commands(app.on_line(hello()));
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
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A delegate's `session_status` in `state`, naming its parent.
fn delegate_status(session: &str, parent: &str, state: Value) -> Line {
    let mut payload = json!({
        "name": "delegate one", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        "parent": parent,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// The home rows' keys.
fn keys(app: &App) -> Vec<u64> {
    app.home_screen()
        .map(|screen| screen.rows.into_iter().map(|(key, _, _)| key).collect())
        .unwrap_or_default()
}

/// Opens the first home row, with the parsed lines going out.
fn open_first(app: &mut App) -> Vec<Value> {
    let key = keys(app)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a row"));
    match app.home_click(HomeSpot::Entry(key)) {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
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
    }
}

/// Reopens `SESSION` from home: opens and acknowledges it, without
/// linking again.
fn reopen(app: &mut App) {
    let out = open_first(app);
    ack(app, &out);
}

/// Opens `SESSION` through home: links, feeds its row, opens and
/// acknowledges it.
fn opened(app: &mut App) {
    linked(app);
    app.on_line(live(SESSION, json!({"state": "streaming"})));
    let out = open_first(app);
    ack(app, &out);
}

/// The acknowledgement of the last subscribe in `out`.
fn ack(app: &mut App, out: &[Value]) {
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &id));
}

/// A session `command_accepted` for `id` from `session`.
fn session_accepted(session: &str, id: &str) -> Line {
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    })
}

/// A session `command_rejected` for `id` from `session`.
fn session_refused(session: &str, id: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), Value::String(id.to_owned())),
            (
                "code".to_owned(),
                Value::String("session_not_found".to_owned()),
            ),
            ("message".to_owned(), Value::String("no".to_owned())),
        ]
        .into_iter()
        .collect(),
    })
}

/// Folds a started job `job` with `description` on the attached session.
fn start_job(app: &mut App, job: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        serde_json::json!({"job_id": job, "description": description,
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a Fiber delegate as job `job` with session `delegate`.
fn start_delegate(app: &mut App, job: &str, delegate: &str) {
    start_job(app, job, &format!("task {job}"));
    app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Folds a delegate on `harness` as job `job` with session `delegate`.
fn start_other(app: &mut App, job: &str, delegate: &str, harness: &str) {
    start_job(app, job, &format!("task {job}"));
    app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": delegate,
            "harness": harness, "model": "other/model", "workspace": "/w"}),
    ));
}

/// The attached session's next `session_status`, with the parsed lines it
/// sends.
fn parent_status(app: &mut App) -> Vec<Value> {
    commands(app.on_line(live(SESSION, json!({"state": "streaming"}))))
}

/// The first `subscribe` id among `out`.
fn subscribe_id(out: &[Value]) -> String {
    subscribes(out)
        .into_iter()
        .next()
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("a subscribe"))
}

/// The `subscribe` lines among `out`.
fn subscribes(out: &[Value]) -> Vec<Value> {
    out.iter()
        .filter(|line| line["command"] == "subscribe")
        .cloned()
        .collect()
}

#[test]
fn a_running_fiber_delegate_is_subscribed_at_summary_on_the_parents_next_status() {
    let mut app = home();
    opened(&mut app);
    let started = commands(app.on_line(session_line(
        "job_started",
        serde_json::json!({"job_id": "j_1", "description": "task",
            "output_path": "/tmp/out"}),
    )));
    assert!(subscribes(&started).is_empty());
    let delegate = commands(app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    )));
    assert!(subscribes(&delegate).is_empty());
    let out = parent_status(&mut app);
    let subscribed = subscribes(&out);
    assert_eq!(subscribed.len(), 1);
    assert_eq!(subscribed[0]["session_id"], DELEGATE);
    assert_eq!(subscribed[0]["args"]["level"], "summary");
}

#[test]
fn replayed_delegates_that_finished_are_never_subscribed() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    app.on_line(session_line(
        "delegate_finished",
        serde_json::json!({"job_id": "j_1", "text": "done",
            "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": null, "subscription_cost": 0.0}}),
    ));
    complete(&mut app, "j_1");
    let out = parent_status(&mut app);
    assert!(subscribes(&out).is_empty());
}

#[test]
fn a_second_status_sends_no_second_subscribe() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let out = parent_status(&mut app);
    let first = subscribes(&out);
    assert_eq!(first.len(), 1);
    let out = parent_status(&mut app);
    assert!(subscribes(&out).is_empty());
}

#[test]
fn a_refused_subscribe_is_not_sent_again_on_this_attachment() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_refused(DELEGATE, &id));
    let out = parent_status(&mut app);
    let second = subscribes(&out);
    assert!(second.is_empty());
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
    assert_eq!(app.panel_state().running_delegates().len(), 1);
}

#[test]
fn a_held_delegate_is_not_subscribed_again_after_going_home_and_back() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_accepted(DELEGATE, &id));
    app.leave();
    reopen(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let out = parent_status(&mut app);
    let second = subscribes(&out);
    assert!(second.is_empty());
}

#[test]
fn a_refused_delegate_is_tried_again_after_reopening_the_parent() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_refused(DELEGATE, &id));
    app.leave();
    reopen(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let out = parent_status(&mut app);
    let second = subscribes(&out);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0]["session_id"], DELEGATE);
}

#[test]
fn a_delegate_on_another_harness_is_never_subscribed() {
    let mut app = home();
    opened(&mut app);
    start_other(&mut app, "j_1", DELEGATE, "claude");
    let out = parent_status(&mut app);
    assert!(subscribes(&out).is_empty());
}

#[test]
fn no_subscribe_with_the_link_down() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    app.disconnected();
    let out = commands(app.on_line(live(SESSION, json!({"state": "streaming"}))));
    assert!(subscribes(&out).is_empty());
}

#[test]
fn no_subscribe_without_the_delegates_card() {
    let mut app = home();
    opened(&mut app);
    app.home
        .as_mut()
        .unwrap_or_else(|| panic!("home"))
        .launch
        .panel_cards = vec!["session".to_owned()];
    start_delegate(&mut app, "j_1", DELEGATE);
    let out = parent_status(&mut app);
    assert!(subscribes(&out).is_empty());
}

#[test]
fn a_delegates_status_never_becomes_a_home_row() {
    let mut app = home();
    opened(&mut app);
    let (rows, _) = app.rail_cards().unwrap_or_else(|| panic!("cards"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.0, SESSION);
    app.on_line(delegate_status(
        DELEGATE,
        SESSION,
        json!({"state": "streaming"}),
    ));
    let (rows, _) = app.rail_cards().unwrap_or_else(|| panic!("cards"));
    assert_eq!(rows.len(), 1);
    assert!(rows.iter().all(|row| row.id.0 != DELEGATE));
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_some()
    );
}

#[test]
fn a_status_without_a_parent_still_becomes_a_home_row() {
    let mut app = home();
    opened(&mut app);
    app.on_line(live("s_bbbbbbbbbbbbbbbb", json!({"state": "streaming"})));
    let (rows, _) = app.rail_cards().unwrap_or_else(|| panic!("cards"));
    assert_eq!(rows.len(), 2);
    assert!(
        app.delegate_row(&contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()))
            .is_none()
    );
}

#[test]
fn the_attached_sessions_own_status_naming_a_parent_folds_as_today() {
    let mut app = home();
    opened(&mut app);
    let mut line = live(SESSION, json!({"state": "streaming"}));
    if let Line::Session(envelope) = &mut line {
        envelope.payload.insert(
            "parent".to_owned(),
            Value::String("s_bbbbbbbbbbbbbbbb".to_owned()),
        );
    }
    app.on_line(line);
    assert!(app.panel_state().status().is_some());
    assert!(
        app.delegate_row(&contract::SessionId(SESSION.to_owned()))
            .is_none()
    );
}

#[test]
fn a_delegates_status_is_kept_across_going_home_and_back() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_accepted(DELEGATE, &id));
    app.on_line(delegate_status(
        DELEGATE,
        SESSION,
        json!({"state": "streaming"}),
    ));
    app.leave();
    reopen(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    // A `job_completed` for a job that is no delegate's removes nothing.
    start_job(&mut app, "j_9", "build");
    app.on_line(session_line(
        "job_completed",
        serde_json::json!({"job_id": "j_9", "status": "completed"}),
    ));
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_some()
    );
}

#[test]
fn sending_a_subscribe_drops_the_stored_row() {
    let mut app = home();
    opened(&mut app);
    app.on_line(delegate_status(
        DELEGATE,
        SESSION,
        json!({"state": "streaming"}),
    ));
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_some()
    );
    start_delegate(&mut app, "j_1", DELEGATE);
    let out = parent_status(&mut app);
    let subscribed = subscribes(&out);
    assert_eq!(subscribed.len(), 1);
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
    let id = subscribed[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    app.on_line(session_refused(DELEGATE, &id));
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
}

#[test]
fn a_delegates_job_completed_drops_its_stored_row() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_accepted(DELEGATE, &id));
    app.on_line(delegate_status(
        DELEGATE,
        SESSION,
        json!({"state": "streaming"}),
    ));
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_some()
    );
    complete(&mut app, "j_1");
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
}

#[test]
fn a_replayed_job_completed_drops_its_stored_row() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_accepted(DELEGATE, &id));
    app.on_line(delegate_status(
        DELEGATE,
        SESSION,
        json!({"state": "streaming"}),
    ));
    app.leave();
    reopen(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    complete(&mut app, "j_1");
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
}

#[test]
fn a_later_run_shows_the_fold_not_the_earlier_status() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", DELEGATE);
    let first = parent_status(&mut app);
    let id = subscribe_id(&first);
    app.on_line(session_accepted(DELEGATE, &id));
    app.on_line(delegate_status(DELEGATE, SESSION, json!({"state": "idle"})));
    complete(&mut app, "j_1");
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
    start_job(&mut app, "j_2", "task again");
    app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": "j_2",
            "delegate_session_id": DELEGATE,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
    let out = parent_status(&mut app);
    assert!(subscribes(&out).is_empty());
    assert!(
        app.delegate_row(&contract::SessionId(DELEGATE.to_owned()))
            .is_none()
    );
    assert_eq!(app.panel_state().running_delegates().len(), 1);
}
