//! The home screen and the workspace picker, for #1628.
//!
//! A throwaway look prototype: it draws home full screen (no panel, no rail,
//! centred) from fixtures in this file, with the palette, surface primitive
//! (`slab`) and roles of `super`. Each case renders one static frame and the
//! program waits for a key; Esc, q or Ctrl+C quits. The conversation view is
//! untouched: a home case replaces it.

use super::input::{Ev, Key, Mods, Mouse};
use super::{Act, Args, Term, Ui};
use super::{
    BI, BLUE, CYAN, ORANGE, RED, SEL, SState, bold, dim, fg, fit, lift, paint,
    row, slab, sp, state_glyph,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use crate::cases::{Case, Surface};
use std::io::{self, Write};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// Every `--home` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case { name: "empty", help: "no sessions yet: logo, input box, chips", check: "the pixel logo four rows tall with `0.0.1` dim on the last row; under it the large input box with `/? for shortcuts`, the chip row and `enter starts a session`; under the box one dim `No sessions yet` line and no headers; the key hint at the foot.", build: || Look { list: List::None, ..base() } },
    Case { name: "sessions", help: "live and past sessions in two-line rows", check: "a bold `Live sessions` header with `2 waiting on you` in orange, then five two-line live rows; one blank row, then a bold `Past sessions` header with `/resume lists all`, then six two-line exited rows; line one is the glyph, the name or first prompt, its age and its verb, line two dim `id · turns · branch · spend` with the workspace segment and the wait.", build: || base() },
    Case { name: "live-only", help: "live sessions only, a still frame", check: "the `Live sessions` header and its five two-line rows; no `Past sessions` header.", build: || Look { list: List::Live, ..base() } },
    Case { name: "past-only", help: "past sessions only, a still frame", check: "the `Past sessions` header and its six two-line rows; no `Live sessions` header.", build: || Look { list: List::Past, ..base() } },
    Case { name: "selected", help: "the first live row selected, a still frame", check: "the first live row with the blue `▸` marker, its prompt bold and its verb blue; the other rows unmarked.", build: || Look { sel: Some(0), ..base() } },
    Case { name: "live", help: "the interactive home: type, pick, open rows", check: "not a still frame: `--home` with no case runs the event loop, and this name only names it in `--help`.", build: base },
    Case { name: "hover-workspace", help: "the workspace chip hovered, a still frame", check: "a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Workspace), ..base() } },
    Case { name: "hover-worktree", help: "the worktree switch hovered, a still frame", check: "a still frame of the hover tint: the switch chip should sit lighter with its ● still blue.", build: || Look { hover: Some(Hover::Worktree), ..base() } },
    Case { name: "hover-model", help: "the model chip hovered, a still frame", check: "a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Model), ..base() } },
    Case { name: "hover-thinking", help: "the thinking chip hovered, a still frame", check: "a still frame of the hover tint: the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Thinking), ..base() } },
    Case { name: "worktree-on", help: "new worktree switched on", check: "the switch should read `[● new worktree]` in blue.", build: || base() },
    Case { name: "worktree-off", help: "new worktree switched off", check: "the switch should read `[○ new worktree]` dim.", build: || Look { worktree: false, ..base() } },
    Case { name: "picker-recent", help: "workspace picker over home, recents", check: "the picker should float centred over home with ▄ ▀ edges and the ▌ stripe; `Workspaces` bold accent; four recent workspaces, the first `›` on a full-width accent bar; a bold-key legend foot.", build: || Look { picker: Some(Picker::Recent), ..base() } },
    Case { name: "picker-typed", help: "workspace picker with a typed path", check: "the typed row should read `› ~/work/fi█` with `fiber` and `fiber-worktrees` under it, the first `›` on the accent bar; the recents below dimmed; same frame and legend foot.", build: || Look { picker: Some(Picker::Typed), ..base() } },
];

/// `--home`, for `--help` and `check/home.md`.
pub(crate) const SURFACE: Surface = Surface { flag: "--home", file: "home", title: "Home (#1628)", docs: || crate::cases::docs(CASES) };

/// The home input box and session list width at 160 columns.
const HOME_W: usize = 84;
/// The fixture's version, as docs/tui.md's logo reads.
const VERSION: &str = "0.0.1";
/// The logo's top row: the four-row logo stands at rows 2..6.
const LOGO_Y: usize = 2;

/// The fewest rows that still fit the four-row logo: the logo, one blank,
/// the input box, then one list line, the hint row and the last row.
/// Computed from what `input_box` returns, not typed in.
fn tall_min() -> usize {
    LOGO_Y + 4 + 1 + input_box(&base(), HOME_W).len() + 3
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hover {
    Workspace,
    Worktree,
    Model,
    Thinking,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Picker {
    Recent,
    Typed,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum List {
    None,
    Live,
    Past,
    Both,
}

struct Look {
    list: List,
    /// the selected session in live-then-past order, if any
    sel: Option<usize>,
    hover: Option<Hover>,
    worktree: bool,
    picker: Option<Picker>,
    /// the workspace picker's selection, an index into `RECENTS`
    wsel: usize,
    /// the chosen recent workspace, shown on the workspace chip
    workspace: &'static str,
    /// the chosen model and thinking level, shown on their chips
    model: &'static str,
    level: &'static str,
    /// the draft, the completion panel and the model picker
    ui: Ui,
    /// the draft Esc dismissed the completion panel on: `sync` stays shut
    /// until the draft changes
    dismissed: Option<String>,
}

fn base() -> Look {
    let (model, level) =
        crate::model_picker::chosen(&crate::model_picker::for_case("list"));
    Look {
        list: List::Both,
        sel: None,
        hover: None,
        worktree: true,
        picker: None,
        wsel: 0,
        workspace: RECENTS[0],
        model,
        level,
        ui: Ui::default(),
        dismissed: None,
    }
}

/// A session on home's list: live ones carry their rail state, exited ones
/// none. The prompt is the name, or the first prompt when it has none; the
/// note is what it waits on, if anything. `ws` is the workspace's last
/// path segment, for a session outside the launch project (`~/work/fiber`);
/// `ws: None` is the launch project. `foreign` is a session whose
/// `schema_version` this terminal cannot read: it says `cannot attach`.
/// Fixture constants; home talks to no hub.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Live(SState),
    Exited,
}

struct Session {
    id: &'static str,
    prompt: &'static str,
    age: &'static str,
    turns: u32,
    branch: &'static str,
    spend: &'static str,
    ws: Option<&'static str>,
    note: &'static str,
    state: State,
    foreign: bool,
}

/// Live sessions, waiting first.
const LIVE: &[Session] = &[
    Session {
        id: "s_7f20aa",
        prompt: "cut the 0.0.1 release",
        age: "2h ago",
        turns: 5,
        branch: "release/0.0.1",
        spend: "$1.10",
        ws: None,
        note: "approval: shell cargo publish --dry-run",
        state: State::Live(SState::NeedsInput),
        foreign: false,
    },
    Session {
        id: "s_3ab702",
        prompt: "audit the event log",
        age: "45m ago",
        turns: 3,
        branch: "audit/events",
        spend: "$0.35",
        ws: None,
        note: "question: 2 of 3 answered",
        state: State::Live(SState::NeedsInput),
        foreign: false,
    },
    Session {
        id: "s_61c9d0",
        prompt: "speed up the render loop",
        age: "12m ago",
        turns: 2,
        branch: "perf/render",
        spend: "$0.58",
        ws: None,
        note: "",
        state: State::Live(SState::Working),
        foreign: false,
    },
    Session {
        id: "s_88f310",
        prompt: "port the router to hyper",
        age: "1h ago",
        turns: 8,
        branch: "port/hyper",
        spend: "$2.02",
        ws: None,
        note: "",
        state: State::Live(SState::Working),
        foreign: true,
    },
    Session {
        id: "s_5e91b4",
        prompt: "probe the lsp",
        age: "3h ago",
        turns: 3,
        branch: "spike/lsp",
        spend: "$0.05",
        ws: None,
        note: "stopped without exiting; 1 job orphaned on resume",
        state: State::Live(SState::Crashed),
        foreign: false,
    },
];

/// Recently exited sessions.
const PAST: &[Session] = &[
    Session {
        id: "s_9f31ac",
        prompt: "fix flaky lock test",
        age: "3h ago",
        turns: 4,
        branch: "fix/flaky-lock",
        spend: "$0.42",
        ws: None,
        note: "",
        state: State::Exited,
        foreign: false,
    },
    Session {
        id: "s_44d2e1",
        prompt: "docs: rail spec",
        age: "5h ago",
        turns: 6,
        branch: "docs/rail-692",
        spend: "$1.10",
        ws: None,
        note: "",
        state: State::Exited,
        foreign: false,
    },
    Session {
        id: "s_77aa01",
        prompt: "migrate every provider adapter to the new streaming contract",
        age: "1d ago",
        turns: 12,
        branch: "migrate/streaming",
        spend: "$2.31",
        ws: Some("pi-rig"),
        note: "",
        state: State::Exited,
        foreign: false,
    },
    Session {
        id: "s_50c9d2",
        prompt: "how do I backfill embeddings for old sessions?",
        age: "2d ago",
        turns: 9,
        branch: "backfill/embed",
        spend: "$0.87",
        ws: Some("fiber-worktrees"),
        note: "continues s_2b8e11 from seq 812",
        state: State::Exited,
        foreign: false,
    },
    Session {
        id: "s_08f3b9",
        prompt: "bump ratatui",
        age: "3d ago",
        turns: 1,
        branch: "deps/ratatui-0.30",
        spend: "$0.08",
        ws: None,
        note: "",
        state: State::Exited,
        foreign: false,
    },
    Session {
        id: "s_6d1e44",
        prompt: "rewrite onboarding tour",
        age: "4d ago",
        turns: 7,
        branch: "tour/rewrite",
        spend: "$0.35",
        ws: Some("beacon"),
        note: "",
        state: State::Exited,
        foreign: false,
    },
];

/// Recent workspaces, from `recent.jsonl`.
const RECENTS: &[&str] = &[
    "~/work/fiber",
    "~/work/fiber-worktrees",
    "~/work/beacon",
    "~/work/pi-rig",
];
/// The fixture's typed prefix and the two completions it shows.
const TYPED: &str = "~/work/fi";
const COMPLETIONS: &[&str] = &["fiber", "fiber-worktrees"];

// ============================================================ pieces

/// One chip: bracketed text on the raised surface, lighter under the pointer.
fn chip(text: &str, st: Style, hovered: bool) -> Span<'static> {
    let bg = if hovered { lift(SEL) } else { SEL };
    sp(format!("[{text}]"), st.bg(bg))
}

/// The chip row's clickable chips: target, text, style and hover.
fn chips(c: &Look) -> Vec<(Target, String, Style, bool)> {
    let h = |k: Hover| c.hover == Some(k);
    // The chips start from the model picker's current model, so a pick
    // agrees with them; a recorded pick moves them.
    let (worktree, wst) = if c.worktree {
        ("● new worktree", fg(BLUE))
    } else {
        ("○ new worktree", dim())
    };
    vec![
        (Target::Workspace, format!("▣ {}", c.workspace), Style::new(), h(Hover::Workspace)),
        (Target::Worktree, worktree.into(), wst, h(Hover::Worktree)),
        (Target::Model, c.model.into(), fg(CYAN), h(Hover::Model)),
        (Target::Thinking, c.level.into(), fg(ORANGE), h(Hover::Thinking)),
    ]
}

fn chip_row(c: &Look) -> Vec<Span<'static>> {
    let mut s = vec![sp("▌ ", fg(BLUE))];
    for (i, (_, text, st, hovered)) in chips(c).iter().enumerate() {
        if i > 0 {
            s.push(sp(" ", Style::new()));
        }
        s.push(chip(text, *st, *hovered));
    }
    // No right-aligned tail: when long chips overflow, the hint gives
    // way, never the chips.
    s.push(sp("  ", Style::new()));
    s.push(sp("enter starts a session", dim()));
    s
}

/// The chip row's click targets as row-relative cell ranges.
fn chip_hits(c: &Look) -> Vec<(usize, usize, Target)> {
    let mut x = "▌ ".width();
    let mut out = vec![];
    for (i, (t, text, _, _)) in chips(c).iter().enumerate() {
        if i > 0 {
            x += 1;
        }
        let w = text.width() + 2;
        out.push((x, x + w, *t));
        x += w;
    }
    out
}

/// The large input box: the draft with its cursor, or the
/// `/? for shortcuts` placeholder when empty, over the chip row,
/// with the prototype's ▌ stripe.
fn input_box(c: &Look, w: usize) -> Vec<super::Row> {
    let stripe = || sp("▌", fg(BLUE));
    let blank = || row(vec![stripe()]);
    let mut prompt = vec![stripe(), sp(" ", Style::new()), sp("› ", fg(CYAN))];
    if c.ui.input.is_empty() {
        prompt.push(sp("/? for shortcuts", dim()));
    } else {
        prompt.push(sp(c.ui.input.clone(), Style::new()));
    }
    prompt.push(sp("█", dim()));
    slab(
        vec![row(prompt), blank(), blank(), row(chip_row(c))],
        BI,
        None,
        w,
    )
}

/// What Enter or a click on a row does: a live session attaches, a crashed
/// or exited one resumes, and a foreign one cannot attach at all.
fn verb(s: &Session) -> &'static str {
    if s.foreign {
        "cannot attach"
    } else if matches!(s.state, State::Live(SState::Crashed) | State::Exited) {
        "resume"
    } else {
        "attach"
    }
}

/// A session row's first line at width `rw`: the marker, the glyph and a
/// space, the prompt fitted to `rw - 4 - 24`, then the age right-aligned
/// in 9 cells dim, two spaces, and the verb left-aligned in 13 cells. A
/// long prompt is cut before the age field.
fn session_line_one(s: &Session, selected: bool, rw: usize) -> Vec<Span<'static>> {
    let mut line = if selected {
        vec![sp("▸ ", fg(BLUE))]
    } else {
        vec![sp("  ", Style::new())]
    };
    line.push(match s.state {
        State::Live(st) => state_glyph(st, 0, true),
        State::Exited => sp("○", dim()),
    });
    line.push(sp(" ", Style::new()));
    let prompt = if selected {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    line.extend(fit(&[sp(s.prompt, prompt)], rw.saturating_sub(28)));
    line.push(sp(format!("{:>9}", s.age), dim()));
    line.push(sp("  ", Style::new()));
    let verb_style = if s.foreign {
        fg(RED).add_modifier(Modifier::DIM)
    } else if selected {
        fg(BLUE)
    } else {
        dim()
    };
    line.push(sp(format!("{:<13}", verb(s)), verb_style));
    line
}

/// A session row's second line: four spaces, then dim `id · N turns ·
/// branch · spend`, the workspace's last segment outside the launch
/// project, and what it waits on when there is one: orange for a waiting
/// session, red dim for a crashed one, else dim.
fn session_line_two(s: &Session, rw: usize) -> Vec<Span<'static>> {
    let turns = format!("{} turn{}", s.turns, if s.turns == 1 { "" } else { "s" });
    let mut body = vec![sp(format!("{} · {turns} · {} · {}", s.id, s.branch, s.spend), dim())];
    if let Some(ws) = s.ws {
        body.push(sp(format!(" · {ws}"), dim()));
    }
    if !s.note.is_empty() {
        let st = match s.state {
            State::Live(SState::NeedsInput) => fg(ORANGE),
            State::Live(SState::Crashed) => fg(RED).add_modifier(Modifier::DIM),
            _ => dim(),
        };
        body.push(sp(format!(" · {}", s.note), st));
    }
    let mut line = vec![sp("    ", Style::new())];
    line.extend(fit(&body, rw.saturating_sub(4)));
    line
}

/// The live section's header: bold `Live sessions`, then, when any live
/// session waits, how many wait on the person, in orange.
fn live_header() -> Vec<Span<'static>> {
    let mut h = vec![sp("Live sessions", bold())];
    let waiting = LIVE
        .iter()
        .filter(|s| matches!(s.state, State::Live(SState::NeedsInput)))
        .count();
    if waiting > 0 {
        h.push(sp(format!("  {waiting} waiting on you"), fg(ORANGE)));
    }
    h
}

