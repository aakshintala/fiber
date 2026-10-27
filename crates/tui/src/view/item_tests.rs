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
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        "started": 0, "calls": 0, "since": 0,
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
        "started": 0, "calls": 0, "since": 0,
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
            crate::home::home_test_rows(&screen)
                .into_iter()
                .map(|(key, _)| key)
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("opening sends"),
    };
    let id = out
        .iter()
        .rfind(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a subscribe"))
        .to_owned();
    // The staged open holds the gate past the first ack: the first
    // frame releases the `full` raise, whose ack ends the gate. One
    // ack would drop the session's later lines.
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
    let mut payload = serde_json::Map::new();
    payload.insert("command_id".to_owned(), Value::String(raise_id(app)));
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

/// The `full` raise the first frame releases, parsed to its id.
fn raise_id(app: &mut App) -> String {
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    serde_json::from_str::<Value>(&line).unwrap_or_else(|err| panic!("{line}: {err}"))["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a raise id"))
        .to_owned()
}

/// Folds a plain job as `job` with `description`: no delegate.
fn start_job(app: &mut App, job: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        0,
        json!({"job_id": job, "description": description, "output_path": "/tmp/out"}),
    ));
}

/// Folds a `shell` call asking for a pseudo-terminal on `a_1`, so the
/// next `job_started` takes a grid.
fn mark_tty(app: &mut App) {
    app.on_line(session_line(
        "tool_call_requested",
        0,
        json!({"name": "shell", "arguments": {"tty": true}}),
    ));
}

