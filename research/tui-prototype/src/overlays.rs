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
use super::{bold, dim, fg, fit, lift, paint, panel, row, slab, sp, t, wrap, wrap_rows, BI, CYAN, ORANGE};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--overlay` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case { name: "keymap", help: "the key map, All tab, empty search", check: "the key map should dock at the bottom full width with ▄ ▀ edges and the ▌ stripe; `Key map` bold accent with a dim ✕, a muted purpose and `34 actions, 24 with other paths`; the All tab inverse with the rest muted; a muted search line with an empty query; group, action and keys columns starting together on three fixed columns with other paths dim in the keys; the first row `›` on a full-width accent bar; a `↓` arrow in the gutter whenever rows hide below (as in keymap-narrow); the foot a bold-key legend naming no body pair; nothing clipped.", build: || Look { kind: Kind::Keymap, narrow: false, tab: None, query: "" } },
    Case { name: "keymap-tab", help: "the key map, Session tab", check: "the Session tab inverse with only Session rows under it, the first `›` on the accent bar; no arrow, everything fits; same columns and legend.", build: || Look { kind: Kind::Keymap, narrow: false, tab: Some("Session"), query: "" } },
    Case { name: "keymap-search", help: "the key map, narrowed by a query", check: "`session` typed after the muted search line narrows the rows to the four bindings naming a session, across groups; the first `›` on the accent bar; no arrow; same columns and legend.", build: || Look { kind: Kind::Keymap, narrow: false, tab: None, query: "session" } },
    Case { name: "keymap-narrow", help: "the key map, narrowed, scrolled", check: "the same panel at 100 columns, rows wrapped, opened scrolled, with an `↑ N more · ↓ M more` indicator on its last line; the three columns keep their starts.", build: || Look { kind: Kind::Keymap, narrow: true, tab: None, query: "" } },
    Case { name: "quit", help: "the quit question", check: "the question should read `2 sessions working` muted under a bold accent `Quit`; `enter` marked `›` with its key bold on a full-width accent bar and no `· default`; the foot sits left with the body and names no choice.", build: || Look { kind: Kind::Quit, narrow: false, tab: None, query: "" } },
    Case { name: "delete", help: "the delete question", check: "the question should name `docs: rail spec` and its spend, and what `--cascade` would add, under a bold accent title; padding all round; the foot a bold-key legend naming no body pair.", build: || Look { kind: Kind::Delete, narrow: false, tab: None, query: "" } },
    Case { name: "history", help: "the prompt-history panel", check: "the typed `back` should read bold after `›`, every hit in the three matches marked, the first row `›` on a full-width accent bar; centred with padding; the foot a bold-key legend naming no body pair.", build: || Look { kind: Kind::History, narrow: false, tab: None, query: "" } },
    Case { name: "notice", help: "a notice shown in full", check: "the whole `key_clash` text should read wrapped to the panel, padded and striped, nothing clipped; the foot a bold-key legend naming no body pair.", build: || Look { kind: Kind::Notice, narrow: false, tab: None, query: "" } },
    Case { name: "close-mouse", help: "how an overlay closes by mouse", check: "the ✕ should read lighter than its neighbours, the foot should read `click ✕ or outside to close` left-aligned, and a dim dotted outline should mark the click-outside target.", build: || Look { kind: Kind::CloseMouse, narrow: false, tab: None, query: "" } },
];

/// `--overlay`, for `--help` and `check/overlays.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--overlay", file: "overlays", title: "Overlays (#1630)", docs: || crate::cases::docs(CASES) };

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
        when: "",
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
        when: "",
    },
    Binding { area: "Model", action: "Open the model picker", key: "Ctrl+L", other: "/model", when: "" },
    Binding { area: "Model", action: "Choose in the model picker for this session only", key: "s", other: "", when: "" },
    Binding { area: "Model", action: "Open the key map", key: "F1", other: "/? or /help", when: "" },
];

