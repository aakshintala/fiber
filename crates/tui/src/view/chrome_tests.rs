//! Snapshots of the session screen's chrome: the floor line, the
//! conversation's gutter and the panel's region.

use super::super::{cursor, render, text};
use crate::app::App;
use crate::home::Launch;
use crate::link::Line;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use std::path::PathBuf;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// An app with home state at `width` by `height`, attached unless
/// `on_home`.
fn app(width: u16, height: u16, on_home: bool) -> App {
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
        panel_cards: Vec::new(),
        ..Default::default()
    });
    if !on_home {
        app.attach(contract::SessionId(SESSION.to_owned()));
    }
    app.set_size(width, height);
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

/// Names the session `name`.
fn named(app: &mut App, name: &str) {
    app.on_line(session_line(
        "session_named",
        serde_json::json!({"name": name, "by": "person"}),
    ));
}

/// Draws `app` at `width` by `height`: the buffer and the target count.
fn draw(app: &App, width: u16, height: u16) -> (Buffer, usize) {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    let targets = render(app, area, &mut buf, None);
    (buf, targets.len())
}

#[test]
fn floor_line_32x8() {
    for on_home in [true, false] {
        let app = app(32, 8, on_home);
        let (buf, targets) = draw(&app, 32, 8);
        assert_eq!(targets, 0);
        assert_eq!(cursor(&app, Rect::new(0, 0, 32, 8)), None);
        insta::assert_snapshot!("floor_line_32x8", text(&buf));
    }
}

#[test]
fn attached_160x40_with_the_panel() {
    let mut app = app(160, 40, false);
    named(&mut app, "fix the parser");
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hello"}]}]}),
    ));
    let (buf, _) = draw(&app, 160, 40);
    insta::assert_snapshot!("attached_160x40_with_the_panel", text(&buf));
    // The panel's first column holds the grip at mid-height, and the
    // cursor sits in the column's input box.
    for y in 19..=21 {
        assert_eq!(buf.cell((126, y)).map(|cell| cell.symbol()), Some("⋮"));
    }
    assert_eq!(buf.cell((126, 18)).map(|cell| cell.symbol()), Some(" "));
    assert_eq!(
        cursor(&app, Rect::new(0, 0, 160, 40)),
        Some(Position::new(4, 38))
    );
}

#[test]
fn attached_100x30_narrow() {
    let mut app = app(100, 30, false);
    named(&mut app, "fix the parser");
    let (buf, _) = draw(&app, 100, 30);
    insta::assert_snapshot!("attached_100x30_narrow", text(&buf));
}

#[test]
fn floor_centres_with_division() {
    // `(area - text) / 2` centres; `% 2` would leave 0 or 1 instead.
    for (width, height, expected_x) in [(10u16, 5u16, 4u16), (9u16, 4u16, 3u16)] {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        super::floor("hi", area, &mut buf);
        let expected_y = height / 2;
        let symbol =
            |x: u16, y: u16, buf: &Buffer| buf.cell((x, y)).map(|cell| cell.symbol().to_owned());
        assert_eq!(symbol(expected_x, expected_y, &buf), Some("h".to_owned()));
        assert_eq!(
            symbol(expected_x + 1, expected_y, &buf),
            Some("i".to_owned())
        );
        assert_eq!(symbol(0, expected_y, &buf), Some(" ".to_owned()));
    }
}

#[test]
fn the_conversation_has_no_header_and_a_blank_column_each_side() {
    use crate::theme::Role;
    use ratatui::style::Color;
    let mut app = app(160, 40, false);
    named(&mut app, "fix the parser");
    app.on_line(session_line(
        "turn_started",
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "say hi"}]}]}),
    ));
    app.on_line(session_line(
        "assistant_message_started",
        serde_json::json!({}),
    ));
    app.on_line(session_line(
        "assistant_message_delta",
        serde_json::json!({"text": "hello world"}),
    ));
    let (buf, _) = draw(&app, 160, 40);
    let row0: String = (0..126)
        .filter_map(|x| buf.cell((x, 0)).map(|cell| cell.symbol().to_owned()))
        .collect();
    assert!(!row0.contains("fix the parser"), "row 0: {row0:?}");
    let y = (0..40)
        .find(|y| {
            let row: String = (0..126)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row");
    let tint = buf.cell((1, y)).map(|cell| cell.bg).unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset, "the reply's tint");
    assert_eq!(buf.cell((0, y)).map(|cell| cell.bg), Some(Color::Reset));
    assert_eq!(buf.cell((1, y)).map(|cell| cell.bg), Some(tint));
    assert_eq!(buf.cell((124, y)).map(|cell| cell.bg), Some(tint));
    assert_eq!(buf.cell((125, y)).map(|cell| cell.bg), Some(Color::Reset));
    // A second live session shows the rail: the card starts one column
    // after the rail's edge.
    app.on_line(session_line(
        "session_status",
        serde_json::json!({
            "name": "two", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        }),
    ));
    // The status above names the attached session; a second session needs
    // its own id. Send it directly.
    app.on_line(crate::link::Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "two", "workspace": "/w", "project": "-w", "state": "idle",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }));
    let layout = app.chrome().layout().expect("a layout with the rail");
    let rail = layout.rail.expect("a rail");
    let (buf, _) = draw(&app, 160, 40);
    let y = (0..40)
        .find(|y| {
            let row: String = (0..160)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row with the rail");
    let tint = buf
        .cell((rail.right().saturating_add(1), y))
        .map(|cell| cell.bg)
        .unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset);
    assert_eq!(
        buf.cell((rail.right(), y)).map(|cell| cell.bg),
        Some(Color::Reset)
    );
    assert_eq!(
        buf.cell((rail.right().saturating_add(1), y))
            .map(|cell| cell.bg),
        Some(tint)
    );
    // Hiding the rail leaves its grip at column 0: blank at 1, card at 2.
    let now = fakes::clock::FakeClock::new().now();
    app.on_key(crate::keys::Key::AltR, now);
    let (buf, _) = draw(&app, 160, 40);
    let y = (0..40)
        .find(|y| {
            let row: String = (0..160)
                .filter_map(|x| buf.cell((x, *y)).map(|cell| cell.symbol().to_owned()))
                .collect();
            row.contains("hello")
        })
        .expect("the reply row with the grip");
    let tint = buf.cell((2, y)).map(|cell| cell.bg).unwrap_or(Color::Reset);
    assert_ne!(tint, Color::Reset);
    assert_eq!(buf.cell((1, y)).map(|cell| cell.bg), Some(Color::Reset));
    assert_eq!(buf.cell((2, y)).map(|cell| cell.bg), Some(tint));
    let _ = Role::Muted;
}