/// The past section's header: bold `Past sessions`, then where the whole
/// list lives, dim.
fn past_header() -> Vec<Span<'static>> {
    vec![sp("Past sessions", bold()), sp("  /resume lists all", dim())]
}

/// One list line: its left edge, its spans, and the session's index for
/// clicks. The blank between sections counts for the scroll but draws
/// nothing.
type Line = Option<(usize, Vec<Span<'static>>, Option<usize>)>;

/// Pushes one section's header and two-line rows onto the list's lines.
/// `at` counts sessions in live-then-past order; `sel_end` becomes the
/// line index just past the selected row's second line.
fn push_section(
    lines: &mut Vec<Line>,
    geom: (usize, usize),
    header: Vec<Span<'static>>,
    sessions: &[Session],
    sel: Option<usize>,
    at: &mut usize,
    sel_end: &mut usize,
) {
    let (x0, rw) = geom;
    lines.push(Some((x0 + 2, header, None)));
    for s in sessions {
        let selected = sel == Some(*at);
        if selected {
            *sel_end = lines.len() + 2;
        }
        let k = *at;
        *at += 1;
        lines.push(Some((x0 + 1, session_line_one(s, selected, rw), Some(k))));
        lines.push(Some((x0 + 1, session_line_two(s, rw), Some(k))));
    }
}

/// How far the list scrolls: the selected row's second line stays on
/// screen, so the offset is the line index just past it less the area.
/// Also scrolls the model picker over home, whose focused row stays
/// visible the same way.
fn list_offset(sel_end: usize, area: usize) -> usize {
    sel_end.saturating_sub(area)
}

/// The workspace picker's rows at an inner width: the typed-path row with
/// its completions over the recents, or recents alone. The selected recent
/// rides a full-width selection bar. The typed-path row is a still frame
/// only: typing never reaches it.
fn picker_body(p: Picker, wsel: usize) -> Vec<super::Row> {
    let mut out = vec![];
    if p == Picker::Typed {
        out.push(row(vec![
            sp("› ", fg(CYAN)),
            sp(TYPED, Style::new()),
            sp("█", dim()),
        ]));
        for (i, name) in COMPLETIONS.iter().enumerate() {
            if i == 0 {
                out.extend(super::panel::bar(vec![row(vec![
                    sp("› ", bold()),
                    sp(*name, bold()),
                ])]));
            } else {
                out.push(row(vec![sp("  ", Style::new()), sp(*name, Style::new())]));
            }
        }
        // The dim section header stands off from the completions above it.
        out.push(row(vec![]));
        out.push(row(vec![sp("── recents ──", dim())]));
    }
    for (i, r) in RECENTS.iter().enumerate() {
        let selected = p == Picker::Recent && i == wsel;
        if selected {
            out.extend(super::panel::bar(vec![row(vec![
                sp("› ", bold()),
                sp(r.to_string(), bold()),
            ])]));
        } else {
            out.push(row(vec![
                sp("  ", Style::new()),
                sp(
                    r.to_string(),
                    if p == Picker::Typed {
                        dim()
                    } else {
                        Style::new()
                    },
                ),
            ]));
        }
    }
    out
}

/// The workspace picker over home: recent workspaces, or the typed-path row
/// with its completions over the recents, in the shared panel frame with a
/// bold-key legend foot.
fn picker(p: Picker, wsel: usize, cols: usize) -> Vec<super::Row> {
    const TITLE: &str = "Workspaces";
    let body = picker_body(p, wsel);
    let natural = body
        .iter()
        .map(|r| super::width(&r.spans))
        .max()
        .unwrap_or(0)
        .max(TITLE.width())
        .max("↑↓ move · enter open · esc closes".width());
    let w = super::panel::fit_width(natural, 55, cols);
    super::panel::frame(
        Some(super::panel::title_row(TITLE, None)),
        body,
        Some(super::panel::footer_legend(&[("↑↓", "move"), ("enter", "open"), ("esc", "closes")])),
        w,
    )
}

// ============================================================ frame
/// A row painted at its own offset and width, so a slab's background stays
/// within its edges instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

/// What a click lands on: home-local targets; the model picker's rows
/// reuse its own `Act`.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Workspace,
    Worktree,
    Model,
    Thinking,
    /// a session row in live-then-past order: both its lines hit
    Session(usize),
    /// a recent workspace in the open workspace picker
    Recent(usize),
    /// a model picker row or chip
    Picker(Act),
}

