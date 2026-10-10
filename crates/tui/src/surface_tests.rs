//! Tests for surfaces: where stripes draw, the inset and edge counts, and
//! the edge and stripe cells (`docs/tui.md`, "Look").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{Edges, Stripe, draw_edges, draw_slab, edge_row, edged, inset, stripe, stripes};
use crate::theme::Role;

/// Stripe detection over `vars`.
fn detected(vars: &[(&str, &str)]) -> bool {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    stripes(&|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    })
}

#[test]
fn stripe_detection() {
    let on: &[&[(&str, &str)]] = &[
        &[("TERM_PROGRAM", "ghostty")],
        &[("TERM_PROGRAM", "WezTerm")],
        &[("TERM", "xterm-ghostty")],
        &[("TERM", "xterm-kitty")],
    ];
    for vars in on {
        assert!(detected(vars), "{vars:?}");
    }
    let off: &[&[(&str, &str)]] = &[
        &[],
        &[("TERM_PROGRAM", "Apple_Terminal")],
        &[("TERM_PROGRAM", "iTerm.app")],
        &[("TERM", "xterm-256color")],
        // Inside a multiplexer, one row per arm, even on a terminal that
        // draws stripes.
        &[
            ("TMUX", "/tmp/tmux-501/default,1,0"),
            ("TERM", "xterm-ghostty"),
        ],
        &[("STY", "1.pts"), ("TERM_PROGRAM", "ghostty")],
        &[("TERM", "tmux-256color"), ("TERM_PROGRAM", "WezTerm")],
        &[("TERM", "screen-256color"), ("TERM_PROGRAM", "WezTerm")],
    ];
    for vars in off {
        assert!(!detected(vars), "{vars:?}");
    }
}

#[test]
fn stripe_is_a_tinted_blank_when_off() {
    let blank = stripe(false, Role::Accent, Role::Surface, false);
    assert_eq!(blank.content, " ");
    assert_eq!(blank.style.bg, Some(Role::Surface.color()));
    for (right, glyph) in [(false, "▌"), (true, "▐")] {
        let cell = stripe(true, Role::Accent, Role::Surface, right);
        assert_eq!(cell.content, glyph);
        assert_eq!(cell.style.fg, Some(Role::Accent.color()));
        assert_eq!(cell.style.bg, Some(Role::Surface.color()));
    }
}

#[test]
fn edge_rows_use_the_tint_as_foreground() {
    assert_eq!(edge_row(3, Role::Surface, true).width(), 3);
    for cell in &edge_row(3, Role::Surface, true).spans {
        assert_eq!(cell.content, "▄▄▄");
        assert_eq!(cell.style.fg, Some(Role::Surface.color()));
    }
    for cell in &edge_row(2, Role::Approval, false).spans {
        assert_eq!(cell.content, "▀▀");
        assert_eq!(cell.style.fg, Some(Role::Approval.color()));
    }
    let painted = edge_row(0, Role::Surface, true);
    assert_eq!(painted.width(), 0);
}

#[test]
fn inset_by_width() {
    for (width, want) in [(0, 0), (1, 1), (2, 2), (3, 1), (4, 2), (5, 3)] {
        assert_eq!(inset(width), want, "width {width}");
    }
}

#[test]
fn edged_needs_two_spare_rows() {
    for (room, want) in [(3, 3), (4, 3), (5, 5), (7, 5)] {
        assert_eq!(edged(3, room), want, "room {room}");
    }
    assert_eq!(edged(0, 1), 0);
    assert_eq!(edged(0, 2), 2);
}

#[test]
fn draw_edges_stays_inside_the_buffer() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
    // At the first row only the edge below draws; at the last row only the
    // edge above.
    draw_edges(&mut buf, Rect::new(0, 0, 6, 1), Role::Surface);
    draw_edges(&mut buf, Rect::new(0, 2, 6, 1), Role::Surface);
    assert_eq!(buf[(0, 0)].symbol(), " ");
    assert_eq!(buf[(0, 1)].symbol(), "▄");
    assert_eq!(buf[(0, 2)].symbol(), " ");
    // A rect filling the buffer draws no edge.
    let mut full = Buffer::empty(Rect::new(0, 0, 4, 2));
    draw_edges(&mut full, Rect::new(0, 0, 4, 2), Role::Surface);
    for cell in &full.content {
        assert_eq!(cell.symbol(), " ");
    }
}

