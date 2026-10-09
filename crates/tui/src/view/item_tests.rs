//! Tests for the item view's header, status row, stop target and Esc:
//! snapshots at 80x24 and 60x20, clicks, focus, the wheel and the elapsed
//! boundary (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;

use contract::SessionId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};

use crate::app::App;
use crate::app::items::Spot;
use crate::home::Launch;
use crate::keys::{Button, Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::{Target, TargetId};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const DELEGATE: &str = "s_dddddddddddddddd";

/// An app on home at `width` by `height`, drawing the default cards.
fn home(width: u16, height: u16) -> App {
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
    app.set_size(width, height);
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

/// One envelope of the attached session at `ts`.
fn session_line(kind: &str, ts: u64, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// One envelope of the delegate at `ts`.
fn delegate_line(kind: &str, ts: u64, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(DELEGATE.to_owned()),
        ts,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for `session`, merged over the defaults.
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

/// The delegate's `session_status` naming its parent.
fn delegate_status(state: Value) -> Line {
    let mut payload = json!({
        "name": "delegate one", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        "parent": SESSION,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: SessionId(DELEGATE.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
        .collect();
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
    let out: Vec<Value> = match app.on_click(TargetId::Home(crate::home::Spot::Entry(key))) {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id));
    app.on_line(Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    }));
}

/// Folds a Fiber delegate as `j_1` with its `delegate_started` at `ts`.
fn start_delegate(app: &mut App, ts: u64) {
    app.on_line(session_line(
        "job_started",
        ts,
        json!({"job_id": "j_1", "description": "review the parser",
            "output_path": "/tmp/out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        ts,
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE,
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    ));
}

/// Opens the delegate's view through its Delegates card row.
fn open_item(app: &mut App) {
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    match app.on_click(TargetId::Panel(crate::app::panel::Spot::Delegate(serial))) {
        crate::app::Effect::Send(lines) => {
            // Acknowledge the `full` subscribe at once, so the transcript
            // replays from a held level.
            for line in lines {
                let line: Value =
                    serde_json::from_str(&line).unwrap_or_else(|err| panic!("{line}: {err}"));
                if line["command"] == "subscribe" {
                    let id = line["id"].as_str().unwrap_or_else(|| panic!("an id"));
                    let session = line["session_id"]
                        .as_str()
                        .unwrap_or_else(|| panic!("a session"));
                    let mut payload = serde_json::Map::new();
                    payload.insert("command_id".to_owned(), Value::String(id.to_owned()));
                    app.on_line(Line::Session(contract::Envelope {
                        kind: "command_accepted".to_owned(),
                        session_id: SessionId(session.to_owned()),
                        ts: 0,
                        schema_version: contract::SCHEMA_VERSION,
                        turn_id: None,
                        action_id: None,
                        seq: None,
                        payload,
                    }));
                }
            }
        }
        crate::app::Effect::None => {}
        crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("opening sends or nothing"),
    }
    assert!(app.item_open());
}

/// Renders `app` at `width` by `height`: the screen's rows and the click
/// targets.
fn rendered(app: &App, width: u16, height: u16) -> (Vec<String>, Vec<Target>) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    let rows: Vec<String> = crate::view::text(&buf).lines().map(str::to_owned).collect();
    (rows, targets)
}

/// Renders `app` and takes the frame's targets, so focus steps through
/// what is drawn.
fn frame(app: &mut App, width: u16, height: u16) -> Vec<Target> {
    let (_, targets) = rendered(app, width, height);
    app.drawn(&targets);
    targets
}

/// The wall time of `clock`, in milliseconds.
fn wall_ms(clock: &std::sync::Arc<fakes::clock::FakeClock>) -> u64 {
    contract::clock::wall_ms(clock.wall())
}

#[test]
fn running_delegate_header_with_held_status() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    // Sixty seconds in: the status row reads whole minutes.
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(61_000));
    // The delegate's own summary is held: the glyph and word name it.
    app.on_line(delegate_status(json!({"state": "streaming"})));
    open_item(&mut app);
    let (rows, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("running_delegate_header_with_held_status", rows.join("\n"));
}

#[test]
fn running_delegate_header_unheld() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(11_000));
    open_item(&mut app);
    let (rows, _) = rendered(&app, 80, 24);
    insta::assert_snapshot!("running_delegate_header_unheld", rows.join("\n"));
}

#[test]
fn running_delegate_header_at_sixty_by_twenty() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(60, 20);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(11_000));
    open_item(&mut app);
    let (rows, _) = rendered(&app, 60, 20);
    insta::assert_snapshot!(
        "running_delegate_header_at_sixty_by_twenty",
        rows.join("\n")
    );
}