/// The key map's group tabs, in area order after All.
const ALL_AREAS: &[&str] = &["Session", "Input", "Conversation", "Panels", "Search", "Steering", "Requests", "Model"];
/// The keys column never squeezes under this, in cells.
const MIN_KW: usize = 20;

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
    /// The selected group tab; None is All.
    tab: Option<&'static str>,
    /// The typed search narrowing the rows.
    query: &'static str,
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

/// The keys cell: alternatives comma-separated, other paths after a `·`.
fn keys_spans(b: &Binding) -> Vec<Span<'static>> {
    if b.other.is_empty() {
        vec![sp(b.key, dim())]
    } else {
        vec![sp(format!("{} · {}", b.key, b.other), dim())]
    }
}

/// Splits styled spans into word tokens, as `wrap` does.
fn toks(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = vec![];
    for s in spans {
        let mut cur = String::new();
        for ch in s.content.chars() {
            if ch == ' ' && !cur.is_empty() && !cur.ends_with(' ') {
                out.push(Span::styled(std::mem::take(&mut cur), s.style));
            }
            cur.push(ch);
        }
        if !cur.is_empty() {
            out.push(Span::styled(cur, s.style));
        }
    }
    out
}

/// Packs tokens into lines this wide; the first token always fits.
fn pack(toks: Vec<Span<'static>>, w: usize) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![vec![]];
    let mut n = 0;
    for t in toks {
        let tw = t.content.width();
        if n > 0 && n + tw > w {
            lines.push(vec![]);
            n = 0;
            let tt = t.content.trim_start().to_string();
            n += tt.width();
            lines.last_mut().unwrap().push(Span::styled(tt, t.style));
        } else {
            n += tw;
            lines.last_mut().unwrap().push(t);
        }
    }
    lines
}

/// The bindings a tab and query leave visible, in table order. Tabs filter
/// the group; the query narrows names, keys and other paths.
fn visible(tab: Option<&str>, query: &str) -> Vec<&'static Binding> {
    let q = query.to_lowercase();
    BINDINGS
        .iter()
        .filter(|b| {
            tab.is_none_or(|g| b.area == g)
                && (q.is_empty()
                    || [b.action, b.key, b.other]
                        .iter()
                        .any(|f| f.to_lowercase().contains(&q)))
        })
        .collect()
}

/// One keymap row over three fixed-width columns: the group dim, the action,
/// and the keys dim. Each column wraps inside its own width; every column
/// starts at the same cell on every row. Groups are ASCII, so padding by
/// chars is padding by cells.
fn binding_row(b: &Binding, selected: bool, gw: usize, aw: usize, kw: usize) -> Vec<super::Row> {
    let gutter = if selected { vec![sp("› ", bold())] } else { vec![sp("  ", Style::new())] };
    let a = pack(toks(desc_spans(b)), aw.max(1));
    let k = pack(toks(keys_spans(b)), kw.max(1));
    let mut out = vec![];
    for i in 0..a.len().max(k.len()) {
        let mut s = if i == 0 { gutter.clone() } else { vec![sp("  ", Style::new())] };
        s.push(sp(if i == 0 { format!("{:<gw$}", b.area) } else { " ".repeat(gw) }, dim()));
        s.push(sp("  ", Style::new()));
        s.extend(fit(a.get(i).map(|v| v.as_slice()).unwrap_or(&[]), aw));
        s.push(sp("  ", Style::new()));
        s.extend(fit(k.get(i).map(|v| v.as_slice()).unwrap_or(&[]), kw));
        out.push(row(s));
    }
    if selected { panel::bar(out) } else { out }
}

/// The visible bindings as rows, the first one selected. Column widths come
/// from the visible rows, computed once.
fn bind_rows(vis: &[&Binding], select_first: bool, inner: usize) -> Vec<super::Row> {
    let gw = vis.iter().map(|b| b.area.width()).max().unwrap_or(0);
    let longest_a = vis.iter().map(|b| super::width(&desc_spans(b))).max().unwrap_or(0);
    let aw = longest_a.min(inner.saturating_sub(2 + gw + 2 + 2 + MIN_KW));
    let kw = inner.saturating_sub(2 + gw + 2 + aw + 2);
    vis.iter()
        .enumerate()
        .flat_map(|(i, b)| binding_row(b, select_first && i == 0, gw, aw, kw))
        .collect()
}