/// One frame: what is on screen, and what a click lands on.
/// Hits are `(row, start cell, end-exclusive cell, target)`.
struct Frame {
    screen: Vec<Vec<Placed>>,
    hits: Vec<(u16, u16, u16, Target)>,
}

/// What a key or a click asks the home loop to do.
enum Step {
    Stay,
    Quit,
    Conversation,
}

/// Where the home loop went: out, or into the conversation view.
pub(crate) enum Done {
    Quit(String),
    Conversation,
}

fn put(
    screen: &mut [Vec<Placed>],
    y: usize,
    x: usize,
    w: usize,
    spans: Vec<Span<'static>>,
    bg: Option<Color>,
) {
    if y < screen.len() {
        screen[y] = vec![Placed {
            x: x as u16,
            w: w as u16,
            row: super::Row {
                spans: fit(&spans, w),
                bg,
                ..Default::default()
            },
        }];
    }
}

fn frame(c: &Look, cols: usize, rows: usize, image: bool) -> Frame {
    let blank = || vec![Placed {
        x: 0,
        w: cols as u16,
        row: row(vec![]),
    }];
    let mut screen: Vec<Vec<Placed>> = (0..rows).map(|_| blank()).collect();
    let mut hits: Vec<(u16, u16, u16, Target)> = vec![];
    // The model picker opens over home as a full-screen swapped view, as
    // `/model` does in the conversation.
    if let Some(p) = &c.ui.picker {
        let mut all = vec![super::view_header("Models", "/model")];
        all.extend(crate::model_picker::view(p, cols));
        // The focused row stays visible: its index, then the header's row.
        let at = all
            .iter()
            .position(|r| r.act == Some(Act::Pick(p.focus)))
            .unwrap_or(0);
        let off = list_offset(at + 1, rows.saturating_sub(1));
        for (k, r) in all.iter().enumerate().skip(off).take(rows) {
            let y = (k - off) as u16;
            let bg = r.bg;
            put(&mut screen, y as usize, 0, cols, r.spans.clone(), bg);
            // A model row's name focuses it: its whole row is a click
            // target, while its chips keep their narrower targets below.
            if let Some(act @ (Act::Back | Act::Pick(_))) = r.act {
                hits.push((y, 0, cols as u16, Target::Picker(act)));
            }
            for &(a, b, act) in &r.hot {
                hits.push((y, a, b, Target::Picker(act)));
            }
        }
        return Frame { screen, hits };
    }
    let w = HOME_W.min(cols.saturating_sub(4)).max(20);
    let x0 = cols.saturating_sub(w) / 2;
    let mut y = LOGO_Y;
    if rows >= tall_min() {
        for l in crate::logo::rows(VERSION, image) {
            put(&mut screen, y, x0, w, l, None);
            y += 1;
        }
    } else {
        // Too short for four rows: the one-row logo, and everything below
        // moves up three rows with it.
        put(&mut screen, y, x0, w, crate::logo::one_row(VERSION), None);
        y += 1;
    }
    y += 1;
    let box_top = y;
    for r in input_box(c, w) {
        let bg = r.bg;
        put(&mut screen, y, x0, w, r.spans, bg);
        y += 1;
    }
    // The chip row is the box's fifth row: its chips are click targets,
    // but only while no workspace picker covers them. An open picker
    // owns every click: background targets do nothing.
    if c.picker.is_none() {
        for (a, b, t) in chip_hits(c) {
            hits.push(((box_top + 4) as u16, (x0 + a) as u16, (x0 + b) as u16, t));
        }
    }
    // The `/` and `@` panel draws directly above the input box, entries
    // first: focus starts at the top, so when the panel is taller than
    // the room above the box its foot gives way, never its head.
    if let Some(cp) = &c.ui.completions {
        let panel = crate::completions::view(cp, &c.ui.input, w);
        let keep = panel.len().min(box_top);
        for (k, r) in panel.iter().take(keep).enumerate() {
            let bg = r.bg;
            put(&mut screen, box_top - keep + k, x0, w, r.spans.clone(), bg);
        }
    }
    // Two blank rows under the box, then the list: live sessions under one
    // header, exited ones under another, each row two lines. The list's
    // lines scroll as one block above the key hint; an empty section draws
    // no header, and with no sessions at all the dim line stays.
    y += 2;
    let live: &[Session] = match c.list {
        List::Live | List::Both => LIVE,
        _ => &[],
    };
    let past: &[Session] = match c.list {
        List::Past | List::Both => PAST,
        _ => &[],
    };
    let rw = w.saturating_sub(2);
    if live.is_empty() && past.is_empty() {
        put(
            &mut screen,
            y,
            x0,
            w,
            fit(
                &[sp(
                    "No sessions yet — type a prompt above and press Enter.",
                    dim(),
                )],
                w,
            ),
            None,
        );
    } else {
        // Each entry is the line's left edge, its spans, and the session's
        // index for clicks; the blank between sections counts for the
        // scroll but draws nothing. The session index is the row's
        // position in live-then-past order.
        let mut lines: Vec<Line> = vec![];
        let (mut at, mut sel_end) = (0, 0);
        if !live.is_empty() {
            push_section(&mut lines, (x0, rw), live_header(), live, c.sel, &mut at, &mut sel_end);
        }
        if !live.is_empty() && !past.is_empty() {
            lines.push(None);
        }
        if !past.is_empty() {
            push_section(&mut lines, (x0, rw), past_header(), past, c.sel, &mut at, &mut sel_end);
        }
        if c.sel.is_some_and(|s| s >= at) {
            sel_end = lines.len();
        }
        let list_top = y;
        let area = rows.saturating_sub(2).saturating_sub(list_top);
        let off = list_offset(sel_end, area);
        for (k, l) in lines.iter().enumerate().skip(off).take(area) {
            if let Some((x, spans, sess)) = l {
                let sy = list_top + k - off;
                put(&mut screen, sy, *x, rw, spans.clone(), None);
                // An open workspace picker covers the list: its blank
                // rows and footer are not session targets, so clicking
                // them never enters a conversation.
                if c.picker.is_none() && let Some(k) = sess {
                    hits.push((sy as u16, *x as u16, (*x + rw) as u16, Target::Session(*k)));
                }
            }
        }
    }
    let hint = "enter starts a session · ↑↓ select · q quits";
    if rows >= 2 {
        let pad = cols.saturating_sub(hint.width()) / 2;
        put(
            &mut screen,
            rows - 2,
            0,
            cols,
            vec![sp(" ".repeat(pad), Style::new()), sp(hint, dim())],
            None,
        );
    }
    if let Some(p) = c.picker {
        let rows = picker(p, c.wsel, cols);
        let pw = super::width(&rows[0].spans);
        let px0 = super::panel::x_for(pw, cols);
        for (k, r) in rows.into_iter().enumerate() {
            let bg = r.bg;
            let text: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
            if let Some(i) = recent_at(&text) {
                hits.push(((12 + k) as u16, px0 as u16, (px0 + pw) as u16, Target::Recent(i)));
            }
            put(&mut screen, 12 + k, px0, pw, r.spans, bg);
        }
    }
    Frame { screen, hits }
}