#[test]
fn completed_delegate_header_freezes_the_elapsed_and_drops_the_stop() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    let started = wall_ms(&clock).saturating_sub(300_000);
    start_delegate(&mut app, started);
    open_item(&mut app);
    app.on_line(session_line(
        "job_completed",
        started.saturating_add(120_000),
        json!({"job_id": "j_1", "status": "failed"}),
    ));
    // Time passing after the completion changes nothing drawn.
    clock.advance(std::time::Duration::from_secs(3600));
    app.set_now(clock.now(), wall_ms(&clock));
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("failed")));
    assert!(rows.iter().any(|row| row.contains("2m 00s")));
    assert!(
        targets
            .iter()
            .all(|target| target.id != TargetId::Item(Spot::Stop)),
        "a completed item draws no stop target"
    );
    insta::assert_snapshot!("completed_delegate_header", rows.join("\n"));
}

#[test]
fn breadcrumb_names_main_without_a_session_name() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    app.on_line(live(SESSION, json!({"state": "streaming", "name": ""})));
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(5_000));
    open_item(&mut app);
    let (rows, _) = rendered(&app, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("main › ◆ review the parser"))
    );
    insta::assert_snapshot!("breadcrumb_names_main", rows.join("\n"));
}

#[test]
fn breadcrumb_names_the_session() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(5_000));
    open_item(&mut app);
    let (rows, _) = rendered(&app, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("fix the parser › ◆ review the parser"))
    );
    insta::assert_snapshot!("breadcrumb_names_the_session", rows.join("\n"));
}

#[test]
fn clicking_the_cross_closes_the_view() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    let (_, targets) = rendered(&app, 80, 24);
    let close = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Close))
        .unwrap_or_else(|| panic!("a ✕ target"));
    let _ = close;
    assert_eq!(
        app.on_click(TargetId::Item(Spot::Close)),
        crate::app::Effect::None
    );
    assert!(!app.item_open());
}

#[test]
fn clicking_stop_sends_job_stop_to_the_parent() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    let (_, targets) = rendered(&app, 80, 24);
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Item(Spot::Stop))
    );
    let lines: Vec<Value> = match app.on_click(TargetId::Item(Spot::Stop)) {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("stop sends"),
    };
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "job_stop");
    assert_eq!(lines[0]["session_id"], SESSION);
    assert_eq!(lines[0]["args"], json!({"job_id": "j_1"}));
}

#[test]
fn the_stop_target_is_reached_from_the_keyboard() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    // A delegate turn gives the conversation an item to move from.
    app.on_line(delegate_line(
        "turn_started",
        0,
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "review it"}]}]}),
    ));
    assert_eq!(
        app.on_key(Key::BackTab, clock.now()),
        crate::app::Effect::None
    );
    frame(&mut app, 80, 24);
    let mut reached = app.focused() == Some(TargetId::Item(Spot::Stop));
    for _ in 0..20 {
        if reached {
            break;
        }
        assert_eq!(app.on_key(Key::Up, clock.now()), crate::app::Effect::None);
        frame(&mut app, 80, 24);
        reached = app.focused() == Some(TargetId::Item(Spot::Stop));
    }
    assert!(reached, "Shift+Tab then ↑ reaches the stop target");
    let lines: Vec<Value> = match app.on_key(Key::Enter, clock.now()) {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("Enter stops"),
    };
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "job_stop");
}

#[test]
fn esc_with_a_notice_open_closes_the_notice_first() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    app.push_notice("later".to_owned());
    // Opening the notice shows its whole text over the view; Esc closes
    // that overlay first and the view stays.
    let (_, targets) = rendered(&app, 80, 24);
    let id = targets
        .iter()
        .find_map(|target| {
            if let TargetId::Notice(id) = target.id {
                Some(id)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("a notice target"));
    assert_eq!(app.on_click(TargetId::Notice(id)), crate::app::Effect::None);
    assert_eq!(app.on_key(Key::Esc, clock.now()), crate::app::Effect::None);
    assert!(app.item_open());
    assert_eq!(app.on_key(Key::Esc, clock.now()), crate::app::Effect::None);
    assert!(!app.item_open());
}

#[test]
fn esc_never_sends_cancel_while_the_parent_is_busy() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    // The parent's turn runs while the delegate's view is open.
    app.on_line(session_line(
        "turn_started",
        0,
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "fix it"}]}]}),
    ));
    start_delegate(&mut app, 0);
    open_item(&mut app);
    assert_eq!(app.on_key(Key::Esc, clock.now()), crate::app::Effect::None);
    assert!(!app.item_open());
}

#[test]
fn the_wheel_over_the_transcript_scrolls_it() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    start_delegate(&mut app, 0);
    open_item(&mut app);
    for n in 0..30 {
        app.on_line(delegate_line(
            "turn_started",
            0,
            json!({"input": [{
                "type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("turn {n}")}]}]}),
        ));
    }
    let height = app.conversation_height();
    assert!(height > 0);
    // Below the header's two rows, on the transcript's last row.
    let row = 2u16.saturating_add(u16::try_from(height).unwrap_or(u16::MAX).saturating_sub(1));
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 0,
        row,
    });
    assert!(app.top().is_some());
    // Over the header the wheel scrolls nothing: the view follows again
    // and a header-row wheel leaves it there.
    app.on_key(Key::End, clock.now());
    assert_eq!(app.top(), None);
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 0,
        row: 0,
    });
    assert_eq!(app.top(), None);
}

