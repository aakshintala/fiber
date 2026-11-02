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

/// Whether `row` is blank once the bar's glyph is trimmed: a gap row in
/// the card (`docs/tui.md`, "Turns").
fn blank_gap(row: &str) -> bool {
    // The grip's ⋮ and the scroll bar are not content: a row holding
    // only blanks, edges and those marks is a gap.
    row.trim_matches([' ', '█', '│', '┃', '⋮']).is_empty()
}

/// Whether `row` is a right-aligned prompt bubble edge inside the column:
/// the bar's glyph may end it. The grip's ⋮ at mid-height may sit in
/// column 0, where the input box's edge overlaps it on a short screen:
/// it reads as the blank it covers. The edge starts at the column's
/// own left edge, past nothing.
fn prompt_edge(row: &str, app: &App) -> bool {
    let left = app
        .chrome()
        .layout()
        .map_or(0, |layout| usize::from(layout.column.x));
    let row = row.trim_end_matches(['█', '│', '┃']);
    let row = row
        .strip_prefix('⋮')
        .map_or_else(|| row.to_owned(), |rest| format!(" {rest}"));
    row.chars()
        .position(|cell| cell != ' ')
        .is_some_and(|first| first >= left)
        && row.chars().all(|cell| matches!(cell, ' ' | '▄' | '▀'))
}

/// The launch description: `/w`, outside git, default shares and cards.
fn launch() -> Launch {
    Launch {
        location: crate::LaunchLocation::Local {
            workspace: PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
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
        "started": 0, "calls": 0, "since": 0,
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

/// The moved segments leave the status line: the working directory, the
/// model and thinking level, the permission mode and the context bar
/// draw in the input box's status row now, so no status-line row
/// carries them at any width (`docs/tui.md`, "The input box").
#[test]
fn the_moved_segments_leave_the_status_line() {
    let mut app = attached(60, 12);
    app.on_line(session_line(
        SESSION,
        "preamble_built",
        serde_json::json!({
            "reason": "start", "model": "test/model", "context_window": 1_000_000,
            "trigger_at": 400_000, "thinking": "high",
            "tool_choice": "auto", "cache_lifetime": "5m",
            "system_prompt": "", "tools": [],
        }),
    ));
    app.on_line(live(
        SESSION,
        "one",
        serde_json::json!({"context": {"tokens": 268_000, "window": 1_000_000}}),
    ));
    for width in [20u16, 60, 200] {
        let rows = crate::view::status_rows::status(&app, width);
        let text = rows
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        for dropped in ["test/model", "high", "auto", "ctx", "268", "/w"] {
            assert!(
                !text.contains(dropped),
                "no {dropped} in the status line at {width}: {text}"
            );
        }
        assert!(
            rows.iter()
                .flat_map(|row| row.spots.iter())
                .all(|(_, _, spot)| !matches!(spot, Spot::Model | Spot::Context)),
            "no row target opens the picker or the breakdown at {width}"
        );
    }
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
fn only_the_box_chrome_counts_with_the_panel_shown() {
    // Wide, with the panel drawn: only the input box's rule and status
    // row count below the conversation; the cards carry the figures.
    let wide = attached(200, 40);
    assert!(!wide.chrome().layout().is_some_and(|layout| layout.narrow));
    assert_eq!(wide.narrow_fit(), None);
    assert_eq!(wide.narrow_rows(wide.below_rows()), 2);
}

#[test]
fn the_hidden_panel_keeps_its_status_line_inside_the_box() {
    // Wide with the panel hidden by the person: the status line draws
    // inside the input box under its status row, so its rows count
    // below the conversation past the rule and the row.
    let mut hidden = attached(100, 30);
    hidden.on_key(Key::AltP, fakes::clock::FakeClock::new().now());
    let layout = hidden.chrome().layout().expect("a laid-out session");
    assert!(!layout.narrow);
    assert_eq!(hidden.narrow_fit(), None);
    let lines = crate::view::status_rows::status(&hidden, layout.column.width).len();
    assert_eq!(hidden.narrow_rows(hidden.below_rows()), 2 + lines);
    assert!(lines > 0, "the fixture keeps a status row to move");
}

#[test]
fn conversation_height_equals_the_drawn_rows_in_the_narrow_layout() {
    let long: String = std::iter::repeat_n('w', 5_000).collect();
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
            let text = crate::view::text(&buf);
            let shown: Vec<&str> = text.lines().take(app.conversation_height()).collect();
            let drawn = shown
                .iter()
                .filter(|row| row.contains("ww") || row.contains("00:00") || prompt_edge(row, &app))
                .count();
            let blank = shown.iter().filter(|row| blank_gap(row)).count();
            assert_eq!(
                drawn + blank,
                app.conversation_height(),
                "{name} at 100x{height}"
            );
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
            let text = crate::view::text(&buf);
            let shown: Vec<&str> = text.lines().take(app.conversation_height()).collect();
            let drawn = shown
                .iter()
                .filter(|row| row.contains("ww") || row.contains("00:00") || prompt_edge(row, &app))
                .count();
            let blank = shown.iter().filter(|row| blank_gap(row)).count();
            assert_eq!(
                drawn + blank,
                app.conversation_height(),
                "{name} at 100x{height}"
            );
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