/// The recent workspace a picker row names, if any: the row's text past
/// its `› ` or two-space marker, matched exactly.
fn recent_at(text: &str) -> Option<usize> {
    let t = text.trim();
    let t = t.strip_prefix("› ").unwrap_or(t);
    let t = t.strip_prefix("  ").unwrap_or(t);
    RECENTS.iter().position(|&r| r == t)
}

/// Whether the kitty image covers the logo on this draw: supported, past
/// the first draw (the pixel logo draws first and the image replaces it,
/// so the first frame never waits), no overlay open (an image at z=0
/// draws above text and would cover a panel), the four-row logo drawn,
/// and room for its 32 cells.
fn image_shown(supported: bool, first: bool, c: &Look, cols: usize, rows: usize) -> bool {
    if !supported || first {
        return false;
    }
    if c.picker.is_some() || c.ui.picker.is_some() || c.ui.completions.is_some() {
        return false;
    }
    if rows < tall_min() {
        return false;
    }
    HOME_W.min(cols.saturating_sub(4)).max(20) >= crate::logo::CELLS_W as usize
}

/// The image's wire state across draws: whether its bytes are up, and
/// whether a placement is up. One pure state machine decides every
/// escape per draw.
#[derive(Default)]
struct Image {
    transmitted: bool,
    placed: bool,
}

impl Image {
    /// The escapes for this draw: when shown, the transmit (once per run)
    /// then the placement; when hidden, the hide (once per placement).
    fn escapes(&mut self, shown: bool, x: u16, y: u16) -> String {
        if shown {
            let mut out = String::new();
            if !self.transmitted {
                out.push_str(&crate::logo::transmit());
                self.transmitted = true;
            }
            out.push_str(&crate::logo::place(x, y));
            self.placed = true;
            out
        } else if self.placed {
            self.placed = false;
            crate::logo::HIDE.to_string()
        } else {
            String::new()
        }
    }
}

/// How long the home loop waits for input: the reader's Esc deadline, as
/// the conversation loop does. `wait(None)` would hold a lone Esc in the
/// reader until the next byte, so Esc would not close a panel or quit
/// until another key.
fn wait_for(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|d| d.saturating_duration_since(now))
}

fn draw(term: &mut Term, c: &Look, img: &mut Image, supported: bool, first: bool) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let w = HOME_W.min(cols.saturating_sub(4) as usize).max(20);
    let shown = image_shown(
        supported,
        first,
        c,
        cols as usize,
        rows as usize,
    );
    let esc = img.escapes(shown, (cols.saturating_sub(w as u16)) / 2, LOGO_Y as u16);
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|fr| {
        let buf = fr.buffer_mut();
        for (y, placements) in frame(c, cols as usize, rows as usize, shown).screen.iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
    })?;
    let be = term.backend_mut();
    be.write_all(esc.as_bytes())?;
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Frees the image: every exit from home leaves no placement behind.
fn free(term: &mut Term) {
    let be = term.backend_mut();
    let _ = be.write_all(crate::logo::FREE.as_bytes());
    let _ = be.flush();
}

/// The look's sessions in live-then-past order.
fn sessions_of(c: &Look) -> Vec<&Session> {
    let live: &[Session] = match c.list {
        List::Live | List::Both => LIVE,
        _ => &[],
    };
    let past: &[Session] = match c.list {
        List::Past | List::Both => PAST,
        _ => &[],
    };
    live.iter().chain(past.iter()).collect()
}

/// Closes the `/` and `@` panel, unless Esc dismissed it on this draft:
/// without the dismissal the panel reopens at once, since the draft
/// still starts with `/`.
fn end_key(c: &mut Look, before: &str) {
    if c.ui.input != before {
        c.dismissed = None;
    }
    if c.dismissed.as_deref() != Some(c.ui.input.as_str()) {
        crate::completions::sync(&mut c.ui, false);
    }
}

/// The live home's keys, first match wins: Ctrl+C quits; the workspace
/// picker takes arrows, Enter and Esc and ignores the rest; the model
/// picker records on Enter and otherwise takes the picker's keys; Ctrl+L
/// and `/model` open the model picker; `/` opens slash completions, and
/// `/mo` Enter completes to `/model ` and opens the picker; Esc quits;
/// arrows walk the list; Enter switches on a draft or a row; Backspace
/// pops; `q` on an empty draft quits; other text fills the box.
fn on_key(c: &mut Look, k: Key, m: Mods) -> Step {
    // Ctrl+C quits, always.
    if k == Key::Char('c') && m.ctrl {
        return Step::Quit;
    }
    // The workspace picker takes its own keys; the rest are ignored.
    if c.picker.is_some() {
        match k {
            Key::Up if !m.alt && !m.ctrl => c.wsel = c.wsel.saturating_sub(1),
            Key::Down if !m.alt && !m.ctrl => c.wsel = (c.wsel + 1).min(RECENTS.len() - 1),
            Key::Enter if !m.ctrl && !m.alt && !m.sup => {
                c.workspace = RECENTS[c.wsel];
                c.picker = None;
            }
            Key::Esc => c.picker = None,
            _ => {}
        }
        return Step::Stay;
    }
    // The model picker records its choice on Enter, then closes.
    if c.ui.picker.is_some() {
        if k == Key::Enter && !m.ctrl && !m.alt && !m.sup {
            let (model, level) =
                crate::model_picker::chosen(c.ui.picker.as_ref().unwrap());
            c.model = model;
            c.level = level;
        }
        crate::model_picker::on_key(&mut c.ui, k, m);
        return Step::Stay;
    }
    let before = c.ui.input.clone();
    if crate::model_picker::on_key(&mut c.ui, k, m) {
        end_key(c, &before);
        return Step::Stay;
    }
    let had = c.ui.completions.is_some();
    if crate::completions::on_key(&mut c.ui, k, m) {
        if k == Key::Esc && had {
            c.dismissed = Some(c.ui.input.clone());
        }
        end_key(c, &before);
        return Step::Stay;
    }
    // Enter that just completed `/mo` to `/model ` opens the picker.
    if had && k == Key::Enter && crate::model_picker::on_key(&mut c.ui, k, m) {
        end_key(c, &before);
        return Step::Stay;
    }
    let plain = !m.ctrl && !m.alt && !m.sup;
    let step = match k {
        Key::Esc => Step::Quit,
        Key::Down if !m.alt && !m.ctrl => {
            let n = sessions_of(c).len();
            c.sel = Some(match c.sel {
                None if n > 0 => 0,
                Some(i) => i.saturating_add(1).min(n.saturating_sub(1)),
                None => return Step::Stay,
            });
            Step::Stay
        }
        Key::Up if !m.alt && !m.ctrl => {
            c.sel = match c.sel {
                Some(0) | None => None,
                Some(i) => Some(i - 1),
            };
            Step::Stay
        }
        Key::Enter if plain => {
            if !c.ui.input.trim().is_empty() {
                Step::Conversation
            } else {
                match c.sel.and_then(|i| sessions_of(c).get(i).copied()) {
                    Some(s) if !s.foreign => Step::Conversation,
                    _ => Step::Stay,
                }
            }
        }
        Key::Backspace if plain => {
            c.ui.input.pop();
            Step::Stay
        }
        Key::Char('q') if plain && c.ui.input.is_empty() => Step::Quit,
        Key::Char(ch) if plain => {
            c.ui.input.push(ch);
            c.sel = None;
            Step::Stay
        }
        _ => Step::Stay,
    };
    end_key(c, &before);
    step
}