/// Opens `job`'s view through the panel's Jobs row.
fn open_job(app: &mut App, job: &str) {
    let serial = app
        .serial_of_job(&contract::JobId(job.to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    match app.on_click(TargetId::Panel(crate::app::panel::Spot::Job(serial))) {
        crate::app::Effect::Send(_) => {}
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("opening sends or nothing"),
    }
    assert!(app.item_open());
}

/// Folds `text` as `job`'s delta.
fn job_text(app: &mut App, job: &str, text: &str) {
    app.on_line(session_line(
        "job_delta",
        0,
        json!({"job_id": job, "text": text}),
    ));
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("opening sends or nothing"),
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

/// Renders `app` at `width` by `height`: the buffer, for style asserts.
fn rendered_buf(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let _ = crate::view::render(app, area, &mut buf, None);
    buf
}

/// The buffer's row holding `needle`, for style asserts.
fn row_with(buf: &Buffer, needle: &str) -> u16 {
    crate::view::text(buf)
        .lines()
        .enumerate()
        .find_map(|(y, row)| {
            row.contains(needle)
                .then(|| u16::try_from(y).unwrap_or(u16::MAX))
        })
        .unwrap_or_else(|| panic!("a row with {needle}"))
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
    use ratatui::style::Modifier;

    use crate::theme::Role;

    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    // Sixty seconds in: the elapsed row reads whole minutes.
    start_delegate(&mut app, wall_ms(&clock).saturating_sub(61_000));
    // The delegate's own summary is held with its thinking level.
    app.on_line(delegate_status(json!({
        "state": "streaming", "started": wall_ms(&clock), "thinking": "high",
    })));
    open_item(&mut app);
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows[0].contains("Delegate  ◆ review the parser"));
    assert!(rows[0].contains("Esc returns"));
    assert!(!rows[0].contains('✕'));
    assert!(rows[1].contains("fix the parser › ◆ review the parser"));
    assert!(rows[2].contains("harness fiber"));
    assert!(rows[2].contains(" · model test/model · high"));
    assert!(rows[3].contains("0 calls ·"));
    assert!(rows[3].contains("1m 01s"));
    assert!(rows[3].contains("session s_dddddddddddddddd"));
    assert!(rows[4].contains("■ stop s_dddddddddddddddd"));
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Item(Spot::Stop))
    );
    let buf = rendered_buf(&app, 80, 24);
    // The breadcrumb is bold.
    let crumb = row_with(&buf, "› ◆ review the parser");
    for x in 0..80 {
        let cell = &buf[(x, crumb)];
        if cell.symbol() != " " {
            assert!(cell.modifier.contains(Modifier::BOLD), "bold crumb {x}");
        }
    }
    // Every card row's cells sit on the `surface` tint.
    for needle in ["harness fiber", "0 calls ·", "■ stop s_dddddddddddddddd"] {
        let y = row_with(&buf, needle);
        let first = (0..80)
            .find(|x| buf[(*x, y)].symbol() != " ")
            .unwrap_or_else(|| panic!("card text {needle}"));
        let last = (0..80)
            .rev()
            .find(|x| buf[(*x, y)].symbol() != " ")
            .unwrap_or_else(|| panic!("card text {needle}"));
        for x in first..=last {
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "card tint {x},{y}");
        }
    }
    // The harness draws plain, the model segment dim.
    let card = row_with(&buf, "harness fiber");
    let line: String = crate::view::text(&buf)
        .lines()
        .nth(usize::from(card))
        .unwrap_or_default()
        .to_owned();
    let harness_at = line
        .find("harness fiber")
        .unwrap_or_else(|| panic!("the harness part"));
    let model_at = line
        .find(" · model test/model · high")
        .unwrap_or_else(|| panic!("the model part"));
    for (index, ch) in line.chars().enumerate() {
        if ch == ' ' {
            continue;
        }
        let x = u16::try_from(index).unwrap_or(u16::MAX);
        let fg = buf[(x, card)].fg;
        if index >= model_at {
            assert_eq!(fg, Role::Muted.color(), "dim model {x}");
        } else if index >= harness_at {
            assert_eq!(fg, Role::Text.color(), "plain harness {x}");
        }
    }
    // The stop target draws in `error`.
    let stop = row_with(&buf, "■ stop");
    for x in 0..80 {
        let cell = &buf[(x, stop)];
        if cell.symbol() != " " {
            assert_eq!(cell.fg, Role::Error.color(), "error stop {x}");
            assert_eq!(cell.bg, Role::Surface.color(), "stop tint {x}");
        }
    }
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
    assert!(rows[0].contains("Delegate  ◆ review the parser"));
    assert!(rows[1].contains("fix the parser › ◆ review the parser"));
    // No held status names no level: the model segment ends the card's
    // first row with no trailing separator.
    assert!(rows[2].contains("harness fiber"));
    assert!(rows[2].trim_end().ends_with("model test/model"));
    assert!(!rows[2].contains("high"));
    assert!(rows[4].contains("■ stop s_dddddddddddddddd"));
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
    // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(std::time::Duration::from_secs(3600));
    app.set_now(clock.now(), wall_ms(&clock));
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows.iter().any(|row| row.contains("failed")));
    assert!(rows.iter().any(|row| row.contains("2m 00s")));
    // The outcome sits where the stop target would be.
    assert!(rows[4].contains("failed"));
    assert!(!rows[4].contains("■ stop"));
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
fn a_click_anywhere_on_the_top_row_closes_the_view() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    let area = Rect::new(3, 2, 80, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let close: Vec<_> = targets
        .iter()
        .filter(|target| target.id == TargetId::Item(Spot::Close))
        .collect();
    assert_eq!(close.len(), 1);
    assert_eq!(close[0].rect, Rect::new(3, 2, 80, 1));
    assert!(
        !crate::view::text(&buf)
            .lines()
            .next()
            .unwrap()
            .contains('✕')
    );
    for x in area.left()..area.right() {
        assert_eq!(
            crate::mouse::hit(&targets, x, area.y),
            Some(TargetId::Item(Spot::Close))
        );
    }
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap();
    assert_eq!(
        crate::mouse::hit(&targets, stop.rect.x, stop.rect.y),
        Some(TargetId::Item(Spot::Stop))
    );
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("stop sends"),
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("Enter stops"),
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
    app.push_notice("extension_failed", "later".to_owned());
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
    // Below the header's five rows, on the transcript's last row: past
    // the grip, which takes column 0.
    let header = u16::try_from(crate::view::item::ITEM_HEADER_ROWS).unwrap_or(u16::MAX);
    let row = header.saturating_add(u16::try_from(height).unwrap_or(u16::MAX).saturating_sub(1));
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        col: 2,
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
    let body_y = app
        .chrome()
        .layout()
        .map(|layout| layout.column.y)
        .unwrap_or(0);
    let header = u16::try_from(crate::view::item::ITEM_HEADER_ROWS).unwrap_or(u16::MAX);
    assert_eq!(app.conversation_area().y, body_y.saturating_add(header));
    assert!(!app.conversation_covered());
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("a release copies"),
    }
}