/// A row of group tabs: the selected one inverse, the rest muted.
fn tabs_row(selected: Option<&str>, inner: usize) -> super::Row {
    let mut spans = vec![];
    for (i, g) in std::iter::once("All").chain(ALL_AREAS.iter().copied()).enumerate() {
        if i > 0 {
            spans.push(sp("  ", Style::new()));
        }
        let on = selected.map_or(g == "All", |x| x == g);
        spans.push(if on {
            sp(g, bold().patch(fg(BI)).patch(Style::new().bg(Color::White)))
        } else {
            sp(g, dim())
        });
    }
    row(fit(&spans, inner))
}

/// The muted search line with the typed query narrowing the rows.
fn search_row(query: &str, inner: usize) -> super::Row {
    row(fit(
        &[
            sp("Type to search shortcuts   ", dim()),
            sp("› ", dim()),
            sp(query, bold()),
            sp("█", dim()),
        ],
        inner,
    ))
}

/// The docked key map: bold title, a muted purpose and counts, group tabs, a
/// search line, and the visible bindings in three columns. The body caps at
/// the viewport; a `↓` arrow marks more below, and the narrowed view scrolls
/// with an indicator on its last line.
fn keymap_panel(c: &Look, cols: usize, rows: usize) -> Vec<super::Row> {
    let inner = panel::inner_w(cols);
    let vis = visible(c.tab, c.query);
    let others = BINDINGS.iter().filter(|b| !b.other.is_empty()).count();
    let mut chrome = vec![
        panel::title_row("Key map", Some(sp("✕", dim())), inner),
        row(fit(&[sp("Every binding by area, with its other paths.", dim())], inner)),
        row(fit(
            &[sp(format!("{} actions, {} with other paths", BINDINGS.len(), others), dim())],
            inner,
        )),
        row(vec![]),
        tabs_row(c.tab, inner),
        search_row(c.query, inner),
    ];
    let mut body = bind_rows(&vis, true, inner);
    let total = body.len();
    // The chrome around the body: edges, blank, footer.
    let cap = rows.saturating_sub(chrome.len() + 4);
    let off = if c.narrow { clamp_scroll(NARROW_SCROLL, total, cap) } else { 0 };
    let rest = total.saturating_sub(off + cap);
    let mut shown: Vec<super::Row> = body.drain(off..).take(cap).collect();
    // More below: an arrow in the gutter of the last shown line.
    if total > off + shown.len() && let Some(last) = shown.last_mut().filter(|r| r.bg.is_none()) {
        last.spans[0] = sp("↓ ", dim());
    }
    chrome.append(&mut shown);
    if c.narrow && rest > 0 {
        chrome.push(row(fit(&[sp(format!("↑ {off} more · ↓ {rest} more"), dim())], inner)));
    }
    chrome.push(row(vec![]));
    chrome.push(panel::footer_legend(&[("↑↓", "move"), ("←→", "tabs"), ("esc", "closes")], inner));
    panel::slab_rows(chrome, cols)
}

/// The quit question: who is working and the three ways out, the default on
/// a full-width selection bar instead of `· default`.
fn quit_body(inner: usize) -> Vec<super::Row> {
    let key_w = panel::key_width(&["enter", "c", "esc"]);
    let mut out = vec![row(fit(&[sp("2 sessions working", dim())], inner)), row(vec![])];
    out.extend(panel::bar(panel::choice_row(true, "enter", "leave them running", key_w, inner)));
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
            (vec![sp("› ", bold())], vec![sp("  ", Style::new())])
        } else {
            (vec![sp("  ", Style::new())], vec![sp("  ", Style::new())])
        };
        let lines: Vec<super::Row> = wrap(spans, inner, first, rest).into_iter().map(row).collect();
        if i == 0 {
            out.extend(panel::bar(lines));
        } else {
            out.extend(lines);
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
    bind_rows(&[&BINDINGS[3], &BINDINGS[5], &BINDINGS[33]], false, inner)
}

