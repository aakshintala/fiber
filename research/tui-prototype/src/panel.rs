//! One shared panel frame for every overlay, picker and popup (#1664).
//!
//! A throwaway look prototype, so this draws the owner's verdict directly:
//! every floating surface reuses the input box's idiom, a ▄ edge above, a ▀
//! edge below, a raised surface, every content row opened by a ▌ stripe.
//! No new frame style lives here; surfaces that need one say so in the PR.

use super::{
    BI, BLUE, CYAN, ORANGE, Row, bold, dim, fg, fit, row, slab, sp, t, width, wrap_rows,
};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// The narrowest a panel ever draws, in cells.
pub(crate) const MIN_W: usize = 40;
/// The widest a panel ever draws, in cells.
pub(crate) const MAX_W: usize = 150;
/// A content row's chrome on the left: the stripe and its two blank columns.
const STRIPE_W: usize = 3;
/// The blank columns on the right of the content.
const PAD_R: usize = 2;

/// The content width inside a panel this wide.
pub(crate) fn inner_w(panel_w: usize) -> usize {
    panel_w.saturating_sub(STRIPE_W + PAD_R)
}

/// The panel width for content this wide in an area this wide: content plus
/// padding, clamped between MIN_W and MAX_W, never wider than the area.
pub(crate) fn width_for(content_w: usize, area_w: usize) -> usize {
    (content_w + STRIPE_W + PAD_R).clamp(MIN_W, MAX_W).min(area_w.max(1))
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
/// (a ✕, a count) dim. Content coordinates: the frame adds the stripe.
pub(crate) fn title_row(title: &str, right: Option<Span<'static>>, inner: usize) -> Row {
    let mut spans = vec![sp(title, bold().patch(fg(BLUE)))];
    match right {
        Some(r) => {
            spans.push(t());
            spans.push(r);
        }
        None => spans.push(sp("", Style::new())),
    }
    row(fit(&spans, inner))
}

/// A section header: dim, never bold beside the title.
pub(crate) fn section_row(head: &str, inner: usize) -> Row {
    row(fit(&[sp(head, dim())], inner))
}

/// A hint line: dim, aligned left with the body, never centred, and never
/// repeating a choice the body lists.
pub(crate) fn footer_row(hint: &str, inner: usize) -> Row {
    row(fit(&[sp(hint, dim())], inner))
}

/// Flows description tokens after an unbreakable prefix, hanging further
/// lines on the indent. Unlike `wrap`, the prefix (a key) never splits.
fn flow(
    prefix: Vec<Span<'static>>,
    desc: Vec<Span<'static>>,
    indent: Vec<Span<'static>>,
    inner: usize,
) -> Vec<Row> {
    let mut toks: Vec<Span<'static>> = vec![];
    for s in desc {
        let mut cur = String::new();
        for ch in s.content.chars() {
            if ch == ' ' && !cur.is_empty() && !cur.ends_with(' ') {
                toks.push(Span::styled(std::mem::take(&mut cur), s.style));
            }
            cur.push(ch);
        }
        if !cur.is_empty() {
            toks.push(Span::styled(cur, s.style));
        }
    }
    let mut out = vec![];
    let mut line = prefix;
    let mut n = width(&line);
    let mut any = false;
    for t in toks {
        let tw = t.content.width();
        if any && n + tw > inner {
            out.push(row(line));
            n = width(&indent);
            line = indent.clone();
            let tt = t.content.trim_start().to_string();
            n += tt.width();
            line.push(Span::styled(tt, t.style));
        } else {
            n += tw;
            line.push(t);
        }
        any = true;
    }
    out.push(row(line));
    let pre = width(&indent) as u16;
    out.into_iter()
        .enumerate()
        .map(|(i, r)| Row { cont: i > 0, pre: if i > 0 { pre } else { 0 }, ..r })
        .collect()
}

/// One key with its description: the key in a fixed-width column, the
/// description starting in the same column on every row, wrapping with a
/// hanging indent. `other` paths get their own dim line under it.
pub(crate) fn key_block(
    key: &str,
    other: &str,
    desc: Vec<Span<'static>>,
    key_w: usize,
    inner: usize,
) -> Vec<Row> {
    let hang = key_w + 2;
    let first = vec![sp(pad_key(key, key_w), fg(CYAN)), sp("  ", Style::new())];
    let rest = vec![sp(" ".repeat(hang), Style::new())];
    let mut out = flow(first, desc, rest, inner);
    if !other.is_empty() {
        let first = vec![sp(" ".repeat(hang), Style::new())];
        out.extend(wrap_rows(vec![sp(other, dim())], inner, first.clone(), first));
    }
    out
}

/// One choice: the focused one marked like the approval card, `▸` in the
/// attention colour with its key bold, the rest two blanks and plain.
pub(crate) fn choice_row(
    focused: bool,
    key: &str,
    desc: &str,
    key_w: usize,
    inner: usize,
) -> Vec<Row> {
    let marker = if focused { sp("▸ ", fg(ORANGE)) } else { sp("  ", Style::new()) };
    let key = if focused {
        sp(pad_key(key, key_w), bold())
    } else {
        sp(pad_key(key, key_w), Style::new())
    };
    let hang = 2 + key_w + 2;
    let first = vec![marker, key, sp("  ", Style::new())];
    let rest = vec![sp(" ".repeat(hang), Style::new())];
    flow(first, vec![sp(desc, dim())], rest, inner)
}

/// Opens a content row with the stripe and its padding. Blank rows stay
/// blank: the frame's padding rows carry no stripe. Click targets move with
/// the text they cover.
fn stripe(r: Row, inner: usize) -> Row {
    if width(&r.spans) == 0 {
        return r;
    }
    let dx = STRIPE_W as u16;
    Row {
        spans: [vec![sp("▌", fg(BLUE)), sp("  ", Style::new())], fit(&r.spans, inner)]
            .concat(),
        hot: r.hot.iter().map(|&(a, b, k)| (a + dx, b + dx, k)).collect(),
        pre: r.pre + dx,
        ..r
    }
}

/// Assembles a panel: the title, a blank row, the body, a blank row, the
/// footer, striped and edged. The body's section gaps are the body's own.
pub(crate) fn frame(
    title: Option<Row>,
    body: Vec<Row>,
    footer: Option<Row>,
    panel_w: usize,
) -> Vec<Row> {
    let inner = inner_w(panel_w);
    let mut rows = vec![];
    if let Some(t) = title {
        rows.push(t);
    }
    rows.push(Row::default());
    rows.extend(body);
    rows.push(Row::default());
    if let Some(f) = footer {
        rows.push(f);
    }
    slab(rows.into_iter().map(|r| stripe(r, inner)).collect(), BI, None, panel_w)
}

/// Centres panel rows in a wider area, for surfaces painted full width: the
/// margins are blank with no background, so the panel never fills the area,
/// and every click target moves with its text.
pub(crate) fn centre(rows: Vec<Row>, panel_w: usize, area_w: usize) -> Vec<Row> {
    let m = x_for(panel_w, area_w);
    rows
        .into_iter()
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

/// The search box's content row: the same striped surface in three rows
/// instead of five, so it keeps floating over the conversation's corner.
pub(crate) fn search_row(spans: Vec<Span<'static>>, panel_w: usize) -> Row {
    stripe(row(spans), inner_w(panel_w))
}

/// The panel's colours, for the mouse-closing case and the tests.
pub(crate) fn surface() -> Color {
    BI
}
pub(crate) fn stripe_colour() -> Color {
    BLUE
}
pub(crate) fn attention() -> Color {
    ORANGE
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
            Some(footer_row("a hint", inner_w(48))),
            48,
        )
    }

    #[test]
    fn the_frame_is_the_input_box_idiom() {
        let rows = demo(Some(title_row("Title", None, inner_w(48))));
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
        assert_eq!(buf[(0, 1)].symbol(), "▌");
        assert_eq!(buf[(0, 1)].fg, BLUE);
        assert_eq!(buf[(4, 3)].bg, BI);
        // No new frame style: no border glyphs anywhere.
        assert!(!text(&rows).chars().any(|c| "│─┌┐└┘├┤┬┴┼".contains(c)));
    }

    #[test]
    fn padding_keeps_text_off_every_edge() {
        let rows = demo(Some(title_row("Title", None, inner_w(48))));
        let t = text(&rows);
        let lines: Vec<&str> = t.split('\n').collect();
        // A blank row after the title and before the footer.
        assert!(lines[2].trim().is_empty(), "no top pad: {:?}", lines[2]);
        let foot = lines.iter().position(|l| l.contains("a hint")).unwrap();
        assert!(lines[foot - 1].trim().is_empty(), "no bottom pad");
        // Without a title the first interior row is the blank pad itself.
        let plain = text(&demo(None));
        assert!(plain.split('\n').nth(1).unwrap().trim().is_empty());
        // Two blank columns after the stripe and at the row's end.
        for i in 1..lines.len() - 1 {
            if lines[i].trim().is_empty() {
                continue;
            }
            let cells: Vec<char> = lines[i].chars().collect();
            assert_eq!(&cells[1..3], &[' ', ' '], "row {i} touches the stripe");
            assert_eq!(
                &cells[cells.len() - 2..],
                &[' ', ' '],
                "row {i} touches the edge"
            );
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
        let title = title_row("Key map", Some(sp("✕", dim())), inner);
        assert!(title.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(title.spans[0].style.fg, Some(BLUE));
        let section = section_row("Session", inner);
        assert!(!section.spans[0].style.add_modifier.contains(Modifier::BOLD));
        // A subtitle in the normal weight never stands bold beside the title.
        let sub = row(fit(&[sp("2 sessions working", Style::new())], inner));
        assert!(!sub.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn key_columns_line_every_description_up() {
        let keys = ["Enter", "⌥1 to ⌥9", "Ctrl+C"];
        let key_w = key_width(&keys);
        assert_eq!(key_w, "⌥1 to ⌥9".width());
        let rows: Vec<Row> = keys
            .iter()
            .flat_map(|k| {
                key_block(
                    k,
                    "/new",
                    vec![sp("does a thing", Style::new())],
                    key_w,
                    60,
                )
            })
            .collect();
        // The description's cell column, not its byte offset, is the same.
        let cols: Vec<usize> = rows
            .iter()
            .filter(|r| plain(r).contains("does a thing"))
            .map(|r| {
                let mut n = 0;
                for s in &r.spans {
                    if s.content.starts_with("does") {
                        return n;
                    }
                    n += s.content.width();
                }
                panic!("lost the description");
            })
            .collect();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols, vec![key_w + 2; 3], "descriptions drift: {cols:?}");
        // The other paths sit dim on their own line, under the description.
        let other = rows.iter().find(|r| plain(r).contains("/new")).unwrap();
        let path = other.spans.iter().find(|s| s.content.contains("/new")).unwrap();
        assert!(path.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn choices_mark_the_focus_like_the_approval() {
        let rows = choice_row(true, "enter", "leave them running", 5, 40);
        let t = text(&rows);
        assert!(t.contains("▸ "));
        assert_eq!(rows[0].spans[0].style.fg, Some(ORANGE));
        assert!(rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
        let rest = choice_row(false, "c", "close all", 5, 40);
        assert!(text(&rest).starts_with("  "));
        assert!(!rest[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_footer_aligns_left_with_the_body() {
        let rows = demo(Some(title_row("Title", None, inner_w(48))));
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
            Some(title_row("Key map", None, inner_w(MIN_W))),
            key_block(
                "Enter or ↓, Shift+Enter or ↑",
                "",
                vec![sp("next match", Style::new())],
                30,
                inner_w(30),
            ),
            None,
            width_for(200, 30),
        );
        assert!(rows.iter().all(|r| crate::width(&r.spans) <= 30));
        let _ = buffer(&rows, 30);
    }
}