#[test]
fn the_transcript_keeps_the_gutter() {
    use ratatui::style::Color;
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
    let layout = app.chrome().layout().expect("a layout");
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let y = (0..24)
        .find(|y| {
            let row: String = (0..80)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("looks good")
        })
        .expect("the transcript reply row");
    let tint = buf
        .cell((layout.column.x.saturating_add(1), y))
        .map(|cell| cell.bg)
        .unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset);
    assert_eq!(
        buf.cell((layout.column.x, y)).map(|cell| cell.bg),
        Some(Color::Reset)
    );
    assert_eq!(
        buf.cell((layout.column.x.saturating_add(1), y))
            .map(|cell| cell.bg),
        Some(tint)
    );
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
        let elapsed = rows
            .iter()
            .find(|row| row.contains("elapsed"))
            .cloned()
            .unwrap_or_default();
        assert!(elapsed.contains(text), "{text} in {elapsed}");
    }
}

#[test]
fn header_guard_rejects_either_zero_dimension() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    for area in [Rect::new(0, 0, 20, 0), Rect::new(0, 0, 0, 2)] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 3));
        let mut targets = Vec::new();
        super::draw_header(&app, area, &mut buf, &mut targets);
        assert!(targets.is_empty(), "zero-sized header area {area:?}");
        assert_eq!(buf.cell((0, 0)).map(|cell| cell.symbol()), Some(" "));
    }
}

#[test]
fn header_draws_each_row_only_with_room_for_it() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    // The delegate's card fills the five header rows: the stop target
    // draws only with room for the card's last row.
    for (height, rows) in [
        (1, ["Delegate", "", "", "", ""]),
        (2, ["Delegate", "◆ review", "", "", ""]),
        (3, ["Delegate", "◆ review", "harness", "", ""]),
        (4, ["Delegate", "◆ review", "harness", "calls", ""]),
        (5, ["Delegate", "◆ review", "harness", "calls", "■ stop"]),
    ] {
        let area = Rect::new(0, 0, 80, height);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 5));
        let mut targets = Vec::new();
        super::draw_header(&app, area, &mut buf, &mut targets);
        let text = crate::view::text(&buf);
        for (index, needle) in rows.iter().enumerate() {
            let line = text.lines().nth(index).unwrap_or_default();
            if needle.is_empty() {
                assert!(line.trim().is_empty(), "height {height} row {index}");
            } else {
                assert!(line.contains(needle), "height {height} row {index}");
            }
        }
        assert_eq!(
            targets
                .iter()
                .any(|target| target.id == TargetId::Item(Spot::Stop)),
            height == 5,
            "height {height}"
        );
        assert_eq!(
            targets
                .iter()
                .filter(|target| target.id == TargetId::Item(Spot::Close))
                .count(),
            1,
            "height {height}"
        );
    }
}

#[test]
fn stop_target_starts_on_the_drawn_stop_label() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    let area = Rect::new(0, 0, 80, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap_or_else(|| panic!("stop target"));
    assert_eq!(
        buf.cell((stop.rect.x, stop.rect.y))
            .map(|cell| cell.symbol()),
        Some("■")
    );
}

#[test]
fn stop_target_covers_the_cut_stop_at_a_narrow_width() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    open_item(&mut app);
    // The message is cut to fit the stop, so the stop always starts
    // inside the row: its target covers the drawn cells.
    let area = Rect::new(0, 0, 10, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap_or_else(|| panic!("stop target"));
    assert_eq!(stop.rect.x, 0);
    assert_eq!(stop.rect.width, 10);
    assert_eq!(
        buf.cell((stop.rect.x, stop.rect.y))
            .map(|cell| cell.symbol()),
        Some("■")
    );
}

