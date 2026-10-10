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
    assert_eq!(buf[(0, 10)].symbol(), "▌");
    assert_eq!(buf[(0, 10)].fg, Role::Accent.color());
    assert_eq!(buf[(2, 10)].symbol(), ">");
    assert_eq!(buf[(4, 10)].symbol(), "h");
    assert_eq!(buf[(5, 10)].symbol(), "i");
}

#[test]
fn the_cursor_sits_in_the_same_column_as_without_edges() {
    // No edges fit a body of 2; they draw in a body of 11. The column
    // counts the stripe and gap in both.
    let app = typed(60, 12, "hi");
    let (_, edged) = super::cursor_row(&app, 60, 11);
    let (_, plain) = super::cursor_row(&app, 60, 2);
    assert_eq!((edged, plain), (6, 6));
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
    assert_eq!(super::cursor_row(&app, 60, 2), (1, 6));
    assert_eq!(super::cursor_row(&app, 60, 5), (2, 6));
}

#[test]
fn the_box_rows_start_past_the_stripe_and_gap() {
    // The draft wraps past the stripe and gap: `▌`, a space, `> `, then
    // the draft (`docs/tui.md`, "Look", "The input box").
    let app = typed(60, 12, "hi");
    let area = Rect::new(0, 0, 60, 12);
    let mut buf = Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    assert_eq!(buf[(0, 10)].symbol(), "▌");
    assert_eq!(buf[(0, 10)].fg, Role::Accent.color());
    assert_eq!(buf[(0, 10)].bg, Role::Surface.color());
    assert_eq!(buf[(1, 10)].symbol(), " ");
    assert_eq!(buf[(1, 10)].bg, Role::Surface.color());
    assert_eq!(buf[(2, 10)].symbol(), ">");
    let row: String = (0..6).map(|x| buf[(x, 10)].symbol().to_owned()).collect();
    assert_eq!(row, "▌ > hi");
}

#[test]
fn the_box_follows_its_area_x() {
    // Beside a rail the box starts at the area's x: stripe, gap and text
    // shift together, and no cell outside the area changes.
    let app = typed(40, 6, "hi");
    let area = Rect::new(5, 0, 40, 6);
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 6));
    let mut bottom = 6;
    draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
    assert_eq!(buf[(5, 4)].symbol(), "▌");
    assert_eq!(buf[(7, 4)].symbol(), ">");
    for x in 5..45 {
        assert_eq!(buf[(x, 3)].symbol(), "▄", "top edge at {x}");
        assert_eq!(buf[(x, 5)].symbol(), "▀", "bottom edge at {x}");
    }
    for y in 0..6 {
        for x in (0..5).chain(45..50) {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn a_box_three_columns_wide_has_its_stripe_and_two_has_none() {
    // Below three columns there is no stripe and the text keeps the
    // width (`docs/tui.md`, "Look").
    for (width, striped, prompt_x) in [(3, true, 2), (2, false, 0)] {
        let app = typed(width, 8, "h");
        let area = Rect::new(0, 0, width, 8);
        let mut buf = Buffer::empty(area);
        let mut bottom = 8;
        draw(&app, area, &mut bottom, &mut buf, &mut Vec::new());
        let stripes = buf
            .content
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .count();
        assert_eq!(stripes > 0, striped, "width {width}");
        let prompt = buf
            .content
            .iter()
            .position(|cell| cell.symbol() == ">")
            .map(|index| index % usize::from(width));
        assert_eq!(prompt, Some(prompt_x), "width {width}");
    }
}

#[test]
fn a_token_target_starts_past_the_stripe() {
    use crate::keys::Edit;
    use crate::mouse::TargetId;

    let mut app = typed(40, 6, "see ");
    let pasted: Vec<String> = (1..=12).map(|n| format!("line {n}")).collect();
    app.on_edit(Edit::Paste(pasted.join("\n")));
    let area = Rect::new(5, 0, 40, 6);
    let mut buf = Buffer::empty(Rect::new(0, 0, 50, 6));
    let mut bottom = 6;
    let mut targets = Vec::new();
    draw(&app, area, &mut bottom, &mut buf, &mut targets);
    // "see " is four columns past the `> ` prompt, itself past the
    // stripe and gap.
    let token = targets
        .iter()
        .find(|target| matches!(target.id, TargetId::Token(1)))
        .expect("a token target");
    assert_eq!(token.rect.x, 5 + 2 + 6);
    assert_eq!(token.rect.y, 4);
    assert!(token.rect.right() <= 5 + 40, "{:?}", token.rect);
}

#[test]
fn the_cursor_column_counts_the_stripe_and_gap() {
    let app = typed(60, 12, "hi");
    assert_eq!(super::cursor_row(&app, 60, 11), (2, 6));
    let narrow = typed(2, 12, "hi");
    assert_eq!(super::cursor_row(&narrow, 2, 11), (2, 2));
}