#[test]
fn edges_and_stripes_follow_the_theme() {
    // The helpers mark cells with roles; the paint pass resolves them, so
    // edges and stripes follow the theme without a repaint.
    let vars = [("COLORTERM", "truecolor")];
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let look = crate::look::Look::new(crate::look::ThemeSetting::Dark, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    })
    .0;
    assert_eq!(look.colour(Role::Surface), Color::Rgb(0x1a, 0x1a, 0x22));
    let edge = &edge_row(1, Role::Surface, true).spans[0];
    assert_eq!(edge.style.fg, Some(Role::Surface.color()));
    let cell = stripe(true, Role::Accent, Role::Surface, false);
    assert_eq!(cell.style.fg, Some(Role::Accent.color()));
}

#[test]
fn draw_stripe_draws_nothing_without_rows_or_columns() {
    use ratatui::layout::Rect;
    // Either arm of the guard: no width with rows, no rows with width.
    for rect in [Rect::new(2, 0, 0, 3), Rect::new(2, 0, 3, 0)] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
        super::draw_stripe(&mut buf, rect, Role::Accent, Role::Surface, false);
        for cell in &buf.content {
            assert_eq!(cell.symbol(), " ");
            assert_ne!(cell.bg, Role::Surface.color());
        }
    }
}

#[test]
#[expect(
    clippy::assertions_on_constants,
    reason = "pins the shared constant's value"
)]
fn edges_both_draws_both() {
    // Both edge rows draw (`docs/tui.md`, "Look").
    assert!(super::Edges::BOTH.top);
    assert!(super::Edges::BOTH.bottom);
}

#[test]
fn init_reads_whether_stripes_draw() {
    // Without init's write the static stays on: a multiplexer must turn
    // the stripe's cell into a tinted blank.
    let multi = [
        ("TMUX", "/tmp/tmux-501/default,1,0"),
        ("TERM_PROGRAM", "ghostty"),
    ];
    super::init(&|name| {
        multi
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, value)| (*value).to_owned())
    });
    assert_eq!(
        super::stripe_cell(Role::Accent, Role::Surface, false).content,
        " "
    );
    // ... and a stripe terminal back on.
    let ghost = [("TERM_PROGRAM", "ghostty")];
    super::init(&|name| {
        ghost
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, value)| (*value).to_owned())
    });
    assert_eq!(
        super::stripe_cell(Role::Accent, Role::Surface, false).content,
        "▌"
    );
}

#[test]
fn draw_edges_keeps_the_background_under_its_edges() {
    // Characterization: the edge rows take the tint as their foreground
    // and leave the background they draw over (`docs/tui.md`, "Look").
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    buf.set_style(
        Rect::new(0, 0, 6, 4),
        ratatui::style::Style::new().bg(Role::Turn.color()),
    );
    draw_edges(&mut buf, Rect::new(0, 1, 6, 2), Role::Surface);
    for x in 0..6 {
        assert_eq!(buf[(x, 0)].symbol(), "▄", "x {x}");
        assert_eq!(buf[(x, 0)].fg, Role::Surface.color(), "x {x}");
        assert_eq!(buf[(x, 0)].bg, Role::Turn.color(), "x {x}");
        assert_eq!(buf[(x, 3)].symbol(), "▀", "x {x}");
        assert_eq!(buf[(x, 3)].fg, Role::Surface.color(), "x {x}");
        assert_eq!(buf[(x, 3)].bg, Role::Turn.color(), "x {x}");
    }
    for y in 1..3 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_eq!(buf[(x, y)].bg, Role::Turn.color(), "({x}, {y})");
        }
    }
}

#[test]
fn draw_edges_at_a_nonzero_x() {
    // Characterization: the edges span the rect's own columns only.
    let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
    draw_edges(&mut buf, Rect::new(2, 1, 3, 1), Role::Surface);
    for x in 0..8 {
        let top = if (2..5).contains(&x) { "▄" } else { " " };
        let bottom = if (2..5).contains(&x) { "▀" } else { " " };
        assert_eq!(buf[(x, 0)].symbol(), top, "x {x}");
        assert_eq!(buf[(x, 1)].symbol(), " ", "x {x}");
        assert_eq!(buf[(x, 2)].symbol(), bottom, "x {x}");
    }
}

#[test]
fn a_slab_tints_its_rect_and_draws_its_edges() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 6, 2),
        Role::Surface,
        None,
        Edges::BOTH,
    );
    for y in 1..3 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for x in 0..6 {
        assert_eq!(buf[(x, 0)].symbol(), "▄", "x {x}");
        assert_eq!(buf[(x, 0)].fg, Role::Surface.color(), "x {x}");
        assert_eq!(buf[(x, 0)].bg, ratatui::style::Color::Reset, "x {x}");
        assert_eq!(buf[(x, 3)].symbol(), "▀", "x {x}");
        assert_eq!(buf[(x, 3)].fg, Role::Surface.color(), "x {x}");
        assert_eq!(buf[(x, 3)].bg, ratatui::style::Color::Reset, "x {x}");
    }
}

