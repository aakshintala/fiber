//! The overlays over the conversation, for #1630.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, floating centred over a dimmed conversation
//! backdrop, and the program waits for a key; Esc, q or Ctrl+C quits. An
//! overlay owns the keyboard while it is up, so the fixture replay never
//! draws under it.

use crate::cases::{Case, Surface};
use super::input::{Ev, Key};
use super::{Args, Term};
use super::{bold, dim, fg, fit, lift, paint, panel, row, slab, sp, t, wrap, wrap_rows, BI, CYAN, ORANGE, SEL};
use ratatui::style::Style;
use ratatui::text::Span;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--overlay` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case { name: "keymap", help: "the key map, two columns", check: "the key map should float centred over the dimmed conversation with ▄ ▀ edges and the ▌ stripe; `Key map` bold accent with a dim ✕; dim section headers with a blank row between sections; every binding grouped by area in two columns sharing one fixed key column, descriptions aligned, other paths dim under; the foot sits left and names no key; nothing clipped.", build: || Look { kind: Kind::Keymap, narrow: false } },
    Case { name: "keymap-narrow", help: "the key map, one column, scrolled", check: "the same bindings in one column through the panel frame, opened scrolled, with an `↑ N more · ↓ M more` indicator on its last line; the key column stays fixed and descriptions aligned.", build: || Look { kind: Kind::Keymap, narrow: true } },
    Case { name: "quit", help: "the quit question", check: "the question should read `2 sessions working` under a bold accent `Quit`; `enter` marked `▸` in attention with its key bold and no `· default`; the foot sits left with the body and names no choice.", build: || Look { kind: Kind::Quit, narrow: false } },
    Case { name: "delete", help: "the delete question", check: "the question should name `docs: rail spec` and its spend, and what `--cascade` would add, under a bold accent title; padding all round; the foot sits left with the body.", build: || Look { kind: Kind::Delete, narrow: false } },
    Case { name: "history", help: "the prompt-history panel", check: "the typed `back` should read bold after `›`, every hit in the three matches marked, the first row `▸` in attention on the lighter tint; centred with padding; the foot sits left.", build: || Look { kind: Kind::History, narrow: false } },
    Case { name: "notice", help: "a notice shown in full", check: "the whole `key_clash` text should read wrapped to the panel, padded and striped, nothing clipped; the foot sits left.", build: || Look { kind: Kind::Notice, narrow: false } },
    Case { name: "close-mouse", help: "how an overlay closes by mouse", check: "the ✕ should read lighter than its neighbours, the foot should read `click ✕ or outside to close` left-aligned, and a dim dotted outline should mark the click-outside target.", build: || Look { kind: Kind::CloseMouse, narrow: false } },
];

/// `--overlay`, for `--help` and `check/overlays.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--overlay", file: "overlays", title: "Overlays (#1630)", docs: || crate::cases::docs(CASES) };

/// Two side-by-side binding columns need at least this overlay width.
const TWO_COL_MIN: usize = 100;
/// The scrolled keymap case opens past its first screenful.
const NARROW_SCROLL: usize = 10;

struct Binding {
    area: &'static str,
    action: &'static str,
    key: &'static str,
    other: &'static str,
    /// What the key used to say after a comma: shown parenthesised in the
    /// description, never inside the key column.
    when: &'static str,
}

/// Every row of docs/tui.md's Bindings table, grouped by area, in area order.
const BINDINGS: &[Binding] = &[
    Binding { area: "Session", action: "Start a new session", key: "Ctrl+N", other: "/new", when: "" },
    Binding { area: "Session", action: "Go home", key: "⌥0", other: "/home", when: "" },
    Binding { area: "Session", action: "Switch to the session of rail card N", key: "⌥1 to ⌥9", other: "click the card", when: "" },
    Binding {
        area: "Session",
        action: "Close what is on top; interrupt the turn when nothing is open",
        key: "Esc",
        other: "click the overlay's ✕ or outside it; click \"esc to interrupt\"",
    },
    Binding {
        area: "Session",
        action: "Clear the draft, then quit",
        key: "Ctrl+C",
        other: "/quit",
        when: "twice within about a second on an empty box",
    },
    Binding { area: "Input", action: "Send a prompt, or a steering message during a turn", key: "Enter", other: "", when: "" },
    Binding { area: "Input", action: "Insert a line break", key: "Shift+Enter", other: "Ctrl+J", when: "" },
    Binding {
        area: "Input",
        action: "Recall an earlier prompt from the project of the session on screen",
        key: "↑",
        other: "",
        when: "in an empty box",
    },
    Binding { area: "Input", action: "Search those prompts", key: "Ctrl+R", other: "", when: "" },
    Binding { area: "Input", action: "Move by word", key: "⌥← ⌥→, Ctrl+← Ctrl+→", other: "", when: "" },
    Binding { area: "Input", action: "Delete a word", key: "⌥Backspace", other: "", when: "" },
    Binding {
        area: "Input",
        action: "Start or end of the line",
        key: "⌘← ⌘→",
        other: "",
        when: "where the terminal passes them",
    },
    Binding { area: "Input", action: "Open the draft, or a pasted token, in `$VISUAL` or `$EDITOR`", key: "Ctrl+G", other: "click the token", when: "" },
    Binding { area: "Input", action: "Paste an image", key: "Ctrl+V", other: "", when: "" },
    Binding {
        area: "Conversation",
        action: "Move focus from the input box into the conversation",
        key: "Shift+Tab",
        other: "click an item",
        when: "",
    },
    Binding { area: "Conversation", action: "Move focus to the next or previous item", key: "↓ ↑, j k", other: "click an item", when: "" },
    Binding { area: "Conversation", action: "Open the focused item", key: "Enter", other: "click it", when: "" },
    Binding { area: "Conversation", action: "Copy the focused item", key: "y", other: "select it", when: "" },
    Binding {
        area: "Conversation",
        action: "Move focus to the panel, the rail, then the conversation",
        key: "Tab",
        other: "click the area",
        when: "",
    },
    Binding { area: "Conversation", action: "Open or close the ledgers", key: "Ctrl+O", other: "click a group's line", when: "" },
    Binding {
        area: "Conversation",
        action: "Delete the selected exited session in the session list",
        key: "Delete or Backspace",
        other: "click the row's ✕",
        when: "on its row",
    },
    Binding { area: "Panels", action: "Show or hide the panel", key: "⌥P", other: "/panel", when: "" },
    Binding { area: "Panels", action: "Show or hide the rail", key: "⌥R", other: "drag its edge", when: "" },
    Binding { area: "Search", action: "Search", key: "Ctrl+F; Cmd+F", other: "", when: "where forwarded" },
    Binding { area: "Search", action: "Open the search results", key: "Ctrl+F", other: "click the match count", when: "with search open" },
    Binding {
        area: "Search",
        action: "Next or previous match",
        key: "Enter or ↓, Shift+Enter or ↑",
        other: "",
        when: "with search open",
    },
    Binding { area: "Search", action: "Jump to the end", key: "End", other: "click \"↓ New messages below\"", when: "" },
    Binding { area: "Steering", action: "Select a queued steering message", key: "⌥↑ ⌥↓", other: "its mouse target", when: "" },
    Binding { area: "Steering", action: "Amend it", key: "Enter", other: "its mouse target", when: "" },
    Binding { area: "Steering", action: "Drop it", key: "⌥X", other: "its mouse target", when: "" },
    Binding {
        area: "Requests",
        action: "Reopen a request put aside, or move to the next, the oldest first, switching to its session",
        key: "⌥A",
        other: "/approvals; click the badge or a waiting card",
    },
    Binding { area: "Model", action: "Open the model picker", key: "Ctrl+L", other: "/model", when: "" },
    Binding { area: "Model", action: "Choose in the model picker for this session only", key: "s", other: "", when: "" },
    Binding { area: "Model", action: "Open the key map", key: "F1", other: "/? or /help", when: "" },
];

/// The two keymap columns hold the same count: 17 bindings each, so both end
/// together and no column scrolls on its own.
const LEFT_AREAS: &[&str] = &["Session", "Input", "Panels", "Requests"];
const RIGHT_AREAS: &[&str] = &["Conversation", "Search", "Steering", "Model"];
const ALL_AREAS: &[&str] = &["Session", "Input", "Conversation", "Panels", "Search", "Steering", "Requests", "Model"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Keymap,
    Quit,
    Delete,
    History,
    Notice,
    CloseMouse,
}

struct Look {
    kind: Kind,
    narrow: bool,
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
/// One binding's description: the action, with its condition parenthesised
/// after it instead of inside the key column.
fn desc_spans(b: &Binding) -> Vec<Span<'static>> {
    let mut out = vec![sp(b.action, Style::new())];
    if !b.when.is_empty() {
        out.push(sp(format!(" ({})", b.when), dim()));
    }
    out
}

/// One binding through the panel: the key in the panel's fixed column, the
/// description starting together, other paths dim under it.
fn binding_block(b: &Binding, key_w: usize, inner: usize) -> Vec<super::Row> {
    panel::key_block(b.key, b.other, desc_spans(b), key_w, inner)
}

/// The panel's fixed key column, computed once over the bindings it shows.
fn key_w_for(bindings: &[&Binding]) -> usize {
    panel::key_width(&bindings.iter().map(|b| b.key).collect::<Vec<_>>())
}

/// One keymap column: a dim header per area, then its bindings, one blank
/// row between sections.
fn keymap_column(areas: &[&str], key_w: usize, inner: usize) -> Vec<super::Row> {
    let mut out = vec![];
    let mut first = true;
    for area in areas {
        let bs: Vec<&Binding> = BINDINGS.iter().filter(|b| b.area == *area).collect();
        if bs.is_empty() {
            continue;
        }
        if !first {
            out.push(row(vec![]));
        }
        first = false;
        out.push(panel::section_row(area, inner));
        for b in bs {
            out.extend(binding_block(b, key_w, inner));
        }
    }
    out
}

/// The quit question: who is working and the three ways out, the default
/// marked like the approval card instead of `· default`.
fn quit_body(inner: usize) -> Vec<super::Row> {
    let key_w = panel::key_width(&["enter", "c", "esc"]);
    let mut out = vec![
        row(fit(&[sp("2 sessions working", Style::new())], inner)),
        row(vec![]),
    ];
    out.extend(panel::choice_row(true, "enter", "leave them running", key_w, inner));
    out.extend(panel::choice_row(false, "c", "close all", key_w, inner));
    out.extend(panel::choice_row(false, "esc", "stay", key_w, inner));
    out
}

/// Home's delete question for an exited session: it names the session and
/// any session `--cascade` would add.
fn delete_body(inner: usize) -> Vec<super::Row> {
    let mut out = wrap_rows(
        vec![sp("Delete \"docs: rail spec\" ($1.10)?", Style::new())],
        inner,
        vec![],
        vec![],
    );
    out.push(row(vec![]));
    out.extend(wrap_rows(
        vec![sp("It deletes the session through the hub, after asking.", Style::new())],
        inner,
        vec![],
        vec![],
    ));
    out.extend(wrap_rows(vec![sp("--cascade would add no other session.", dim())], inner, vec![], vec![]));
    out
}

/// The typed Ctrl+R query; every fixture prompt holds it.
const QUERY: &str = "back";
/// Prompt history, newest first: the prompt and where it was recalled from.
const PROMPTS: &[(&str, &str)] = &[
    ("how do I backfill embeddings for old sessions?", "this session"),
    ("back up the state file before the migration", "project"),
    ("roll back the handoff note", "project"),
];

/// The query's every occurrence marked, the rest in the base style. The
/// fixtures are ASCII, so byte offsets from the lowercased copy hold.
fn hl(text: &str, q: &str, base: Style, mark: Style) -> Vec<Span<'static>> {
    if q.is_empty() {
        return vec![sp(text, base)];
    }
    let lower = text.to_lowercase();
    let q = q.to_lowercase();
    let mut out = vec![];
    let mut i = 0;
    while let Some(j) = lower[i..].find(&q) {
        let (a, b) = (i + j, i + j + q.len());
        if a > i {
            out.push(sp(text[i..a].to_string(), base));
        }
        out.push(sp(text[a..b].to_string(), mark));
        i = b;
    }
    if i < text.len() {
        out.push(sp(text[i..].to_string(), base));
    }
    out
}