#[test]
fn grip_needs_both_bounds() {
    // A one-row region: the mid window reaches past it on both sides.
    // `||` would draw outside while `&&` clips.
    let region = Rect::new(10, 5, 20, 1);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    super::grip(&mut buf, 10, region);
    let symbol = |y: u16, buf: &Buffer| buf.cell((10, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(5, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(4, &buf), Some(" ".to_owned()));
    assert_eq!(symbol(6, &buf), Some(" ".to_owned()));
}

#[test]
fn grip_excludes_the_bottom_edge() {
    // The window reaches the region's bottom edge, which `<` excludes
    // while `<=` would draw.
    let region = Rect::new(10, 5, 20, 2);
    let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
    super::grip(&mut buf, 10, region);
    let symbol = |y: u16, buf: &Buffer| buf.cell((10, y)).map(|cell| cell.symbol().to_owned());
    assert_eq!(symbol(5, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(6, &buf), Some("⋮".to_owned()));
    assert_eq!(symbol(7, &buf), Some(" ".to_owned()));
}

#[test]
fn the_rail_and_panel_regions_take_the_panel_tint() {
    use crate::theme::Role;
    use ratatui::style::Color;
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
        panel_cards: vec!["session".to_owned()],
        ..Default::default()
    });
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.set_size(160, 40);
    for session in [SESSION, "s_bbbbbbbbbbbbbbbb"] {
        app.on_line(crate::link::Line::Session(contract::Envelope {
            kind: "session_status".to_owned(),
            session_id: contract::SessionId(session.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::json!({
                "name": "fix the parser", "workspace": "/w",
                "project": "-w", "state": "idle", "since": 0,
                "spend": {"tokens": {"input": 1, "cache_read": 0,
                    "cache_write": {}, "output": 2},
                    "cost": 0.0, "subscription_cost": 0.0},
                "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        }));
    }
    let (buf, _) = draw(&app, 160, 40);
    let layout = app.chrome().layout().expect("a layout");
    let panel = layout.panel.expect("a panel");
    let rail = layout.rail.expect("a rail");
    let panel_tint = Role::Panel.color();
    for y in panel.top()..panel.bottom() {
        assert_eq!(buf[(panel.x, y)].bg, panel_tint, "panel edge row {y}");
    }
    for x in panel.left()..panel.right() {
        assert_eq!(buf[(x, panel.y)].bg, panel_tint, "panel top column {x}");
    }
    for y in panel.top()..panel.bottom() {
        for x in panel.left()..panel.right() {
            assert_ne!(buf[(x, y)].bg, Color::Reset, "panel cell ({x}, {y})");
        }
    }
    let edge = rail.right().saturating_sub(1);
    for y in rail.top()..rail.bottom() {
        assert_eq!(buf[(edge, y)].bg, panel_tint, "rail edge row {y}");
    }
    for x in rail.left()..rail.right() {
        assert_eq!(
            buf[(x, rail.bottom().saturating_sub(1))].bg,
            panel_tint,
            "rail bottom column {x}"
        );
    }
    for y in rail.top()..rail.bottom() {
        for x in rail.left()..rail.right() {
            assert_ne!(buf[(x, y)].bg, Color::Reset, "rail cell ({x}, {y})");
        }
    }
    // A card row of the panel still carries the surface tint.
    let surface = Role::Surface.color();
    let card = (panel.y..panel.bottom())
        .find(|y| buf[(panel.x + 1, *y)].bg == surface)
        .expect("a card row");
    assert!(card > panel.y);
}
