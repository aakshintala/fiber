//! One shared panel frame for every overlay, picker and popup (#1664).
//!
//! A throwaway look prototype, so this draws the owner's verdict directly:
//! every floating surface reuses the input box's idiom, a ▄ edge above, a ▀
//! edge below, a raised surface, every content row opened by a ▌ stripe.
//! No new frame style lives here; surfaces that need one say so in the PR.

use super::{BI, BLUE, Row, bold, dim, fg, fit, row, slab, sp, t, width, wrap_rows};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;

/// Which chrome a panel draws on its content rows (#1765): the stripe on
/// the left only, on both sides, or neither. The default is `Left`, which
/// is exactly what every panel drew before the flag existed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Stripe {
    None,
    Both,
    #[default]
    Left,
}

thread_local! {
    static MODE: Cell<Stripe> = const { Cell::new(Stripe::Left) };
}

/// Sets the stripe mode, from `--panel-stripe`. Main-thread only, like the
/// render path that reads it; tests that switch mode reset it to `Left`.
pub(crate) fn set_stripe(s: Stripe) {
    MODE.set(s);
}

/// The current stripe mode.
pub(crate) fn stripe_mode() -> Stripe {
    MODE.get()
}

/// The narrowest a panel ever draws, in cells.
pub(crate) const MIN_W: usize = 40;
/// The widest a panel ever draws, in cells.
pub(crate) const MAX_W: usize = 150;
/// A content row's chrome on the left: the stripe and its two blank columns,
/// or two blank columns with no stripe.
fn left_w() -> usize {
    match stripe_mode() {
        Stripe::None => 2,
        Stripe::Left | Stripe::Both => 3,
    }
}
/// The blank columns on the right of the content: two, or two plus the
/// right stripe with `Both`.
fn pad_r() -> usize {
    match stripe_mode() {
        Stripe::Both => 3,
        Stripe::None | Stripe::Left => 2,
    }
}

/// The content width inside a panel this wide.
pub(crate) fn inner_w(panel_w: usize) -> usize {
    panel_w.saturating_sub(left_w() + pad_r())
}

/// The panel width for content this wide in an area this wide: content plus
/// padding, clamped between MIN_W and MAX_W, never wider than the area.
pub(crate) fn width_for(content_w: usize, area_w: usize) -> usize {
    (content_w + left_w() + pad_r())
        .clamp(MIN_W, MAX_W)
        .min(area_w.max(1))
}

/// The panel width for content that wraps: wrap at `prefer`, then size to
/// what the wrapped content needs, so short content shrinks the panel.
pub(crate) fn fit_width(natural_w: usize, prefer: usize, area_w: usize) -> usize {
    width_for(natural_w.min(prefer), area_w)
}

/// The left margin that centres a panel this wide in an area this wide.
pub(crate) fn x_for(panel_w: usize, area_w: usize) -> usize {
    area_w.saturating_sub(panel_w) / 2
}

/// Pads a key to a fixed cell width, so every description starts together.
fn pad_key(key: &str, key_w: usize) -> String {
    format!("{key}{}", " ".repeat(key_w.saturating_sub(key.width())))
}

/// The widest key, in cells, computed once per panel.
pub(crate) fn key_width(keys: &[&str]) -> usize {
    keys.iter().map(|k| k.width()).max().unwrap_or(0)
}

/// A panel's title row: the title bold in the accent, an optional right end
/// (a cross, a count) dim. Content coordinates: the frame fits and stripes.
/// Callers measuring content width read true widths, never padding.
pub(crate) fn title_row(title: &str, right: Option<Span<'static>>) -> Row {
    let mut spans = vec![sp(title, bold().patch(fg(BLUE)))];
    match right {
        Some(r) => {
            spans.push(t());
            spans.push(r);
        }
        None => spans.push(sp("", Style::new())),
    }
    row(spans)
}