#[test]
fn a_slab_edge_keeps_the_colour_around_it() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    buf.set_style(
        Rect::new(0, 0, 6, 4),
        ratatui::style::Style::new().bg(Role::Turn.color()),
    );
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 6, 2),
        Role::Surface,
        None,
        Edges::BOTH,
    );
    for (y, edge) in [(0, "▄"), (3, "▀")] {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), edge, "({x}, {y})");
            assert_eq!(buf[(x, y)].fg, Role::Surface.color(), "({x}, {y})");
            assert_eq!(buf[(x, y)].bg, Role::Turn.color(), "({x}, {y})");
        }
    }
}

#[test]
fn a_slab_draws_only_the_edges_asked_for() {
    for (edges, top, bottom) in [
        (
            Edges {
                top: true,
                bottom: false,
            },
            "▄",
            " ",
        ),
        (
            Edges {
                top: false,
                bottom: true,
            },
            " ",
            "▀",
        ),
    ] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
        draw_slab(&mut buf, Rect::new(0, 1, 6, 2), Role::Surface, None, edges);
        for x in 0..6 {
            assert_eq!(buf[(x, 0)].symbol(), top, "x {x}");
            assert_eq!(buf[(x, 3)].symbol(), bottom, "x {x}");
        }
    }
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 6, 2),
        Role::Surface,
        None,
        Edges {
            top: false,
            bottom: false,
        },
    );
    for cell in &buf.content {
        assert_eq!(cell.symbol(), " ");
    }
}

#[test]
fn a_slab_stripe_sits_on_its_side() {
    for (stripe, x, glyph) in [
        (
            Stripe {
                colour: Role::Accent,
                right: false,
            },
            2,
            "▌",
        ),
        (
            Stripe {
                colour: Role::Accent,
                right: true,
            },
            6,
            "▐",
        ),
    ] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        draw_slab(
            &mut buf,
            Rect::new(2, 1, 5, 2),
            Role::Surface,
            Some(stripe),
            Edges::BOTH,
        );
        for y in 1..3 {
            assert_eq!(buf[(x, y)].symbol(), glyph, "y {y}");
            assert_eq!(buf[(x, y)].fg, Role::Accent.color(), "y {y}");
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "y {y}");
        }
    }
}