#[test]
fn a_drag_selection_starts_at_the_transcript_first_row() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    app.on_line(delegate_line(
        "turn_started",
        0,
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "review it"}]}]}),
    ));
    app.on_line(delegate_line(
        "text_completed",
        0,
        json!({"text": "looks good"}),
    ));
    let (_, targets) = rendered(&app, 80, 24);
    // The view bottom-aligns a short transcript: the press goes on its
    // last drawn row, the drag to the area's top clamps to its first.
    let area = app.conversation_area();
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: area.x,
        row: area.bottom().saturating_sub(1),
    };
    assert_eq!(app.on_select(&press, &targets), crate::app::Effect::None);
    let drag = Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: area.x.saturating_add(10),
        row: area.y.saturating_add(2),
    };
    assert_eq!(app.on_select(&drag, &targets), crate::app::Effect::None);
    match app.on_select(
        &Mouse {
            kind: MouseKind::Release,
            col: area.x.saturating_add(10),
            row: area.y.saturating_add(2),
        },
        &targets,
    ) {
        crate::app::Effect::Copy(text) => assert!(text.contains("review it"), "{text}"),
        crate::app::Effect::None
        | crate::app::Effect::Send(_)
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("a release copies"),
    }
}

#[test]
fn elapsed_counts_whole_seconds_at_each_side_of_the_minute() {
    let clock = fakes::clock::FakeClock::new();
    for (ago, text) in [(59_000, "59s"), (60_000, "1m 00s"), (61_000, "1m 01s")] {
        let mut app = home(80, 24);
        opened(&mut app);
        app.set_now(clock.now(), wall_ms(&clock));
        start_delegate(&mut app, wall_ms(&clock).saturating_sub(ago));
        open_item(&mut app);
        let (rows, _) = rendered(&app, 80, 24);
        let status = rows
            .iter()
            .find(|row| row.contains("harness"))
            .cloned()
            .unwrap_or_default();
        assert!(status.contains(text), "{text} in {status}");
    }
}

#[test]
fn control_f_opens_no_search_in_an_item_view() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    assert_eq!(
        app.on_key(Key::CtrlF, clock.now()),
        crate::app::Effect::None
    );
    assert!(app.find_bar().is_none());
}

#[test]
fn another_harness_body_shows_the_output_path() {
    let mut app = home(80, 24);
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        0,
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        0,
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    app.on_click(TargetId::Panel(crate::app::panel::Spot::Delegate(serial)));
    let (rows, _) = rendered(&app, 80, 24);
    assert!(
        rows.iter()
            .any(|row| row.contains("Output: /tmp/other.out"))
    );
}

#[test]
fn no_selection_starts_in_a_view_without_a_transcript() {
    let mut app = home(80, 24);
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        0,
        json!({"job_id": "j_1", "description": "task j_1", "output_path": "/tmp/other.out"}),
    ));
    app.on_line(session_line(
        "delegate_started",
        0,
        json!({"job_id": "j_1",
            "delegate_session_id": DELEGATE,
            "harness": "claude", "model": "other/model", "workspace": "/w"}),
    ));
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    app.on_click(TargetId::Panel(crate::app::panel::Spot::Delegate(serial)));
    let (_, targets) = rendered(&app, 80, 24);
    // The conversation is covered: a press starts no selection.
    let area = app.conversation_area();
    assert_eq!(
        app.on_select(
            &Mouse {
                kind: MouseKind::Press(Button::Left),
                col: area.x,
                row: area.bottom().saturating_sub(1),
            },
            &targets
        ),
        crate::app::Effect::None
    );
}

#[test]
fn a_refusal_message_draws_on_the_status_row() {
    let mut app = home(160, 40);
    opened(&mut app);
    start_delegate(&mut app, 0);
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let out: Vec<Value> = match app
        .on_click(TargetId::Panel(crate::app::panel::Spot::Delegate(serial)))
    {
        crate::app::Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|err| panic!("{line}: {err}")))
            .collect(),
        crate::app::Effect::None
        | crate::app::Effect::Quit
        | crate::app::Effect::ListFiles
        | crate::app::Effect::FindPause { .. }
        | crate::app::Effect::Search { .. }
        | crate::app::Effect::Editor { .. }
        | crate::app::Effect::Exit(_)
        | crate::app::Effect::Copy(_)
        | crate::app::Effect::OpenLink(_)
        | crate::app::Effect::OpenFile(_)
        | crate::app::Effect::ReadImage(_) => panic!("opening sends"),
    };
    let id = out
        .iter()
        .find(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("an id"))
        .to_owned();
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(id));
    payload.insert(
        "code".to_owned(),
        Value::String("invalid_arguments".to_owned()),
    );
    payload.insert(
        "message".to_owned(),
        Value::String("already full".to_owned()),
    );
    app.on_line(Line::Session(contract::Envelope {
        kind: "command_rejected".to_owned(),
        session_id: SessionId(DELEGATE.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload,
    }));
    let (rows, _) = rendered(&app, 160, 40);
    // The row is cut at the column's edge, so only the message's head
    // shows beside the stop target.
    assert!(rows.iter().any(|row| row.contains("Could not open")));
}