/// A hint line: dim, aligned left with the body, never centred, and never
/// repeating a choice the body lists.
pub(crate) fn footer_row(hint: &str) -> Row {
    row(vec![sp(hint, dim())])
}

/// One choice: the focused one carries the gutter marker and a bold key.
/// The caller draws the bar over the focused rows with [`bar`].
pub(crate) fn choice_row(
    focused: bool,
    key: &str,
    desc: &str,
    key_w: usize,
    inner: usize,
) -> Vec<Row> {
    let marker = if focused {
        sp("› ", bold())
    } else {
        sp("  ", Style::new())
    };
    let key = if focused {
        sp(pad_key(key, key_w), bold())
    } else {
        sp(pad_key(key, key_w), Style::new())
    };
    let hang = 2 + key_w + 2;
    let first = vec![marker, key, sp("  ", Style::new())];
    let rest = vec![sp(" ".repeat(hang), Style::new())];
    // The focused choice reads bold throughout, like the approval card.
    let desc = if focused {
        sp(desc, dim().add_modifier(Modifier::BOLD))
    } else {
        sp(desc, dim())
    };
    wrap_rows(vec![desc], inner, first, rest)
}

/// A full-width selection bar over rows: the accent behind, dark ink over
/// it, emphasis kept. Click targets and copy flags ride along untouched.
pub(crate) fn bar(rows: Vec<Row>) -> Vec<Row> {
    rows.into_iter()
        .map(|r| Row {
            spans: r
                .spans
                .into_iter()
                .map(|s| Span::styled(s.content, s.style.fg(Color::Black).bg(BLUE)))
                .collect(),
            bg: Some(BLUE),
            ..r
        })
        .collect()
}

/// A footer legend: keys bold, labels muted, `·` between items.
pub(crate) fn footer_legend(items: &[(&str, &str)]) -> Row {
    let mut spans = vec![];
    for (i, (key, label)) in items.iter().enumerate() {
        if i > 0 {
            spans.push(sp(" · ", dim()));
        }
        if !key.is_empty() {
            spans.push(sp(*key, bold()));
            spans.push(sp(" ", Style::new()));
        }
        spans.push(sp(*label, dim()));
    }
    row(spans)
}

/// Opens a content row with the stripe and its padding. Blank rows stay
/// blank: the frame's padding rows carry no stripe. Click targets move with
/// the text they cover.
fn stripe(r: Row, inner: usize) -> Row {
    if width(&r.spans) == 0 {
        return r;
    }
    let dx = left_w() as u16;
    let mut spans = match stripe_mode() {
        Stripe::None => vec![sp("  ", Style::new())],
        Stripe::Left | Stripe::Both => vec![sp("▌", fg(BLUE)), sp("  ", Style::new())],
    };
    spans.extend(fit(&r.spans, inner));
    if stripe_mode() == Stripe::Both {
        // The row is `fit` to inner, so the right stripe sits after the
        // same two blank cells the left stripe keeps before the text.
        spans.push(sp("  ", Style::new()));
        spans.push(sp("▐", fg(BLUE)));
    }
    Row {
        spans,
        hot: r.hot.iter().map(|&(a, b, k)| (a + dx, b + dx, k)).collect(),
        pre: r.pre + dx,
        ..r
    }
}

/// Stripes content rows and edges the panel. Blank rows stay blank.
pub(crate) fn slab_rows(rows: Vec<Row>, panel_w: usize) -> Vec<Row> {
    let inner = inner_w(panel_w);
    slab(
        rows.into_iter().map(|r| stripe(r, inner)).collect(),
        BI,
        None,
        panel_w,
    )
}

