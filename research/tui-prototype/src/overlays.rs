//! The overlays over the conversation, for #1630.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, floating centred over a dimmed conversation
//! backdrop, and the program waits for a key; Esc, q or Ctrl+C quits. An
//! overlay owns the keyboard while it is up, so the fixture replay never
//! draws under it.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{bold, dim, fg, fit, lift, paint, row, slab, sp, t, wrap, BI, BLUE, CYAN, ORANGE, SEL};
use ratatui::style::Style;
use ratatui::text::Span;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--overlay` case, named in README.md.
pub(crate) const CASES: &[&str] = &["keymap", "keymap-narrow", "quit", "delete", "history", "notice", "close-mouse"];

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

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Keymap,
    Quit,
    Delete,
    History,
    Notice,
    CloseMouse,
}

struct Case {
    kind: Kind,
    narrow: bool,
}

fn parse(name: &str) -> Option<Case> {
    match name {
        "keymap" => Some(Case { kind: Kind::Keymap, narrow: false }),
        "keymap-narrow" => Some(Case { kind: Kind::Keymap, narrow: true }),
        "quit" => Some(Case { kind: Kind::Quit, narrow: false }),
        "delete" => Some(Case { kind: Kind::Delete, narrow: false }),
        "history" => Some(Case { kind: Kind::History, narrow: false }),
        "notice" => Some(Case { kind: Kind::Notice, narrow: false }),
        "close-mouse" => Some(Case { kind: Kind::CloseMouse, narrow: false }),
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

/// One choice row: the key bold, what it does dim.
fn choice(key: &str, what: &str, w: usize) -> super::Row {
    row(fit(&[sp(key, bold()), sp(format!("  {what}"), dim())], w))
}

/// The quit question: who is working and the three ways out.
fn quit_body(ow: usize) -> Vec<super::Row> {
    vec![
        row(fit(&[sp("2 sessions working", bold())], ow)),
        row(vec![]),
        choice("enter", "leave them running · default", ow),
        choice("c", "close all", ow),
        choice("esc", "stay", ow),
    ]
}

/// Home's delete question for an exited session: it names the session and
/// any session `--cascade` would add.
fn delete_body(ow: usize) -> Vec<super::Row> {
    vec![
        row(fit(&[sp("Delete \"docs: rail spec\" ($1.10)?", bold())], ow)),
        row(vec![]),
        row(fit(&[sp("It deletes the session through the hub, after asking.", Style::new())], ow)),
        row(fit(&[sp("--cascade would add no other session.", dim())], ow)),
    ]
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
/// matches with every hit marked, the first one selected.
fn history_body(ow: usize) -> Vec<super::Row> {
    let mut out = vec![
        row(fit(&[sp("› ", fg(CYAN)), sp(QUERY, bold()), sp("█", dim())], ow)),
        row(fit(&[sp(format!("{} matches · newest first", PROMPTS.len()), dim())], ow)),
    ];
    for (i, (prompt, from)) in PROMPTS.iter().enumerate() {
        let mut spans = if i == 0 { vec![sp("▌ ", fg(BLUE))] } else { vec![sp("  ", Style::new())] };
        spans.extend(hl(prompt, QUERY, Style::new(), bold()));
        spans.push(sp(format!(" · {from}"), dim()));
        let line = fit(&spans, ow);
        if i == 0 {
            out.push(super::Row { spans: line, bg: Some(lift(SEL)), ..Default::default() });
        } else {
            out.push(row(line));
        }
    }
    out
}

/// A notice's whole text, wrapped to the overlay width.
const NOTICE_TEXT: &str = "Two actions share the key Ctrl+F: `search` and `search_results` act in a context they share, so both entries reverted to their defaults. Rebind one of them with /keys. An entry equal to the defaults counts as unset.";

fn notice_body(ow: usize) -> Vec<super::Row> {
    wrap(vec![sp(NOTICE_TEXT, Style::new())], ow, vec![], vec![]).into_iter().map(row).collect()
}

/// The mouse-closing case: a small keymap sample, with the other path naming
/// the ✕ and the click outside.
fn close_mouse_body(ow: usize) -> Vec<super::Row> {
    [3, 5, 33].iter().flat_map(|&i| binding_lines(&BINDINGS[i], ow)).map(row).collect()
}

fn content(c: &Case, ow: usize, rows: usize) -> (&'static str, Vec<super::Row>, &'static str) {
    match c.kind {
        Kind::Keymap => ("Key map", keymap_body(c, ow, rows), "esc closes"),
        Kind::Quit => ("Quit", quit_body(ow), "enter leave them running · c close all · esc stay"),
        Kind::Delete => ("Delete session", delete_body(ow), "enter deletes · esc keeps it"),
        Kind::History => ("Prompt history", history_body(ow), "enter recalls · esc closes"),
        Kind::Notice => ("Notice · key_clash", notice_body(ow), "esc closes"),
        Kind::CloseMouse => ("Key map", close_mouse_body(ow), "click ✕ or outside to close"),
    }
}

/// A centred slab with a bold title row, a ✕ at its right end, and a dim foot
/// line naming the close keys.
fn overlay(title: &str, body: Vec<super::Row>, foot: &str, ow: usize, hover_x: bool) -> Vec<super::Row> {
    // The ✕ reads dim, and lighter under the pointer, as `--hover` tints it.
    let x = if hover_x { sp("✕", dim().bg(lift(SEL))) } else { sp("✕", dim()) };
    let mut inner = vec![row(vec![sp(title, bold()), t(), x])];
    inner.extend(body);
    let pad = ow.saturating_sub(foot.width()) / 2;
    inner.push(row(fit(&[sp(" ".repeat(pad), Style::new()), sp(foot, dim())], ow)));
    slab(inner, SEL, None, ow)
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

/// The small overlays' width: room for a question and its answers.
const SMALL_W: usize = 76;

fn overlay_w(c: &Case, cols: usize) -> usize {
    match c.kind {
        Kind::Keymap => cols.saturating_sub(10).min(150).max(40),
        _ => SMALL_W.min(cols.saturating_sub(4)),
    }
}

fn keymap_body(c: &Case, ow: usize, rows: usize) -> Vec<super::Row> {
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
                row(s)
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
    let mut out: Vec<super::Row> = all.into_iter().skip(off).take(vis).map(row).collect();
    out.push(row(fit(&[sp(format!("↑ {off} more · ↓ {rest} more"), dim())], ow)));
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