/// The live home's clicks: the chips open their pickers, the worktree
/// switch toggles, a session row switches unless foreign, and a recent
/// chooses its workspace.
fn on_click(c: &mut Look, x: u16, y: u16, cols: usize, rows: usize) -> Step {
    let fr = frame(c, cols, rows, false);
    // Later hits sit above earlier ones: the picker floats over the list.
    let hit = fr.hits.iter().rev().find(|&&(hy, x0, x1, _)| hy == y && x >= x0 && x < x1);
    let Some((_, _, _, t)) = hit.copied() else {
        return Step::Stay;
    };
    match t {
        Target::Workspace => {
            c.picker = Some(Picker::Recent);
            c.wsel = 0;
            Step::Stay
        }
        Target::Worktree => {
            c.worktree = !c.worktree;
            Step::Stay
        }
        Target::Model => {
            c.ui.picker = Some(crate::model_picker::opened_at(c.model, None));
            c.ui.vscroll = 0;
            Step::Stay
        }
        Target::Thinking => {
            c.ui.picker =
                Some(crate::model_picker::opened_at(c.model, Some(c.level)));
            c.ui.vscroll = 0;
            Step::Stay
        }
        Target::Session(i) => {
            let foreign = sessions_of(c).get(i).is_some_and(|s| s.foreign);
            if foreign { Step::Stay } else { Step::Conversation }
        }
        Target::Recent(i) => {
            c.workspace = RECENTS[i];
            c.wsel = i;
            c.picker = None;
            Step::Stay
        }
        Target::Picker(Act::Back) => {
            c.ui.picker = None;
            Step::Stay
        }
        Target::Picker(act) => {
            crate::model_picker::click(&mut c.ui, act);
            Step::Stay
        }
    }
}

/// Draws home and waits for a key: `--home` bare or `live` runs the
/// interactive home, the other cases one still frame each. Mutually
/// exclusive with the conversation view: the fixture replay never draws.
/// Every result passes through here, and here frees the image, so a
/// propagated error never leaves a placement behind.
pub(crate) fn run_home(a: &Args, term: &mut Term) -> io::Result<Done> {
    let r = run_home_inner(a, term);
    free(term);
    r
}

fn run_home_inner(a: &Args, term: &mut Term) -> io::Result<Done> {
    let name = a.home.clone().unwrap_or("live".into());
    if name == "live" {
        return run_live(term);
    }
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --home case {name:?}; one of: {}", crate::cases::names(CASES)),
        ));
    };
    let supported = crate::logo::supported(|k| std::env::var(k).ok());
    let mut img = Image::default();
    // The first frame always draws the pixel logo; where supported, a
    // second draw at once transmits and places the image, so it shows
    // without a keystroke.
    draw(term, &c, &mut img, supported, true)?;
    if supported {
        draw(term, &c, &mut img, supported, false)?;
    }
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(wait_for(rd.deadline(), Instant::now()))?;
        if resized {
            draw(term, &c, &mut img, supported, false)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => {
                    return Ok(Done::Quit(format!("home {name}\n")));
                }
                Ev::Key(Key::Esc, _) => {
                    return Ok(Done::Quit(format!("home {name}\n")));
                }
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(Done::Quit(format!("home {name}\n")));
                }
                _ => {}
            }
        }
    }
}

