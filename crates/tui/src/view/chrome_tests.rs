//! Snapshots of the session screen's chrome: the floor line, the header
//! and the panel's region.

use super::super::{cursor, render, text};
use crate::app::App;
use crate::home::Launch;
use crate::layout::Layout;
use crate::link::Line;
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
        Some(Position::new(2, 39))
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
fn header_with_a_long_name_is_cut() {
    let mut app = app(160, 12, false);
    let long: String = std::iter::repeat_n('p', 300).collect();
    named(&mut app, &long);
    let (buf, _) = draw(&app, 160, 12);
    let header: String = (0..160)
        .filter_map(|x| buf.cell((x, 0)).map(|cell| cell.symbol().to_owned()))
        .collect();
    assert_eq!(header.trim_end().chars().count(), 126);
    insta::assert_snapshot!("header_with_a_long_name_is_cut", text(&buf));
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
fn header_draws_only_when_column_has_rows() {
    let mut app = app(80, 10, false);
    named(&mut app, "fix the parser");
    let first: String = app.header().chars().take(1).collect();
    assert!(!first.is_empty());
    let area = Rect::new(0, 0, 30, 10);
    let one = Layout {
        rail: None,
        grip: None,
        column: Rect::new(5, 3, 20, 1),
        panel: None,
        narrow: false,
    };
    let mut buf = Buffer::empty(area);
    super::draw(&app, &one, &mut buf);
    assert_eq!(
        buf.cell((5, 3)).map(|cell| cell.symbol().to_owned()),
        Some(first)
    );
    let empty = Layout {
        rail: None,
        grip: None,
        column: Rect::new(5, 3, 20, 0),
        panel: None,
        narrow: false,
    };
    let mut buf = Buffer::empty(area);
    super::draw(&app, &empty, &mut buf);
    assert_eq!(
        buf.cell((5, 3)).map(|cell| cell.symbol().to_owned()),
        Some(" ".to_owned())
    );
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
