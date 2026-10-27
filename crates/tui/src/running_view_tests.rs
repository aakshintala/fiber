//! Tests for the running delegates and jobs lists: the cards' rows with
//! one serial cell each, in start order, and the empty line.

use std::path::PathBuf;

use crate::theme::Role;
use contract::SessionId;
use contract::clock::Clock;
use serde_json::{Value, json};

use super::*;
use crate::home::Launch;
use crate::link::Line;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// An app on home at 80x24, drawing the default cards.
fn home() -> App {
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

/// A live `session_status` for `session` in `state`.
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

/// Links the app and opens the attached session through home.
fn opened(app: &mut App) {
    let lines: Vec<Value> = app
        .on_line(hello())
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
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
    let out: Vec<Value> =
        match app.on_click(crate::mouse::TargetId::Home(crate::home::Spot::Entry(key))) {
            crate::app::Effect::Send(lines) => lines
                .iter()
                .map(|line| {
                    serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}"))
                })
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
    app.on_line(session_accepted(SESSION, &id));
    // The staged open holds the gate past the first ack: the first
    // frame releases the `full` raise, whose ack ends the gate. One
    // ack would drop the session's later lines.
    let line = app.take_raise().unwrap_or_else(|| panic!("a raise"));
    let raise: Value =
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{line}: {error}"));
    let raise_id = raise["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a raise id"))
        .to_owned();
    app.on_line(session_accepted(SESSION, &raise_id));
}

/// Folds a delegate as `job` with session `delegate`.
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

/// Folds a plain job as `job`.
fn start_job(app: &mut App, job: &str) {
    app.on_line(session_line(
        "job_started",
        json!({"job_id": job, "description": format!("task {job}"),
            "output_path": "/tmp/out"}),
    ));
}

/// Each row's serial cell, in frame order.
fn serials(frame: &Frame) -> Vec<u64> {
    frame
        .rows
        .iter()
        .filter(|cells| !cells.is_empty())
        .map(|cells| {
            let serial = cells[0].1;
            assert!(cells.iter().all(|cell| cell.1 == serial));
            serial
        })
        .filter_map(|spot| match spot {
            Some(Spot::Item(serial)) => Some(serial),
            None => None,
            _ => panic!("only item targets in a running list"),
        })
        .collect()
}

#[test]
fn delegates_frame_lists_two_rows_per_delegate_in_start_order() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    let frame = delegates_frame(&app, List::default());
    assert_eq!(frame.title, "Running delegates");
    assert_eq!(frame.rows.len(), 7);
    assert_eq!(
        frame.rows[0],
        [("Delegates · 2 running".to_owned(), None, Ink::Heading)]
    );
    let first = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let second = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(serials(&frame), [first, first, second, second]);
    assert!(frame.rows[1][0].0.contains("task j_1"));
    assert!(frame.rows[2][0].0.contains("test/model"));
    assert!(frame.rows[3][0].0.contains("task j_2"));
    assert!(frame.rows[5].is_empty());
    assert_eq!(
        frame.rows[6],
        [("a row opens that item's view".to_owned(), None, Ink::Muted)]
    );
    assert!(frame.footer.is_empty());
}

