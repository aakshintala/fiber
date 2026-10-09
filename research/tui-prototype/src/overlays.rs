//! The overlays over the conversation, for #1630.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, floating centred over a dimmed conversation
//! backdrop, and the program waits for a key; Esc, q or Ctrl+C quits. An
//! overlay owns the keyboard while it is up, so the fixture replay never
//! draws under it.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{bold, dim, fg, fit, paint, row, slab, sp, t, wrap, BI, CYAN, ORANGE, SEL};
use ratatui::style::Style;
use ratatui::text::Span;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--overlay` case, named in README.md.
pub(crate) const CASES: &[&str] = &["keymap", "keymap-narrow"];

/// Two side-by-side binding columns need at least this overlay width.
const TWO_COL_MIN: usize = 100;
/// The scrolled keymap case opens past its first screenful.
const NARROW_SCROLL: usize = 10;

struct Binding {
    area: &'static str,
    action: &'static str,
    id: &'static str,
    key: &'static str,
    other: &'static str,
}

/// Every row of docs/tui.md's Bindings table, grouped by area, in area order.
const BINDINGS: &[Binding] = &[
    Binding { area: "Session", action: "Start a new session", id: "new_session", key: "Ctrl+N", other: "/new" },
    Binding { area: "Session", action: "Go home", id: "go_home", key: "⌥0", other: "/home" },
    Binding { area: "Session", action: "Switch to the session of rail card N", id: "rail_row_n", key: "⌥1 to ⌥9", other: "click the card" },
    Binding {
        area: "Session",
        action: "Close what is on top; interrupt the turn when nothing is open",
        id: "close_or_interrupt",
        key: "Esc",
        other: "click the overlay's ✕ or outside it; click \"esc to interrupt\"",
    },
    Binding {
        area: "Session",
        action: "Clear the draft, then quit",
        id: "clear_then_quit",
        key: "Ctrl+C, twice within about a second on an empty box",
        other: "/quit",
    },
    Binding { area: "Input", action: "Send a prompt, or a steering message during a turn", id: "send", key: "Enter", other: "" },
    Binding { area: "Input", action: "Insert a line break", id: "line_break", key: "Shift+Enter", other: "Ctrl+J" },
    Binding {
        area: "Input",
        action: "Recall an earlier prompt from the project of the session on screen",
        id: "recall_prompt",
        key: "↑ in an empty box",
        other: "",
    },
    Binding { area: "Input", action: "Search those prompts", id: "search_prompts", key: "Ctrl+R", other: "" },
    Binding { area: "Input", action: "Move by word", id: "move_word", key: "⌥← ⌥→, Ctrl+← Ctrl+→", other: "" },
    Binding { area: "Input", action: "Delete a word", id: "delete_word", key: "⌥Backspace", other: "" },
    Binding {
        area: "Input",
        action: "Start or end of the line",
        id: "line_start_end",
        key: "⌘← ⌘→, where the terminal passes them",
        other: "",
    },
    Binding { area: "Input", action: "Open the draft, or a pasted token, in `$VISUAL` or `$EDITOR`", id: "open_in_editor", key: "Ctrl+G", other: "click the token" },
    Binding { area: "Input", action: "Paste an image", id: "paste_image", key: "Ctrl+V", other: "" },
    Binding {
        area: "Conversation",
        action: "Move focus from the input box into the conversation",
        id: "navigate",
        key: "Shift+Tab",
        other: "click an item",
    },
    Binding { area: "Conversation", action: "Move focus to the next or previous item", id: "focus_next_prev", key: "↓ ↑, j k", other: "click an item" },
    Binding { area: "Conversation", action: "Open the focused item", id: "open_focused", key: "Enter", other: "click it" },
    Binding { area: "Conversation", action: "Copy the focused item", id: "copy_focused", key: "y", other: "select it" },
    Binding {
        area: "Conversation",
        action: "Move focus to the panel, the rail, then the conversation",
        id: "focus_area",
        key: "Tab",
        other: "click the area",
    },
    Binding { area: "Conversation", action: "Open or close the ledgers", id: "toggle_ledgers", key: "Ctrl+O", other: "click a group's line" },
    Binding {
        area: "Conversation",
        action: "Delete the selected exited session in the session list",
        id: "delete_session",
        key: "Delete, or Backspace, on the row",
        other: "click the row's ✕",
    },
    Binding { area: "Panels", action: "Show or hide the panel", id: "toggle_panel", key: "⌥P", other: "/panel" },
    Binding { area: "Panels", action: "Show or hide the rail", id: "toggle_rail", key: "⌥R", other: "drag its edge" },
    Binding { area: "Search", action: "Search", id: "search", key: "Ctrl+F; Cmd+F where forwarded", other: "" },
    Binding { area: "Search", action: "Open the search results", id: "search_results", key: "Ctrl+F with search open", other: "click the match count" },
    Binding {
        area: "Search",
        action: "Next or previous match",
        id: "search_next_prev",
        key: "Enter or ↓, Shift+Enter or ↑, with search open",
        other: "",
    },
    Binding { area: "Search", action: "Jump to the end", id: "jump_to_end", key: "End", other: "click \"↓ New messages below\"" },
    Binding { area: "Steering", action: "Select a queued steering message", id: "select_steering", key: "⌥↑ ⌥↓", other: "its mouse target" },
    Binding { area: "Steering", action: "Amend it", id: "amend_steering", key: "Enter", other: "its mouse target" },
    Binding { area: "Steering", action: "Drop it", id: "drop_steering", key: "⌥X", other: "its mouse target" },
    Binding {
        area: "Requests",
        action: "Reopen a request put aside, or move to the next, the oldest first, switching to its session",
        id: "next_request",
        key: "⌥A",
        other: "/approvals; click the badge or a waiting card",
    },
    Binding { area: "Model", action: "Open the model picker", id: "model_picker", key: "Ctrl+L", other: "/model" },
    Binding { area: "Model", action: "Choose in the model picker for this session only", id: "session_only", key: "s", other: "" },
    Binding { area: "Model", action: "Open the key map", id: "key_map", key: "F1", other: "/? or /help" },
];

/// The two keymap columns hold the same count: 17 bindings each, so both end
/// together and no column scrolls on its own.
const LEFT_AREAS: &[&str] = &["Session", "Input", "Panels", "Requests"];
const RIGHT_AREAS: &[&str] = &["Conversation", "Search", "Steering", "Model"];
const ALL_AREAS: &[&str] = &["Session", "Input", "Conversation", "Panels", "Search", "Steering", "Requests", "Model"];

struct Case {
    narrow: bool,
}

fn parse(name: &str) -> Option<Case> {
    match name {
        "keymap" => Some(Case { narrow: false }),
        "keymap-narrow" => Some(Case { narrow: true }),
        _ => None,
    }
}

/// Two aligned binding columns fit at this overlay width and above.
fn two_col(ow: usize) -> bool {
    ow >= TWO_COL_MIN
}

/// The scroll offset kept inside what shows a full screen: 0 when everything
/// fits, else the offset clamped to the last full screen.
fn clamp_scroll(off: usize, total: usize, vis: usize) -> usize {
    off.min(total.saturating_sub(vis))
}

// ============================================================ pieces
/// One binding as shown: the action, then the id, key and other paths under
/// it, wrapping instead of clipping so every id stays readable whole.
fn binding_lines(b: &Binding, w: usize) -> Vec<Vec<Span<'static>>> {
    let mut out = wrap(vec![sp(b.action, Style::new())], w, vec![], vec![sp("  ", Style::new())]);
    let mut key = vec![sp("  ", Style::new()), sp(b.key, fg(CYAN))];
    if !b.other.is_empty() {
        key.push(sp(" · ", dim()));
        key.push(sp(b.other, dim()));
    }
    key.push(sp(" · ", dim()));
    key.push(sp(b.id, dim()));
    out.extend(wrap(key, w, vec![], vec![sp("    ", Style::new())]));
    out
}

/// One keymap column: a bold header per area, then its bindings.
fn keymap_column(areas: &[&str], w: usize) -> Vec<Vec<Span<'static>>> {
    let mut out = vec![];
    let mut area = "";
    for b in BINDINGS.iter().filter(|b| areas.contains(&b.area)) {
        if b.area != area {
            area = b.area;
            out.push(fit(&[sp(b.area, bold())], w));
        }
        out.extend(binding_lines(b, w));
    }
    out
}

/// A centred slab with a bold title row, a ✕ at its right end, and a dim foot
/// line naming the close keys.
fn overlay(title: &str, body: Vec<Vec<Span<'static>>>, foot: &str, ow: usize) -> Vec<super::Row> {
    let mut inner = vec![vec![sp(title, bold()), t(), sp("✕", dim())]];
    inner.extend(body);
    let pad = ow.saturating_sub(foot.width()) / 2;
    inner.push(fit(&[sp(" ".repeat(pad), Style::new()), sp(foot, dim())], ow));
    slab(inner.into_iter().map(row).collect(), SEL, None, ow)
}

/// The dimmed conversation under an overlay: static dim rows and the input
/// box, never the replayed session.
fn backdrop(cols: usize, rows: usize) -> Vec<super::Row> {
    let blank = || row(vec![]);
    let mut out: Vec<super::Row> = (0..rows).map(|_| blank()).collect();
    if rows > 9 {
        out[1] = row(fit(&[t(), sp("how do I backfill embeddings for old sessions?", dim())], cols));
        out[2] = row(fit(&[t(), sp("you 14:20", dim())], cols));
        out[4] = row(fit(&[sp("● Edited 2 files, ran 3 commands · 12s ▾", dim())], cols));
        out[5] = row(fit(&[sp("      1 ✓ read   lock.rs · 41 lines", dim())], cols));
        out[6] = row(fit(&[sp("      2 ✓ shell  cargo test · exit 0 · 41 lines", dim())], cols));
        out[8] = row(fit(&[sp("● Searched 2 patterns, thought once · 9s ▸", dim())], cols));
    }
    if rows > 5 {
        out[rows - 4] = row(fit(
            &[
                sp("  ⚠ ", fg(ORANGE)),
                sp("key_clash", fg(ORANGE)),
                sp(" · two actions share a key, both reverted", dim()),
                t(),
                sp("✕", dim()),
            ],
            cols,
        ));
        let input = slab(
            vec![row(vec![sp("› ", dim()), sp("█", dim())])],
            BI,
            None,
            cols,
        );
        for (k, r) in input.into_iter().enumerate() {
            out[rows - 3 + k] = r;
        }
    }
    out
}

// ============================================================ frame
/// A row painted at its own offset and width, so the overlay's background
/// stays within its edges instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

fn overlay_w(_c: &Case, cols: usize) -> usize {
    cols.saturating_sub(10).min(150).max(40)
}

fn body_lines(c: &Case, ow: usize, rows: usize) -> Vec<Vec<Span<'static>>> {
    if two_col(ow) && !c.narrow {
        let colw = ow.saturating_sub(4) / 2;
        let mut left = keymap_column(LEFT_AREAS, colw);
        let mut right = keymap_column(RIGHT_AREAS, colw);
        let n = left.len().max(right.len());
        left.resize(n, vec![]);
        right.resize(n, vec![]);
        return left
            .into_iter()
            .zip(right)
            .map(|(l, r)| {
                let mut s = fit(&l, colw);
                s.push(sp("    ", Style::new()));
                s.extend(fit(&r, colw));
                s
            })
            .collect();
    }
    // The narrow screenful: one column, opened past the first screen, with
    // how much hides above and below on its last line.
    let all = keymap_column(ALL_AREAS, ow);
    let total = all.len();
    let oh = rows.saturating_sub(6);
    let vis = oh.saturating_sub(5);
    let off = clamp_scroll(NARROW_SCROLL, total, vis);
    let rest = total.saturating_sub(off + vis);
    let mut out: Vec<Vec<Span>> = all.into_iter().skip(off).take(vis).collect();
    out.push(fit(&[sp(format!("↑ {off} more · ↓ {rest} more"), dim())], ow));
    out
}

fn frame(c: &Case, cols: usize, rows: usize) -> Vec<Vec<Placed>> {
    let mut screen: Vec<Vec<Placed>> = backdrop(cols, rows)
        .into_iter()
        .map(|r| {
            vec![Placed {
                x: 0,
                w: cols as u16,
                row: r,
            }]
        })
        .collect();
    let ow = overlay_w(c, cols);
    let ov = overlay("Key map", body_lines(c, ow, rows), "esc closes", ow);
    let x0 = cols.saturating_sub(ow) / 2;
    let y0 = rows.saturating_sub(ov.len()) / 2;
    for (k, r) in ov.into_iter().enumerate() {
        if y0 + k >= screen.len() {
            break;
        }
        // The overlay covers the backdrop's middle cells; its side margins
        // stay blank, and the dimmed conversation reads above and below it.
        let mut spans = vec![sp(" ".repeat(x0), Style::new())];
        spans.extend(r.spans);
        screen[y0 + k] = vec![Placed {
            x: 0,
            w: cols as u16,
            row: super::Row {
                spans: fit(&spans, cols),
                ..Default::default()
            },
        }];
    }
    screen
}

fn draw(term: &mut Term, c: &Case) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|fr| {
        let buf = fr.buffer_mut();
        for (y, placements) in frame(c, cols as usize, rows as usize).iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--overlay` case and waits for a key. Mutually exclusive with the
