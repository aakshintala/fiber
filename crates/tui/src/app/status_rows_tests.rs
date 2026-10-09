//! Tests for the narrow layout's rows: the fit, the widget row's toggle
//! and "N waiting" (`docs/tui.md`, "The narrow layout", "Shedding").

use std::path::PathBuf;

use super::{Fit, fit};
use crate::app::panel::Spot;
use crate::home::Launch;
use crate::keys::Key;
use crate::link::Line;
use crate::mouse::TargetId;
use contract::clock::Clock;

use super::super::App;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// Whether `row` is a right-aligned prompt bubble edge inside the column.
fn prompt_edge(row: &str, app: &App) -> bool {
    let left = app
        .chrome()
        .layout()
        .map_or(0, |layout| usize::from(layout.column.x));
    row.chars()
        .position(|cell| cell != ' ')
        .is_some_and(|first| first > left)
        && row.chars().all(|cell| matches!(cell, ' ' | '▄' | '▀'))
}

/// The launch description: `/w`, outside git, default shares and cards.
fn launch() -> Launch {
    Launch {
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
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    }
}

/// An app with home state, attached, at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(launch());
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of `session`.
fn session_line(session: &str, kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A live `session_status` for `session` named `name`, in `state`.
fn live(session: &str, name: &str, state: serde_json::Value) -> Line {
    let mut payload = serde_json::json!({
        "name": name, "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    session_line(session, "session_status", payload)
}

/// A live idle `session_status` for `session`.
fn idle(session: &str, name: &str) -> Line {
    live(session, name, serde_json::json!({"state": "idle"}))
}

/// A waiting `session_status` for `session`.
fn waiting(session: &str) -> Line {
    live(
        session,
        "other",
        serde_json::json!({"state": "waiting", "waiting": {"request_id": "r_1",
            "kind": "approval", "summary": "Run cargo test"}}),
    )
}

/// An `extension_ui` widget's lines.
fn widget(extension: &str, name: &str, lines: &[&str]) -> Line {
    session_line(
        SESSION,
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": name, "lines": lines}),
    )
}

/// A `turn_started` with one message.
fn turn_started(text: &str) -> Line {
    session_line(
        SESSION,
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    )
}

#[test]
fn fit_table() {
    // (height, base, delegates, status, kept delegates, kept status)
    let cases = [
        (30, 5, 4, 2, 4, 2),
        (20, 4, 4, 2, 4, 2),
        (20, 5, 4, 2, 0, 2),
        (10, 4, 0, 2, 0, 2),
        (10, 6, 0, 2, 0, 1),
        (10, 7, 0, 2, 0, 0),
        (10, 9, 0, 2, 0, 0),
    ];
    for (height, base, delegates, status, kept_delegates, kept_status) in cases {
        assert_eq!(
            fit(height, base, delegates, status),
            Fit {
                delegates: kept_delegates,
                status: kept_status,
            },
            "fit({height}, {base}, {delegates}, {status})"
        );
    }
}

#[test]
fn delegates_shed_before_status_rows() {
    for height in 10..=40 {
        for base in 0..height {
            for delegates in 1..=4 {
                let kept = fit(height, base, delegates, 2);
                if kept.delegates > 0 {
                    assert_eq!(kept.status, 2, "fit({height}, {base}, {delegates}, 2)");
                }
            }
        }
    }
}

#[test]
fn no_rows_outside_the_narrow_layout() {
    // Wide, with the panel drawn.
    let wide = attached(200, 40);
    assert!(!wide.chrome().layout().is_some_and(|layout| layout.narrow));
    assert_eq!(wide.narrow_fit(), None);
    assert_eq!(wide.narrow_rows(wide.below_rows()), 0);
    // Narrow, with the panel hidden by the person.
    let mut hidden = attached(100, 30);
    hidden.on_key(Key::AltP, fakes::clock::FakeClock::new().now());
    assert!(!hidden.chrome().layout().is_some_and(|layout| layout.narrow));
    assert_eq!(hidden.narrow_fit(), None);
    assert_eq!(hidden.narrow_rows(hidden.below_rows()), 0);
}

#[test]
fn conversation_height_equals_the_drawn_rows_in_the_narrow_layout() {
    let long: String = std::iter::repeat_n('w', 20_000).collect();
    for height in [10, 12, 16, 24, 40] {
        for (name, setup) in [
            ("plain", vec![]),
            ("shut", vec![("ex", "wid", vec!["abc"])]),
            ("open", vec![("ex", "wid", vec!["abc", "def", "ghi"])]),
        ] {
            let mut app = attached(100, height);
            app.on_line(idle(SESSION, "one"));
            app.on_line(session_line(
                SESSION,
                "job_started",
                serde_json::json!({"job_id": "j_1", "description": "build",
                    "output_path": "/tmp/out"}),
            ));
            for (extension, name, lines) in &setup {
                app.on_line(widget(extension, name, lines));
            }
            if name == "open" {
                app.on_click(TargetId::Panel(Spot::Widget));
            }
            app.on_line(turn_started(&long));
            let area = ratatui::layout::Rect::new(0, 0, 100, height);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            crate::view::render(&app, area, &mut buf, None);
            let drawn = crate::view::text(&buf)
                .lines()
                .filter(|row| {
                    row.contains("www") || row.contains("00:00") || prompt_edge(row, &app)
                })
                .count();
            assert_eq!(drawn, app.conversation_height(), "{name} at 100x{height}");
        }
    }
}

#[test]
fn conversation_height_equals_the_drawn_rows_with_delegates() {
    let long: String = std::iter::repeat_n('w', 20_000).collect();
    for height in [12, 16, 24, 40] {
        for (name, setup) in [
            ("plain", vec![]),
            ("shut", vec!["abc"]),
            ("open", vec!["abc", "def", "ghi"]),
        ] {
            let mut app = attached(100, height);
            app.on_line(idle(SESSION, "one"));
            app.on_line(session_line(
                SESSION,
                "job_started",
                serde_json::json!({"job_id": "j_1", "description": "alpha",
                    "output_path": "/tmp/out"}),
            ));
            app.on_line(session_line(
                SESSION,
                "delegate_started",
                serde_json::json!({"job_id": "j_1",
                    "delegate_session_id": "s_cccccccccccccccc",
                    "harness": "fiber", "model": "test/model", "workspace": "/w"}),
            ));
            if !setup.is_empty() {
                app.on_line(widget("ex", "wid", &setup));
            }
            if name == "open" {
                app.on_click(TargetId::Panel(Spot::Widget));
            }
            app.on_line(turn_started(&long));
            let area = ratatui::layout::Rect::new(0, 0, 100, height);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            crate::view::render(&app, area, &mut buf, None);
            let drawn = crate::view::text(&buf)
                .lines()
                .filter(|row| {
                    row.contains("www") || row.contains("00:00") || prompt_edge(row, &app)
                })
                .count();
            assert_eq!(drawn, app.conversation_height(), "{name} at 100x{height}");
        }
    }
}

#[test]
fn the_widget_row_expands_and_collapses_on_click() {
    let mut app = attached(100, 30);
    app.on_line(widget("ex", "wid", &["abc", "def"]));
    assert!(!app.widget_open());
    assert_eq!(crate::view::status_rows::widget(&app, 100).len(), 1);
    app.on_click(TargetId::Panel(Spot::Widget));
    assert!(app.widget_open());
    assert_eq!(crate::view::status_rows::widget(&app, 100).len(), 2);
    app.on_click(TargetId::Panel(Spot::Widget));
    assert!(!app.widget_open());
    assert_eq!(crate::view::status_rows::widget(&app, 100).len(), 1);
}

#[test]
fn a_click_on_n_waiting_shows_the_rail() {
    // At 110 columns the rail fits beside no panel.
    let mut app = attached(110, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    assert!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.grip)
            .is_some()
    );
    let area = ratatui::layout::Rect::new(0, 0, 110, 30);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buf, None);
    let waiting = targets
        .iter()
        .find(|target| target.id == TargetId::Panel(Spot::Waiting))
        .expect("a waiting target");
    assert!(waiting.rect.width > 0);
    app.on_click(TargetId::Panel(Spot::Waiting));
    assert!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .is_some()
    );
}

#[test]
fn a_click_on_n_waiting_where_the_rail_does_not_fit_clears_the_hide() {
    // At 100 columns the rail fits nowhere.
    let mut app = attached(100, 30);
    app.on_line(idle(SESSION, "one"));
    app.on_line(waiting(OTHER));
    app.on_key(Key::AltR, fakes::clock::FakeClock::new().now());
    app.on_click(TargetId::Panel(Spot::Waiting));
    assert_eq!(app.chrome().layout().and_then(|layout| layout.rail), None);
    // The hide is cleared: grown, the rail returns instead of its grip.
    app.set_size(200, 40);
    assert!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.rail)
            .is_some()
    );
}