#[test]
fn jobs_frame_lists_one_row_per_job_and_no_delegate() {
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    start_delegate(&mut app, "j_2", "s_dddddddddddddddd");
    start_job(&mut app, "j_3");
    let frame = jobs_frame(&app, List::default());
    assert_eq!(frame.title, "Running jobs");
    assert_eq!(frame.rows.len(), 5);
    assert_eq!(
        frame.rows[0],
        [("Jobs · 2 running".to_owned(), None, Ink::Heading)]
    );
    let first = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let third = app
        .serial_of_job(&contract::JobId("j_3".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(serials(&frame), [first, third]);
    assert!(frame.rows[1][0].0.contains("task j_1"));
    assert!(frame.rows[2][0].0.contains("task j_3"));
    assert!(frame.footer.is_empty());
}

fn text(cells: &[(String, Option<Spot>, Ink)]) -> String {
    cells.iter().map(|cell| cell.0.as_str()).collect()
}

#[test]
fn the_delegates_frame_matches_the_doc() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    let wall = contract::clock::wall_ms(clock.wall());
    app.set_now(clock.now(), wall);
    let mut line = session_line(
        "job_started",
        json!({
            "job_id": "j_1", "description": "task j_1", "output_path": "/tmp/out"
        }),
    );
    if let Line::Session(line) = &mut line {
        line.ts = wall.saturating_sub(61_000);
    }
    app.on_line(line);
    app.on_line(session_line(
        "delegate_started",
        json!({
            "job_id": "j_1", "delegate_session_id": "s_dddddddddddddddd",
            "harness": "fiber", "model": "test/model", "workspace": "/w"
        }),
    ));
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    app.on_line(live(
        "s_dddddddddddddddd",
        json!({
            "state": "streaming", "parent": SESSION, "thinking": "high", "calls": 3,
            "spend": {"tokens": {"input": 650, "cache_read": 0, "cache_write": {}, "output": 0},
                "cost": 0.42, "subscription_cost": 0.0},
            "context": {"tokens": 650, "window": 1000}
        }),
    ));
    let frame = delegates_frame(&app, List::default());
    let serial = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap();
    assert_eq!(
        frame.rows[1],
        [(
            "◆ task j_1 · 1m".to_owned(),
            Some(Spot::Item(serial)),
            Ink::Plain
        )]
    );
    assert_eq!(
        frame.rows[2],
        [
            (
                "  test/model · high · 3 calls · s_dddddddddddddddd · $0.42 ".to_owned(),
                Some(Spot::Item(serial)),
                Ink::Muted
            ),
            (
                "▆".repeat(6),
                Some(Spot::Item(serial)),
                Ink::Role(Role::Warning)
            ),
            (
                "░".repeat(4),
                Some(Spot::Item(serial)),
                Ink::Role(Role::Rule)
            ),
            (" 65%".to_owned(), Some(Spot::Item(serial)), Ink::Muted),
        ]
    );
}

#[test]
fn the_bar_fill_turns_at_60_and_85_percent() {
    for (tokens, filled, role, pct) in [
        (0, 0, Role::Accent, 0),
        (590, 5, Role::Accent, 59),
        (599, 5, Role::Accent, 59),
        (600, 6, Role::Warning, 60),
        (601, 6, Role::Warning, 60),
        (840, 8, Role::Warning, 84),
        (849, 8, Role::Warning, 84),
        (850, 8, Role::Error, 85),
        (851, 8, Role::Error, 85),
        (999, 9, Role::Error, 99),
        (1000, 10, Role::Error, 100),
        (1001, 10, Role::Error, 100),
        (1500, 10, Role::Error, 150),
    ] {
        assert_eq!(bar(tokens, 1000), (filled, role, pct), "{tokens}");
    }
}

#[test]
fn a_delegate_without_a_level_status_or_context_leaves_them_out() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    assert_eq!(
        text(&delegates_frame(&app, List::default()).rows[2]),
        "  test/model · s_dddddddddddddddd"
    );
    app.on_line(live(
        "s_dddddddddddddddd",
        json!({"state": "streaming", "parent": SESSION}),
    ));
    assert_eq!(
        text(&delegates_frame(&app, List::default()).rows[2]),
        "  test/model · 0 calls · s_dddddddddddddddd · $0.00 "
    );
}

#[test]
fn a_context_with_a_window_of_zero_draws_no_bar() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    app.on_line(live(
        "s_dddddddddddddddd",
        json!({
            "state": "streaming", "parent": SESSION, "context": {"tokens": 5, "window": 0}
        }),
    ));
    let frame = delegates_frame(&app, List::default());
    assert_eq!(frame.rows[2].len(), 1);
    assert_eq!(
        text(&frame.rows[2]),
        "  test/model · 0 calls · s_dddddddddddddddd · $0.00 "
    );
}

#[test]
fn the_jobs_frame_matches_the_doc() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    let wall = contract::clock::wall_ms(clock.wall());
    app.set_now(clock.now(), wall);
    let mut line = session_line(
        "job_started",
        json!({
            "job_id": "j_1", "description": "task j_1", "output_path": "/tmp/out"
        }),
    );
    if let Line::Session(line) = &mut line {
        line.ts = wall.saturating_sub(61_000);
    }
    app.on_line(line);
    start_job(&mut app, "j_2");
    let frame = jobs_frame(&app, List::default());
    assert_eq!(text(&frame.rows[1]), "◆ task j_1 · 1m · j_1");
    assert_eq!(text(&frame.rows[4]), "a row opens that item's view");
    assert_eq!(serials(&frame).len(), 2);
}