#[test]
fn a_slab_has_no_stripe_below_three_columns() {
    // Two columns keep the width for text; three draw the stripe.
    let mut narrow = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut narrow,
        Rect::new(0, 1, 2, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for y in 1..3 {
        for x in 0..2 {
            assert_eq!(narrow[(x, y)].symbol(), " ", "({x}, {y})");
            assert_eq!(narrow[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    let mut wide = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut wide,
        Rect::new(0, 1, 3, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    assert_eq!(wide[(0, 1)].symbol(), "▌");
}

#[test]
fn a_slab_without_a_stripe_draws_none() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 6, 2),
        Role::Surface,
        None,
        Edges::BOTH,
    );
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
        assert_ne!(cell.symbol(), "▐");
    }
}

#[test]
fn a_slab_at_widths_0_to_3() {
    // Width 0 changes no cell; 1 and 2 tint and edge with no stripe; 3
    // draws the stripe at the rect's x.
    for (width, striped) in [(0, false), (1, false), (2, false), (3, true)] {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
        draw_slab(
            &mut buf,
            Rect::new(1, 1, width, 2),
            Role::Surface,
            Some(Stripe {
                colour: Role::Accent,
                right: false,
            }),
            Edges::BOTH,
        );
        for y in 0..4 {
            for x in 0..6 {
                let tinted = x >= 1 && x < 1 + width && (1..3).contains(&y);
                let edge = y == 0 || y == 3;
                if width == 0 {
                    assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
                    continue;
                }
                if tinted {
                    let want = if striped && x == 1 { "▌" } else { " " };
                    assert_eq!(buf[(x, y)].symbol(), want, "({x}, {y})");
                    assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
                } else if edge && x >= 1 && x < 1 + width {
                    assert_eq!(
                        buf[(x, y)].symbol(),
                        if y == 0 { "▄" } else { "▀" },
                        "({x}, {y})"
                    );
                } else {
                    assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
                }
            }
        }
    }
}

#[test]
fn a_slab_past_the_buffer_is_clipped() {
    // The rect's right columns sit outside the buffer: only column 5
    // tints, edges and stripes clip to it, and nothing panics.
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(5, 1, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for y in 1..3 {
        assert_eq!(buf[(5, y)].symbol(), "▌", "y {y}");
        assert_eq!(buf[(5, y)].bg, Role::Surface.color(), "y {y}");
        for x in 0..5 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    assert_eq!(buf[(5, 0)].symbol(), "▄");
    assert_eq!(buf[(5, 3)].symbol(), "▀");
    for x in 0..5 {
        assert_eq!(buf[(x, 0)].symbol(), " ");
        assert_eq!(buf[(x, 3)].symbol(), " ");
    }
    // A right stripe's column sits outside: it draws nothing.
    let mut right = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut right,
        Rect::new(5, 1, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: true,
        }),
        Edges::BOTH,
    );
    for cell in &right.content {
        assert_ne!(cell.symbol(), "▐");
    }
    assert_eq!(right[(5, 1)].bg, Role::Surface.color());
}

#[test]
fn a_slab_outside_the_buffer_draws_nothing() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(7, 0, 3, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for cell in &buf.content {
        assert_eq!(cell.symbol(), " ");
        assert_ne!(cell.bg, Role::Surface.color());
    }
}

#[test]
fn a_slab_below_the_buffer_still_draws_its_top_edge() {
    // The slab's rows sit below the buffer: nothing tints, no stripe
    // draws, and its top edge shows on the last row (`docs/tui.md`,
    // "Look", "History and paging").
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 4, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for y in 0..3 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for x in 0..6 {
        assert_eq!(buf[(x, 3)].symbol(), "▄", "x {x}");
        assert_eq!(buf[(x, 3)].fg, Role::Surface.color(), "x {x}");
        assert_ne!(buf[(x, 3)].bg, Role::Surface.color(), "x {x}");
    }
}

#[test]
fn a_slab_above_the_buffer_still_draws_its_bottom_edge() {
    let mut buf = Buffer::empty(Rect::new(0, 2, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 0, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for x in 0..6 {
        assert_eq!(buf[(x, 2)].symbol(), "▀", "x {x}");
        assert_eq!(buf[(x, 2)].fg, Role::Surface.color(), "x {x}");
    }
    for y in 3..6 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
}

#[test]
fn a_slab_overhanging_the_bottom_clips_its_stripe_and_edges() {
    // The slab's last row sits past the buffer: its tint and stripe
    // cover only row 3, its top edge shows on row 2, and its bottom
    // edge stays off screen (`docs/tui.md`, "Look").
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 3, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for x in 0..6 {
        assert_eq!(buf[(x, 2)].symbol(), "▄", "x {x}");
        assert_eq!(buf[(x, 2)].fg, Role::Surface.color(), "x {x}");
    }
    assert_eq!(buf[(0, 3)].symbol(), "▌");
    assert_eq!(buf[(0, 3)].fg, Role::Accent.color());
    for x in 0..6 {
        assert_eq!(buf[(x, 3)].bg, Role::Surface.color(), "x {x}");
    }
    for y in 0..2 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for x in 1..6 {
        assert_eq!(buf[(x, 3)].symbol(), " ", "x {x}");
    }
}

#[test]
fn a_slab_overhanging_the_top_clips_its_stripe_and_edges() {
    // Rows 0 and 1 sit above the buffer: the tint and stripe cover rows
    // 2 to 4, no top edge draws, and the bottom edge lands on the slab's
    // last shown row, over its stripe (`docs/tui.md`, "Look").
    let mut buf = Buffer::empty(Rect::new(0, 2, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 0, 6, 4),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for y in 2..4 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for y in 2..4 {
        assert_eq!(buf[(0, y)].symbol(), "▌", "y {y}");
        assert_eq!(buf[(0, y)].fg, Role::Accent.color(), "y {y}");
    }
    for x in 0..6 {
        assert_eq!(buf[(x, 4)].symbol(), "▀", "x {x}");
        assert_eq!(buf[(x, 4)].fg, Role::Surface.color(), "x {x}");
        assert_eq!(
            buf[(x, 4)].bg,
            ratatui::style::Color::Reset,
            "the edge keeps the untinted background at {x}"
        );
    }
    for x in 1..6 {
        for y in 2..4 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
        }
    }
    for y in 5..6 {
        for x in 0..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
}

#[test]
fn a_slab_overhanging_the_left_keeps_its_stripe_column() {
    // Columns 0 and 1 sit left of the buffer: the tint and edges cover
    // columns 2 to 5, and no stripe draws, since its column stays where
    // the rect puts it (`docs/tui.md`, "Look").
    let mut buf = Buffer::empty(Rect::new(2, 0, 6, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 6, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: false,
        }),
        Edges::BOTH,
    );
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
    }
    for y in 1..3 {
        for x in 2..6 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
        for x in 6..8 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for (y, edge) in [(0, "▄"), (3, "▀")] {
        for x in 2..6 {
            assert_eq!(buf[(x, y)].symbol(), edge, "({x}, {y})");
            assert_eq!(buf[(x, y)].fg, Role::Surface.color(), "({x}, {y})");
        }
        for x in 6..8 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn a_slab_overhanging_both_sides_draws_a_right_stripe_inside() {
    // The buffer starts at column 2: the left stripe's column sits
    // outside and draws nothing, while the right stripe's sits inside
    // and draws (`docs/tui.md`, "Look").
    let mut buf = Buffer::empty(Rect::new(2, 0, 8, 4));
    draw_slab(
        &mut buf,
        Rect::new(0, 1, 8, 2),
        Role::Surface,
        Some(Stripe {
            colour: Role::Accent,
            right: true,
        }),
        Edges::BOTH,
    );
    for cell in &buf.content {
        assert_ne!(cell.symbol(), "▌");
    }
    for y in 1..3 {
        assert_eq!(buf[(7, y)].symbol(), "▐", "y {y}");
        assert_eq!(buf[(7, y)].fg, Role::Accent.color(), "y {y}");
        assert_eq!(buf[(7, y)].bg, Role::Surface.color(), "y {y}");
        for x in 2..8 {
            if x != 7 {
                assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            }
            assert_eq!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
        for x in 8..10 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
            assert_ne!(buf[(x, y)].bg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for (y, edge) in [(0, "▄"), (3, "▀")] {
        for x in 2..8 {
            assert_eq!(buf[(x, y)].symbol(), edge, "({x}, {y})");
            assert_eq!(buf[(x, y)].fg, Role::Surface.color(), "({x}, {y})");
        }
        for x in 8..10 {
            assert_eq!(buf[(x, y)].symbol(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn draw_stripe_clips_its_rows_and_columns_to_the_buffer() {
    // Rows past the buffer never draw: a stripe overhanging the bottom
    // tints only the rows inside.
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
    super::draw_stripe(
        &mut buf,
        Rect::new(1, 3, 2, 3),
        Role::Accent,
        Role::Surface,
        false,
    );
    assert_eq!(buf[(1, 3)].symbol(), "▌");
    for y in 0..3 {
        for x in 0..6 {
            assert_ne!(buf[(x, y)].symbol(), "▌", "({x}, {y})");
        }
    }
    // A stripe column outside the buffer draws nothing.
    let mut outside = Buffer::empty(Rect::new(0, 0, 6, 4));
    super::draw_stripe(
        &mut outside,
        Rect::new(7, 0, 3, 2),
        Role::Accent,
        Role::Surface,
        false,
    );
    super::draw_stripe(
        &mut outside,
        Rect::new(4, 0, 6, 2),
        Role::Accent,
        Role::Surface,
        true,
    );
    for cell in &outside.content {
        assert_ne!(cell.symbol(), "▌");
        assert_ne!(cell.symbol(), "▐");
    }
    // Rows above a nonzero-origin buffer never draw.
    let mut above = Buffer::empty(Rect::new(0, 2, 6, 4));
    super::draw_stripe(
        &mut above,
        Rect::new(0, 0, 6, 4),
        Role::Accent,
        Role::Surface,
        false,
    );
    for y in 2..4 {
        assert_eq!(above[(0, y)].symbol(), "▌", "y {y}");
    }
    for y in 4..6 {
        for x in 0..6 {
            assert_ne!(above[(x, y)].symbol(), "▌", "({x}, {y})");
        }
    }
}

#[test]
fn draw_edges_clips_its_columns_at_a_nonzero_origin() {
    // Columns 0, 1, 8 and past sit outside the buffer: both edge rows
    // cover columns 2 to 7 only.
    let mut buf = Buffer::empty(Rect::new(2, 1, 6, 3));
    draw_edges(&mut buf, Rect::new(0, 2, 10, 1), Role::Surface);
    for (y, edge) in [(1, "▄"), (3, "▀")] {
        for x in 2..8 {
            assert_eq!(buf[(x, y)].symbol(), edge, "({x}, {y})");
            assert_eq!(buf[(x, y)].fg, Role::Surface.color(), "({x}, {y})");
        }
    }
    for x in 2..8 {
        assert_eq!(buf[(x, 2)].symbol(), " ", "x {x}");
    }
}
