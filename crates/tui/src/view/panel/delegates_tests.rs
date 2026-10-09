//! Tests for the Delegates card: which delegates it lists and their rows.

use super::DELEGATES_SHOWN;
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use crate::view::panel::{Card, cards, rows};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// Names as a card list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// An app with home state, attached, at `width` by `height`, drawing
/// `names`.
fn attached_with(width: u16, height: u16, names: &[&str]) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: list(names),
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// An app drawing the default card list.
fn attached(width: u16, height: u16) -> App {
    attached_with(width, height, &CARDS)
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

/// Folds a started job `job` of `description`.
fn fold_job(app: &mut App, job: &str, description: &str) {
    app.on_line(session_line(
        "job_started",
        serde_json::json!({"job_id": job, "description": description,
            "output_path": "/tmp/out"}),
    ));
}

/// Folds a delegate on `harness` with `model` as job `job`.
fn fold_delegate(app: &mut App, job: &str, harness: &str, model: &str, description: &str) {
    fold_job(app, job, description);
    app.on_line(session_line(
        "delegate_started",
        serde_json::json!({"job_id": job,
            "delegate_session_id": format!("s_{job:0>16}"),
            "harness": harness, "model": model, "workspace": "/w"}),
    ));
}

/// Folds the end of delegate job `job`: `delegate_finished`, then
/// `job_completed`.
fn finish(app: &mut App, job: &str) {
    app.on_line(session_line(
        "delegate_finished",
        serde_json::json!({"job_id": job, "text": "done",
            "usage": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": null, "subscription_cost": 0.0}}),
    ));
    app.on_line(session_line(
        "job_completed",
        serde_json::json!({"job_id": job, "status": "completed"}),
    ));
}

/// Folds Fiber delegate `j_<k>`, described `task <k>`.
fn fold_fiber(app: &mut App, k: usize) {
    fold_delegate(
        app,
        &format!("j_{k}"),
        "fiber",
        "test/model",
        &format!("task {k}"),
    );
}

/// Folds `n` Fiber delegates, `j_1` to `j_<n>`.
fn fold_delegates(app: &mut App, n: usize) {
    for k in 1..=n {
        fold_fiber(app, k);
    }
}

/// The panel rect's width.
fn panel_width(app: &App) -> u16 {
    app.chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"))
        .width
}

/// The text width of the panel's cards.
fn text_width(app: &App) -> usize {
    usize::from(panel_width(app)).saturating_sub(3)
}

/// The Delegates card's rows' text.
fn card_texts(app: &App) -> Vec<String> {
    super::rows(app, text_width(app))
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// Every panel row's text.
fn texts(app: &App) -> Vec<String> {
    rows(app, panel_width(app))
        .iter()
        .map(|row| row.line.to_string())
        .collect()
}

/// Draws only the panel rect of `app`, with its click targets.
fn draw_targets(app: &App) -> (Buffer, Vec<crate::mouse::Target>) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let area = Rect::new(0, 0, rect.width, rect.height);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    crate::view::panel::draw(app, area, &mut buf, &mut targets);
    (buf, targets)
}

#[test]
fn a_started_delegate_shows_until_its_job_completes() {
    let mut app = attached(160, 40);
    fold_delegate(&mut app, "j_1", "fiber", "test/model", "review the parser");
    assert_eq!(
        card_texts(&app),
        vec![
            "● fiber  test/model".to_owned(),
            "  review the parser".to_owned()
        ]
    );
    finish(&mut app, "j_1");
    assert!(card_texts(&app).is_empty());
}

#[test]
fn a_job_without_delegate_started_is_not_a_delegate() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_1", "build");
    assert!(card_texts(&app).is_empty());
    assert!(app.panel_state().running_delegates().is_empty());
}

#[test]
fn without_a_status_the_rows_show_the_harness_model_and_description() {
    let mut app = attached(160, 40);
    fold_delegate(&mut app, "j_1", "claude", "opus", "write the docs");
    let drawn = super::rows(&app, text_width(&app));
    let first = drawn.first().unwrap_or_else(|| panic!("a first row"));
    assert_eq!(first.line.to_string(), "● claude  opus");
    let glyph = first
        .line
        .spans
        .first()
        .unwrap_or_else(|| panic!("a glyph span"));
    assert_eq!(glyph.content, "● claude");
    assert_eq!(
        glyph.style,
        crate::markdown::style(crate::markdown::Role::Accent)
    );
    let model = first
        .line
        .spans
        .last()
        .unwrap_or_else(|| panic!("a model span"));
    assert_eq!(model.content, "opus");
    assert_eq!(model.style, ratatui::style::Style::default());
    assert_eq!(
        drawn.get(1).map(|row| row.line.to_string()),
        Some("  write the docs".to_owned())
    );
}

#[test]
fn three_delegates_show_and_a_fourth_does_not() {
    assert_eq!(DELEGATES_SHOWN, 3);
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 3);
    let three = card_texts(&app);
    assert_eq!(three.len(), 6);
    assert_eq!(three.get(5).map(String::as_str), Some("  task 3"));
    fold_fiber(&mut app, 4);
    let four = card_texts(&app);
    assert_eq!(four, three);
    assert!(!texts(&app).iter().any(|row| row == "  task 4"));
}

#[test]
fn the_description_is_cut_at_the_text_width() {
    let mut app = attached(160, 40);
    let text = text_width(&app);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "test/model",
        &"x".repeat(text + 10),
    );
    let drawn = card_texts(&app);
    let second = drawn.get(1).unwrap_or_else(|| panic!("a second row"));
    assert_eq!(crate::format::width(second), text);
    assert_eq!(*second, format!("  {}", "x".repeat(text - 2)));
}

