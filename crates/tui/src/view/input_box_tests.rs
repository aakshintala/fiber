//! Tests for the input box's surface: its tint, its edges, its cursor and
//! short screens (`docs/tui.md`, "Look").

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::draw;
use crate::app::App;
use crate::keys::Key;
use crate::theme::Role;
use contract::clock::Clock;

/// An attached app `width` by `height` with `draft` typed.
fn typed(width: u16, height: u16, draft: &str) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(width, height);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let now = fakes::clock::FakeClock::new().now();
    for ch in draft.chars() {
        app.on_key(Key::Char(ch), now);
    }
    app
}

/// Renders `app` whole on a `width` by `height` screen as text.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

#[test]
fn input_box_on_its_surface_80x12() {
    insta::assert_snapshot!(
        "input_box_on_its_surface_80x12",
        screen(&typed(80, 12, "hi"), 80, 12)
    );
}

#[test]
fn input_box_with_completions_above_its_edge() {
    insta::assert_snapshot!(
        "input_box_with_completions_above_its_edge",
        screen(&typed(80, 12, "/h"), 80, 12)
    );
}

#[test]
fn every_cell_of_the_box_rows_is_surface() {
    let app = typed(80, 12, "hi");
    let area = Rect::new(0, 0, 80, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    // One draft row in a room of 11: the box is three rows at the bottom.
    let edge = Role::Surface.color();
    for x in 0..80 {
        assert_eq!(buf[(x, 9)].fg, edge, "top edge at {x}");
        assert_eq!(buf[(x, 11)].fg, edge, "bottom edge at {x}");
        assert_eq!(buf[(x, 10)].bg, edge, "text row at {x}");
    }
    assert_eq!(buf[(0, 10)].symbol(), ">");
    assert_eq!(buf[(2, 10)].symbol(), "h");
    assert_eq!(buf[(3, 10)].symbol(), "i");
}

#[test]
fn the_cursor_sits_in_the_same_column_as_without_edges() {
    // No edges fit a body of 2; they draw in a body of 11. The column is
    // the draft's own in both.
    let app = typed(60, 12, "hi");
    let (_, edged) = super::cursor_row(&app, 60, 11);
    let (_, plain) = super::cursor_row(&app, 60, 2);
    assert_eq!((edged, plain), (4, 4));
}

#[test]
fn a_screen_too_short_for_the_edges_keeps_the_input_row() {
    // One draft row: the edges draw only when the row plus 2 fits.
    for (height, edges) in [(1, false), (2, false), (3, true), (5, true)] {
        let app = typed(20, height, "hi");
        let area = Rect::new(0, 0, 20, height);
        let mut buf = Buffer::empty(area);
        let mut bottom = height;
        draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
        let edge = Role::Surface.color();
        let text = (0..height).find(|y| (0..20).any(|x| buf[(x, *y)].symbol() == "h"));
        assert_eq!(text, Some(height.saturating_sub(if edges { 2 } else { 1 })));
        let edge_rows = if edges {
            vec![height.saturating_sub(3), height.saturating_sub(1)]
        } else {
            Vec::new()
        };
        for y in 0..height {
            for x in 0..20 {
                assert_eq!(
                    buf[(x, y)].fg == edge,
                    edge_rows.contains(&y),
                    "height {height}, column {x}, row {y}"
                );
            }
        }
    }
}

#[test]
fn the_cursor_counts_the_bottom_edge_only_where_it_draws() {
    let app = typed(60, 12, "hi");
    // One draft row: no edges fit a body of 2, two fit a body of 5.
    assert_eq!(super::cursor_row(&app, 60, 2), (1, 4));
    assert_eq!(super::cursor_row(&app, 60, 5), (2, 4));
}