/// The Ctrl+R prompt-history panel: the typed query, how many match, and the
/// matches with every hit marked, the first one focused like a choice.
fn history_body(inner: usize) -> Vec<super::Row> {
    let mut out = vec![
        row(fit(&[sp("› ", fg(CYAN)), sp(QUERY, bold()), sp("█", dim())], inner)),
        row(fit(&[sp(format!("{} matches · newest first", PROMPTS.len()), dim())], inner)),
    ];
    for (i, (prompt, from)) in PROMPTS.iter().enumerate() {
        let mut spans = hl(prompt, QUERY, Style::new(), bold());
        spans.push(sp(format!(" · {from}"), dim()));
        let (first, rest) = if i == 0 {
            (vec![sp("▸ ", fg(ORANGE))], vec![sp("  ", Style::new())])
        } else {
            (vec![sp("  ", Style::new())], vec![sp("  ", Style::new())])
        };
        let bg = if i == 0 { Some(lift(BI)) } else { None };
        for mut r in wrap(spans, inner, first, rest).into_iter().map(row) {
            r.bg = bg;
            out.push(r);
        }
    }
    out
}

/// A notice's whole text, wrapped to the overlay width.
const NOTICE_TEXT: &str = "Two actions share the key Ctrl+F: `search` and `search_results` act in a context they share, so both entries reverted to their defaults. Rebind one of them with /keys. An entry equal to the defaults counts as unset.";