#[test]
fn the_card_draws_no_targets() {
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 2);
    let (_, targets) = draw_targets(&app);
    assert!(targets.is_empty());
}

#[test]
fn delegates_places_one_card() {
    let none: [(&str, &str); 0] = [];
    assert_eq!(
        cards(&list(&["delegates", "delegates"]), &none),
        vec![Card::Delegates]
    );
}

#[test]
fn the_card_takes_its_listed_place() {
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["jobs", "delegates", "session"]), &widgets),
        vec![Card::Jobs, Card::Delegates, Card::Session, Card::Widget(0)]
    );
    let mut app = attached_with(160, 40, &["jobs", "delegates"]);
    fold_job(&mut app, "j_0", "build");
    fold_delegates(&mut app, 1);
    let edge = "▄".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let edge_bottom = "▀".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    assert_eq!(
        texts(&app),
        vec![
            edge.clone(),
            "1 job running".to_owned(),
            edge_bottom.clone(),
            String::new(),
            edge,
            "● fiber  test/model".to_owned(),
            "  task 1".to_owned(),
            edge_bottom,
        ]
    );
}

#[test]
fn no_delegates_draws_no_card_and_no_blank_row() {
    let mut app = attached_with(160, 40, &["delegates", "jobs"]);
    fold_job(&mut app, "j_0", "build");
    let edge = "▄".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    let edge_bottom = "▀".repeat(usize::from(panel_width(&app).saturating_sub(1)));
    assert_eq!(
        texts(&app),
        vec![edge, "1 job running".to_owned(), edge_bottom]
    );
}

#[test]
fn delegates_card_from_the_fold() {
    let mut app = attached(160, 40);
    fold_job(&mut app, "j_0", "build the workspace");
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "anthropic/opus",
        "review the parser",
    );
    fold_delegate(&mut app, "j_2", "claude", "sonnet", "write the docs");
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_from_the_fold", crate::view::text(&buf));
}

#[test]
fn delegates_card_at_the_panel_floor() {
    let mut app = attached(140, 24);
    assert_eq!(panel_width(&app), 30);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "anthropic/claude-opus-5-5-with-a-long-name",
        "review the parser and the lexer together",
    );
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_at_the_panel_floor", crate::view::text(&buf));
}

/// Scrolls the card down `steps` delegates with the wheel over its first
/// row.
fn scroll_card(app: &mut App, steps: usize) {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    for _ in 0..steps {
        app.on_wheel(&crate::keys::Mouse {
            kind: crate::keys::MouseKind::WheelDown,
            col: rect.x.saturating_add(4),
            row: rect.y.saturating_add(1),
        });
    }
}

#[test]
fn the_card_shows_from_its_scroll_offset() {
    let mut app = attached_with(160, 40, &["delegates"]);
    fold_delegates(&mut app, 5);
    scroll_card(&mut app, 2);
    assert_eq!(app.panel_state().delegate_scroll(), 2);
    let descriptions: Vec<String> = card_texts(&app).into_iter().skip(1).step_by(2).collect();
    assert_eq!(descriptions, vec!["  task 3", "  task 4", "  task 5"]);
    // Two delegates finish while scrolled: the offset clamps when drawn.
    for job in ["j_4", "j_5"] {
        app.on_line(session_line(
            "job_completed",
            serde_json::json!({"job_id": job, "status": "completed"}),
        ));
    }
    let descriptions: Vec<String> = card_texts(&app).into_iter().skip(1).step_by(2).collect();
    assert_eq!(descriptions, vec!["  task 1", "  task 2", "  task 3"]);
}

#[test]
fn the_span_covers_only_the_card_rows() {
    let mut app = attached_with(160, 40, &["session", "delegates", "jobs"]);
    app.on_line(session_line(
        "session_status",
        serde_json::json!({
            "name": "work", "workspace": "/w", "project": "-w",
            "state": "idle", "since": 0,
            "spend": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {},
                "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 2, "jobs": 3, "clients": 0,
        }),
    ));
    fold_job(&mut app, "j_0", "build");
    fold_delegates(&mut app, 2);
    let (drawn, span) = crate::view::panel::rows_and_delegates(&app, panel_width(&app));
    let texts: Vec<String> = drawn.iter().map(|row| row.line.to_string()).collect();
    let start = texts
        .iter()
        .position(|row| row == "● fiber  test/model")
        .unwrap_or_else(|| panic!("the card's first row"));
    assert!(start > 1);
    assert_eq!(span, Some(start - 1..start + 5));
    assert_eq!(texts.get(start - 2).map(String::as_str), Some(""));
    assert_eq!(texts.get(start + 5).map(String::as_str), Some(""));
    assert!(
        texts.get(start + 6).is_some_and(|row| row.starts_with('▄')),
        "{texts:?}"
    );
    assert_eq!(
        texts.get(start + 7).map(String::as_str),
        Some("1 job running")
    );
}

