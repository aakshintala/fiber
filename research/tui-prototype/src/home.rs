//! The home screen and the workspace picker, for #1628.
//!
//! A throwaway look prototype: it draws home full screen (no panel, no rail,
//! centred) from fixtures in this file, with the palette, surface primitive
//! (`slab`) and roles of `super`. Each case renders one static frame and the
//! program waits for a key; Esc, q or Ctrl+C quits. The conversation view is
//! untouched: a home case replaces it.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{
    BI, BLUE, CYAN, ORANGE, RED, SEL, SState, bold, dim, fg, fit, lift, paint,
    row, slab, sp, state_glyph, t,
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

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    Workspace,
    Worktree,
    Model,
    Thinking,
}

#[derive(Clone, Copy, PartialEq)]
enum Picker {
    Recent,
    Typed,
}

#[derive(Clone, Copy, PartialEq)]
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
}

fn base() -> Look {
    Look { list: List::Both, sel: None, hover: None, worktree: true, picker: None }
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

fn chip_row(c: &Look) -> Vec<Span<'static>> {
    let h = |k: Hover| c.hover == Some(k);
    // The chips start from the model picker's current model, so a pick
    // agrees with them.
    let (model, level) = crate::model_picker::chosen(&crate::model_picker::for_case("list"));
    let mut s = vec![sp("▌ ", fg(BLUE))];
    s.push(chip("▣ ~/work/fiber", Style::new(), h(Hover::Workspace)));
    s.push(sp(" ", Style::new()));
    let (glyph, st) = if c.worktree {
        ("● new worktree", fg(BLUE))
    } else {
        ("○ new worktree", dim())
    };
    s.push(chip(glyph, st, h(Hover::Worktree)));
    s.push(sp(" ", Style::new()));
    s.push(chip(model, fg(CYAN), h(Hover::Model)));
    s.push(sp(" ", Style::new()));
    s.push(chip(level, fg(ORANGE), h(Hover::Thinking)));
    s.push(t());
    s.push(sp("enter starts a session ", dim()));
    s
}

/// The large input box: the `/? for shortcuts` placeholder over the chip row,
/// with the prototype's ▌ stripe.
fn input_box(c: &Look, w: usize) -> Vec<super::Row> {
    let stripe = || sp("▌", fg(BLUE));
    let blank = || row(vec![stripe()]);
    slab(
        vec![
            row(vec![
                stripe(),
                sp(" ", Style::new()),
                sp("› ", fg(CYAN)),
                sp("/? for shortcuts", dim()),
                sp("█", dim()),
            ]),
            blank(),
            blank(),
            row(chip_row(c)),
        ],
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

/// Pushes one section's header and two-line rows onto the list's lines.
/// `at` counts sessions in live-then-past order; `sel_end` becomes the
/// line index just past the selected row's second line.
fn push_section(
    lines: &mut Vec<Option<(usize, Vec<Span<'static>>)>>,
    geom: (usize, usize),
    header: Vec<Span<'static>>,
    sessions: &[Session],
    sel: Option<usize>,
    at: &mut usize,
    sel_end: &mut usize,
) {
    let (x0, rw) = geom;
    lines.push(Some((x0 + 2, header)));
    for s in sessions {
        let selected = sel == Some(*at);
        if selected {
            *sel_end = lines.len() + 2;
        }
        *at += 1;
        lines.push(Some((x0 + 1, session_line_one(s, selected, rw))));
        lines.push(Some((x0 + 1, session_line_two(s, rw))));
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
/// its completions over the recents, or recents alone. The first row rides
/// a full-width selection bar.
fn picker_body(p: Picker) -> Vec<super::Row> {
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
        let selected = p == Picker::Recent && i == 0;
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
fn picker(p: Picker, cols: usize) -> Vec<super::Row> {
    const TITLE: &str = "Workspaces";
    let body = picker_body(p);
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

fn frame(c: &Look, cols: usize, rows: usize, image: bool) -> Vec<Vec<Placed>> {
    let blank = || vec![Placed {
        x: 0,
        w: cols as u16,
        row: row(vec![]),
    }];
    let mut screen: Vec<Vec<Placed>> = (0..rows).map(|_| blank()).collect();
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
    for r in input_box(c, w) {
        let bg = r.bg;
        put(&mut screen, y, x0, w, r.spans, bg);
        y += 1;
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
        // Each entry is the line's left edge and its spans; the blank
        // between sections counts for the scroll but draws nothing. The
        // session index is the row's position in live-then-past order.
        let mut lines: Vec<Option<(usize, Vec<Span<'static>>)>> = vec![];
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
            if let Some((x, spans)) = l {
                put(&mut screen, list_top + k - off, *x, rw, spans.clone(), None);
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
        let rows = picker(p, cols);
        let pw = super::width(&rows[0].spans);
        let px0 = super::panel::x_for(pw, cols);
        for (k, r) in rows.into_iter().enumerate() {
            let bg = r.bg;
            put(&mut screen, 12 + k, px0, pw, r.spans, bg);
        }
    }
    screen
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
    if c.picker.is_some() {
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
        for (y, placements) in frame(c, cols as usize, rows as usize, shown).iter().enumerate() {
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

/// Draws the `--home` case and waits for a key. Mutually exclusive with the
/// conversation view: the fixture replay never draws.
pub(crate) fn run_home(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.home.clone().unwrap_or_default();
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
    let mut err = draw(term, &c, &mut img, supported, true).err();
    if err.is_none() && supported {
        err = draw(term, &c, &mut img, supported, false).err();
    }
    if let Some(e) = err {
        free(term);
        return Err(e);
    }
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(wait_for(rd.deadline(), Instant::now()))?;
        let err = if resized {
            draw(term, &c, &mut img, supported, false).err()
        } else {
            None
        };
        if let Some(e) = err {
            free(term);
            return Err(e);
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => {
                    free(term);
                    return Ok(format!("home {name}\n"));
                }
                Ev::Key(Key::Esc, _) => {
                    free(term);
                    return Ok(format!("home {name}\n"));
                }
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    free(term);
                    return Ok(format!("home {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|c| crate::cases::lookup(CASES, c.name).is_some()));
        assert_eq!(CASES.len(), 13);
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
        for (y, placements) in frame(c, cols as usize, rows as usize, image).iter().enumerate() {
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

    #[test]
    fn every_workspace_picker_pads_text_off_both_edges() {
        for p in [Picker::Recent, Picker::Typed] {
            let rows = picker(p, 160);
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
        let rows = picker(Picker::Recent, 160);
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