fn notice_body(inner: usize) -> Vec<super::Row> {
    wrap_rows(vec![sp(NOTICE_TEXT, Style::new())], inner, vec![], vec![])
}

/// The mouse-closing case: a small keymap sample, with the other path naming
/// the ✕ and the click outside.
fn close_mouse_body(inner: usize) -> Vec<super::Row> {
    let bs = [&BINDINGS[3], &BINDINGS[5], &BINDINGS[33]];
    let key_w = key_w_for(&bs);
    bs.iter().flat_map(|b| binding_block(b, key_w, inner)).collect()
}

fn content(c: &Look, inner: usize, rows: usize) -> (&'static str, Vec<super::Row>, &'static str) {
    match c.kind {
        Kind::Keymap => ("Key map", keymap_body(c, inner, rows), "every binding, grouped by area, with its other paths"),
        Kind::Quit => ("Quit", quit_body(inner), "click a choice · they keep running meanwhile"),
        Kind::Delete => ("Delete session", delete_body(inner), "enter deletes · esc keeps it"),
        Kind::History => ("Prompt history", history_body(inner), "enter recalls · esc closes"),
        Kind::Notice => ("Notice · key_clash", notice_body(inner), "esc closes"),
        Kind::CloseMouse => ("Key map", close_mouse_body(inner), "click ✕ or outside to close"),
    }
}

