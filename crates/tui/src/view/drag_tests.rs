//! Tests for the drag's visuals: the edge column's tint and the pill
//! naming the live share and columns (`docs/tui.md`, "Layout").

use std::path::PathBuf;

use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::app::App;
use crate::home::Launch;
use crate::keys::{Button, Mouse, MouseKind};
use crate::link::Line;
use crate::markdown::Role;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const OTHER: &str = "s_bbbbbbbbbbbbbbbb";
const CARDS: [&str; 5] = ["session", "changed_files", "delegates", "jobs", "quota"];

/// An app with home state, attached, with two live sessions.
fn two(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(Launch {
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
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app.on_line(status(SESSION, "one"));
    app.on_line(status(OTHER, "two"));
    app
}

/// A live idle `session_status` for `session` named `name`.
fn status(session: &str, name: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": name, "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

/// Draws `app` at 200x40 with the pointer at `at`.
fn draw(app: &App, at: Option<(u16, u16)>) -> Buffer {
    let area = Rect::new(0, 0, 200, 40);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, at);
    buf
}

/// The background of `buf`'s cell.
fn bg(buf: &Buffer, x: u16, y: u16) -> ratatui::style::Color {
    buf.cell((x, y))
        .map_or(ratatui::style::Color::Reset, |cell| cell.bg)
}

#[test]
fn the_edge_under_the_pointer_tints() {
    use ratatui::style::Modifier;
    let app = two(200, 40);
    // The rail's edge is column 29, the panel's 158.
    let buf = draw(&app, Some((29, 10)));
    for y in [0, 10, 39] {
        assert_eq!(bg(&buf, 29, y), Role::Rule.color(), "rail edge row {y}");
    }
    assert_ne!(bg(&buf, 28, 10), Role::Rule.color());
    assert_ne!(bg(&buf, 30, 10), Role::Rule.color());
    for y in 19..=21 {
        let cell = buf.cell((29, y)).expect("a grip cell");
        assert_eq!(cell.symbol(), "⋮");
        assert_eq!(cell.fg, Role::Accent.color());
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
    let buf = draw(&app, Some((158, 10)));
    for y in [0, 10, 39] {
        assert_eq!(bg(&buf, 158, y), Role::Rule.color(), "panel edge row {y}");
    }
    assert_ne!(bg(&buf, 157, 10), Role::Rule.color());
    assert_ne!(bg(&buf, 159, 10), Role::Rule.color());
}

#[test]
fn the_grip_under_the_pointer_tints() {
    use ratatui::style::Modifier;
    let mut app = two(200, 40);
    app.on_key(crate::keys::Key::AltR, fakes::clock::FakeClock::new().now());
    assert!(
        app.chrome()
            .layout()
            .and_then(|layout| layout.grip)
            .is_some()
    );
    let buf = draw(&app, Some((0, 10)));
    for y in [0, 10, 39] {
        assert_eq!(bg(&buf, 0, y), Role::Rule.color(), "grip row {y}");
    }
    assert_ne!(bg(&buf, 1, 10), Role::Rule.color());
    for y in 19..=21 {
        let cell = buf.cell((0, y)).expect("a grip cell");
        assert_eq!(cell.symbol(), "⋮");
        assert_eq!(cell.fg, Role::Accent.color());
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn no_tint_without_a_pointer() {
    use ratatui::style::Modifier;
    let app = two(200, 40);
    let buf = draw(&app, None);
    for (x, row) in [(29, 10), (158, 10)] {
        assert_ne!(bg(&buf, x, row), Role::Rule.color(), "column {x}");
    }
    for (x, y) in [
        (29, 19),
        (29, 20),
        (29, 21),
        (158, 19),
        (158, 20),
        (158, 21),
    ] {
        let cell = buf.cell((x, y)).expect("a grip cell");
        assert_eq!(cell.fg, Role::Muted.color(), "grip ({x}, {y})");
        assert!(!cell.modifier.contains(Modifier::BOLD), "grip ({x}, {y})");
    }
}

#[test]
fn the_dragged_edge_stays_tinted_off_the_pointer() {
    let mut app = two(200, 40);
    app.on_drag(&Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 29,
        row: 20,
    });
    app.on_drag(&Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: 30,
        row: 25,
    });
    // The rail is 31 wide now, so its edge moved to column 30.
    let buf = draw(&app, None);
    for y in [0, 10, 39] {
        assert_eq!(bg(&buf, 30, y), Role::Rule.color(), "edge row {y}");
    }
    assert_ne!(bg(&buf, 31, 10), Role::Rule.color());
}

#[test]
fn drag_pill_rail() {
    let mut app = two(200, 40);
    app.on_drag(&Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 29,
        row: 20,
    });
    app.on_drag(&Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: 30,
        row: 20,
    });
    let buf = draw(&app, Some((30, 20)));
    insta::assert_snapshot!("drag_pill_rail", crate::view::text(&buf));
}

#[test]
fn drag_pill_panel() {
    let mut app = two(200, 40);
    app.on_drag(&Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 158,
        row: 20,
    });
    app.on_drag(&Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: 150,
        row: 20,
    });
    let buf = draw(&app, Some((150, 20)));
    insta::assert_snapshot!("drag_pill_panel", crate::view::text(&buf));
}

#[test]
fn no_pill_without_a_drag() {
    let app = two(200, 40);
    let buf = draw(&app, Some((29, 20)));
    assert!(
        !crate::view::text(&buf).contains("cols"),
        "{}",
        crate::view::text(&buf)
    );
    // A grip below the floor has no share yet, so it draws none either.
    let mut app = two(200, 40);
    app.on_key(crate::keys::Key::AltR, fakes::clock::FakeClock::new().now());
    app.on_drag(&Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 0,
        row: 20,
    });
    app.on_drag(&Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: 20,
        row: 20,
    });
    let buf = draw(&app, Some((20, 20)));
    assert!(
        !crate::view::text(&buf).contains("cols"),
        "{}",
        crate::view::text(&buf)
    );
}
