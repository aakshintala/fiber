//! Tests for surfaces: where stripes draw, the inset and edge counts, and
//! the edge and stripe cells (`docs/tui.md`, "Look").

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::{draw_edges, edge_row, edged, inset, stripe, stripes};
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