#[test]
fn enter_on_the_heading_opens_nothing_and_on_an_item_row_opens_it() {
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    start_job(&mut app, "j_1");
    app.on_click(crate::mouse::TargetId::Panel(
        crate::app::panel::Spot::JobList,
    ));
    app.on_key(crate::keys::Key::Enter, clock.now());
    assert!(!app.item_open());
    assert!(app.session_view_screen(80).is_some());
    app.on_key(crate::keys::Key::Down, clock.now());
    app.on_key(crate::keys::Key::Enter, clock.now());
    assert!(app.item_open());
    assert!(app.session_view_screen(80).is_none());
    assert_eq!(app.item_view().unwrap().description, "task j_1");
}

#[test]
fn empty_frames_draw_no_rows_and_say_nothing_running() {
    let mut app = home();
    opened(&mut app);
    for frame in [
        delegates_frame(&app, List::default()),
        jobs_frame(&app, List::default()),
    ] {
        assert!(frame.rows.is_empty());
        assert_eq!(frame.below, ["Nothing running."]);
    }
}

/// A `job_completed` for `job` with `status` at `ts`, so the finished
/// figures read a known elapsed time.
fn complete(app: &mut App, job: &str, status: &str, ts: u64) {
    let mut line = session_line("job_completed", json!({"job_id": job, "status": status}));
    if let Line::Session(envelope) = &mut line {
        envelope.ts = ts;
    }
    app.on_line(line);
}