#[test]
fn delegates_card_scrolled() {
    let mut app = attached(160, 40);
    fold_delegates(&mut app, 5);
    scroll_card(&mut app, 1);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_scrolled", crate::view::text(&buf));
}

/// A delegate's `session_status` in `state`, naming the attached session.
fn delegate_status(app: &mut App, job: &str, state: serde_json::Value) {
    let mut payload = serde_json::json!({
        "name": "delegate one", "workspace": "/w", "project": "-w",
        "since": 0,
        "spend": {"tokens": {"input": 1, "cache_read": 0,
            "cache_write": {}, "output": 2},
            "cost": 0.0, "subscription_cost": 0.0},
        "model": "status/model", "delegates": 0, "jobs": 0, "clients": 0,
        "parent": SESSION,
    });
    for (key, value) in state.as_object().cloned().unwrap_or_default() {
        payload[key] = value;
    }
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(format!("s_{job:0>16}")),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }));
}

/// A delegate's unreadable `session_status`: a schema this terminal does
/// not read. Its payload still names the parent, as the hub's does.
fn unreadable_status(app: &mut App, job: &str) {
    app.on_line(Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(format!("s_{job:0>16}")),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION + 1,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: serde_json::json!({"parent": SESSION})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }));
}

#[test]
fn with_a_status_the_row_shows_its_glyph_and_word() {
    for (state, glyph, word, role) in [
        (
            serde_json::json!({"state": "streaming"}),
            "●",
            "WORKING",
            crate::markdown::Role::Accent,
        ),
        (
            serde_json::json!({"state": "tool", "tool": "read"}),
            "●",
            "WORKING",
            crate::markdown::Role::Accent,
        ),
        (
            serde_json::json!({"state": "retrying"}),
            "●",
            "RETRYING",
            crate::markdown::Role::Warning,
        ),
        (
            serde_json::json!({"state": "jobs"}),
            "●",
            "WORKING",
            crate::markdown::Role::Accent,
        ),
        (
            serde_json::json!({"state": "idle"}),
            "✓",
            "READY",
            crate::markdown::Role::Muted,
        ),
    ] {
        let mut app = attached(160, 40);
        fold_fiber(&mut app, 1);
        delegate_status(&mut app, "j_1", state);
        let drawn = super::rows(&app, text_width(&app));
        let first = drawn.first().unwrap_or_else(|| panic!("a first row"));
        assert_eq!(
            first.line.to_string(),
            format!("{glyph} {word}  test/model")
        );
        let state_span = first
            .line
            .spans
            .first()
            .unwrap_or_else(|| panic!("a state span"));
        assert_eq!(state_span.content, format!("{glyph} {word}"));
        assert_eq!(state_span.style, crate::markdown::style(role));
    }
    let mut app = attached(160, 40);
    fold_fiber(&mut app, 1);
    unreadable_status(&mut app, "j_1");
    let drawn = super::rows(&app, text_width(&app));
    let first = drawn.first().unwrap_or_else(|| panic!("a first row"));
    assert_eq!(first.line.to_string(), "? CANNOT ATTACH  test/model");
    let state_span = first
        .line
        .spans
        .first()
        .unwrap_or_else(|| panic!("a state span"));
    assert_eq!(
        state_span.style,
        crate::markdown::style(crate::markdown::Role::Muted)
    );
}

#[test]
fn the_status_row_keeps_the_started_model() {
    let mut app = attached(160, 40);
    fold_delegate(
        &mut app,
        "j_1",
        "fiber",
        "started/model",
        "review the parser",
    );
    delegate_status(&mut app, "j_1", serde_json::json!({"state": "streaming"}));
    let drawn = super::rows(&app, text_width(&app));
    let first = drawn.first().unwrap_or_else(|| panic!("a first row"));
    assert_eq!(first.line.to_string(), "● WORKING  started/model");
}

#[test]
fn delegates_card_with_statuses() {
    let mut app = attached(160, 40);
    fold_fiber(&mut app, 1);
    delegate_status(&mut app, "j_1", serde_json::json!({"state": "streaming"}));
    fold_delegate(&mut app, "j_2", "claude", "sonnet", "write the docs");
    fold_fiber(&mut app, 3);
    let (buf, _) = draw_targets(&app);
    insta::assert_snapshot!("delegates_card_with_statuses", crate::view::text(&buf));
}