/// A centred panel with a bold accent title, a ✕ at its right end, and a dim
/// foot line aligned left with the body.
fn overlay(title: &str, body: Vec<super::Row>, foot: &str, w: usize, inner: usize, hover_x: bool) -> Vec<super::Row> {
    // The ✕ reads dim, and lighter under the pointer, as `--hover` tints it.
    let x = if hover_x { sp("✕", dim().bg(lift(BI))) } else { sp("✕", dim()) };
    panel::frame(
        Some(panel::title_row(title, Some(x), inner)),
        body,
        Some(panel::footer_row(foot, inner)),
        w,
    )
}

/// The key map's width: fluid content, so the area decides within the clamps.
fn keymap_w(cols: usize) -> usize {
    cols.saturating_sub(10).clamp(panel::MIN_W, panel::MAX_W).min(cols)
}

/// A small panel's width: what its unwrapped content needs, wrapped at a
/// preferred width first so long text does not blow it out.
fn small_w(natural: usize, cols: usize) -> usize {
    panel::fit_width(natural, 71, cols)
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

fn keymap_body(c: &Look, w: usize, inner: usize, rows: usize) -> Vec<super::Row> {
    let all: Vec<&Binding> = BINDINGS.iter().collect();
    let key_w = key_w_for(&all);
    if two_col(w) && !c.narrow {
        let colw = inner.saturating_sub(4) / 2;
        let mut left = keymap_column(LEFT_AREAS, key_w, colw);
        let mut right = keymap_column(RIGHT_AREAS, key_w, colw);
        let n = left.len().max(right.len());
        left.resize(n, row(vec![]));
        right.resize(n, row(vec![]));
        return left
            .into_iter()
            .zip(right)
            .map(|(l, r)| {
                let mut s = fit(&l.spans, colw);
                s.push(sp("    ", Style::new()));
                s.extend(fit(&r.spans, colw));
                row(s)
            })
            .collect();
    }
    // The narrow screenful: one column, opened past the first screen, with
    // how much hides above and below on its last line.
    let all = keymap_column(ALL_AREAS, key_w, inner);
    let total = all.len();
    let oh = rows.saturating_sub(6);
    let vis = oh.saturating_sub(5);
    let off = clamp_scroll(NARROW_SCROLL, total, vis);
    let rest = total.saturating_sub(off + vis);
    let mut out: Vec<super::Row> = all.into_iter().skip(off).take(vis).map(row).collect();
    out.push(row(fit(&[sp(format!("↑ {off} more · ↓ {rest} more"), dim())], ow)));
    out
}

fn frame(c: &Look, cols: usize, rows: usize) -> Vec<Vec<Placed>> {
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
    let close = c.kind == Kind::CloseMouse;
    let (title, body, foot) = content(c, ow, rows);
    let ov = overlay(title, body, foot, ow, close);
    let oh = ov.len();
    let side = if close { 2 } else { 0 };
    let x0 = cols.saturating_sub(ow) / 2;
    let y0 = rows.saturating_sub(ov.len()) / 2;
    for (k, r) in ov.into_iter().enumerate() {
        if y0 + k >= screen.len() {
            break;
        }
        // The overlay covers the backdrop's middle cells; its side margins
        // stay blank, and the dimmed conversation reads above and below it.
        // The mouse case marks its click-outside target with dim dots.
        let mut spans = vec![sp(" ".repeat(x0.saturating_sub(side)), Style::new())];
        if close {
            spans.push(sp("· ", dim()));
        }
        spans.extend(r.spans);
        if close {
            spans.push(sp(" ·", dim()));
        }
        screen[y0 + k] = vec![Placed {
            x: 0,
            w: cols as u16,
            row: super::Row {
                spans: fit(&spans, cols),
                ..Default::default()
            },
        }];
    }
    // The click-outside target's outline, two cells out from the edges.
    if close && y0 >= 2 && y0 + oh + 1 < rows {
        for y in [y0 - 2, y0 + oh + 1] {
            let spans = vec![
                sp(" ".repeat(x0.saturating_sub(2)), Style::new()),
                sp("·".repeat(ow + 4), dim()),
            ];
            screen[y] = vec![Placed {
                x: 0,
                w: cols as u16,
                row: super::Row {
                    spans: fit(&spans, cols),
                    ..Default::default()
                },
            }];
        }
    }
    screen
}

fn draw(term: &mut Term, c: &Look) -> io::Result<()> {
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
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --overlay case {name:?}; one of: {}", crate::cases::names(CASES)),
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

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
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
        assert!(CASES.iter().all(|c| crate::cases::lookup(CASES, c.name).is_some()));
        assert_eq!(CASES.len(), 7);
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
    fn quit_asks_with_the_doc_line() {
        let t = text(&parse("quit").unwrap(), 160, 48);
        assert!(t.contains("2 sessions working"));
        assert!(t.contains("enter leave them running · c close all · esc stay"));
    }

    #[test]
    fn delete_names_the_session_and_its_cascade() {
        let t = text(&parse("delete").unwrap(), 160, 48);
        assert!(t.contains("docs: rail spec"));
        assert!(t.contains("$1.10"));
        assert!(t.contains("--cascade"));
    }

    #[test]
    fn history_selects_its_first_match() {
        let t = text(&parse("history").unwrap(), 160, 48);
        assert!(t.contains("Prompt history"));
        assert!(t.contains("▌"));
        assert!(t.contains("3 matches"));
    }

    #[test]
    fn query_hits_read_marked() {
        let base = ratatui::style::Style::new();
        let mark = super::bold();
        let spans = super::hl("back up", "back", base, mark);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "back");
        let spans = super::hl("Backfill", "back", base, mark);
        assert_eq!(spans[0].content, "Back");
        let spans = super::hl("nothing here", "back", base, mark);
        assert_eq!(spans.len(), 1);
        let spans = super::hl("back", "", base, mark);
        assert_eq!(spans.len(), 1);
        let spans = super::hl("back and back", "back", base, mark);
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn notice_shows_the_whole_text() {
        let t = text(&parse("notice").unwrap(), 160, 48);
        assert!(t.contains("key_clash"));
        // Wrapping splits at spaces only, so every word reads whole.
        for w in super::NOTICE_TEXT.split(' ') {
            assert!(t.contains(w), "missing {w}");
        }
    }

    #[test]
    fn mouse_close_names_its_two_targets() {
        let t = text(&parse("close-mouse").unwrap(), 160, 48);
        assert!(t.contains("click ✕ or outside to close"));
        assert!(t.contains("click the overlay's ✕ or outside it"));
    }

    #[test]
    fn no_row_overflows_its_screen() {
        for (cols, rows) in [(160usize, 48usize), (100, 40)] {
            for name in CASES.iter().map(|c| c.name) {
                let c = crate::cases::lookup(CASES, name).unwrap();
                for ps in frame(&c, cols, rows) {
                    for p in &ps {
                        assert!(crate::width(&p.row.spans) <= cols, "{name} overflows at {cols}x{rows}");
                    }
                }
            }
        }
    }
}