/// The interactive home: typing fills the box, the pickers open over it,
/// arrows walk the session list, and Enter switches to the conversation.
fn run_live(term: &mut Term) -> io::Result<Done> {
    let mut c = base();
    let supported = crate::logo::supported(|k| std::env::var(k).ok());
    let mut img = Image::default();
    draw(term, &c, &mut img, supported, true)?;
    if supported {
        draw(term, &c, &mut img, supported, false)?;
    }
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(wait_for(rd.deadline(), Instant::now()))?;
        if resized {
            draw(term, &c, &mut img, supported, false)?;
        }
        for ev in evs {
            let step = match ev {
                Ev::Key(k, m) => on_key(&mut c, k, m),
                Ev::Mouse(Mouse::Down, x, y, _) => {
                    let size = term.size()?;
                    on_click(&mut c, x, y, size.width as usize, size.height as usize)
                }
                _ => Step::Stay,
            };
            match step {
                Step::Stay => {}
                Step::Quit => {
                    return Ok(Done::Quit("home live\n".into()));
                }
                Step::Conversation => {
                    return Ok(Done::Conversation);
                }
            }
            draw(term, &c, &mut img, supported, false)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|c| crate::cases::lookup(CASES, c.name).is_some()));
        assert_eq!(CASES.len(), 14);
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    /// `frame` painted with `paint` exactly as `draw` does.
    fn buffer(c: &Look, cols: u16, rows: u16) -> Buffer {
        painted(c, cols, rows, false)
    }

    /// `buffer` with the image shown, as the second startup draw paints it.
    fn buffer_image(c: &Look, cols: u16, rows: u16) -> Buffer {
        painted(c, cols, rows, true)
    }

    fn painted(c: &Look, cols: u16, rows: u16, image: bool) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        for (y, placements) in frame(c, cols as usize, rows as usize, image).screen.iter().enumerate() {
            for p in placements {
                paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    #[test]
    fn home_draws_the_pixel_logo_at_its_place() {
        // At 160 columns `w = 84` and `x0 = 38`.
        let c = crate::cases::lookup(CASES, "empty").unwrap();
        let buf = buffer(&c, 160, 48);
        assert_eq!(buf[(38, 2)].symbol(), "▀");
        assert_eq!(buf[(38, 2)].fg, BLUE);
        let mut text = String::new();
        for x in 71..76 {
            text.push_str(buf[(x, 5)].symbol());
            assert!(
                buf[(x, 5)].modifier.contains(Modifier::DIM),
                "version not dim at {x}"
            );
        }
        assert_eq!(text, "0.0.1");
        assert!(
            !buf.content.iter().any(|c| c.symbol() == "⌇"),
            "the hand-drawn logo is still drawn"
        );
    }

    #[test]
    fn image_shown_boundaries() {
        let plain = || crate::cases::lookup(CASES, "empty").unwrap();
        // Supported and past the first draw, nothing open: the image shows.
        assert!(image_shown(true, false, &plain(), 160, 48));
        // Without support, or on the first draw, the pixel logo stays.
        assert!(!image_shown(false, false, &plain(), 160, 48));
        assert!(!image_shown(true, true, &plain(), 160, 48));
        // An open overlay covers the logo's cells, so the image hides.
        assert!(!image_shown(
            true,
            false,
            &crate::cases::lookup(CASES, "picker-recent").unwrap(),
            160,
            48
        ));
        let mut model = plain();
        model.ui.picker = Some(crate::model_picker::for_case("list"));
        assert!(!image_shown(true, false, &model, 160, 48));
        let mut panel = plain();
        panel.ui.input = "/".into();
        panel.ui.completions = Some(crate::completions::for_case("slash"));
        assert!(!image_shown(true, false, &panel, 160, 48));
        // The image needs the four-row logo and its 32 cells.
        assert!(image_shown(true, false, &plain(), 160, tall_min()));
        assert!(!image_shown(true, false, &plain(), 160, tall_min() - 1));
        assert!(image_shown(true, false, &plain(), 36, 48));
        assert!(!image_shown(true, false, &plain(), 35, 48));
    }

    #[test]
    fn image_escapes_sequence() {
        let mut img = Image { transmitted: false, placed: false };
        assert_eq!(img.escapes(false, 38, 2), "");
        assert_eq!(
            img.escapes(true, 38, 2),
            crate::logo::transmit() + &crate::logo::place(38, 2)
        );
        assert_eq!(img.escapes(true, 38, 2), crate::logo::place(38, 2));
        assert_eq!(img.escapes(false, 38, 2), crate::logo::HIDE);
        assert_eq!(img.escapes(false, 38, 2), "");
        assert_eq!(img.escapes(true, 38, 2), crate::logo::place(38, 2));
    }

    #[test]
    fn wait_for_boundaries() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert_eq!(wait_for(None, now), None);
        assert_eq!(wait_for(Some(now + Duration::from_millis(30)), now), Some(Duration::from_millis(30)));
        assert_eq!(wait_for(Some(now), now), Some(Duration::ZERO));
        assert_eq!(wait_for(Some(now - Duration::from_millis(5)), now), Some(Duration::ZERO));
    }

    #[test]
    fn home_blanks_the_logo_under_the_image() {
        let c = crate::cases::lookup(CASES, "empty").unwrap();
        let buf = buffer_image(&c, 160, 48);
        for y in 2..6 {
            for x in 38..70 {
                assert_eq!(buf[(x, y)].symbol(), " ", "cell ({x}, {y})");
            }
        }
        let mut text = String::new();
        for x in 71..76 {
            text.push_str(buf[(x, 5)].symbol());
        }
        assert_eq!(text, "0.0.1");
    }

    #[test]
    fn short_screens_get_the_one_row_logo() {
        let tall = tall_min();
        let empty = || crate::cases::lookup(CASES, "empty").unwrap();
        let buf = buffer(&empty(), 160, tall as u16);
        assert_eq!(buf[(38, 2)].symbol(), "▀");
        assert_eq!(buf[(38, 2)].fg, BLUE);
        let buf = buffer(&empty(), 160, tall as u16 - 1);
        let mut got = String::new();
        for x in 38..51 {
            got.push_str(buf[(x, 2)].symbol());
        }
        assert_eq!(got, "⌇ fiber 0.0.1");
        // The input box's top edge sits three rows higher than in the tall frame.
        // The box edge is a full-width run of ▄; the logo's half blocks never
        // run that long.
        let edge = |rows: u16| {
            let t = text(&empty(), 160, rows as usize);
            t.split('\n')
                .position(|l| l.chars().filter(|&c| c == '▄').count() >= 40)
                .unwrap()
        };
        assert_eq!(edge(tall as u16) - edge(tall as u16 - 1), 3);
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
        frame(c, cols, rows, false)
            .screen
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

    /// The first screen row holding `needle`, reading cells left to right.
    fn find_row(buf: &Buffer, cols: u16, rows: u16, needle: &str) -> u16 {
        for y in 0..rows {
            let mut line = String::new();
            for x in 0..cols {
                line.push_str(buf[(x, y)].symbol());
            }
            if line.contains(needle) {
                return y;
            }
        }
        panic!("{needle:?} not on screen");
    }

    /// The symbols of one buffer run as text.
    fn run_text(buf: &Buffer, x: u16, y: u16, len: u16) -> String {
        (x..x + len).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// The home column of one text row as text.
    fn home_col(line: &str, x0: usize, w: usize) -> String {
        line.chars().skip(x0).take(w).collect()
    }

    fn sessions() -> Look {
        crate::cases::lookup(CASES, "sessions").unwrap()
    }

    #[test]
    fn both_headers_in_order() {
        // At 160 columns `w = 84` and `x0 = 38`: headers at 40, rows at 39.
        let buf = buffer(&sessions(), 160, 48);
        let live_y = find_row(&buf, 160, 48, "Live sessions");
        let past_y = find_row(&buf, 160, 48, "Past sessions");
        assert!(live_y < past_y, "the live header is not first");
        assert_eq!(run_text(&buf, 40, live_y, 13), "Live sessions");
        assert!(buf[(40, live_y)].modifier.contains(Modifier::BOLD));
        assert_eq!(run_text(&buf, 40, past_y, 13), "Past sessions");
        assert!(buf[(40, past_y)].modifier.contains(Modifier::BOLD));
        assert_eq!(run_text(&buf, 53, live_y, 18), "  2 waiting on you");
        for x in 55..69 {
            assert_eq!(buf[(x, live_y)].fg, ORANGE, "waiting count not orange at {x}");
        }
        assert!(run_text(&buf, 53, past_y, 19).contains("/resume lists all"));
    }

    #[test]
    fn two_blank_rows_under_the_box_and_one_between_sections() {
        let t = text(&sessions(), 160, 48);
        let lines: Vec<&str> = t.split('\n').collect();
        let edge = lines
            .iter()
            .rposition(|l| l.chars().filter(|&c| c == '▀').count() >= 40)
            .unwrap();
        assert!(home_col(lines[edge + 1], 38, 84).trim().is_empty(), "no first blank");
        assert!(home_col(lines[edge + 2], 38, 84).trim().is_empty(), "no second blank");
        let past = lines.iter().position(|l| l.contains("Past sessions")).unwrap();
        assert!(lines[past - 1].trim().is_empty(), "no blank over the past header");
        assert!(lines[past - 2].contains("s_5e91b4"), "the last live row is not above the blank");
    }

    #[test]
    fn waiting_row_tints() {
        let buf = buffer(&sessions(), 160, 48);
        let y2 = find_row(&buf, 160, 48, "s_7f20aa");
        let y1 = y2 - 1;
        assert_eq!(buf[(41, y1)].symbol(), "!");
        assert_eq!(buf[(41, y1)].fg, ORANGE);
        assert!(buf[(41, y1)].modifier.contains(Modifier::BOLD));
        assert_eq!(run_text(&buf, 43, y1, 21), "cut the 0.0.1 release");
        for x in 43..64 {
            assert!(!buf[(x, y1)].modifier.contains(Modifier::BOLD), "prompt went bold at {x}");
        }
        // The age sits right-aligned in its 9-cell field, the verb after two spaces.
        assert_eq!(run_text(&buf, 97, y1, 9), "   2h ago");
        for x in 97..106 {
            assert!(buf[(x, y1)].modifier.contains(Modifier::DIM), "age not dim at {x}");
        }
        assert_eq!(run_text(&buf, 108, y1, 13), "attach       ");
        assert!(buf[(108, y1)].modifier.contains(Modifier::DIM));
        assert_eq!(run_text(&buf, 43, y2, 34), "s_7f20aa · 5 turns · release/0.0.1");
        for x in 43..80 {
            assert!(buf[(x, y2)].modifier.contains(Modifier::DIM), "line two not dim at {x}");
        }
        let note_y = find_row(&buf, 160, 48, "approval:");
        assert_eq!(note_y, y2);
        for x in 88..97 {
            assert_eq!(buf[(x, y2)].fg, ORANGE, "the wait is not orange at {x}");
        }
    }

    #[test]
    fn crashed_row_tints() {
        let buf = buffer(&sessions(), 160, 48);
        let y2 = find_row(&buf, 160, 48, "s_5e91b4");
        let y1 = y2 - 1;
        assert_eq!(buf[(41, y1)].symbol(), "✗");
        assert_eq!(buf[(41, y1)].fg, RED);
        assert!(buf[(41, y1)].modifier.contains(Modifier::BOLD));
        assert_eq!(run_text(&buf, 108, y1, 13), "resume       ");
        let y = find_row(&buf, 160, 48, "stopped");
        assert_eq!(y, y2);
        assert_eq!(buf[(84, y2)].fg, RED);
        assert!(buf[(84, y2)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn foreign_row_says_cannot_attach() {
        let buf = buffer(&sessions(), 160, 48);
        let y1 = find_row(&buf, 160, 48, "s_88f310") - 1;
        assert_eq!(run_text(&buf, 108, y1, 13), "cannot attach");
        assert_eq!(buf[(108, y1)].fg, RED);
        assert!(buf[(108, y1)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn working_row_glyph() {
        let buf = buffer(&sessions(), 160, 48);
        let y1 = find_row(&buf, 160, 48, "s_61c9d0") - 1;
        assert_eq!(buf[(41, y1)].symbol(), "●");
        assert_eq!(buf[(41, y1)].fg, BLUE);
    }

    #[test]
    fn exited_row() {
        let buf = buffer(&sessions(), 160, 48);
        let y1 = find_row(&buf, 160, 48, "fix flaky");
        assert_eq!(buf[(41, y1)].symbol(), "○");
        assert!(buf[(41, y1)].modifier.contains(Modifier::DIM));
        assert_eq!(run_text(&buf, 108, y1, 13), "resume       ");
        let y2 = find_row(&buf, 160, 48, "$2.31");
        assert!(run_text(&buf, 43, y2, 60).contains("· pi-rig"));
        // A launch-project session carries no workspace segment.
        assert_eq!(
            run_text(&buf, 43, y1 + 1, 43).trim_end(),
            "s_9f31ac · 4 turns · fix/flaky-lock · $0.42"
        );
    }

    #[test]
    fn one_turn_is_singular() {
        let buf = buffer(&sessions(), 160, 48);
        let y2 = find_row(&buf, 160, 48, "bump ratatui") + 1;
        assert!(run_text(&buf, 43, y2, 60).contains("1 turn ·"));
        assert!(!run_text(&buf, 43, y2, 60).contains("1 turns"));
    }

    #[test]
    fn long_prompt_keeps_the_age_column() {
        let buf = buffer(&sessions(), 160, 48);
        let y_long = find_row(&buf, 160, 48, "migrate every");
        let y_short = find_row(&buf, 160, 48, "fix flaky");
        // Both verbs sit at the same columns: the long prompt was cut first.
        assert_eq!(run_text(&buf, 108, y_long, 13), "resume       ");
        assert_eq!(run_text(&buf, 108, y_short, 13), "resume       ");
        assert!(run_text(&buf, 43, y_long, 54).ends_with('…'));
    }

    #[test]
    fn selected_row() {
        let c = crate::cases::lookup(CASES, "selected").unwrap();
        let buf = buffer(&c, 160, 48);
        let y1 = find_row(&buf, 160, 48, "cut the 0.0.1 release");
        assert_eq!(buf[(39, y1)].symbol(), "▸");
        assert_eq!(buf[(39, y1)].fg, BLUE);
        for x in 43..64 {
            assert!(buf[(x, y1)].modifier.contains(Modifier::BOLD), "prompt not bold at {x}");
        }
        assert_eq!(run_text(&buf, 108, y1, 13), "attach       ");
        assert_eq!(buf[(108, y1)].fg, BLUE);
        let y2 = find_row(&buf, 160, 48, "audit the event");
        assert_eq!(run_text(&buf, 39, y2, 2), "  ");
    }

    #[test]
    fn live_only_has_no_past_header() {
        let c = crate::cases::lookup(CASES, "live-only").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("Live sessions"));
        assert!(!t.contains("Past sessions"));
    }

    #[test]
    fn past_only_has_no_live_header() {
        let c = crate::cases::lookup(CASES, "past-only").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("Past sessions"));
        assert!(!t.contains("Live sessions"));
    }

    #[test]
    fn empty_has_no_headers() {
        let c = crate::cases::lookup(CASES, "empty").unwrap();
        let t = text(&c, 160, 48);
        assert!(!t.contains("Live sessions"));
        assert!(!t.contains("Past sessions"));
        assert!(t.contains("No sessions yet"));
    }

    #[test]
    fn chips_start_at_the_current_model() {
        let buf = buffer(&sessions(), 160, 48);
        // The chip row starts at `x0`: scan for the model chip's start.
        let y = find_row(&buf, 160, 48, "[claude-opus-5-5]");
        let mut line = String::new();
        for x in 0..160 {
            line.push_str(buf[(x, y)].symbol());
        }
        let at = line.find("[claude-opus-5-5]").unwrap();
        let col = line[..at].chars().count() as u16;
        for x in col..col + 17 {
            assert_eq!(buf[(x, y)].fg, CYAN, "model chip not cyan at {x}");
        }
        let yh = find_row(&buf, 160, 48, "[high]");
        assert_eq!(yh, y);
        let ath = line.find("[high]").unwrap();
        let colh = line[..ath].chars().count() as u16;
        for x in colh..colh + 6 {
            assert_eq!(buf[(x, yh)].fg, ORANGE, "thinking chip not orange at {x}");
        }
    }

    #[test]
    fn list_offset_boundaries() {
        assert_eq!(list_offset(0, 10), 0);
        assert_eq!(list_offset(10, 10), 0);
        assert_eq!(list_offset(11, 10), 1);
        assert_eq!(list_offset(25, 10), 15);
    }

    #[test]
    fn selected_last_row_stays_visible() {
        let c = Look { sel: Some(10), ..base() };
        let buf = buffer(&c, 160, 30);
        let y1 = find_row(&buf, 160, 30, "rewrite onboarding tour");
        // Both lines sit above the hint row.
        assert!(y1 + 1 < 28, "the last row scrolled off");
    }

    fn live_look() -> Look {
        base()
    }

    fn press(c: &mut Look, k: Key) -> Step {
        on_key(c, k, Mods::default())
    }

    fn type_str(c: &mut Look, s: &str) {
        for ch in s.chars() {
            press(c, Key::Char(ch));
        }
    }

    /// The first click target for `t` at 160 by 48.
    fn hit(c: &Look, t: Target) -> (u16, u16) {
        let fr = frame(c, 160, 48, false);
        let (y, x0, _, _) = fr.hits.iter().find(|h| h.3 == t).unwrap();
        (*x0, *y)
    }

    fn session_hits(c: &Look, i: usize) -> Vec<(u16, u16)> {
        frame(c, 160, 48, false)
            .hits
            .iter()
            .filter(|h| h.3 == Target::Session(i))
            .map(|&(y, x0, _, _)| (x0, y))
            .collect()
    }

    #[test]
    fn typing_fills_the_box() {
        let mut c = live_look();
        press(&mut c, Key::Char('a'));
        press(&mut c, Key::Char('b'));
        let buf = buffer(&c, 160, 48);
        find_row(&buf, 160, 48, "› ab█");
        press(&mut c, Key::Backspace);
        let buf = buffer(&c, 160, 48);
        find_row(&buf, 160, 48, "› a█");
    }

    #[test]
    fn enter_with_a_draft_switches() {
        let mut c = live_look();
        type_str(&mut c, "fix it");
        assert!(matches!(press(&mut c, Key::Enter), Step::Conversation));
        let mut c = live_look();
        type_str(&mut c, " ");
        assert!(matches!(press(&mut c, Key::Enter), Step::Stay));
    }

    #[test]
    fn arrows_walk_the_list() {
        let mut c = live_look();
        press(&mut c, Key::Down);
        assert_eq!(c.sel, Some(0));
        press(&mut c, Key::Up);
        assert_eq!(c.sel, None);
        press(&mut c, Key::Up);
        assert_eq!(c.sel, None);
        c.sel = Some(10);
        press(&mut c, Key::Down);
        assert_eq!(c.sel, Some(10));
        let mut empty = Look { list: List::None, ..base() };
        press(&mut empty, Key::Down);
        assert_eq!(empty.sel, None);
        let mut c = live_look();
        press(&mut c, Key::Down);
        let buf = buffer(&c, 160, 48);
        let y1 = find_row(&buf, 160, 48, "cut the 0.0.1 release");
        assert_eq!(buf[(39, y1)].symbol(), "▸");
    }

    #[test]
    fn enter_opens_a_selected_row() {
        let mut c = live_look();
        c.sel = Some(0);
        assert!(matches!(press(&mut c, Key::Enter), Step::Conversation));
        // The foreign row cannot attach: Enter does nothing.
        let mut c = live_look();
        c.sel = Some(3);
        assert!(matches!(press(&mut c, Key::Enter), Step::Stay));
    }

    #[test]
    fn typing_clears_the_selection() {
        let mut c = live_look();
        c.sel = Some(2);
        press(&mut c, Key::Char('x'));
        assert_eq!(c.sel, None);
        assert_eq!(c.ui.input, "x");
    }

    #[test]
    fn q_quits_only_on_an_empty_draft() {
        let mut c = live_look();
        assert!(matches!(press(&mut c, Key::Char('q')), Step::Quit));
        let mut c = live_look();
        press(&mut c, Key::Char('a'));
        assert!(matches!(press(&mut c, Key::Char('q')), Step::Stay));
        assert_eq!(c.ui.input, "aq");
        let mut c = live_look();
        c.picker = Some(Picker::Recent);
        assert!(matches!(press(&mut c, Key::Char('q')), Step::Stay));
        assert!(c.picker.is_some());
    }

    #[test]
    fn ctrl_c_always_quits() {
        let mut c = live_look();
        c.ui.picker = Some(crate::model_picker::for_case("list"));
        let m = Mods { ctrl: true, ..Default::default() };
        assert!(matches!(on_key(&mut c, Key::Char('c'), m), Step::Quit));
    }

    #[test]
    fn esc_closes_then_quits() {
        let mut c = live_look();
        c.picker = Some(Picker::Recent);
        assert!(matches!(press(&mut c, Key::Esc), Step::Stay));
        assert!(c.picker.is_none());
        assert!(matches!(press(&mut c, Key::Esc), Step::Quit));
    }

    #[test]
    fn slash_opens_completions() {
        let mut c = live_look();
        press(&mut c, Key::Char('/'));
        assert!(c.ui.completions.is_some());
        let buf = buffer(&c, 160, 48);
        let entry = find_row(&buf, 160, 48, "pick the model for this session");
        let box_top = find_row(&buf, 160, 48, "enter starts a session");
        assert!(entry < box_top, "the panel is not above the input box");
    }

    #[test]
    fn esc_dismisses_completions_until_the_draft_changes() {
        let mut c = live_look();
        press(&mut c, Key::Char('/'));
        assert!(matches!(press(&mut c, Key::Esc), Step::Stay));
        assert_eq!(c.ui.input, "/");
        assert!(c.ui.completions.is_none());
        assert!(!text(&c, 160, 48).contains("/model"));
        press(&mut c, Key::Char('m'));
        assert!(c.ui.completions.is_some());
        assert_eq!(c.ui.input, "/m");
        let mut c = live_look();
        press(&mut c, Key::Char('/'));
        press(&mut c, Key::Esc);
        press(&mut c, Key::Backspace);
        assert_eq!(c.ui.input, "");
        assert!(c.ui.completions.is_none());
    }

    #[test]
    fn slash_mo_enter_opens_the_picker() {
        let mut c = live_look();
        type_str(&mut c, "/mo");
        assert!(matches!(press(&mut c, Key::Enter), Step::Stay));
        assert!(c.ui.picker.is_some());
        assert_eq!(c.ui.input, "");
        let buf = buffer(&c, 160, 48);
        assert!(run_text(&buf, 0, 0, 20).contains("Models"));
    }

    #[test]
    fn slash_model_enter_opens_the_picker() {
        let mut c = live_look();
        type_str(&mut c, "/model");
        press(&mut c, Key::Enter);
        assert!(c.ui.picker.is_some());
    }

    #[test]
    fn picker_enter_records_the_model() {
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Model);
        assert!(matches!(on_click(&mut c, x, y, 160, 48), Step::Stay));
        press(&mut c, Key::Down);
        press(&mut c, Key::Enter);
        assert!(c.ui.picker.is_none());
        let buf = buffer(&c, 160, 48);
        find_row(&buf, 160, 48, "[claude-sonnet-5-5]");
        find_row(&buf, 160, 48, "[medium]");
    }

    #[test]
    fn picker_esc_records_nothing() {
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Model);
        on_click(&mut c, x, y, 160, 48);
        press(&mut c, Key::Down);
        press(&mut c, Key::Esc);
        assert!(c.ui.picker.is_none());
        assert_eq!(c.model, "claude-opus-5-5");
        assert_eq!(c.level, "high");
    }

    #[test]
    fn picker_scroll_follows_focus() {
        let last = crate::model_picker::fixture().last().unwrap().models.last().unwrap().id;
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Model);
        on_click(&mut c, x, y, 160, 48);
        for _ in 0..11 {
            press(&mut c, Key::Down);
        }
        let buf = buffer(&c, 160, 12);
        let y = find_row(&buf, 160, 12, last);
        let mut barred = false;
        for x in 0..160 {
            if buf[(x, y)].bg == BLUE {
                barred = true;
            }
        }
        assert!(barred, "the focused model is not on the accent bar");
        for _ in 0..11 {
            press(&mut c, Key::Up);
        }
        let buf = buffer(&c, 160, 12);
        assert!(run_text(&buf, 0, 0, 20).contains("Models"));
        find_row(&buf, 160, 12, "claude-opus-5-5");
    }

    #[test]
    fn workspace_picker_flow() {
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Workspace);
        on_click(&mut c, x, y, 160, 48);
        assert_eq!((c.picker, c.wsel), (Some(Picker::Recent), 0));
        press(&mut c, Key::Down);
        press(&mut c, Key::Enter);
        assert!(c.picker.is_none());
        let buf = buffer(&c, 160, 48);
        find_row(&buf, 160, 48, "[▣ ~/work/fiber-worktrees]");
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Workspace);
        on_click(&mut c, x, y, 160, 48);
        for _ in 0..5 {
            press(&mut c, Key::Down);
        }
        assert_eq!(c.wsel, 3);
    }

    #[test]
    fn chip_clicks() {
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Worktree);
        on_click(&mut c, x, y, 160, 48);
        assert!(!c.worktree);
        let (x, y) = hit(&c, Target::Thinking);
        on_click(&mut c, x, y, 160, 48);
        let p = c.ui.picker.as_ref().unwrap();
        assert_eq!((p.focus, p.chip), (0, Some(2)));
        // After recording sonnet/medium, a model click opens on sonnet.
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Model);
        on_click(&mut c, x, y, 160, 48);
        press(&mut c, Key::Down);
        press(&mut c, Key::Enter);
        assert_eq!((c.model, c.level), ("claude-sonnet-5-5", "medium"));
        let (x, y) = hit(&c, Target::Model);
        on_click(&mut c, x, y, 160, 48);
        assert_eq!(c.ui.picker.as_ref().unwrap().focus, 1);
    }

    #[test]
    fn model_name_click_focuses_that_model() {
        let mut c = live_look();
        let (x, y) = hit(&c, Target::Model);
        on_click(&mut c, x, y, 160, 48);
        assert_eq!(c.ui.picker.as_ref().unwrap().focus, 0);
        // The model name in the buffer is a click target: clicking its
        // row focuses that model instead of doing nothing.
        let buf = buffer(&c, 160, 48);
        let y = find_row(&buf, 160, 48, "claude-sonnet-5-5");
        assert!(matches!(on_click(&mut c, 0, y, 160, 48), Step::Stay));
        assert!(c.ui.picker.is_some());
        assert_eq!(c.ui.picker.as_ref().unwrap().focus, 1);
    }

    #[test]
    fn open_workspace_picker_swallows_background_clicks() {
        let mut c = live_look();
        // Session and chip coordinates from the plain home frame.
        let rows = session_hits(&c, 0);
        assert!(!rows.is_empty());
        let (wx, wy) = hit(&c, Target::Workspace);
        let fr = frame(&c, 160, 48, false);
        let (cy, cx0, _, _) = fr.hits.iter().find(|h| h.3 == Target::Worktree).copied().unwrap();
        on_click(&mut c, wx, wy, 160, 48);
        assert_eq!(c.picker, Some(Picker::Recent));
        // Clicking where a session row sits now does nothing: the
        // picker stays open and home never enters a conversation.
        for (x, y) in rows {
            assert!(matches!(on_click(&mut c, x, y, 160, 48), Step::Stay));
            assert_eq!(c.picker, Some(Picker::Recent));
        }
        // A chip click while the picker is open does nothing either.
        assert!(matches!(on_click(&mut c, cx0, cy, 160, 48), Step::Stay));
        assert_eq!((c.picker, c.worktree), (Some(Picker::Recent), true));
    }

    #[test]
    fn row_click_switches() {        let c = live_look();
        for (x, y) in session_hits(&c, 0) {
            let mut c = live_look();
            assert!(matches!(on_click(&mut c, x, y, 160, 48), Step::Conversation));
        }
        assert_eq!(session_hits(&c, 0).len(), 2);
        let c = live_look();
        for (x, y) in session_hits(&c, 3) {
            let mut c = live_look();
            assert!(matches!(on_click(&mut c, x, y, 160, 48), Step::Stay));
        }
    }

    #[test]
    fn every_workspace_picker_pads_text_off_both_edges() {
        for p in [Picker::Recent, Picker::Typed] {
            let rows = picker(p, 0, 160);
            let t = rows
                .iter()
                .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            let lines: Vec<&str> = t.split('\n').collect();
            assert!(lines[1].trim().is_empty(), "no top pad");
            assert!(lines[lines.len() - 2].trim().is_empty(), "no bottom pad");
        }
    }

    #[test]
    fn the_workspace_picker_is_a_panel_with_a_bar_and_legend() {
        let c = crate::cases::lookup(CASES, "picker-recent").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("Workspaces"));
        assert!(t.contains("› "));
        assert!(t.contains("~/work/fiber-worktrees"));
        assert!(t.contains("↑↓ move · enter open · esc closes"));
        // Centred: the picker's edges sit inside the margins, not full width.
        let fr = frame(&c, 160, 48, false);
        let edge = fr
            .screen
            .iter()
            .flatten()
            .find(|p| p.row.spans.iter().any(|s| s.content.contains("▄▄")))
            .unwrap();
        assert!(edge.x > 0, "picker flush left");
        assert!(edge.x + edge.w < 160, "picker fills the row");
        let typed = text(&crate::cases::lookup(CASES, "picker-typed").unwrap(), 160, 48);
        assert!(typed.contains("› ~/work/fi"));
        assert!(typed.contains("── recents ──"));
        // The dim section header stands off on a blank row.
        let lines: Vec<&str> = typed.split('\n').collect();
        let div = lines.iter().position(|l| l.contains("── recents ──")).unwrap();
        assert!(lines[div - 1].trim().is_empty(), "no blank over the recents");
    }

    #[test]
    fn the_selected_workspace_reads_bold_in_the_buffer() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::Modifier;
        let rows = picker(Picker::Recent, 0, 160);
        let w = crate::width(&rows[0].spans);
        let mut buf = Buffer::empty(Rect::new(0, 0, w as u16, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            crate::paint(&mut buf, 0, y as u16, w as u16, r);
        }
        let t = rows
            .iter()
            .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        // The first workspace row is the selected one.
        let y = t.split('\n').position(|l| l.contains("~/work/fiber")).unwrap() as u16;
        let line = t.split('\n').nth(y as usize).unwrap();
        let at = line.find("~/work/fiber").unwrap();
        let col = line[..at].chars().count();
        assert!(buf[(col as u16, y)].modifier.contains(Modifier::BOLD), "selection not bold");
    }
}
