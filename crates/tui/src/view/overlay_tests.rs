//! Tests for the overlay frame: edges, padding, titles, bars, widths,
//! placement and clipping.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};
use ratatui::text::Span;

use super::{Place, Row, bar, choices, draw, height, hint, inner, legend, width};
use crate::mouse::{Target, TargetId};
use crate::theme::Role;

const SURFACE: Color = Color::Indexed(25);
const ACCENT: Color = Color::Indexed(2);
const BLACK: Color = Color::Rgb(0, 0, 0);

/// An overlay with a title, two body rows and a footer.
fn overlay() -> super::Overlay {
    super::Overlay {
        title: Some(("Title".to_owned(), Some(Span::raw("✕")))),
        close: None,
        body: vec![
            Row {
                spans: vec![Span::raw("first")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
            Row {
                spans: vec![Span::raw("second")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
        ],
        footer: Some(hint("foot")),
        prefer: 71,
    }
}

/// Draws `overlay` in `area`, returning the buffer, the slab and the targets.
fn drawn(overlay: &super::Overlay, area: Rect, place: Place) -> (Buffer, Rect, Vec<Target>) {
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let slab = draw(&mut buf, area, overlay, place, &mut targets);
    (buf, slab, targets)
}

/// The symbols of `buf` as rows.
fn rows(buf: &Buffer) -> Vec<String> {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn edges_pad_title_body_and_footer() {
    let area = Rect::new(0, 0, 80, 24);
    let (buf, slab, _) = drawn(&overlay(), area, Place::Centre);
    let shown = rows(&buf);
    // The slab is the content plus four columns: "second" is 6, the
    // footer 4, the title 5 with its ✕, so 40, centred at column 20.
    assert_eq!(slab, Rect::new(20, 7, 40, 10));
    // The ▄ edge above and the ▀ edge below, in `surface` with the
    // background untouched.
    for x in 20u16..60 {
        assert_eq!(shown[7].chars().nth(x.into()).unwrap(), '▄', "col {x}");
        assert_eq!(buf[(x, 7)].fg, SURFACE);
        assert_eq!(buf[(x, 7)].bg, Color::Reset);
        assert_eq!(shown[16].chars().nth(x.into()).unwrap(), '▀', "col {x}");
        assert_eq!(buf[(x, 16)].fg, SURFACE);
        assert_eq!(buf[(x, 16)].bg, Color::Reset);
    }
    // One blank row inside each edge, tinted and empty.
    for y in [8u16, 15u16] {
        for x in 20u16..60 {
            assert_eq!(
                shown[usize::from(y)].chars().nth(x.into()).unwrap(),
                ' ',
                "row {y} col {x}"
            );
            assert_eq!(buf[(x, y)].bg, SURFACE);
        }
    }
    // The title, one blank row, the body, one blank row, the footer.
    assert!(shown[9].contains("Title"), "{}", shown[9]);
    assert!(shown[11].contains("first"), "{}", shown[11]);
    assert!(shown[12].contains("second"), "{}", shown[12]);
    assert!(shown[14].contains("foot"), "{}", shown[14]);
    assert!(shown[10].trim().is_empty(), "{}", shown[10]);
    assert!(shown[13].trim().is_empty(), "{}", shown[13]);
    assert_eq!(height(&overlay()), 10);
}

#[test]
fn text_rows_open_and_close_with_two_blank_columns_and_no_stripe() {
    let area = Rect::new(0, 0, 80, 24);
    let (buf, slab, _) = drawn(&overlay(), area, Place::Centre);
    let shown = rows(&buf);
    for y in [9, 11, 12, 14] {
        let row = &shown[y];
        let cells: Vec<char> = row.chars().collect();
        assert_eq!(cells[20], ' ', "row {y}");
        assert_eq!(cells[21], ' ', "row {y}");
        assert_eq!(cells[58], ' ', "row {y}");
        assert_eq!(cells[59], ' ', "row {y}");
        assert_eq!(slab, Rect::new(20, 7, 40, 10));
    }
    for (y, row) in shown.iter().enumerate() {
        assert!(!row.contains('▌'), "row {y}: {row}");
    }
    // Outside the slab the screen behind reads: no tint, symbols kept.
    assert_eq!(buf[(0, 9)].bg, Color::Reset);
    assert_eq!(buf[(79, 9)].bg, Color::Reset);
}

#[test]
fn the_title_is_bold_accent_with_its_end_dim_at_the_right() {
    let area = Rect::new(0, 0, 80, 24);
    let (buf, slab, _) = drawn(&overlay(), area, Place::Centre);
    let title = &buf[(slab.x + 2, slab.y + 2)];
    assert_eq!(title.fg, Role::Accent.color());
    assert!(title.modifier.contains(Modifier::BOLD));
    // The ✕ ends the content's right end, dim.
    let cross = &buf[(slab.right() - 3, slab.y + 2)];
    assert_eq!(cross.symbol(), "✕");
    assert!(cross.modifier.contains(Modifier::DIM));
}

#[test]
fn the_barred_row_is_black_on_accent_with_its_gutter() {
    let picked = super::Overlay {
        title: None,
        close: None,
        body: choices(
            &[("Enter", "leave them running"), ("c", "close all")],
            0,
            36,
        ),
        footer: None,
        prefer: 71,
    };
    let area = Rect::new(0, 0, 80, 24);
    let (buf, slab, _) = drawn(&picked, area, Place::Centre);
    // Every cell of the overlay's width carries the bar's tint on the
    // focused row, and the surface tint on the other.
    let wide = usize::from(slab.width);
    for dx in 0..wide {
        let x = slab.x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
        assert_eq!(buf[(x, slab.y + 2)].bg, ACCENT, "bar col {dx}");
        assert_eq!(buf[(x, slab.y + 3)].bg, SURFACE, "plain col {dx}");
    }
    // The focused row's text is black and bold, with "› " in its gutter
    // past the frame's two blank columns.
    let gutter: String = (0..2)
        .map(|dx| buf[(slab.x + 2 + dx, slab.y + 2)].symbol().to_owned())
        .collect();
    assert_eq!(gutter, "› ");
    for dx in 0..wide {
        let x = slab.x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
        let cell = &buf[(x, slab.y + 2)];
        if cell.symbol() != " " {
            assert_eq!(cell.fg, BLACK, "col {dx}");
            assert!(cell.modifier.contains(Modifier::BOLD), "col {dx}");
        }
    }
    // Exactly one barred row: the focused choice.
    let barred = picked.body.iter().filter(|row| row.barred).count();
    assert_eq!(barred, 1);
}

#[test]
fn the_legend_is_keys_bold_labels_dim() {
    let foot = legend(&[("↑↓", "move"), ("Esc", "closes")]);
    let area = Rect::new(0, 0, 80, 24);
    let framed = super::Overlay {
        title: None,
        close: None,
        body: Vec::new(),
        footer: Some(foot),
        prefer: 71,
    };
    let (buf, slab, _) = drawn(&framed, area, Place::Centre);
    let row = slab.y + 2;
    let text: String = (slab.x..slab.right())
        .map(|x| buf[(x, row)].symbol().to_owned())
        .collect();
    assert!(text.contains("↑↓ move · Esc closes"), "{text}");
    let key = &buf[(slab.x + 2, row)];
    assert_eq!(key.symbol(), "↑");
    assert!(key.modifier.contains(Modifier::BOLD));
    assert!(!key.modifier.contains(Modifier::DIM));
    let label_at = text.find("move").unwrap_or_else(|| panic!("{text}")) + usize::from(slab.x);
    let label = &buf[(u16::try_from(label_at).unwrap_or(u16::MAX), row)];
    assert!(label.modifier.contains(Modifier::DIM));
    assert!(!label.modifier.contains(Modifier::BOLD));
}

#[test]
fn width_boundaries() {
    assert_eq!(width(35, 71, 80), 40);
    assert_eq!(width(36, 71, 80), 40);
    assert_eq!(width(146, 200, 200), 150);
    assert_eq!(width(147, 200, 200), 150);
    // Capped at the screen, and the preference shrinks on a narrow one.
    assert_eq!(width(100, 96, 50), 50);
    assert_eq!(width(52, 71, 60), 56);
    assert_eq!(inner(0), 0);
    assert_eq!(inner(4), 0);
    assert_eq!(inner(5), 1);
}

#[test]
fn centring_at_odd_and_even_margins() {
    let framed = super::Overlay {
        title: None,
        close: None,
        body: vec![Row {
            spans: vec![Span::raw("x".repeat(36))],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }],
        footer: None,
        prefer: 71,
    };
    // Content 36 gives width 40.
    let (_, even, _) = drawn(&framed, Rect::new(0, 0, 80, 24), Place::Centre);
    assert_eq!(even, Rect::new(20, 9, 40, 5));
    let (_, odd, _) = drawn(&framed, Rect::new(0, 0, 81, 24), Place::Centre);
    assert_eq!(odd.x, 20);
    assert_eq!(odd.width, 40);
}

#[test]
fn dock_is_full_width_at_the_bottom() {
    let area = Rect::new(0, 0, 80, 24);
    let (buf, slab, _) = drawn(&overlay(), area, Place::Dock);
    assert_eq!(slab, Rect::new(0, 14, 80, 10));
    assert_eq!(buf.area, area);
    let shown = rows(&buf);
    assert!(shown[23].chars().all(|ch| ch == '▀'), "{}", shown[23]);
}

#[test]
fn targets_shift_by_the_frame_and_clip_to_the_area() {
    let framed = super::Overlay {
        title: Some(("Title".to_owned(), None)),
        close: Some(TargetId::CloseOverlay),
        body: vec![Row {
            spans: vec![Span::raw("first")],
            right: Vec::new(),
            targets: vec![(0, 5, TargetId::Badge), (30, 90, TargetId::NewBelow)],
            barred: false,
        }],
        footer: None,
        prefer: 71,
    };
    let area = Rect::new(10, 5, 80, 24);
    let (_, slab, targets) = drawn(&framed, area, Place::Centre);
    // The ✕ target sits on the title row's right end.
    assert!(
        targets
            .iter()
            .any(|target| target.id == TargetId::CloseOverlay
                && target.rect == Rect::new(slab.right() - 3, slab.y + 2, 1, 1)),
        "{targets:?}"
    );
    // The row target shifts past the frame's two blank columns.
    assert!(
        targets.iter().any(|target| target.id == TargetId::Badge
            && target.rect == Rect::new(slab.x + 2, slab.y + 4, 5, 1)),
        "{targets:?}"
    );
    // Past the content's right end it clips to the inner width.
    let clipped = targets
        .iter()
        .find(|target| target.id == TargetId::NewBelow)
        .expect("the clipped target");
    assert_eq!(clipped.rect.right(), slab.right() - 2, "{targets:?}");
    assert!(clipped.rect.width > 0, "{targets:?}");
    for target in &targets {
        assert!(
            slab.contains(Position::new(target.rect.x, target.rect.y)),
            "{target:?}"
        );
        assert!(
            area.contains(Position::new(target.rect.x, target.rect.y)),
            "{target:?}"
        );
    }
}

/// A buffer full of text, as a conversation behind an overlay.
fn full_of_text(area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_symbol("x");
        }
    }
    buf
}

#[test]
fn pad_and_gap_rows_blank_the_screen_behind() {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = full_of_text(area);
    let mut targets = Vec::new();
    let slab = draw(&mut buf, area, &overlay(), Place::Centre, &mut targets);
    assert_eq!(slab, Rect::new(20, 7, 40, 10));
    let shown = rows(&buf);
    // The pad rows inside the edges and the gaps between sections
    // blank the conversation behind them instead of tinting its text.
    for y in [8u16, 10, 13, 15] {
        for x in 20u16..60 {
            assert_eq!(
                shown[usize::from(y)].chars().nth(x.into()).unwrap(),
                ' ',
                "row {y} col {x}"
            );
        }
    }
    // The content rows still draw their text.
    assert!(shown[9].contains("Title"), "{}", shown[9]);
    assert!(shown[11].contains("first"), "{}", shown[11]);
}

#[test]
fn short_areas_write_no_edge_outside_the_area() {
    for height in [1u16, 2] {
        let area = Rect::new(0, 5, 80, height);
        let mut buf = full_of_text(Rect::new(0, 0, 80, 24));
        let mut targets = Vec::new();
        let slab = draw(&mut buf, area, &overlay(), Place::Centre, &mut targets);
        assert_eq!(slab.height, height, "height {height}");
        // Above and below the area the buffer keeps its text: no ▄
        // or ▀ edge spills out of a trimmed stack.
        for y in [4u16, 7] {
            for x in 0u16..80 {
                assert_eq!(buf[(x, y)].symbol(), "x", "height {height} row {y} col {x}");
            }
        }
        // The area itself keeps the title.
        let shown: String = (0..80).map(|x| buf[(x, 5)].symbol().to_owned()).collect();
        assert!(shown.contains("Title"), "height {height}: {shown}");
    }
}

#[test]
fn an_empty_area_draws_nothing() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    let mut targets = Vec::new();
    let slab = draw(
        &mut buf,
        Rect::new(5, 5, 0, 0),
        &overlay(),
        Place::Centre,
        &mut targets,
    );
    assert!(slab.is_empty());
    assert!(targets.is_empty());
}

#[test]
fn short_areas_give_way_in_order() {
    let framed = || super::Overlay {
        title: Some(("Title".to_owned(), None)),
        close: None,
        body: vec![
            Row {
                spans: vec![Span::raw("first")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
            Row {
                spans: vec![Span::raw("second")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
        ],
        footer: None,
        prefer: 71,
    };
    let shown_at = |height: u16| {
        let area = Rect::new(0, 0, 80, height);
        let (buf, slab, _) = drawn(&framed(), area, Place::Centre);
        (rows(&buf), slab)
    };
    // At 1 a single row, the title, with no edges.
    let (shown, _) = shown_at(1);
    assert_eq!(shown.len(), 1);
    assert!(shown[0].contains("Title"), "{shown:?}");
    assert!(
        !shown[0].contains('▄') && !shown[0].contains('▀'),
        "{shown:?}"
    );
    // At 2 that row and the ▀ edge.
    let (shown, _) = shown_at(2);
    assert!(shown[0].contains("Title"), "{shown:?}");
    assert!(shown[1].trim().chars().all(|ch| ch == '▀'), "{shown:?}");
    // At 3 both edges.
    let (shown, _) = shown_at(3);
    assert!(shown[0].trim().chars().all(|ch| ch == '▄'), "{shown:?}");
    assert!(shown[1].contains("Title"), "{shown:?}");
    assert!(shown[2].trim().chars().all(|ch| ch == '▀'), "{shown:?}");
    // At 4 the top pad row too, at 5 both pad rows.
    let (shown, _) = shown_at(4);
    assert!(shown[1].trim().is_empty(), "{shown:?}");
    assert!(shown[2].contains("Title"), "{shown:?}");
    let (shown, _) = shown_at(5);
    assert!(
        shown[1].trim().is_empty() && shown[3].trim().is_empty(),
        "{shown:?}"
    );
    assert!(shown[2].contains("Title"), "{shown:?}");
    // At 6 the title gap.
    let (shown, _) = shown_at(6);
    assert!(shown[2].contains("Title"), "{shown:?}");
    assert!(shown[3].trim().is_empty(), "{shown:?}");
}

#[test]
fn a_short_body_only_area_keeps_its_first_row() {
    let framed = super::Overlay {
        title: None,
        close: None,
        body: vec![
            Row {
                spans: vec![Span::raw("first")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
            Row {
                spans: vec![Span::raw("second")],
                right: Vec::new(),
                targets: Vec::new(),
                barred: false,
            },
        ],
        footer: None,
        prefer: 71,
    };
    let area = Rect::new(0, 0, 80, 1);
    let (buf, _, _) = drawn(&framed, area, Place::Centre);
    assert!(rows(&buf)[0].contains("first"), "{:?}", rows(&buf));
    let area = Rect::new(0, 0, 80, 6);
    let (buf, _, _) = drawn(&framed, area, Place::Centre);
    let shown = rows(&buf).join("\n");
    assert!(shown.contains("second"), "{shown}");
}

#[test]
fn choices_keep_the_key_column_and_hang_wrapped_text() {
    let made = choices(
        &[("Enter", "leave them running"), ("c", "close all")],
        1,
        40,
    );
    assert_eq!(made.len(), 2);
    assert!(!made[0].barred);
    assert!(made[1].barred);
    let first: String = made[0]
        .spans
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    let second: String = made[1]
        .spans
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(first, "  Enter  leave them running");
    assert_eq!(second, "› c      close all");
    // A long description wraps under its own start.
    let wrapped = choices(&[("c", "close all the sessions now please")], 0, 20);
    assert_eq!(wrapped.len(), 3);
    let hang: String = wrapped[1]
        .spans
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert!(hang.starts_with("  "), "{hang}");
    assert!(hang.contains("sessions"), "{hang}");
    assert!(wrapped[1].barred);
}

#[test]
fn bar_marks_the_focused_row() {
    let row = Row {
        spans: vec![Span::raw("enter")],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    };
    assert!(bar(row).barred);
}

#[test]
fn the_right_end_keeps_priority_when_cut() {
    let framed = super::Overlay {
        title: None,
        close: None,
        body: vec![Row {
            spans: vec![Span::raw("a very long left text that overflows")],
            right: vec![Span::raw("tag")],
            targets: Vec::new(),
            barred: false,
        }],
        footer: None,
        prefer: 71,
    };
    let area = Rect::new(0, 0, 40, 24);
    let (buf, slab, _) = drawn(&framed, area, Place::Centre);
    assert_eq!(slab.width, 40);
    let row = slab.y + 2;
    let text: String = (slab.x..slab.right())
        .map(|x| buf[(x, row)].symbol().to_owned())
        .collect();
    assert!(text.ends_with("tag  "), "{text}");
}

/// A body row of one plain span.
fn one_row() -> Row {
    Row {
        spans: vec![Span::raw("row")],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

#[test]
fn height_counts_the_title_its_gap_and_the_footer() {
    let overlay = |title: bool, body: usize, footer: bool| super::Overlay {
        title: title.then(|| ("Title".to_owned(), None)),
        close: None,
        body: (0..body).map(|_| one_row()).collect(),
        footer: footer.then(|| hint("foot")),
        prefer: 71,
    };
    assert_eq!(height(&overlay(false, 0, false)), 4);
    // The title is a row; its gap comes only with a body or footer.
    assert_eq!(height(&overlay(true, 0, false)), 5);
    assert_eq!(height(&overlay(false, 1, false)), 5);
    assert_eq!(height(&overlay(true, 1, false)), 7);
    assert_eq!(height(&overlay(false, 0, true)), 5);
    assert_eq!(height(&overlay(true, 0, true)), 7);
    assert_eq!(height(&overlay(false, 1, true)), 7);
}

#[test]
fn choices_wrap_at_the_width_left_after_the_key_column() {
    // A four-wide key column and two columns of gutter put the
    // description eight columns in: an inner width of 18 leaves ten.
    let made = choices(&[("abcd", "one two three four")], 0, 18);
    assert_eq!(made.len(), 2);
    let text: String = made[1]
        .spans
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    assert_eq!(text, format!("{}three four", " ".repeat(8)));
}

#[test]
fn across_and_under_centre_the_slab_across_the_width() {
    let framed = super::Overlay {
        title: None,
        close: None,
        body: vec![Row {
            spans: vec![Span::raw("x".repeat(36))],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }],
        footer: None,
        prefer: 71,
    };
    // Content 36 gives width 40: a margin of 40 splits to 20 a side.
    let area = Rect::new(0, 0, 80, 24);
    let (_, across, _) = drawn(&framed, area, Place::Across { bottom: 20 });
    assert_eq!(across.x, 20);
    let (_, under, _) = drawn(&framed, area, Place::Under { top: 5 });
    assert_eq!(under.x, 20);
}

#[test]
fn a_lone_bottom_edge_draws_no_tint() {
    // With no title, body or footer, a two-row area keeps its two edge
    // slots and no text row between them: nothing is tinted.
    let bare = super::Overlay {
        title: None,
        close: None,
        body: Vec::new(),
        footer: None,
        prefer: 71,
    };
    let (buf, _, _) = drawn(&bare, Rect::new(0, 0, 40, 2), Place::Dock);
    let shown = rows(&buf);
    assert!(
        shown.iter().all(|row| row.chars().all(|ch| ch == ' ')),
        "{shown:?}"
    );
}