#[test]
fn all_delegates_frame_lists_running_then_finished_with_two_rows_each() {
    let mut app = home();
    opened(&mut app);
    start_delegate(&mut app, "j_1", "s_dddddddddddddddd");
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    start_delegate(&mut app, "j_3", "s_ffffffffffffffff");
    start_delegate(&mut app, "j_4", "s_0000000000000000");
    start_job(&mut app, "j_9");
    complete(&mut app, "j_2", "completed", 8_000);
    complete(&mut app, "j_3", "failed", 8_000);
    complete(&mut app, "j_4", "cancelled", 8_000);
    let frame = all_delegates_frame(&app, List::default());
    assert_eq!(frame.title, "Delegates");
    assert_eq!(frame.command, "/delegates");
    // Two rows per delegate (docs/tui.md, "The panel"): four delegates
    // draw eight data rows, then the blank and hint footer rows.
    assert_eq!(frame.rows.len(), 10);
    assert!(frame.rows[8].is_empty());
    assert_eq!(
        frame.rows[9],
        [("a row opens that item's view".to_owned(), None, Ink::Muted)]
    );
    // Row data starts at once: no heading and no count (docs/tui.md,
    // "Swapped views"), and the empty text never draws as a row.
    assert!(frame.rows[0][0].0.contains("task j_1"));
    assert!(frame.below.is_empty());
    for cells in &frame.rows {
        for (cell, _, _) in cells {
            assert!(!cell.contains("running"), "{cell}");
            assert!(!cell.contains("No delegates"), "{cell}");
        }
    }
    assert!(
        frame
            .rows
            .iter()
            .flatten()
            .all(|(_, _, ink)| *ink != Ink::Heading)
    );
    // Running first, then finished, each in start order (docs/tui.md,
    // "Swapped views"): the plain job never lists. The running row
    // carries its elapsed suffix; the finished rows are exact.
    let firsts: Vec<&str> = frame
        .rows
        .iter()
        .step_by(2)
        .take(4)
        .map(|cells| cells[0].0.as_str())
        .collect();
    for (got, want) in firsts
        .iter()
        .zip(["◆ task j_1", "✓ task j_2", "✗ task j_3", "⊘ task j_4"])
    {
        assert!(got.starts_with(want), "{got}");
    }
    assert!(
        frame
            .rows
            .iter()
            .flatten()
            .all(|(cell, _, _)| !cell.contains("task j_9"))
    );
    // Every delegate fills two rows with one serial (docs/tui.md, "The
    // panel").
    let running = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let done = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let failed = app
        .serial_of_job(&contract::JobId("j_3".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    let cancelled = app
        .serial_of_job(&contract::JobId("j_4".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        serials(&frame),
        [
            running, running, done, done, failed, failed, cancelled, cancelled
        ]
    );
    // Each outcome keeps its word: done, failed, cancelled. Finished
    // rows never take attention: the delegate no longer works.
    assert_eq!(text(&frame.rows[3]), "  done");
    assert_eq!(text(&frame.rows[5]), "  failed");
    assert_eq!(text(&frame.rows[7]), "  cancelled");
    assert!(
        frame
            .rows
            .iter()
            .flatten()
            .all(|(_, _, ink)| *ink != Ink::Role(Role::Attention))
    );
    assert!(frame.footer.is_empty());
}

#[test]
fn the_all_delegates_frame_matches_the_card_under_the_fake_clock() {
    // Figures run under the injected clock (docs/testing.md, "Values
    // that change every run").
    let clock = fakes::clock::FakeClock::new();
    let mut app = home();
    opened(&mut app);
    let wall = contract::clock::wall_ms(clock.wall());
    app.set_now(clock.now(), wall);
    let mut started = session_line(
        "job_started",
        json!({
            "job_id": "j_1", "description": "task j_1", "output_path": "/tmp/out"
        }),
    );
    if let Line::Session(envelope) = &mut started {
        envelope.ts = wall.saturating_sub(61_000);
    }
    app.on_line(started);
    app.on_line(session_line(
        "delegate_started",
        json!({
            "job_id": "j_1", "delegate_session_id": "s_dddddddddddddddd",
            "harness": "fiber", "model": "test/model", "workspace": "/w"
        }),
    ));
    app.on_line(live(
        "s_dddddddddddddddd",
        json!({
            "state": "streaming", "parent": SESSION, "thinking": "high", "calls": 3,
            "spend": {"tokens": {"input": 650, "cache_read": 0, "cache_write": {}, "output": 0},
                "cost": 0.42, "subscription_cost": 0.0},
            "context": {"tokens": 650, "window": 1000}
        }),
    ));
    start_delegate(&mut app, "j_2", "s_eeeeeeeeeeeeeeee");
    app.on_line(live(
        "s_eeeeeeeeeeeeeeee",
        json!({"state": "streaming", "parent": SESSION, "calls": 6, "started": 2_000}),
    ));
    complete(&mut app, "j_2", "completed", 8_000);
    let frame = all_delegates_frame(&app, List::default());
    let running = app
        .serial_of_job(&contract::JobId("j_1".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        frame.rows[0],
        [(
            "◆ task j_1 · 1m".to_owned(),
            Some(Spot::Item(running)),
            Ink::Plain
        )]
    );
    assert_eq!(
        frame.rows[1],
        [
            (
                "  test/model · high · 3 calls · s_dddddddddddddddd · $0.42 ".to_owned(),
                Some(Spot::Item(running)),
                Ink::Muted
            ),
            (
                "▆".repeat(6),
                Some(Spot::Item(running)),
                Ink::Role(Role::Warning)
            ),
            (
                "░".repeat(4),
                Some(Spot::Item(running)),
                Ink::Role(Role::Rule)
            ),
            (" 65%".to_owned(), Some(Spot::Item(running)), Ink::Muted),
        ]
    );
    let done = app
        .serial_of_job(&contract::JobId("j_2".to_owned()))
        .unwrap_or_else(|| panic!("a serial"));
    assert_eq!(
        frame.rows[2],
        [("✓ task j_2".to_owned(), Some(Spot::Item(done)), Ink::Plain)]
    );
    assert_eq!(
        frame.rows[3],
        [(
            "  6 calls · 6s · done".to_owned(),
            Some(Spot::Item(done)),
            Ink::Muted
        )]
    );
}

#[test]
fn the_all_delegates_frame_with_no_delegates_says_no_delegates() {
    let mut app = home();
    opened(&mut app);
    let frame = all_delegates_frame(&app, List::default());
    assert_eq!(frame.title, "Delegates");
    assert_eq!(frame.command, "/delegates");
    assert!(frame.rows.is_empty());
    assert_eq!(frame.below, ["No delegates"]);
}