/// A small overlay's title, body and footer. The foot is a bold-key legend,
/// except the quit question, whose body already lists every key.
fn small_content(kind: Kind, inner: usize) -> (&'static str, Vec<super::Row>, super::Row) {
    match kind {
        Kind::Quit => ("Quit", quit_body(inner), panel::footer_row("click a choice · they keep running meanwhile", inner)),
        Kind::Delete => ("Delete session", delete_body(inner), panel::footer_legend(&[("enter", "deletes"), ("esc", "keeps it")], inner)),
        Kind::History => ("Prompt history", history_body(inner), panel::footer_legend(&[("enter", "recalls"), ("esc", "closes")], inner)),
        Kind::Notice => ("Notice · key_clash", notice_body(inner), panel::footer_legend(&[("esc", "closes")], inner)),
        Kind::CloseMouse => ("Key map", close_mouse_body(inner), row(fit(&[sp("click ✕ or outside", bold()), sp(" to close", dim())], inner))),
        Kind::Keymap => unreachable!("the key map docks"),
    }
}

/// A centred panel with a bold accent title, a ✕ at its right end, and its
/// foot line aligned left with the body.
fn overlay(title: &str, body: Vec<super::Row>, foot: super::Row, w: usize, inner: usize, hover_x: bool) -> Vec<super::Row> {
    // The ✕ reads dim, and lighter under the pointer, as `--hover` tints it.
    let x = if hover_x { sp("✕", dim().bg(lift(BI))) } else { sp("✕", dim()) };
    panel::frame(Some(panel::title_row(title, Some(x), inner)), body, Some(foot), w)
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
    let close = c.kind == Kind::CloseMouse;
    if c.kind == Kind::Keymap {
        // The key map docks at the bottom, full width, over the backdrop.
        let ov = keymap_panel(c, cols, rows);
        let y0 = rows.saturating_sub(ov.len());
        for (k, r) in ov.into_iter().enumerate() {
            if y0 + k >= screen.len() {
                break;
            }
            screen[y0 + k] = vec![Placed {
                x: 0,
                w: cols as u16,
                row: super::Row { spans: fit(&r.spans, cols), ..r },
            }];
        }
        return screen;
    }
    // A small overlay sizes to its content and floats centred.
    let probe = small_content(c.kind, 10_000);
    let natural = probe
        .1
        .iter()
        .map(|r| super::width(&r.spans))
        .max()
        .unwrap_or(0)
        .max(probe.0.width())
        .max(super::width(&probe.2.spans));
    let ow = small_w(natural, cols);
    let inner = panel::inner_w(ow);
    let (title, body, foot) = small_content(c.kind, inner);
    let ov = overlay(title, body, foot, ow, inner, close);
    let oh = ov.len();
    let side = if close { 2 } else { 0 };
    let x0 = panel::x_for(ow, cols);
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
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

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

    /// Paints a case into a buffer, as the frame loop does.
    fn buffer(c: &Look, cols: usize, rows: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        for (y, ps) in frame(c, cols, rows).iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    fn line_with<'a>(t: &'a str, needle: &str) -> &'a str {
        t.split('\n').find(|l| l.contains(needle)).unwrap()
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|c| crate::cases::lookup(CASES, c.name).is_some()));
        assert_eq!(CASES.len(), 9);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn keymap_has_every_binding_without_their_ids() {
        assert_eq!(BINDINGS.len(), 34);
        let t = text(&parse("keymap").unwrap(), 160, 48);
        for b in BINDINGS {
            // Wrapping never splits a token, so every word reads whole.
            for w in b.action.split(' ').chain(b.key.split(' ')) {
                assert!(t.contains(w), "missing {w}");
            }
            for w in b.other.split(' ') {
                assert!(t.contains(w), "missing {w}");
            }
            if !b.when.is_empty() {
                assert!(t.contains(&format!("({})", b.when)), "missing ({})", b.when);
            }
        }
        // Action ids are not shown: every underscore id stays out.
        for id in [
            "rail_row_n", "close_or_interrupt", "clear_then_quit", "focus_next_prev",
            "toggle_ledgers", "next_request", "select_steering", "key_map",
        ] {
            assert!(!t.contains(id), "id leaks: {id}");
        }
        // Option shows as ⌥, never spelled out.
        assert!(t.contains("⌥"));
        assert!(!t.contains("Alt"), "spelled-out Alt");
        assert!(!t.contains("Option"), "spelled-out Option");
        // Nothing clipped: even the longest action and keys wrap whole.
        assert!(!t.contains('\u{2026}'), "clipped content");
    }

    #[test]
    fn keymap_chrome_reads_title_purpose_counts_tabs_search() {
        let t = text(&parse("keymap").unwrap(), 160, 48);
        assert!(t.contains("Key map"));
        assert!(t.contains("Every binding by area, with its other paths."));
        assert!(t.contains("34 actions, 24 with other paths"));
        for tab in ["All", "Session", "Input", "Conversation", "Panels", "Search", "Steering", "Requests", "Model"] {
            assert!(t.contains(tab), "missing tab {tab}");
        }
        assert!(t.contains("Type to search shortcuts"));
        assert!(t.contains("\u{203a} "));
        // The foot is a bold-key legend, and no pair repeats the body.
        assert!(t.contains("\u{2191}\u{2193} move · \u{2190}\u{2192} tabs · esc closes"));
        // Docked full width: the edges span the screen.
        assert!(t.contains("\u{2584}".repeat(160).as_str()));
    }

    #[test]
    fn keymap_columns_start_together_on_every_row() {
        let vis = visible(None, "");
        let inner = panel::inner_w(160);
        let gw = vis.iter().map(|b| b.area.width()).max().unwrap();
        let longest_a = vis.iter().map(|b| crate::width(&desc_spans(b))).max().unwrap();
        let aw = longest_a.min(inner - (2 + gw + 2 + 2 + MIN_KW));
        let kw = inner - (2 + gw + 2 + aw + 2);
        // Gutter, group and the action's first word sit fixed on every row.
        for b in &vis {
            let first = &binding_row(b, false, gw, aw, kw)[0];
            assert_eq!(first.spans[0].content.width(), 2, "gutter drifts");
            assert_eq!(first.spans[1].content.width(), gw, "group drifts");
            assert_eq!(&first.spans[2].content, "  ");
            let word = b.action.split(' ').next().unwrap();
            let mut n = 2 + gw + 2;
            let mut at = None;
            for s in &first.spans[3..] {
                if s.content.trim_start().starts_with(word) {
                    at = Some(n);
                    break;
                }
                n += s.content.width();
            }
            assert_eq!(at, Some(2 + gw + 2), "{} drifts", b.action);
        }
        // The keys cell starts past the action on short and longest keys.
        // The matched prefixes are ASCII, so byte and cell offsets agree.
        for (bi, cell) in [(0, "Ctrl+N · /new"), (25, "Enter or ↓")] {
            let first = &binding_row(&BINDINGS[bi], false, gw, aw, kw)[0];
            let plain: String = first.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(
                plain.find(cell),
                Some(2 + gw + 2 + aw + 2),
                "{} drifts",
                BINDINGS[bi].action
            );
        }
        // Wrapped lines hang under their own column, narrow included.
        let narrow = bind_rows(&vis, true, panel::inner_w(90));
        let second = narrow.iter().find(|r| {
            crate::plain(r).contains("switching to its")
        }).unwrap();
        assert_eq!(&second.spans[0].content, "  ");
        assert_eq!(second.spans[1].content.width(), gw);
        assert_eq!(&second.spans[2].content, "  ");
    }

    #[test]
    fn keymap_selects_its_first_row() {
        let c = parse("keymap").unwrap();
        let buf = buffer(&c, 160, 48);
        let t = text(&c, 160, 48);
        let y = t.split('\n').position(|l| l.contains("Start a new session")).unwrap();
        assert!(buf[(5, y as u16)].bg == crate::BLUE, "no bar on the selected row");
        // Everything fits at this height, so no gutter arrow.
        assert!(!t.contains("\u{258c}  \u{2193} "), "arrow with nothing below");
    }

    #[test]
    fn keymap_tab_shows_one_group_with_no_arrow() {
        let c = parse("keymap-tab").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("Clear the draft, then quit"));
        assert!(!t.contains("Copy the focused item"), "another group's row leaks");
        assert!(!t.contains("\u{258c}  \u{2193} "), "arrow with nothing below");
        let buf = buffer(&c, 160, 48);
        let y = t.split('\n').position(|l| l.contains("Start a new session")).unwrap();
        assert!(buf[(5, y as u16)].bg == crate::BLUE, "no bar on the tab's first row");
    }

    #[test]
    fn keymap_search_narrows_to_the_four_session_rows() {
        let c = parse("keymap-search").unwrap();
        let t = text(&c, 160, 48);
        for a in [
            "Start a new session",
            "Switch to the session of rail card N",
            "Delete the selected exited session in the session list",
            "Choose in the model picker for this session only",
        ] {
            // Wrapping may split a row, so every word reads, not whole lines.
            for w in a.split(' ') {
                assert!(t.contains(w), "missing {w}");
            }
        }
        assert!(!t.contains("Copy the focused item"), "a ruled-out row leaks");
        assert!(t.contains("session"), "the query is not shown");
        assert!(!t.contains("\u{258c}  \u{2193} "), "arrow with nothing below");
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
    fn narrow_opens_scrolled_with_how_much_hides() {
        let t = text(&parse("keymap-narrow").unwrap(), 100, 40);
        assert!(t.contains("\u{2191} 10 more"), "scroll offset");
        assert!(t.contains("\u{258c}  \u{2193} "), "scroll arrow");
        assert!(!t.contains("\u{2193} 0 more"), "bottom hidden");
    }

    #[test]
    fn quit_bars_its_default_with_a_keyless_foot() {
        let c = parse("quit").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("2 sessions working"));
        assert!(t.contains("\u{203a} "));
        assert!(t.contains("leave them running"));
        assert!(!t.contains("default"), "the rejected marker is back");
        let foot = line_with(&t, "click a choice");
        assert!(!foot.contains("enter"), "foot repeats a choice");
        assert!(!foot.contains("esc"), "foot repeats a choice");
        let buf = buffer(&c, 160, 48);
        let y = t.split('\n').position(|l| l.contains("leave them running")).unwrap();
        assert!((0..160).any(|x| buf[(x, y as u16)].bg == crate::BLUE), "no bar on enter");
        // Only the default rides the bar.
        let barred = (0..48).filter(|&y| (0..160).any(|x| buf[(x, y)].bg == crate::BLUE)).count();
        assert_eq!(barred, 1, "more than enter is barred");
    }

    #[test]
    fn delete_names_the_session_and_its_cascade() {
        let t = text(&parse("delete").unwrap(), 160, 48);
        assert!(t.contains("docs: rail spec"));
        assert!(t.contains("$1.10"));
        assert!(t.contains("--cascade"));
        assert!(t.contains("enter deletes · esc keeps it"));
    }

    #[test]
    fn history_selects_its_first_match() {
        let t = text(&parse("history").unwrap(), 160, 48);
        assert!(t.contains("Prompt history"));
        assert!(t.contains("\u{258c}"));
        assert!(t.contains("\u{203a} "));
        assert!(t.contains("3 matches"));
        assert!(t.contains("enter recalls · esc closes"));
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
        assert!(t.contains("click \u{2715} or outside to close"));
        // The keys column wraps, so every word reads, not the whole path.
        for w in "click the overlay's \u{2715} or outside it".split(' ') {
            assert!(t.contains(w), "missing {w}");
        }
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