/// conversation view: the fixture replay never draws.
pub(crate) fn run_overlay(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.overlay.clone().unwrap_or_default();
    let Some(c) = parse(&name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --overlay case {name:?}; one of: {}", CASES.join(", ")),
        ));
    };
    draw(term, &c)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("overlay {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("overlay {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("overlay {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(c: &Case, cols: usize, rows: usize) -> String {
        frame(c, cols, rows)
            .into_iter()
            .map(|ps| {
                ps.iter()
                    .map(|p| p.row.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|n| parse(n).is_some()));
        assert_eq!(CASES.len(), 2);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn keymap_has_every_binding_from_the_table() {
        assert_eq!(BINDINGS.len(), 34);
        let t = text(&parse("keymap").unwrap(), 160, 48);
        for b in BINDINGS {
            // Wrapping never splits a token, so every id reads whole.
            assert!(t.contains(b.id), "missing {}", b.id);
        }
        // Nothing clipped: even the longest action and key line wrap whole.
        assert!(!t.contains('…'), "clipped content");
    }

    #[test]
    fn scroll_stays_on_the_last_full_screen() {
        assert_eq!(clamp_scroll(0, 5, 10), 0);
        assert_eq!(clamp_scroll(3, 10, 10), 0);
        assert_eq!(clamp_scroll(7, 11, 10), 1);
        assert_eq!(clamp_scroll(1, 11, 10), 1);
        assert_eq!(clamp_scroll(10, 42, 29), 10);
        assert_eq!(clamp_scroll(30, 42, 29), 13);
    }

    #[test]
    fn two_columns_need_the_minimum_width() {
        assert!(!two_col(99));
        assert!(two_col(100));
        assert!(two_col(101));
    }

    #[test]
    fn narrow_opens_scrolled_with_how_much_hides() {
        let t = text(&parse("keymap-narrow").unwrap(), 100, 40);
        assert!(t.contains("↑ 10 more"), "scroll offset");
        assert!(t.contains("↓ "), "scroll indicator");
        assert!(!t.contains("↓ 0 more"), "bottom hidden");
    }

    #[test]
    fn no_row_overflows_its_screen() {
        for (cols, rows) in [(160usize, 48usize), (100, 40)] {
            for name in CASES {
                let c = parse(name).unwrap();
                for ps in frame(&c, cols, rows) {
                    for p in &ps {
                        assert!(crate::width(&p.row.spans) <= cols, "{name} overflows at {cols}x{rows}");
                    }
                }
            }
        }
    }
}
