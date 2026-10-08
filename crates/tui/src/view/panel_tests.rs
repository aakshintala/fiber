//! Tests for the panel's card order and widget cards.

use super::{cards, rows};
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::PathBuf;

const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// The default card list.
fn list(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// An app with home state, attached, at `width` by `height`, drawing the
/// default card list.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: CARDS.map(str::to_owned).to_vec(),
        ..Default::default()
    });
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app.set_size(width, height);
    app
}

/// One envelope of the attached session.
fn session_line(kind: &str, payload: serde_json::Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId("a_1".to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Folds a widget of `lines` into `app`.
fn fold_widget(app: &mut App, extension: &str, widget: &str, lines: &[&str]) {
    app.on_line(session_line(
        "extension_ui",
        serde_json::json!({"extension": extension, "widget": widget, "lines": lines}),
    ));
}

#[test]
fn cards_follow_the_list() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["plan/tasks", "other/list"]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn a_listed_widget_takes_its_listed_place() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn an_unlisted_widget_shows_after_the_listed_cards_in_arrival_order() {
    let widgets = [("plan", "tasks"), ("other", "list")];
    assert_eq!(
        cards(&list(&[]), &widgets),
        vec![super::Card::Widget(0), super::Card::Widget(1)]
    );
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(1), super::Card::Widget(0)]
    );
}

#[test]
fn quota_delegates_and_unknown_names_place_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["quota"]), &none).is_empty());
    assert!(cards(&list(&["delegates"]), &none).is_empty());
    assert!(cards(&list(&["nope"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(
            &list(&["quota", "delegates", "nope", "plan/tasks"]),
            &widgets
        ),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_listed_widget_that_is_absent_places_nothing() {
    let none: [(&str, &str); 0] = [];
    assert!(cards(&list(&["other/list"]), &none).is_empty());
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["other/list"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

#[test]
fn a_name_listed_twice_places_one_card() {
    let widgets = [("plan", "tasks")];
    assert_eq!(
        cards(&list(&["plan/tasks", "plan/tasks"]), &widgets),
        vec![super::Card::Widget(0)]
    );
}

/// Draws only the panel rect of `app`.
fn draw_panel(app: &App) -> Buffer {
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let mut buf = Buffer::empty(Rect::new(0, 0, rect.width, rect.height));
    let mut targets = Vec::new();
    super::draw(
        app,
        Rect::new(0, 0, rect.width, rect.height),
        &mut buf,
        &mut targets,
    );
    assert!(targets.is_empty());
    buf
}

#[test]
fn widget_card() {
    let mut app = attached(160, 40);
    fold_widget(&mut app, "plan", "tasks", &["one", "two", "three"]);
    let buf = draw_panel(&app);
    insta::assert_snapshot!("widget_card", super::super::text(&buf));
}

#[test]
fn widget_rows_are_cut_at_the_text_width() {
    let mut app = attached(160, 40);
    let rect = app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel rect"));
    let text = usize::from(rect.width).saturating_sub(3);
    fold_widget(&mut app, "plan", "tasks", &["x".repeat(text + 10).as_str()]);
    let drawn: Vec<String> = rows(&app, rect.width)
        .iter()
        .map(|row| row.line.to_string())
        .collect();
    assert_eq!(drawn.len(), 2);
    assert!(drawn.iter().all(|row| crate::format::width(row) <= text));
}