/// Assembles a panel: a blank row below the top edge and above the bottom
/// edge, so no text touches either; the title, body and footer keep one
/// blank row between them. The body's section gaps are the body's own.
/// Panels with custom chrome assemble their own rows and call [`slab_rows`].
pub(crate) fn frame(
    title: Option<Row>,
    body: Vec<Row>,
    footer: Option<Row>,
    panel_w: usize,
) -> Vec<Row> {
    let mut rows = vec![Row::default()];
    if let Some(t) = title {
        rows.push(t);
        rows.push(Row::default());
    }
    rows.extend(body);
    if let Some(f) = footer {
        rows.push(Row::default());
        rows.push(f);
    }
    rows.push(Row::default());
    slab_rows(rows, panel_w)
}

/// Centres panel rows in a wider area, for surfaces painted full width: the
/// margins are blank with no background, so the panel never fills the area,
/// and every click target moves with its text.
pub(crate) fn centre(rows: Vec<Row>, panel_w: usize, area_w: usize) -> Vec<Row> {
    let m = x_for(panel_w, area_w);
    rows.into_iter()
        .map(|r| {
            let dx = m as u16;
            Row {
                spans: [vec![sp(" ".repeat(m), Style::new())], r.spans].concat(),
                bg: None,
                hot: r.hot.iter().map(|&(a, b, k)| (a + dx, b + dx, k)).collect(),
                pre: r.pre + dx,
                ..r
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    fn text(rows: &[Row]) -> String {
        rows.iter().map(plain).collect::<Vec<_>>().join("\n")
    }

    /// Paints rows into a buffer, as the frame loop does.
    fn buffer(rows: &[Row], w: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, w as u16, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            crate::paint(&mut buf, 0, y as u16, w as u16, r);
        }
        buf
    }

    fn demo(title: Option<Row>) -> Vec<Row> {
        frame(
            title,
            vec![row(vec![sp("body line", Style::new())])],
            Some(footer_row("a hint")),
            48,
        )
    }

    #[test]
    fn the_frame_is_the_input_box_idiom() {
        let rows = demo(Some(title_row("Title", None)));
        let buf = buffer(&rows, 48);
        // A ▄ edge above, a ▀ edge below, in the surface colour.
        for x in 0..48 {
            assert_eq!(buf[(x, 0)].symbol(), "▄");
            assert_eq!(buf[(x, 0)].fg, BI);
            let y = rows.len() as u16 - 1;
            assert_eq!(buf[(x, y)].symbol(), "▀");
            assert_eq!(buf[(x, y)].fg, BI);
        }
        // A raised surface behind the content, the stripe opening each row.
        assert_eq!(buf[(0, 2)].symbol(), "▌");
        assert_eq!(buf[(0, 2)].fg, BLUE);
        assert_eq!(buf[(4, 4)].bg, BI);
        // No new frame style: no border glyphs anywhere.
        assert!(!text(&rows).chars().any(|c| "│─┌┐└┘├┤┬┴┼".contains(c)));
    }

    #[test]
    fn padding_keeps_text_off_every_edge() {
        for title in [Some(title_row("Title", None)), None] {
            let rows = demo(title);
            let t = text(&rows);
            let lines: Vec<&str> = t.split('\n').collect();
            // The first and last interior rows are blank: no text touches an edge.
            assert!(lines[1].trim().is_empty(), "no top pad: {:?}", lines[1]);
            assert!(lines[lines.len() - 2].trim().is_empty(), "no bottom pad");
            // Two blank columns after the stripe and at the row's end.
            for (i, l) in lines.iter().enumerate().skip(1).take(lines.len() - 2) {
                if l.trim().is_empty() {
                    continue;
                }
                let cells: Vec<char> = l.chars().collect();
                assert_eq!(&cells[1..3], &[' ', ' '], "row {i} touches the stripe");
                assert_eq!(
                    &cells[cells.len() - 2..],
                    &[' ', ' '],
                    "row {i} touches the edge"
                );
            }
        }
    }

    #[test]
    fn width_is_content_plus_padding_between_min_and_max() {
        // At, just below and just above both clamps; the area wins when tiny.
        let wide = 200;
        assert_eq!(width_for(MIN_W - 5 - 1, wide), MIN_W);
        assert_eq!(width_for(MIN_W - 5, wide), MIN_W);
        assert_eq!(width_for(MIN_W - 5 + 1, wide), MIN_W + 1);
        assert_eq!(width_for(MAX_W - 5 - 1, wide), MAX_W - 1);
        assert_eq!(width_for(MAX_W - 5, wide), MAX_W);
        assert_eq!(width_for(MAX_W - 5 + 1, wide), MAX_W);
        assert_eq!(width_for(60, 50), 50);
        // Wrapping content shrinks to what it needs, never under the floor.
        assert_eq!(fit_width(20, 71, 160), MIN_W);
        assert_eq!(fit_width(210, 71, 160), 76);
        assert_eq!(fit_width(210, 71, 60), 60);
    }

    #[test]
    fn the_panel_sits_centred_odd_and_even() {
        assert_eq!(x_for(76, 160), 42);
        assert_eq!(x_for(77, 160), 41);
        assert_eq!(41 + 77 + 42, 160);
        assert_eq!(x_for(200, 160), 0);
    }

    #[test]
    fn titles_are_bold_accent_subtitles_are_not() {
        let inner = inner_w(48);
        let title = title_row("Key map", Some(sp("✕", dim())));
        assert!(title.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title.spans[0].style.fg, Some(BLUE));
        // Section headers dim too; the pickers assert their own dim sections.
        // A subtitle in the normal weight never stands bold beside the title.
        let sub = row(fit(&[sp("2 sessions working", Style::new())], inner));
        assert!(!sub.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_fixed_key_column_is_computed_once() {
        let keys = ["Enter", "⌥1 to ⌥9", "Ctrl+C"];
        assert_eq!(key_width(&keys), "⌥1 to ⌥9".width());
        assert_eq!(key_width(&[]), 0);
    }

    #[test]
    fn choices_carry_the_gutter_marker_and_a_bold_key() {
        let rows = choice_row(true, "enter", "leave them running", 5, 40);
        assert!(text(&rows).contains("› "));
        assert!(rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
        let rest = choice_row(false, "c", "close all", 5, 40);
        assert!(text(&rest).starts_with("  "));
        assert!(!rest[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_focused_description_reads_bold_in_the_buffer() {
        for (focused, want) in [(true, true), (false, false)] {
            let rows = choice_row(focused, "enter", "leave them running", 5, 40);
            let buf = buffer(&rows, 40);
            let mut bold = false;
            for x in 0..40 {
                if buf[(x, 0)].symbol() == "l" {
                    bold = buf[(x, 0)].modifier.contains(Modifier::BOLD);
                    break;
                }
            }
            assert_eq!(bold, want, "focused={focused}: description bold wrong");
        }
    }

    #[test]
    fn long_choices_wrap_onto_the_hanging_indent() {
        let rows = choice_row(
            false,
            "enter",
            "leave them running while the turn winds down",
            5,
            30,
        );
        assert!(rows.len() > 1, "nothing wrapped");
        assert!(!rows[0].cont);
        for r in &rows[1..] {
            assert!(r.cont, "continuation not marked for copy");
            assert_eq!(r.pre, (2 + 5 + 2) as u16, "hanging indent drifted");
        }
        let t = text(&rows);
        assert!(
            t.contains("leave them running"),
            "first line lost the start"
        );
    }

    #[test]
    fn the_bar_is_full_width_accent_with_dark_ink() {
        let rows = bar(choice_row(true, "enter", "leave them running", 5, 40));
        for r in &rows {
            assert_eq!(r.bg, Some(BLUE));
            for s in &r.spans {
                assert_eq!(s.style.fg, Some(ratatui::style::Color::Black));
                assert_eq!(s.style.bg, Some(BLUE));
            }
        }
        // Emphasis survives the bar: the key stays bold.
        assert!(rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_legend_pairs_bold_keys_with_muted_labels() {
        let f = footer_legend(&[("↑↓", "move"), ("esc", "closes")]);
        let keys: Vec<&Span> = f
            .spans
            .iter()
            .filter(|s| s.content == "↑↓" || s.content == "esc")
            .collect();
        assert_eq!(keys.len(), 2);
        assert!(
            keys.iter()
                .all(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        for lab in ["move", "closes"] {
            let s = f.spans.iter().find(|s| s.content == lab).unwrap();
            assert!(s.style.add_modifier.contains(Modifier::DIM));
        }
        assert!(plain(&f).contains(" · "));
    }

    #[test]
    fn the_footer_aligns_left_with_the_body() {
        let rows = demo(Some(title_row("Title", None)));
        let foot = rows.iter().find(|r| plain(r).contains("a hint")).unwrap();
        let t = plain(foot);
        assert!(t.starts_with("▌  a hint"), "centred or flush: {t:?}");
    }

    #[test]
    fn centring_moves_click_targets_with_the_text() {
        let body = vec![crate::hot_row(vec![(
            sp("thinking", dim()),
            Some(crate::Act::PickRefresh),
        )])];
        let rows = centre(frame(None, body, None, 40), 40, 100);
        let m = x_for(40, 100);
        let hot: Vec<(u16, u16)> = rows
            .iter()
            .flat_map(|r| r.hot.iter().map(|&(a, b, _)| (a, b)))
            .collect();
        assert_eq!(hot.len(), 1);
        // Past the margin and the stripe: the target covers the word alone.
        assert_eq!(hot[0], (m as u16 + 3, m as u16 + 3 + 8));
        // The margins carry no background: the panel never fills the area.
        let buf = buffer(&rows, 100);
        assert_ne!(buf[(0, 2)].bg, BI);
        assert_eq!(buf[(m as u16 + 4, 2)].bg, BI);
    }

    #[test]
    fn tiny_terminals_clamp_without_panicking() {
        let rows = frame(
            Some(title_row("Key map", None)),
            choice_row(false, "esc", "stay", 5, inner_w(MIN_W)),
            None,
            width_for(200, 30),
        );
        assert!(rows.iter().all(|r| crate::width(&r.spans) <= 30));
        let _ = buffer(&rows, 30);
    }

    #[test]
    fn stripe_modes_resize_the_chrome() {
        // (mode, chrome): left keeps today's 3 + 2, none drops the stripe
        // to 2 + 2, both adds a right stripe for 3 + 3.
        for (mode, chrome) in [(Stripe::Left, 5), (Stripe::None, 4), (Stripe::Both, 6)] {
            set_stripe(mode);
            assert_eq!(left_w() + pad_r(), chrome, "chrome in {mode:?}");
            assert_eq!(inner_w(48), 48 - chrome, "inner_w in {mode:?}");
            assert_eq!(inner_w(MIN_W), MIN_W - chrome, "inner_w floor in {mode:?}");
            assert_eq!(
                inner_w(MAX_W),
                MAX_W - chrome,
                "inner_w ceiling in {mode:?}"
            );
            let wide = 200;
            assert_eq!(width_for(MIN_W - chrome, wide), MIN_W, "floor in {mode:?}");
            assert_eq!(
                width_for(MIN_W - chrome + 1, wide),
                MIN_W + 1,
                "above the floor in {mode:?}"
            );
            assert_eq!(
                width_for(MAX_W - chrome, wide),
                MAX_W,
                "ceiling in {mode:?}"
            );
            assert_eq!(
                width_for(MAX_W - chrome + 1, wide),
                MAX_W,
                "above the ceiling in {mode:?}"
            );
            assert_eq!(width_for(60, 50), 50, "the area wins in {mode:?}");
        }
        set_stripe(Stripe::Left);
    }

    #[test]
    fn stripe_modes_draw_in_the_buffer() {
        // Left is exactly today's output: the stripe opens row 2 and the
        // row ends two cells short of the edge, with no right stripe.
        set_stripe(Stripe::Left);
        let rows = demo(Some(title_row("Title", None)));
        let buf = buffer(&rows, 48);
        assert_eq!(buf[(0, 2)].symbol(), "▌");
        assert_eq!(buf[(0, 2)].fg, BLUE);
        assert_eq!(buf[(47, 2)].symbol(), " ");
        assert_eq!(inner_w(48), 43);
        // None draws no stripe on either side, with symmetric padding:
        // two blank cells left and right of every content row.
        set_stripe(Stripe::None);
        let rows = demo(Some(title_row("Title", None)));
        let buf = buffer(&rows, 48);
        for y in 0..rows.len() as u16 {
            for x in 0..48 {
                assert!(
                    !matches!(buf[(x, y)].symbol(), "▌" | "▐"),
                    "a stripe in none at ({x}, {y})"
                );
            }
        }
        let t = text(&rows);
        let lines: Vec<&str> = t.split('\n').collect();
        for (y, l) in lines.iter().enumerate().skip(1).take(lines.len() - 2) {
            if l.trim().is_empty() {
                continue;
            }
            let cells: Vec<char> = l.chars().collect();
            assert_eq!(&cells[0..2], &[' ', ' '], "row {y} touches the left edge");
            assert_eq!(
                &cells[cells.len() - 2..],
                &[' ', ' '],
                "row {y} touches the right edge"
            );
        }
        // Both keeps the left stripe and closes each content row with a
        // right one, two blank columns between each stripe and the text.
        set_stripe(Stripe::Both);
        let rows = demo(Some(title_row("Title", None)));
        let buf = buffer(&rows, 48);
        let t = text(&rows);
        let lines: Vec<&str> = t.split('\n').collect();
        for (y, l) in lines.iter().enumerate().skip(1).take(lines.len() - 2) {
            let y = y as u16;
            if l.trim().is_empty() {
                // Blank rows stay blank: no stripe on either side.
                assert_eq!(buf[(0, y)].symbol(), " ", "a left stripe on blank row {y}");
                assert_eq!(
                    buf[(47, y)].symbol(),
                    " ",
                    "a right stripe on blank row {y}"
                );
                continue;
            }
            assert!(l.starts_with('▌'), "no left stripe on row {y}: {l:?}");
            assert!(l.ends_with('▐'), "no right stripe on row {y}: {l:?}");
            assert_eq!(buf[(0, y)].symbol(), "▌", "no left stripe on row {y}");
            assert_eq!(buf[(0, y)].fg, BLUE, "the left stripe lost its colour");
            assert_eq!(buf[(47, y)].symbol(), "▐", "no right stripe on row {y}");
            assert_eq!(buf[(47, y)].fg, BLUE, "the right stripe lost its colour");
            for x in [1, 2, 45, 46] {
                assert_eq!(buf[(x, y)].symbol(), " ", "row {y} touches a stripe at {x}");
            }
        }
        set_stripe(Stripe::Left);
    }

    #[test]
    fn stripe_modes_shift_click_targets_with_the_chrome() {
        for (mode, dx) in [
            (Stripe::Left, 3u16),
            (Stripe::None, 2u16),
            (Stripe::Both, 3u16),
        ] {
            set_stripe(mode);
            let body = vec![crate::hot_row(vec![(
                sp("thinking", dim()),
                Some(crate::Act::PickRefresh),
            )])];
            let rows = frame(None, body, None, 40);
            let hot: Vec<(u16, u16)> = rows
                .iter()
                .flat_map(|r| r.hot.iter().map(|&(a, b, _)| (a, b)))
                .collect();
            assert_eq!(hot.len(), 1);
            // Past the left chrome: the target covers the word alone.
            assert_eq!(hot[0], (dx, dx + 8), "click target in {mode:?}");
            let marked = rows.iter().find(|r| !r.hot.is_empty()).unwrap();
            assert_eq!(marked.pre, dx, "pre in {mode:?}");
        }
        set_stripe(Stripe::Left);
    }
}