#[test]
fn a_refusal_message_cut_leaves_six_cells_for_the_stop() {
    let mut app = home(40, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    refused_open(&mut app);
    // At 40 columns the 28-cell refusal keeps 12 cells: the cut is the
    // row minus the 25-cell stop and its 3-cell separator. Six cells of
    // slack would keep 18 cells and push the stop off the row, so the
    // full stop below pins the cut width.
    let area = Rect::new(0, 0, 40, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let text = crate::view::text(&buf);
    let stop_row = text.lines().nth(4).unwrap_or_default();
    assert!(
        stop_row.contains("Could not op · ■ stop s_dddddddddddddddd"),
        "the 12-cell cut:\n{stop_row}"
    );
    assert!(
        !stop_row.contains("Could not open: al"),
        "no 18-cell cut:\n{stop_row}"
    );
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap_or_else(|| panic!("stop target"));
    assert_eq!(stop.rect.x, 15);
    assert_eq!(stop.rect.width, 25);
    assert_eq!(
        buf.cell((stop.rect.x, stop.rect.y))
            .map(|cell| cell.symbol()),
        Some("■")
    );
}

#[test]
fn body_guard_rejects_a_zero_height_area() {
    let mut app = home(80, 24);
    opened(&mut app);
    app.on_line(session_line(
        "job_started",
        0,
        json!({"job_id": "j_1", "description": "job", "output_path": "/tmp/out"}),
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
    let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
    super::draw_body(&app, Rect::new(0, 0, 20, 0), &mut buf);
    assert_eq!(buf.cell((0, 0)).map(|cell| cell.symbol()), Some(" "));
}

#[test]
fn elapsed_asks_for_the_next_whole_second_using_milliseconds() {
    let clock = fakes::clock::FakeClock::new();
    let now = clock.now();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(now, 10_500);
    start_delegate(&mut app, 1_500);
    open_item(&mut app);
    app.motion().clear_wake();
    let area = Rect::new(0, 0, 80, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    assert_eq!(
        app.motion().take_wake(),
        Some(now + std::time::Duration::from_secs(1))
    );
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
            .any(|row| row.contains("Output since this terminal attached · /tmp/other.out"))
    );
    insta::assert_snapshot!("another_harness_body", rows.join("\n"));
}

#[test]
fn job_header_draws_the_status_and_live_screen_line() {
    use ratatui::style::Modifier;

    use crate::theme::Role;

    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    mark_tty(&mut app);
    // Eleven seconds in: the status row reads whole seconds.
    app.on_line(session_line(
        "job_started",
        wall_ms(&clock).saturating_sub(11_000),
        json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
    open_job(&mut app, "j_2");
    let (rows, targets) = rendered(&app, 80, 24);
    assert!(rows[0].contains("Job  ◆ build the workspace"));
    assert!(rows[0].contains("Esc returns"));
    assert!(!rows[0].contains('✕'));
    assert!(rows[1].contains("fix the parser › ▸ build the workspace"));
    assert!(rows[2].contains("job j_2 · build the workspace · running 11s"));
    assert!(rows[2].contains("■ stop j_2"));
    assert!(rows[3].contains("live screen · 80 columns × 24 rows from attach"));
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Item(Spot::Stop))
    );
    let buf = rendered_buf(&app, 80, 24);
    // The top row and the job rows sit on the `surface` tint; the
    // breadcrumb is bold on the default ground.
    for y in [0u16, 2, 3] {
        let first = (0..80)
            .find(|x| buf[(*x, y)].symbol() != " ")
            .unwrap_or_else(|| panic!("header row {y}"));
        let last = (0..80)
            .rev()
            .find(|x| buf[(*x, y)].symbol() != " ")
            .unwrap_or_else(|| panic!("header row {y}"));
        for x in first..=last {
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "job tint {x},{y}");
        }
    }
    let crumb = row_with(&buf, "› ▸ build the workspace");
    for x in 0..80 {
        let cell = &buf[(x, crumb)];
        if cell.symbol() != " " {
            assert!(cell.modifier.contains(Modifier::BOLD), "bold crumb {x}");
        }
    }
    // The separator is dim and the stop target is in `error`.
    let status = row_with(&buf, "■ stop j_2");
    let stop_at = (0..80)
        .find(|x| buf[(*x, status)].symbol() == "■")
        .unwrap_or_else(|| panic!("the stop label"));
    for x in stop_at.saturating_sub(3)..stop_at {
        assert_eq!(
            buf[(x, status)].fg,
            Role::Muted.color(),
            "dim separator {x}"
        );
    }
    for x in 0..80 {
        let cell = &buf[(x, status)];
        if cell.symbol() != " " && x >= stop_at {
            assert_eq!(cell.fg, Role::Error.color(), "error stop {x}");
        }
    }
    let live = row_with(&buf, "live screen");
    for x in 0..80 {
        let cell = &buf[(x, live)];
        if cell.symbol() != " " {
            assert_eq!(cell.fg, Role::Muted.color(), "dim live screen {x}");
        }
    }
}

#[test]
fn plain_job_header_has_no_live_screen_line() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    // Eleven seconds in: the status row reads whole seconds.
    app.on_line(session_line(
        "job_started",
        wall_ms(&clock).saturating_sub(11_000),
        json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
    open_job(&mut app, "j_2");
    let (rows, _) = rendered(&app, 80, 24);
    assert!(rows[0].contains("Job  ◆ build the workspace"));
    assert!(rows[2].contains("job j_2 · build the workspace · running 11s"));
    assert!(rows[2].contains("■ stop j_2"));
    // The live-screen line is pty-only (`docs/tui.md`, "Swapped
    // views"): a plain job must not claim to have one.
    assert!(
        !rows.iter().any(|row| row.contains("live screen")),
        "no live-screen line for a plain job"
    );
}

#[test]
fn a_job_status_cut_leaves_six_cells_for_the_stop() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(40, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    mark_tty(&mut app);
    app.on_line(session_line(
        "job_started",
        wall_ms(&clock).saturating_sub(11_000),
        json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
    open_job(&mut app, "j_2");
    // At 40 columns the 43-cell status keeps 27 cells: the cut is the
    // row minus the 10-cell stop and its 3-cell separator. Six cells of
    // slack would keep 33 cells and push the stop off the row, so the
    // full stop below pins the cut width.
    let area = Rect::new(0, 0, 40, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let text = crate::view::text(&buf);
    let status_row = text.lines().nth(2).unwrap_or_default();
    assert!(
        status_row.contains("job j_2 · build the workspa · ■ stop j_2"),
        "the 27-cell cut:\n{status_row}"
    );
    assert!(
        !status_row.contains("workspace · r"),
        "no 33-cell cut:\n{status_row}"
    );
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap_or_else(|| panic!("stop target"));
    assert_eq!(stop.rect.x, 30);
    assert_eq!(stop.rect.width, 10);
    assert_eq!(
        buf.cell((stop.rect.x, stop.rect.y))
            .map(|cell| cell.symbol()),
        Some("■")
    );
}

#[test]
fn a_job_stop_target_starts_on_the_drawn_stop_label() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    mark_tty(&mut app);
    app.on_line(session_line(
        "job_started",
        wall_ms(&clock).saturating_sub(11_000),
        json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
    open_job(&mut app, "j_2");
    // The 43-cell status draws whole at 80 columns, so the stop starts
    // after it and its separator: three cells of slack would start it
    // early, and a scaled start would leave the row.
    let area = Rect::new(0, 0, 80, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let stop = targets
        .iter()
        .find(|target| target.id == TargetId::Item(Spot::Stop))
        .unwrap_or_else(|| panic!("stop target"));
    assert_eq!(stop.rect.x, 46);
    assert_eq!(stop.rect.width, 10);
    assert_eq!(stop.rect.y, 2);
    assert_eq!(
        buf.cell((stop.rect.x, stop.rect.y))
            .map(|cell| cell.symbol()),
        Some("■")
    );
    assert_eq!(
        buf.cell((
            stop.rect
                .x
                .saturating_add(stop.rect.width)
                .saturating_sub(1),
            stop.rect.y
        ))
        .map(|cell| cell.symbol()),
        Some("2")
    );
}

#[test]
fn a_tty_job_draws_its_live_screen_line_only_from_four_rows() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    mark_tty(&mut app);
    app.on_line(session_line(
        "job_started",
        wall_ms(&clock).saturating_sub(11_000),
        json!({"job_id": "j_2", "description": "build the workspace",
            "output_path": "/tmp/out"}),
    ));
    open_job(&mut app, "j_2");
    // Four rows fit the top, the breadcrumb, the status and the live
    // line; three rows stop after the status. Either boundary flip
    // would move that edge.
    let area = Rect::new(0, 0, 80, 4);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let four = crate::view::text(&buf);
    assert!(
        four.lines()
            .nth(3)
            .unwrap_or_default()
            .contains("live screen · 80 columns × 24 rows from attach"),
        "the live line draws at four rows:\n{four}"
    );
    let area = Rect::new(0, 0, 80, 3);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let three = crate::view::text(&buf);
    assert!(
        !three.lines().any(|row| row.contains("live screen")),
        "no live line at three rows:\n{three}"
    );
}

#[test]
fn tty_body_sits_on_a_turn_slab_eighty_columns_wide() {
    use crate::theme::Role;

    let mut app = home(100, 24);
    opened(&mut app);
    mark_tty(&mut app);
    start_job(&mut app, "j_9", "run the editor");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    open_job(&mut app, "j_9");
    let area = Rect::new(0, 0, 100, 10);
    let mut buf = Buffer::empty(area);
    super::draw_body(&app, area, &mut buf);
    let text = crate::view::text(&buf);
    assert!(
        !text.lines().any(|row| row.contains("Output since")),
        "a tty job draws no output-since line"
    );
    let screen = text
        .lines()
        .find(|row| row.contains("$ edit file.txt"))
        .unwrap_or_else(|| panic!("the screen row"));
    assert!(screen.contains("$ edit file.txt"));
    // The slab keeps its own 80 columns: tinted within, default past.
    for y in 0..10 {
        for x in 0..80 {
            assert_eq!(buf[(x, y)].bg, Role::Turn.color(), "screen tint {x},{y}");
        }
        for x in 80..100 {
            assert_ne!(buf[(x, y)].bg, Role::Turn.color(), "past the slab {x},{y}");
        }
    }
}

#[test]
fn tty_body_clips_below_eighty_columns_without_reflow() {
    use crate::theme::Role;

    let mut app = home(70, 24);
    opened(&mut app);
    mark_tty(&mut app);
    start_job(&mut app, "j_9", "run the editor");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    open_job(&mut app, "j_9");
    // Narrower than the slab: the screen clips, with no panic and no
    // reflow of its rows.
    let area = Rect::new(0, 0, 70, 10);
    let mut buf = Buffer::empty(area);
    super::draw_body(&app, area, &mut buf);
    let text = crate::view::text(&buf);
    assert!(
        !text.lines().any(|row| row.contains("Output since")),
        "a tty job draws no output-since line"
    );
    assert!(
        text.lines().any(|row| row.contains("$ edit file.txt")),
        "the clipped screen keeps its rows"
    );
    for y in 0..10 {
        for x in 0..70 {
            assert_eq!(buf[(x, y)].bg, Role::Turn.color(), "screen tint {x},{y}");
        }
    }
}

#[test]
fn plain_job_body_keeps_the_output_since_line() {
    use crate::theme::Role;

    let mut app = home(100, 24);
    opened(&mut app);
    start_job(&mut app, "j_2", "build the workspace");
    job_text(&mut app, "j_2", "done\n");
    open_job(&mut app, "j_2");
    let area = Rect::new(0, 0, 100, 10);
    let mut buf = Buffer::empty(area);
    super::draw_body(&app, area, &mut buf);
    let text = crate::view::text(&buf);
    assert!(
        text.lines()
            .any(|row| row.contains("Output since this terminal attached · /tmp/out")),
        "a plain job keeps its path line"
    );
    assert!(
        text.lines().any(|row| row.contains("done")),
        "a plain job keeps its tail"
    );
    // No slab: the tail draws on the default ground.
    let tail = text
        .lines()
        .enumerate()
        .find_map(|(y, row)| {
            row.contains("done")
                .then(|| u16::try_from(y).unwrap_or(u16::MAX))
        })
        .unwrap_or_else(|| panic!("the tail row"));
    for x in 0..100 {
        let cell = &buf[(x, tail)];
        if cell.symbol() != " " {
            assert_ne!(cell.bg, Role::Turn.color(), "no slab {x}");
        }
    }
}

#[test]
fn a_finished_tty_job_keeps_its_slab_after_its_output_clears() {
    use crate::theme::Role;

    let mut app = home(80, 24);
    opened(&mut app);
    mark_tty(&mut app);
    start_job(&mut app, "j_9", "run the editor");
    job_text(&mut app, "j_9", "$ edit file.txt\n");
    // Finishing while closed clears the live output ...
    app.on_line(session_line(
        "job_completed",
        0,
        json!({"job_id": "j_9", "status": "completed"}),
    ));
    // ... but opening afterward still selects the slab.
    open_job(&mut app, "j_9");
    let area = Rect::new(0, 0, 80, 10);
    let mut buf = Buffer::empty(area);
    super::draw_body(&app, area, &mut buf);
    let text = crate::view::text(&buf);
    assert!(
        !text.lines().any(|row| row.contains("Output since")),
        "a finished tty job draws no output-since line"
    );
    for y in 0..10 {
        for x in 0..80 {
            assert_eq!(buf[(x, y)].bg, Role::Turn.color(), "screen tint {x},{y}");
        }
    }
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
    assert!(app.conversation_covered());
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

/// Refuses the open delegate's subscribe, so its view shows the refusal
/// message "Could not open: already full" on the stop row.
fn refused_open(app: &mut App) {
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
        | crate::app::Effect::ReadImage(_)
        | crate::app::Effect::Feedback { .. } => panic!("opening sends"),
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
}

#[test]
fn a_refusal_message_draws_on_the_stop_row() {
    let mut app = home(160, 40);
    opened(&mut app);
    start_delegate(&mut app, 0);
    refused_open(&mut app);
    let (rows, _) = rendered(&app, 160, 40);
    // The message draws on the stop row, cut to fit the stop target.
    let stop = rows
        .iter()
        .find(|row| row.contains("■ stop s_dddddddddddddddd"))
        .unwrap_or_else(|| panic!("the stop row"));
    assert!(stop.contains("Could not open: already full"));
}

#[test]
fn a_refusal_message_cut_to_empty_leaves_no_separator() {
    let mut app = home(80, 24);
    opened(&mut app);
    start_delegate(&mut app, 0);
    refused_open(&mut app);
    // Too narrow for any message text: the stop still starts the row
    // with its target, and no dangling separator precedes it.
    let area = Rect::new(0, 0, 10, 5);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    super::draw_header(&app, area, &mut buf, &mut targets);
    let text = crate::view::text(&buf);
    let stop = text.lines().nth(4).unwrap_or_default();
    assert!(!stop.contains("·"), "{stop}");
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::Item(Spot::Stop)),
        "the cut stop keeps its target"
    );
}

#[test]
fn an_item_views_transcript_draws_no_pin() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home(80, 24);
    opened(&mut app);
    app.set_now(clock.now(), wall_ms(&clock));
    start_delegate(&mut app, 0);
    open_item(&mut app);
    app.on_line(delegate_line(
        "turn_started",
        0,
        json!({"input": [{
            "type": "message", "source": "driver",
            "content": [{"type": "text", "text": "review it"}]}]}),
    ));
    app.on_line(delegate_line("assistant_message_started", 0, json!({})));
    app.on_line(delegate_line(
        "tool_call_requested",
        0,
        json!({"name": "read", "arguments": {"path": "src/0.rs"}}),
    ));
    let detail: Vec<String> = (1..=30).map(|n| format!("L{n:04} of call")).collect();
    app.on_line(delegate_line(
        "tool_call_completed",
        0,
        json!({"status": "completed",
            "content": [{"type": "text", "text": detail.join("\n")}]}),
    ));
    let group = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| match target {
            crate::app::Target::Group(_) => Some(target),
            crate::app::Target::Call(_)
            | crate::app::Target::CallView(_)
            | crate::app::Target::Thought(_)
            | crate::app::Target::Login
            | crate::app::Target::Note(_)
            | crate::app::Target::Image(_)
            | crate::app::Target::Orphans(_)
            | crate::app::Target::Copy { .. } => None,
        })
        .expect("a group target");
    app.open(group);
    let call = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| match target {
            crate::app::Target::Call(_) => Some(target),
            crate::app::Target::Group(_)
            | crate::app::Target::CallView(_)
            | crate::app::Target::Thought(_)
            | crate::app::Target::Login
            | crate::app::Target::Note(_)
            | crate::app::Target::Image(_)
            | crate::app::Target::Orphans(_)
            | crate::app::Target::Copy { .. } => None,
        })
        .expect("a call target");
    app.open(call);
    // The first opened content row at this width.
    let width = app.pages().wrap_width();
    let (rows, texts) = app.pages().page_text(0).expect("the resident page");
    let mut row = 0usize;
    let mut first = None;
    for ((line, _), text) in rows.iter().zip(&texts) {
        if first.is_none() && text.scopes.last() == Some(&call) {
            first = Some(row);
        }
        row += crate::view::rows(line.clone(), width);
    }
    let first = first.expect("opened rows");
    app.jump(first + 2);
    assert!(app.item_open(), "the transcript stays open");
    assert!(app.pinned_row().is_none(), "no pin in an item view");
    let (shown, targets) = rendered(&app, 80, 24);
    // The transcript's first drawn row is the covered row itself.
    // The item's five header rows draw above the conversation area.
    let _ = crate::view::item::ITEM_HEADER_ROWS;
    let top = app.top().expect("a scrolled transcript");
    let (page, _) = app.pages().page_text(0).expect("the resident page");
    let mut row = 0usize;
    let mut covered = None;
    for (line, _) in &page {
        let height = crate::view::rows(line.clone(), width);
        if row + height > top && covered.is_none() {
            covered = Some(line.to_string());
        }
        row += height;
    }
    let covered = covered.expect("the covered row");
    // The item's five header rows draw above the conversation area, so
    // the transcript's first row is the area's own first row.
    let body_y = app.conversation_area().y;
    assert_eq!(shown[usize::from(body_y)].trim(), covered.trim());
    assert!(
        targets.iter().all(|target| match target.id {
            crate::mouse::TargetId::Line(_) => target.rect.y != body_y,
            crate::mouse::TargetId::Badge
            | crate::mouse::TargetId::NewBelow
            | crate::mouse::TargetId::Interrupt
            | crate::mouse::TargetId::Token(_)
            | crate::mouse::TargetId::Steering(_)
            | crate::mouse::TargetId::DropSteering(_)
            | crate::mouse::TargetId::Notice(_)
            | crate::mouse::TargetId::DismissNotice(_)
            | crate::mouse::TargetId::CloseOverlay
            | crate::mouse::TargetId::Home(_)
            | crate::mouse::TargetId::Offer(_)
            | crate::mouse::TargetId::Panel(_)
            | crate::mouse::TargetId::Item(_)
            | crate::mouse::TargetId::Rail(_)
            | crate::mouse::TargetId::Form(_)
            | crate::mouse::TargetId::Turn(_)
            | crate::mouse::TargetId::Link { .. }
            | crate::mouse::TargetId::FindCount
            | crate::mouse::TargetId::FindResult(_)
            | crate::mouse::TargetId::View(_)
            | crate::mouse::TargetId::MoreNotices => true,
        }),
        "no line target covers the transcript's first row"
    );
}
