//! Tests for the Delegates card's wheel: over the card it scrolls the
//! card by one delegate, anywhere else on the panel it scrolls the panel.

use super::super::App;
use crate::home::Launch;
use crate::keys::{Mouse, MouseKind};
use crate::link::Line;
use ratatui::layout::Rect;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

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
    // The card's six rows, a blank row, then the widget's title row.
    let last = first_row(&app).saturating_add(5);
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
    // The widget's 61 rows and a blank row come before the card.
    let card = first.saturating_add(u16::try_from(62 - skip).unwrap_or(u16::MAX));
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
