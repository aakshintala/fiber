//! The full-screen fleet view and the focused-rail still, for #2424.
//!
//! A throwaway look prototype: the full-screen view draws its still frames
//! from the fixtures in this file (eight live sessions across three
//! projects, the mock's layout A with the banner frame), and `--home` bare
//! runs the interactive fleet. `rail-focused` draws the conversation view
//! with the rail focused instead. The conversation view itself is untouched:
//! a home case replaces it. Where the mock and the prototype's palette or
//! card drawing differ, the prototype wins.

use super::input::{Ev, Key, Mods, Mouse};
use super::{Act, Args, Term, Ui};
use super::{
    BI, BLUE, BP, CYAN, ORANGE, RED, SEL, SState, bold, cut, dim, fg, fit, lift, paint, row, slab,
    sp, state_glyph,
};
use crate::cases::{Case, Surface};
use ratatui::style::Style;
use ratatui::text::Span;
use std::io::{self, Write};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// Every `--home` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "full-sessions",
        help: "the fleet with eight live sessions and exited ones, first row focused",
        check: "the four-row logo top left with `0.0.1` and the coloured counts beside it; one section per project, launch project first, each headed `name +`, dim `N live · $spend` and orange `N waiting on you`; two-line live rows with `⌥N`, the glyph, the name, the state word and the elapsed, then dim `id · turns · ⎇ branch · spend · context%` with the full wait in orange; dim `exited · N`, up to three dim `○` rows and `… N more · /resume lists all`; the first live row on the selection bar; the prompt box at the bottom and the key hint at the foot.",
        build: || Look {
            sel: Some(0),
            fleet: FleetUi {
                sel: Some(0),
                ..fleet()
            },
            ..base()
        },
    },
    Case {
        name: "full-empty",
        help: "the fleet with no sessions",
        check: "the header's second line dim `no sessions yet`; the middle dim `No sessions yet.` and `Type below and press Enter to start one.`, centred; the prompt box and the hint as usual, no sections.",
        build: || Look {
            projs: vec![],
            ..base()
        },
    },
    Case {
        name: "full-focus-exited",
        help: "the fleet with the first exited row focused",
        check: "the first exited row on the selection bar; live rows unmarked.",
        build: || Look {
            sel: Some(5),
            fleet: FleetUi {
                sel: Some(5),
                ..fleet()
            },
            ..base()
        },
    },
    Case {
        name: "full-closing",
        help: "the fleet with one live row CLOSING after /clear",
        check: "the first live row with dim `CLOSING` where the state word sits, its other cells as a live row.",
        build: || {
            let mut l = base();
            l.projs[0].live[0].closing = true;
            l.sel = Some(0);
            l.fleet.sel = Some(0);
            l
        },
    },
    Case {
        name: "full-picker",
        help: "the workspace picker open over the fleet",
        check: "the picker centred over the fleet with `Workspaces` bold accent, four recent workspaces, the first `›` on the bar, and the bold-key legend foot.",
        build: || Look {
            sel: Some(0),
            fleet: FleetUi {
                sel: Some(0),
                wpick: Some(0),
                wsel: 0,
                ..fleet()
            },
            ..base()
        },
    },
    Case {
        name: "rail-focused",
        help: "the conversation view with the rail focused on a NEEDS INPUT card",
        check: "the rail left with the needs-input card lifted across its whole surface; the dimmed conversation behind; the input box with a dim stripe and dim text; the foot hint with bold keys.",
        build: || Look {
            rail: true,
            ..base()
        },
    },
    Case {
        name: "live",
        help: "the interactive fleet: move, open rows, start sessions",
        check: "not a still frame: `--home` with no case runs the event loop, and this name only names it in `--help`.",
        build: base,
    },
];

/// `--home`, for `--help` and `check/home.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--home",
    file: "home",
    title: "Home (#2424)",
    docs: || crate::cases::docs(CASES),
};

/// The fixture's version, beside the logo as docs/tui.md's logo reads.
const VERSION: &str = "0.0.1";
/// The day's spend across all projects, after the counts.
const LIVE_SPEND: &str = "$5.78";
/// The header's rows: the four-row logo, starting at the top.
const LOGO_Y: usize = 0;
/// The two lines beside the logo start past its 32 cells and a gap.
const HTEXT_X: usize = 35;
/// The session list's first row, below the header.
const LIST_TOP: usize = 5;
/// The prompt box's rows: the slab's thin edge, the entry row, a blank
/// row, the chip row, and the slab's thin edge.
const BOX_H: usize = 5;
/// The workspace picker's first row over the fleet.
const PICK_Y: usize = 12;

/// The fewest rows that still draw header, one list line, box and hint.
const fn tall_min() -> usize {
    LOGO_Y + 4 + 1 + 1 + BOX_H + 1
}

// ============================================================ fixtures

/// A live session on the fleet, after the mock's layout A. `n` is the
/// start-order number the rail keeps (`⌥N`); `wait` is what it waits on in
/// full, or empty; `closing` is a session finishing after `/clear`.
struct Live {
    n: usize,
    st: SState,
    name: &'static str,
    model: &'static str,
    branch: &'static str,
    spend: &'static str,
    dollars: f64,
    ctx: u8,
    age: &'static str,
    id: &'static str,
    turns: u32,
    wait: &'static str,
    closing: bool,
}

/// An exited session on the fleet.
struct Gone {
    name: &'static str,
    id: &'static str,
    turns: u32,
    spend: &'static str,
    age: &'static str,
}

struct Proj {
    name: &'static str,
    live: Vec<Live>,
    gone: Vec<Gone>,
}

fn projs() -> Vec<Proj> {
    vec![
        Proj {
            name: "fiber",
            live: vec![
                Live {
                    n: 1,
                    st: SState::Working,
                    name: "fix flaky lock test",
                    model: "opus-5-5 · high",
                    branch: "main",
                    spend: "$1.35",
                    dollars: 1.35,
                    ctx: 12,
                    age: "16s",
                    id: "s_9f31ac",
                    turns: 4,
                    wait: "",
                    closing: false,
                },
                Live {
                    n: 2,
                    st: SState::NeedsInput,
                    name: "review #688",
                    model: "gpt-6.1-sol · medium",
                    branch: "review-688",
                    spend: "$1.10",
                    dollars: 1.10,
                    ctx: 41,
                    age: "22m",
                    id: "s_3ab702",
                    turns: 3,
                    wait: "approval: shell cargo publish --dry-run",
                    closing: false,
                },
                Live {
                    n: 4,
                    st: SState::Crashed,
                    name: "lsp probe",
                    model: "grok-4.7-high · low",
                    branch: "spike/lsp",
                    spend: "$0.05",
                    dollars: 0.05,
                    ctx: 91,
                    age: "3h",
                    id: "s_5e91b4",
                    turns: 3,
                    wait: "stopped without exiting; 1 job orphaned",
                    closing: false,
                },
                Live {
                    n: 5,
                    st: SState::Ready,
                    name: "docs: rail spec",
                    model: "opus-5-5 · high",
                    branch: "docs/rail-692",
                    spend: "$0.42",
                    dollars: 0.42,
                    ctx: 27,
                    age: "9m",
                    id: "s_44d2e1",
                    turns: 6,
                    wait: "",
                    closing: false,
                },
                Live {
                    n: 7,
                    st: SState::NeedsInput,
                    name: "bump ratatui",
                    model: "gpt-6.1-sol · low",
                    branch: "deps/ratatui-0.30",
                    spend: "$1.00",
                    dollars: 1.00,
                    ctx: 64,
                    age: "4m",
                    id: "s_08f3b9",
                    turns: 1,
                    wait: "question: 2 of 3 answered",
                    closing: false,
                },
            ],
            gone: vec![
                Gone {
                    name: "cut the 0.0.1 release",
                    id: "s_7f20aa",
                    turns: 5,
                    spend: "$1.10",
                    age: "3h",
                },
                Gone {
                    name: "speed up the render loop",
                    id: "s_61c9d0",
                    turns: 2,
                    spend: "$0.58",
                    age: "5h",
                },
                Gone {
                    name: "port the router to hyper",
                    id: "s_88f310",
                    turns: 8,
                    spend: "$2.02",
                    age: "1d",
                },
                Gone {
                    name: "audit the event log",
                    id: "s_3ab7f2",
                    turns: 3,
                    spend: "$0.35",
                    age: "2d",
                },
            ],
        },
        Proj {
            name: "pi-rig",
            live: vec![
                Live {
                    n: 3,
                    st: SState::Retrying,
                    name: "backfill embeddings",
                    model: "grok-4.7-high · high",
                    branch: "backfill/embed",
                    spend: "$0.87",
                    dollars: 0.87,
                    ctx: 33,
                    age: "12m",
                    id: "s_50c9d2",
                    turns: 9,
                    wait: "retry 2 of 5 in 8s · 529 overloaded",
                    closing: false,
                },
                Live {
                    n: 6,
                    st: SState::Working,
                    name: "migrate every provider adapter to the new streaming contract",
                    model: "grok-4.7-high · max",
                    branch: "migrate/streaming",
                    spend: "$0.64",
                    dollars: 0.64,
                    ctx: 58,
                    age: "2m",
                    id: "s_77aa01",
                    turns: 12,
                    wait: "",
                    closing: false,
                },
            ],
            gone: vec![Gone {
                name: "how do I backfill embeddings for old sessions?",
                id: "s_50c9a1",
                turns: 9,
                spend: "$0.87",
                age: "2d",
            }],
        },
        Proj {
            name: "beacon",
            live: vec![Live {
                n: 8,
                st: SState::Ready,
                name: "rewrite onboarding tour",
                model: "opus-5-5 · medium",
                branch: "tour/rewrite",
                spend: "$0.35",
                dollars: 0.35,
                ctx: 19,
                age: "1h",
                id: "s_6d1e44",
                turns: 7,
                wait: "",
                closing: false,
            }],
            gone: vec![],
        },
    ]
}

/// Recent workspaces, from `recent.jsonl`.
pub(crate) const RECENTS: &[&str] = &[
    "~/work/fiber",
    "~/work/fiber-worktrees",
    "~/work/beacon",
    "~/work/pi-rig",
];

/// The fleet's keyboard and picker state, shared by home's own loop and the
/// conversation view's widened fleet. `sel` is the focused row across every
/// section, live and exited; `sel` None with `chip` None is the entry bar.
#[derive(Default)]
pub(crate) struct FleetUi {
    pub sel: Option<usize>,
    pub chip: Option<usize>,
    pub last_chip: usize,
    pub worktree: bool,
    /// the open workspace picker with its selection, if any
    pub wpick: Option<usize>,
    pub wsel: usize,
    /// the chosen workspace, shown on the workspace chip
    pub ws: String,
}

struct Look {
    projs: Vec<Proj>,
    /// the focused row in live-then-gone order across sections, if any.
    /// `fleet.sel` mirrors it: keys move `fleet`, and stills read it.
    sel: Option<usize>,
    fleet: FleetUi,
    /// the chosen model and thinking level, shown on their chips
    model: &'static str,
    level: &'static str,
    /// the draft, the completion panel and the model picker
    ui: Ui,
    /// the draft Esc dismissed the completion panel on: `sync` stays shut
    /// until the draft changes
    dismissed: Option<String>,
    /// the rail-focused still: the conversation view, not the fleet
    rail: bool,
}

/// The stills' fleet state: the launch workspace, a new worktree.
fn fleet() -> FleetUi {
    FleetUi {
        ws: RECENTS[0].into(),
        worktree: true,
        ..FleetUi::default()
    }
}

fn base() -> Look {
    let (model, level) = crate::model_picker::chosen(&crate::model_picker::for_case("list"));
    Look {
        projs: projs(),
        sel: None,
        fleet: fleet(),
        model,
        level,
        ui: Ui::default(),
        dismissed: None,
        rail: false,
    }
}

// ============================================================ the shared fleet

/// A live row for the shared renderer. Home's fixtures fill every field;
/// the conversation view has no id or turns, so those are optional.
pub(crate) struct RLive<'a> {
    pub n: usize,
    pub st: SState,
    pub name: &'a str,
    pub branch: &'a str,
    pub spend: &'a str,
    /// live spend in dollars, summed into the project header
    pub dollars: f64,
    pub ctx: u8,
    pub age: &'a str,
    pub id: Option<&'a str>,
    pub turns: Option<u32>,
    pub wait: &'a str,
    pub closing: bool,
    /// what a click on the row does in the conversation view, if anything
    pub act: Option<Act>,
}

pub(crate) struct RGone<'a> {
    pub name: &'a str,
    pub id: Option<&'a str>,
    pub turns: Option<u32>,
    pub spend: &'a str,
    pub age: &'a str,
}

pub(crate) struct RProj<'a> {
    pub name: &'a str,
    pub live: Vec<RLive<'a>>,
    pub gone: Vec<RGone<'a>>,
}

/// One focusable row: a live or an exited session with its address.
pub(crate) enum FItem {
    Live(usize, usize),
    Gone(usize, usize),
}

/// The focusable rows in on-screen order: every live row, then each
/// project's exited rows up to the three the fleet shows.
pub(crate) fn fleet_items(projs: &[RProj<'_>]) -> Vec<FItem> {
    let mut out = vec![];
    for (pi, p) in projs.iter().enumerate() {
        for (li, _) in p.live.iter().enumerate() {
            out.push(FItem::Live(pi, li));
        }
        for (gi, _) in p.gone.iter().enumerate().take(3) {
            out.push(FItem::Gone(pi, gi));
        }
    }
    out
}

/// What the shared renderer needs: the sections, the fleet state, the chip
/// labels, the entry draft with its completion panel, and the screen.
pub(crate) struct FleetView<'a> {
    pub projs: &'a [RProj<'a>],
    pub st: &'a FleetUi,
    pub model: &'a str,
    pub level: &'a str,
    pub input: &'a str,
    pub comp: Option<&'a [super::Row]>,
    pub spend: &'a str,
    pub cols: usize,
    pub rows: usize,
    pub image: bool,
}

/// What a rendered row is, so home and the conversation view map rows to
/// their own click targets. `Live` carries the conversation view's click
/// act and the row's flat index; home ignores the act.
#[derive(Clone, Copy)]
pub(crate) enum FTag {
    Pad,
    Live(Option<Act>, usize),
    Gone(usize),
}

/// A state's colour on the fleet: waiting reads attention, ready dim.
fn state_style(st: SState) -> Style {
    match st {
        SState::Working => fg(BLUE),
        SState::Retrying => fg(ORANGE),
        SState::NeedsInput => fg(ORANGE),
        SState::Ready => dim(),
        SState::Crashed => fg(RED),
        SState::Idle => dim(),
    }
}

fn state_word(st: SState) -> &'static str {
    match st {
        SState::Working => "WORKING",
        SState::Retrying => "RETRYING",
        SState::NeedsInput => "NEEDS INPUT",
        SState::Ready => "READY",
        SState::Crashed => "CRASHED",
        SState::Idle => "IDLE",
    }
}

/// The header's counts across all projects: each nonzero state in its
/// colour joined by dim ` · `, then the day's spend dim.
fn counts_row(projs: &[RProj<'_>], spend: &str) -> Vec<Span<'static>> {
    let count = |st: SState| {
        projs
            .iter()
            .flat_map(|p| &p.live)
            .filter(|s| s.st == st && !s.closing)
            .count()
    };
    let mut parts: Vec<(usize, &'static str, Style)> = vec![];
    for (st, label) in [
        (SState::NeedsInput, "waiting on you"),
        (SState::Working, "working"),
        (SState::Retrying, "retrying"),
        (SState::Crashed, "crashed"),
        (SState::Ready, "ready"),
    ] {
        let n = count(st);
        if n > 0 {
            parts.push((n, label, state_style(st)));
        }
    }
    if parts.is_empty() {
        return vec![sp("no sessions yet", dim())];
    }
    let mut out = vec![];
    for (i, (n, label, st)) in parts.iter().enumerate() {
        if i > 0 {
            out.push(sp(" · ", dim()));
        }
        out.push(sp(format!("{n} {label}"), *st));
    }
    out.push(sp(format!(" · {spend} live"), dim()));
    out
}

/// One section's header: the name bold in the accent, ` +`, dim
/// `N live · $spend`, then orange `N waiting on you` when nonzero.
fn proj_head(p: &RProj<'_>) -> Vec<Span<'static>> {
    let spend: f64 = p.live.iter().map(|s| s.dollars).sum();
    let mut h = vec![
        sp(p.name, bold().fg(BLUE)),
        sp(" +", fg(BLUE)),
        sp("  ", Style::new()),
        sp(format!("{} live · ${spend:.2}", p.live.len()), dim()),
    ];
    let waiting = p.live.iter().filter(|s| s.st == SState::NeedsInput).count();
    if waiting > 0 {
        h.push(sp(format!("  {waiting} waiting on you"), fg(ORANGE)));
    }
    h
}

/// A live row's first line at `w` cells: dim `⌥N`, the state glyph, the
/// name, then the state word in its colour and the elapsed dim,
/// right-aligned. A closing row reads dim `CLOSING` for its word.
fn live_line_one(s: &RLive<'_>, w: usize) -> Vec<Span<'static>> {
    let left_n = format!("⌥{} ", s.n);
    let mut line = vec![sp(left_n.clone(), dim()), state_glyph(s.st, 0, true)];
    line.push(sp(" ", Style::new()));
    let (word, wst) = if s.closing {
        ("CLOSING", dim())
    } else {
        (state_word(s.st), state_style(s.st))
    };
    let rw = word.width() + 2 + s.age.width();
    let name_max = w.saturating_sub(left_n.width() + 2 + rw + 2);
    let name = cut(s.name, name_max.max(1));
    let used = left_n.width() + 2 + name.width() + rw;
    line.push(sp(name, Style::new()));
    line.push(sp(" ".repeat(w.saturating_sub(used).max(2)), Style::new()));
    line.push(sp(word, wst));
    line.push(sp("  ", Style::new()));
    line.push(sp(s.age, dim()));
    line
}

/// A live row's second line, indented: dim `id · N turns · ⎇ branch ·
/// spend · context%`, then ` · ` and the full wait in attention when it
/// waits. The conversation view has no id or turns, so those give way.
fn live_line_two(s: &RLive<'_>, w: usize) -> Vec<Span<'static>> {
    let mut body = vec![];
    if let Some(id) = s.id {
        body.push(sp(format!("{id} · "), dim()));
    }
    if let Some(t) = s.turns {
        body.push(sp(
            format!("{t} turn{} · ", if t == 1 { "" } else { "s" }),
            dim(),
        ));
    }
    body.push(sp(
        format!("⎇ {} · {} · {}%", s.branch, s.spend, s.ctx),
        dim(),
    ));
    if !s.wait.is_empty() {
        body.push(sp(format!(" · {}", s.wait), fg(ORANGE)));
    }
    let mut line = vec![sp("    ", Style::new())];
    line.extend(fit(&body, w.saturating_sub(4)));
    line
}

/// An exited row, dim: `○ name` with `id · turns · spend · age ago` right.
fn gone_line(g: &RGone<'_>, w: usize) -> Vec<Span<'static>> {
    let mut right = String::new();
    if let Some(id) = g.id {
        right.push_str(&format!("{id} · "));
    }
    if let Some(t) = g.turns {
        right.push_str(&format!("{t} turn{} · ", if t == 1 { "" } else { "s" }));
    }
    right.push_str(&format!("{} · {} ago", g.spend, g.age));
    let name_max = w.saturating_sub("   ○ ".width() + right.width() + 2);
    let name = cut(g.name, name_max.max(1));
    let used = "   ○ ".width() + name.width() + right.width();
    vec![
        sp("   ○ ", dim()),
        sp(name, dim()),
        sp(" ".repeat(w.saturating_sub(used).max(2)), dim()),
        sp(right, dim()),
    ]
}

/// One chip: bracketed text on the raised surface, lighter with focus.
fn chip(text: &str, st: Style, on: bool) -> Span<'static> {
    let bg = if on { lift(SEL) } else { SEL };
    sp(format!("[{text}]"), st.bg(bg))
}

/// The chip row's chips: text, style and focus.
fn chip_defs(
    st: &FleetUi,
    workspace: &str,
    model: &str,
    level: &str,
) -> Vec<(String, Style, bool)> {
    let on = |i: usize| st.chip == Some(i);
    let (worktree, wst) = if st.worktree {
        ("● new worktree", fg(BLUE))
    } else {
        ("○ new worktree", dim())
    };
    vec![
        (format!("▣ {workspace}"), Style::new(), on(0)),
        (worktree.into(), wst, on(1)),
        (model.into(), fg(CYAN), on(2)),
        (level.into(), fg(ORANGE), on(3)),
    ]
}

fn chip_row(st: &FleetUi, workspace: &str, model: &str, level: &str) -> Vec<Span<'static>> {
    let mut s = vec![sp("▌ ", fg(BLUE))];
    for (i, (text, cst, focused)) in chip_defs(st, workspace, model, level).iter().enumerate() {
        if i > 0 {
            s.push(sp(" ", Style::new()));
        }
        s.push(chip(text, *cst, *focused));
    }
    s.push(sp("  ", Style::new()));
    s.push(sp("Enter starts a session", dim()));
    s
}

/// The chip row's click targets as row-relative cell ranges with the chip.
fn chip_hits(
    st: &FleetUi,
    workspace: &str,
    model: &str,
    level: &str,
) -> Vec<(usize, usize, usize)> {
    let mut x = "▌ ".width();
    let mut out = vec![];
    for (i, (text, _, _)) in chip_defs(st, workspace, model, level).iter().enumerate() {
        if i > 0 {
            x += 1;
        }
        let w = text.width() + 2;
        out.push((i, x, x + w));
        x += w;
    }
    out
}

/// The fleet's prompt box where the conversation view's input box sits:
/// the entry row, a blank row, then the chip row.
fn prompt_box(
    st: &FleetUi,
    workspace: &str,
    model: &str,
    level: &str,
    input: &str,
    w: usize,
) -> Vec<super::Row> {
    let mut prompt = vec![sp("▌", fg(BLUE)), sp(" ", Style::new()), sp("› ", fg(CYAN))];
    if input.is_empty() {
        prompt.push(sp("/? for shortcuts", dim()));
    } else {
        prompt.push(sp(input.to_string(), Style::new()));
    }
    prompt.push(sp("█", dim()));
    slab(
        vec![
            row(prompt),
            row(vec![]),
            row(chip_row(st, workspace, model, level)),
        ],
        BI,
        None,
        w,
    )
}

/// One bold key with its dim label, joined by dim ` · `.
fn hint_key(s: &mut Vec<Span<'static>>, k: &str, label: &str) {
    if !s.is_empty() {
        s.push(sp(" · ", dim()));
    }
    s.push(sp(k, bold()));
    s.push(sp(format!(" {label}"), dim()));
}

/// The foot hint with its keys bold.
fn full_hint() -> Vec<Span<'static>> {
    let mut s = vec![];
    hint_key(
        &mut s,
        "Enter",
        "opens the focused session, or starts one from the box",
    );
    hint_key(&mut s, "↑↓", "move");
    s.push(sp(" · ", dim()));
    s.push(sp("→", bold()));
    s.push(sp(" or ", dim()));
    s.push(sp("Esc", bold()));
    s.push(sp(" back to the session", dim()));
    hint_key(&mut s, "Delete", "deletes an exited one");
    s
}

/// The rail's foot hint while it has focus, keys bold. The conversation
/// view draws it centred under the input box.
pub(crate) fn rail_hint() -> Vec<Span<'static>> {
    let mut s = vec![];
    hint_key(&mut s, "←", "full screen");
    hint_key(&mut s, "↑↓", "move");
    hint_key(&mut s, "Enter", "switch");
    s.push(sp(" · ", dim()));
    s.push(sp("→", bold()));
    s.push(sp(" or ", dim()));
    s.push(sp("Esc", bold()));
    s.push(sp(" back to the session", dim()));
    hint_key(&mut s, "⌥1–9", "jump");
    s
}

/// Centres spans in `cols` cells.
fn centered(spans: &[Span<'static>], cols: usize) -> Vec<Span<'static>> {
    let w: usize = spans.iter().map(|s| s.content.width()).sum();
    let mut out = vec![sp(" ".repeat(cols.saturating_sub(w) / 2), Style::new())];
    out.extend(spans.iter().cloned());
    out
}

/// The fleet's rows at `cols` by `rows`: the header, the scrolling session
/// list, the prompt box and the foot hint. The list scrolls so the focused
/// row's last line stays on screen.
pub(crate) fn fleet_rows(v: &FleetView<'_>) -> (Vec<super::Row>, Vec<FTag>) {
    let (cols, rows) = (v.cols, v.rows);
    let mut screen: Vec<super::Row> = (0..rows).map(|_| row(vec![])).collect();
    let mut tags: Vec<FTag> = (0..rows).map(|_| FTag::Pad).collect();
    if rows == 0 || cols == 0 {
        return (screen, tags);
    }
    let ws = if v.st.ws.is_empty() {
        RECENTS[0]
    } else {
        v.st.ws.as_str()
    };
    // The header: the four-row logo top left, beside it the version and the
    // counts across projects, vertically centred on the logo. Too short
    // for four rows: the one-row logo, and everything below keeps its rows.
    if rows < tall_min() {
        screen[0] = super::Row {
            spans: fit(&crate::logo::one_row(VERSION), cols),
            bg: None,
            ..Default::default()
        };
    } else {
        for (k, l) in crate::logo::rows("", v.image).iter().enumerate() {
            let y = LOGO_Y + k;
            if y >= rows {
                break;
            }
            let mut spans = l.clone();
            let lw: usize = spans.iter().map(|s| s.content.width()).sum();
            if HTEXT_X > lw {
                spans.push(sp(" ".repeat(HTEXT_X - lw), Style::new()));
            }
            if k == 1 {
                spans.push(sp(VERSION, dim()));
            } else if k == 2 {
                spans.extend(counts_row(v.projs, v.spend));
            }
            screen[y] = super::Row {
                spans: fit(&spans, cols),
                bg: None,
                ..Default::default()
            };
        }
    }
    let box_top = rows.saturating_sub(1 + BOX_H);
    // The session list between header and prompt box: one block of lines
    // per section, scrolled as one.
    let items = fleet_items(v.projs);
    let n = items.len();
    let sel = v.st.sel.filter(|&i| i < n);
    // Each entry is the row's spans, its tag, and whether the selection bar
    // covers it.
    let mut lines: Vec<(Vec<Span<'static>>, FTag, bool)> = vec![];
    let mut sel_end = 0;
    for (fi, it) in items.iter().enumerate() {
        let focused = sel == Some(fi);
        match it {
            FItem::Live(pi, li) => {
                if *li == 0 {
                    lines.push((proj_head(&v.projs[*pi]), FTag::Pad, false));
                }
                let s = &v.projs[*pi].live[*li];
                if focused {
                    sel_end = lines.len() + 2;
                }
                lines.push((live_line_one(s, cols), FTag::Live(s.act, fi), focused));
                lines.push((live_line_two(s, cols), FTag::Live(s.act, fi), focused));
            }
            FItem::Gone(pi, gi) => {
                if *gi == 0 {
                    lines.push((
                        vec![sp(format!("  exited · {}", v.projs[*pi].gone.len()), dim())],
                        FTag::Pad,
                        false,
                    ));
                }
                let g = &v.projs[*pi].gone[*gi];
                if focused {
                    sel_end = lines.len() + 1;
                }
                lines.push((gone_line(g, cols), FTag::Gone(fi), focused));
                let shown = v.projs[*pi].gone.len().min(3);
                if *gi + 1 == shown && v.projs[*pi].gone.len() > 3 {
                    lines.push((
                        vec![sp(
                            format!(
                                "     … {} more · /resume lists all",
                                v.projs[*pi].gone.len() - 3
                            ),
                            dim(),
                        )],
                        FTag::Pad,
                        false,
                    ));
                }
            }
        }
    }
    let area = box_top.saturating_sub(LIST_TOP);
    if lines.is_empty() {
        let mid = LIST_TOP + area.saturating_sub(2) / 2;
        for (k, text) in [
            "No sessions yet.",
            "Type below and press Enter to start one.",
        ]
        .iter()
        .enumerate()
        {
            let y = mid + k;
            if y < rows {
                screen[y] = super::Row {
                    spans: fit(&centered(&[sp(*text, dim())], cols), cols),
                    bg: None,
                    ..Default::default()
                };
            }
        }
    } else {
        let off = sel_end.saturating_sub(area);
        for (k, (spans, tag, focused)) in lines.iter().enumerate().skip(off).take(area) {
            let y = LIST_TOP + k - off;
            if y < rows {
                screen[y] = super::Row {
                    spans: fit(spans, cols),
                    bg: focused.then_some(SEL),
                    ..Default::default()
                };
                tags[y] = *tag;
            }
        }
    }
    // The `/` and `@` panel draws directly above the prompt box, over the
    // list's end.
    if let Some(cp) = v.comp {
        let keep = cp.len().min(box_top.saturating_sub(LIST_TOP));
        for (k, r) in cp.iter().take(keep).enumerate() {
            let y = box_top - keep + k;
            if y < rows {
                screen[y] = super::Row {
                    spans: fit(&r.spans, cols),
                    bg: r.bg,
                    ..Default::default()
                };
            }
        }
    }
    for (k, r) in prompt_box(v.st, ws, v.model, v.level, v.input, cols)
        .into_iter()
        .enumerate()
    {
        let y = box_top + k;
        if y < rows {
            screen[y] = super::Row {
                spans: fit(&r.spans, cols),
                bg: r.bg,
                ..Default::default()
            };
        }
    }
    if rows >= 1 {
        screen[rows - 1] = super::Row {
            spans: fit(&centered(&full_hint(), cols), cols),
            bg: None,
            ..Default::default()
        };
    }
    (screen, tags)
}

// ============================================================ home frame

/// What a click lands on: home-local targets; the model picker's rows
/// reuse its own `Act`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Workspace,
    Worktree,
    Model,
    Thinking,
    /// a fleet row in live-then-gone order across sections
    Row(usize),
    /// a recent workspace in the open workspace picker
    Recent(usize),
    /// a model picker row or chip
    Picker(Act),
}

/// One frame: what is on screen, what a click lands on, and the focused
/// rail card's cells to lift. Hits are `(row, start, end-exclusive, target)`.
struct Frame {
    screen: Vec<Vec<Placed>>,
    hits: Vec<(u16, u16, u16, Target)>,
    lift_rect: Option<(u16, u16, u16, u16)>,
}

/// A row painted at its own offset and width, so a slab's background stays
/// within its edges instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
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

/// Converts home's owned sections to the shared renderer's borrow.
fn to_rprojs(c: &Look) -> Vec<RProj<'_>> {
    c.projs
        .iter()
        .map(|p| RProj {
            name: p.name,
            live: p
                .live
                .iter()
                .map(|s| RLive {
                    n: s.n,
                    st: s.st,
                    name: s.name,
                    branch: s.branch,
                    spend: s.spend,
                    dollars: s.dollars,
                    ctx: s.ctx,
                    age: s.age,
                    id: Some(s.id),
                    turns: Some(s.turns),
                    wait: s.wait,
                    closing: s.closing,
                    act: None,
                })
                .collect(),
            gone: p
                .gone
                .iter()
                .map(|g| RGone {
                    name: g.name,
                    id: Some(g.id),
                    turns: Some(g.turns),
                    spend: g.spend,
                    age: g.age,
                })
                .collect(),
        })
        .collect()
}

/// The workspace picker over the fleet: recent workspaces, the selected one
/// on a full-width selection bar, in the shared panel frame with a bold-key
/// legend foot.
fn picker(wsel: usize, cols: usize) -> Vec<super::Row> {
    const TITLE: &str = "Workspaces";
    let mut body = vec![];
    for (i, r) in RECENTS.iter().enumerate() {
        if i == wsel {
            body.extend(super::panel::bar(vec![row(vec![
                sp("› ", bold()),
                sp(r.to_string(), bold()),
            ])]));
        } else {
            body.push(row(vec![
                sp("  ", Style::new()),
                sp(r.to_string(), Style::new()),
            ]));
        }
    }
    let natural = body
        .iter()
        .map(|r| super::width(&r.spans))
        .max()
        .unwrap_or(0)
        .max(TITLE.width())
        .max("↑↓ move · Enter open · Esc closes".width());
    let w = super::panel::fit_width(natural, 55, cols);
    super::panel::frame(
        Some(super::panel::title_row(TITLE, None)),
        body,
        Some(super::panel::footer_legend(&[
            ("↑↓", "move"),
            ("Enter", "open"),
            ("Esc", "closes"),
        ])),
        w,
    )
}

/// The recent workspace a picker row names, if any: the row's text past
/// its `› ` or two-space marker, matched exactly.
fn recent_at(text: &str) -> Option<usize> {
    let t = text.trim();
    let t = t.strip_prefix("› ").unwrap_or(t);
    let t = t.strip_prefix("  ").unwrap_or(t);
    RECENTS.iter().position(|&r| r == t)
}

fn frame(c: &Look, cols: usize, rows: usize, image: bool) -> Frame {
    // The model picker opens over the fleet as a full-screen swapped view,
    // as `/model` does in the conversation.
    if let Some(p) = &c.ui.picker {
        let mut screen: Vec<Vec<Placed>> = (0..rows)
            .map(|_| {
                vec![Placed {
                    x: 0,
                    w: cols as u16,
                    row: row(vec![]),
                }]
            })
            .collect();
        let mut hits: Vec<(u16, u16, u16, Target)> = vec![];
        let mut all = vec![super::view_header("Models", "/model")];
        all.extend(crate::model_picker::view(p, cols));
        // The focused row stays visible: its index, then the header's row.
        let at = all
            .iter()
            .position(|r| r.act == Some(Act::Pick(p.focus)))
            .unwrap_or(0);
        let off = (at + 1).saturating_sub(rows.saturating_sub(1));
        for (k, r) in all.iter().enumerate().skip(off).take(rows) {
            let y = (k - off) as u16;
            let bg = r.bg;
            screen[y as usize] = vec![Placed {
                x: 0,
                w: cols as u16,
                row: super::Row {
                    spans: fit(&r.spans, cols),
                    bg,
                    ..Default::default()
                },
            }];
            if let Some(act @ (Act::Back | Act::Pick(_))) = r.act {
                hits.push((y, 0, cols as u16, Target::Picker(act)));
            }
            for &(a, b, act) in &r.hot {
                hits.push((y, a, b, Target::Picker(act)));
            }
        }
        return Frame {
            screen,
            hits,
            lift_rect: None,
        };
    }
    if c.rail {
        return rail_frame(c, cols, rows);
    }
    let rprojs = to_rprojs(c);
    let comp =
        c.ui.completions
            .as_ref()
            .map(|cp| crate::completions::view(cp, &c.ui.input, cols));
    let view = FleetView {
        projs: &rprojs,
        st: &c.fleet,
        model: c.model,
        level: c.level,
        input: &c.ui.input,
        comp: comp.as_deref(),
        spend: LIVE_SPEND,
        cols,
        rows,
        image,
    };
    let (frows, ftags) = fleet_rows(&view);
    let mut screen: Vec<Vec<Placed>> = frows
        .into_iter()
        .map(|r| {
            vec![Placed {
                x: 0,
                w: cols as u16,
                row: r,
            }]
        })
        .collect();
    while screen.len() < rows {
        screen.push(vec![Placed {
            x: 0,
            w: cols as u16,
            row: row(vec![]),
        }]);
    }
    let mut hits: Vec<(u16, u16, u16, Target)> = vec![];
    for (y, tag) in ftags.iter().enumerate() {
        match tag {
            // An open workspace picker owns every click: background rows do
            // nothing while it covers them.
            FTag::Live(_, i) | FTag::Gone(i) if c.fleet.wpick.is_none() => {
                hits.push((y as u16, 0, cols as u16, Target::Row(*i)));
            }
            _ => {}
        }
    }
    // The chip row is the box's last row: its chips are click targets, but
    // only while no workspace picker covers them.
    let box_top = rows.saturating_sub(1 + BOX_H);
    if c.fleet.wpick.is_none() {
        let ws = if c.fleet.ws.is_empty() {
            RECENTS[0]
        } else {
            c.fleet.ws.as_str()
        };
        for (i, a, b) in chip_hits(&c.fleet, ws, c.model, c.level) {
            let t = match i {
                0 => Target::Workspace,
                1 => Target::Worktree,
                2 => Target::Model,
                _ => Target::Thinking,
            };
            hits.push(((box_top + BOX_H - 1) as u16, a as u16, b as u16, t));
        }
    }
    if let Some(wsel) = c.fleet.wpick {
        let prows = picker(wsel, cols);
        let pw = super::width(&prows[0].spans);
        let px0 = super::panel::x_for(pw, cols);
        for (k, r) in prows.into_iter().enumerate() {
            let y = PICK_Y + k;
            if y >= rows {
                break;
            }
            let text: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
            if let Some(i) = recent_at(&text) {
                hits.push((y as u16, px0 as u16, (px0 + pw) as u16, Target::Recent(i)));
            }
            screen[y] = vec![Placed {
                x: px0 as u16,
                w: pw as u16,
                row: r,
            }];
        }
    }
    Frame {
        screen,
        hits,
        lift_rect: None,
    }
}

/// The rail-focused still: the rail left with the needs-input card lifted,
/// the dimmed conversation behind it, a dimmed input box and the foot hint.
/// Compact cards, so all eight sessions fit the still.
fn rail_frame(c: &Look, cols: usize, rows: usize) -> Frame {
    let rail_w = 24.min(cols.saturating_sub(2)).max(2);
    let fakes: Vec<super::Fake> = c
        .projs
        .iter()
        .flat_map(|p| {
            p.live.iter().map(|s| super::Fake {
                name: s.name,
                project: p.name,
                wait: s.wait.to_string(),
                model: s.model.into(),
                branch: s.branch,
                ws: p.name,
                spend: s.spend.to_string(),
                dollars: s.dollars,
                ctx: s.ctx,
                age: s.age.to_string(),
                st: s.st,
            })
        })
        .collect();
    // The focused card is the waiting one, as ⌥2 jumps to it.
    let screen_idx = fakes
        .iter()
        .position(|s| s.st == SState::NeedsInput)
        .unwrap_or(0);
    let shown: Vec<usize> = (0..fakes.len()).collect();
    let done: Vec<u32> = c.projs.iter().map(|p| p.gone.len() as u32).collect();
    let (rrows, rmap) = super::rail_full_b(&fakes, &shown, screen_idx, 0, true, rail_w, 3, &done);
    let back = crate::overlays::backdrop(cols.saturating_sub(rail_w + 1), rows);
    let mut screen = vec![];
    for y in 0..rows {
        let r = rrows.get(y).cloned().unwrap_or(super::Row {
            spans: vec![],
            bg: Some(BP),
            ..Default::default()
        });
        screen.push(vec![
            Placed {
                x: 0,
                w: rail_w as u16,
                row: super::Row {
                    spans: fit(&r.spans, rail_w),
                    bg: r.bg,
                    ..Default::default()
                },
            },
            Placed {
                x: rail_w as u16,
                w: 1,
                row: row(vec![sp(" ", Style::new())]),
            },
            Placed {
                x: rail_w as u16 + 1,
                w: cols.saturating_sub(rail_w + 1) as u16,
                row: back.get(y).cloned().unwrap_or_default(),
            },
        ]);
    }
    // The input box keeps its place with a dim stripe and dim text while
    // the rail has focus: the conversation's one-row box, dimmed.
    if rows >= 4 {
        let w = cols.saturating_sub(rail_w + 1);
        let entry = vec![
            sp("▌", dim()),
            sp(" ", Style::new()),
            sp("› ", dim()),
            sp("the rail has the keyboard", dim()),
        ];
        for (k, r) in slab(vec![row(entry)], BI, None, w).into_iter().enumerate() {
            let y = rows - 4 + k;
            screen[y][2] = Placed {
                x: rail_w as u16 + 1,
                w: w as u16,
                row: super::Row {
                    spans: fit(&r.spans, w),
                    bg: r.bg,
                    ..Default::default()
                },
            };
        }
        screen[rows - 1] = vec![Placed {
            x: 0,
            w: cols as u16,
            row: super::Row {
                spans: fit(&centered(&rail_hint(), cols), cols),
                bg: None,
                ..Default::default()
            },
        }];
    }
    // The focused card lifts the way `--hover` lifts it.
    let lift_rect = rmap
        .iter()
        .find(|&&(i, _, _)| i == screen_idx)
        .map(|&(_, srow, len)| (0, rail_w as u16, srow as u16, (srow + len) as u16));
    Frame {
        screen,
        hits: vec![],
        lift_rect,
    }
}

/// Whether the kitty image covers the logo on this draw: supported, past
/// the first draw (the pixel logo draws first and the image replaces it,
/// so the first frame never waits), no overlay open (an image at z=0
/// draws above text and would cover a panel), the four-row logo drawn,
/// and room for its 32 cells. The rail still draws no logo.
fn image_shown(supported: bool, first: bool, c: &Look, cols: usize, rows: usize) -> bool {
    if !supported || first || c.rail {
        return false;
    }
    if c.fleet.wpick.is_some() || c.ui.picker.is_some() || c.ui.completions.is_some() {
        return false;
    }
    if rows < tall_min() {
        return false;
    }
    cols >= crate::logo::CELLS_W as usize + HTEXT_X
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
/// the conversation loop does.
fn wait_for(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|d| d.saturating_duration_since(now))
}

fn draw(
    term: &mut Term,
    c: &Look,
    img: &mut Image,
    supported: bool,
    first: bool,
) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let shown = image_shown(supported, first, c, cols as usize, rows as usize);
    let esc = img.escapes(shown, 0, LOGO_Y as u16);
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    let fr = frame(c, cols as usize, rows as usize, shown);
    term.draw(|f| {
        let buf = f.buffer_mut();
        for (y, placements) in fr.screen.iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
        if let Some((x, w, y0, y1)) = fr.lift_rect {
            let (bw, bh) = (buf.area.width, buf.area.height);
            for y in y0..y1.min(bh) {
                for xx in x..(x + w).min(bw) {
                    let cell = &mut buf[(xx, y)];
                    cell.set_bg(lift(cell.bg));
                }
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

/// What a chip does when Enter or a click activates it: the workspace chip
/// opens the workspace picker, the worktree switch toggles, and the model
/// and thinking chips open the model picker where the click opens it.
/// Focus stays on the chip: the caller keeps `chip` as it was.
fn open_chip(c: &mut Look, i: usize) {
    match i {
        0 => {
            c.fleet.wpick = Some(c.fleet.wsel);
        }
        1 => {
            c.fleet.worktree = !c.fleet.worktree;
        }
        2 => {
            c.ui.picker = Some(crate::model_picker::opened_at(c.model, None));
            c.ui.vscroll = 0;
        }
        _ => {
            c.ui.picker = Some(crate::model_picker::opened_at(c.model, Some(c.level)));
            c.ui.vscroll = 0;
        }
    }
}

/// The live fleet's keys, first match wins: Ctrl+C quits; the workspace
/// picker takes arrows, Enter and Esc and ignores the rest; the model
/// picker records on Enter and otherwise takes the picker's keys; Ctrl+L
/// and `/model` open the model picker; `/` opens slash completions;
/// → or Esc goes back to the session; focus runs from the session list
/// down to the entry bar and the chip row: ↓ past the last row reaches the
/// entry, ↓ from the entry reaches the chips, ↑ returns the same way, and
/// ↓ from a chip wraps to the first row; ← → move between chips; Enter
/// opens the focused row or starts a session from the box; Delete deletes
/// the focused exited row; other text fills the box.
fn on_key(c: &mut Look, k: Key, m: Mods) -> Step {
    // Ctrl+C quits, always.
    if k == Key::Char('c') && m.ctrl {
        return Step::Quit;
    }
    // The workspace picker takes its own keys; the rest are ignored.
    if c.fleet.wpick.is_some() {
        match k {
            Key::Up if !m.alt && !m.ctrl => c.fleet.wsel = c.fleet.wsel.saturating_sub(1),
            Key::Down if !m.alt && !m.ctrl => {
                c.fleet.wsel = (c.fleet.wsel + 1).min(RECENTS.len() - 1);
            }
            Key::Enter if !m.ctrl && !m.alt && !m.sup => {
                c.fleet.ws = RECENTS[c.fleet.wsel].into();
                c.fleet.wpick = None;
            }
            Key::Esc => c.fleet.wpick = None,
            _ => {}
        }
        return Step::Stay;
    }
    // The model picker records its choice on Enter, then closes. With no
    // matching row Enter keeps the picker open and records nothing.
    if c.ui.picker.is_some() {
        if k == Key::Enter && !m.ctrl && !m.alt && !m.sup {
            let empty =
                c.ui.picker
                    .as_ref()
                    .is_some_and(|p| crate::model_picker::visible(p).is_empty());
            if !empty {
                let (model, level) = crate::model_picker::chosen(c.ui.picker.as_ref().unwrap());
                c.model = model;
                c.level = level;
            }
        }
        crate::model_picker::on_key(&mut c.ui, k, m);
        return Step::Stay;
    }
    let before = c.ui.input.clone();
    let plain = !m.ctrl && !m.alt && !m.sup;
    // A focused chip's Enter always acts as a click: before the
    // draft-based `/model` dispatch below, which would otherwise consume
    // Enter on a `/model` draft, clear it and open the wrong picker.
    if c.fleet.chip.is_some() && c.ui.completions.is_none() && k == Key::Enter && plain {
        open_chip(c, c.fleet.chip.unwrap());
        return Step::Stay;
    }
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
    // With the completion panel open, arrows keep the panel's keys.
    let overlay = c.ui.completions.is_some();
    let rprojs = to_rprojs(c);
    let n = fleet_items(&rprojs).len();
    let step = match k {
        Key::Esc => Step::Conversation,
        Key::Right if plain && c.fleet.chip.is_none() => Step::Conversation,
        Key::Down if !m.alt && !m.ctrl && !overlay => {
            if c.fleet.chip.is_some() {
                c.fleet.chip = None;
                c.sel = if n > 0 { Some(0) } else { None };
                c.fleet.sel = c.sel;
            } else if let Some(i) = c.sel {
                c.sel = (i + 1 < n).then_some(i + 1);
                c.fleet.sel = c.sel;
            } else {
                c.fleet.chip = Some(c.fleet.last_chip);
            }
            Step::Stay
        }
        Key::Up if !m.alt && !m.ctrl && !overlay => {
            if c.fleet.chip.is_some() {
                c.fleet.chip = None;
            } else if let Some(i) = c.sel {
                c.sel = i.checked_sub(1);
                c.fleet.sel = c.sel;
            } else if n > 0 {
                c.sel = Some(n - 1);
                c.fleet.sel = c.sel;
            }
            Step::Stay
        }
        Key::Left if plain => {
            if let Some(i) = c.fleet.chip {
                c.fleet.chip = Some(i.saturating_sub(1));
                c.fleet.last_chip = c.fleet.chip.unwrap();
            }
            Step::Stay
        }
        Key::Right if plain => {
            if let Some(i) = c.fleet.chip {
                c.fleet.chip = Some((i + 1).min(3));
                c.fleet.last_chip = c.fleet.chip.unwrap();
            }
            Step::Stay
        }
        Key::Enter if plain => {
            if c.sel.is_some() || !c.ui.input.trim().is_empty() {
                Step::Conversation
            } else {
                Step::Stay
            }
        }
        Key::Delete if plain => {
            if let Some(i) = c.sel
                && let Some(item) = fleet_items(&rprojs).get(i)
                && let FItem::Gone(pi, gi) = item
            {
                c.projs[*pi].gone.remove(*gi);
                let left = fleet_items(&to_rprojs(c)).len();
                c.sel = (left > 0).then(|| i.min(left - 1));
                c.fleet.sel = c.sel;
            }
            Step::Stay
        }
        Key::Backspace if plain => {
            c.fleet.chip = None;
            c.ui.input.pop();
            Step::Stay
        }
        Key::Char(ch) if plain => {
            c.fleet.chip = None;
            c.sel = None;
            c.fleet.sel = None;
            c.ui.input.push(ch);
            Step::Stay
        }
        _ => Step::Stay,
    };
    end_key(c, &before);
    step
}

/// The live fleet's clicks: the chips open their pickers, the worktree
/// switch toggles, a row opens its session, and a recent chooses its
/// workspace. Any click clears chip focus.
fn on_click(c: &mut Look, x: u16, y: u16, cols: usize, rows: usize) -> Step {
    // Any mouse click clears chip focus.
    c.fleet.chip = None;
    let fr = frame(c, cols, rows, false);
    // Later hits sit above earlier ones: the picker floats over the list.
    let hit = fr
        .hits
        .iter()
        .rev()
        .find(|&&(hy, x0, x1, _)| hy == y && x >= x0 && x < x1);
    let Some((_, _, _, t)) = hit.copied() else {
        return Step::Stay;
    };
    match t {
        Target::Workspace => {
            open_chip(c, 0);
            Step::Stay
        }
        Target::Worktree => {
            open_chip(c, 1);
            Step::Stay
        }
        Target::Model => {
            open_chip(c, 2);
            Step::Stay
        }
        Target::Thinking => {
            open_chip(c, 3);
            Step::Stay
        }
        Target::Row(_) => Step::Conversation,
        Target::Recent(i) => {
            c.fleet.ws = RECENTS[i].into();
            c.fleet.wsel = i;
            c.fleet.wpick = None;
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
/// interactive fleet, the other cases one still frame each. Mutually
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
            format!(
                "unknown --home case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        ));
    };
    let supported = !c.rail && crate::logo::supported(|k| std::env::var(k).ok());
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

/// The interactive fleet: typing fills the box, the pickers open over it,
/// arrows walk every row, and Enter opens the focused session or starts
/// one from the box.
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
    fn enter_with_no_matches_records_nothing() {
        let mut c = base();
        let (model, level) = (c.model, c.level);
        c.ui.input = "draft".into();
        let mut p = crate::model_picker::for_case("list");
        p.query = "zzz".into();
        c.ui.picker = Some(p);
        on_key(&mut c, Key::Enter, Mods::default());
        assert!(c.ui.picker.is_some(), "empty Enter closed the picker");
        assert_eq!(c.ui.input, "draft");
        assert_eq!((c.model, c.level), (model, level));
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 7);
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    /// `frame` painted with `paint` exactly as `draw` does, with the
    /// focused rail card lifted.
    fn buffer(c: &Look, cols: u16, rows: u16) -> Buffer {
        painted(c, cols, rows, false)
    }

    /// `buffer` with the image shown, as the second startup draw paints it.
    fn buffer_image(c: &Look, cols: u16, rows: u16) -> Buffer {
        painted(c, cols, rows, true)
    }

    fn painted(c: &Look, cols: u16, rows: u16, image: bool) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        let fr = frame(c, cols as usize, rows as usize, image);
        for (y, placements) in fr.screen.iter().enumerate() {
            for p in placements {
                paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        if let Some((x, w, y0, y1)) = fr.lift_rect {
            for y in y0..y1.min(rows) {
                for xx in x..(x + w).min(cols) {
                    let cell = &mut buf[(xx, y)];
                    cell.set_bg(lift(cell.bg));
                }
            }
        }
        buf
    }

    fn text(c: &Look, cols: u16, rows: u16) -> String {
        let fr = frame(c, cols as usize, rows as usize, false);
        fr.screen
            .into_iter()
            .map(|ps| {
                let mut ps = ps;
                ps.sort_by_key(|p| p.x);
                ps.iter()
                    .map(|p| {
                        p.row
                            .spans
                            .iter()
                            .map(|s| s.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn header_counts_and_version() {
        let c = crate::cases::lookup(CASES, "full-sessions").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("0.0.1"), "version missing");
        for part in [
            "2 waiting on you",
            "2 working",
            "1 retrying",
            "1 crashed",
            "2 ready",
            "$5.78 live",
        ] {
            assert!(t.contains(part), "counts miss {part}");
        }
        // The version hangs past nothing: no name text beside the counts.
        assert!(!t.contains("fiber ·"), "header repeats the name");
    }

    #[test]
    fn sections_live_and_exited_rows() {
        let c = crate::cases::lookup(CASES, "full-sessions").unwrap();
        let t = text(&c, 160, 48);
        for proj in ["fiber", "pi-rig", "beacon"] {
            assert!(t.contains(proj), "missing section {proj}");
        }
        assert!(t.contains("5 live · $3.92"), "fiber header wrong");
        assert!(t.contains("2 waiting on you"), "fiber wait count wrong");
        assert!(t.contains("⌥1"), "numbers missing");
        assert!(t.contains("⌥8"), "last number missing");
        assert!(t.contains("NEEDS INPUT"), "state word missing");
        assert!(
            t.contains("approval: shell cargo publish --dry-run"),
            "full wait missing"
        );
        assert!(t.contains("⎇ review-688"), "branch missing");
        assert!(t.contains("41%"), "context missing");
        assert!(t.contains("s_3ab702 · 3 turns"), "id and turns missing");
        assert!(t.contains("exited · 4"), "exited header missing");
        assert!(t.contains("○ cut the 0.0.1 release"), "exited row missing");
        assert!(
            t.contains("s_7f20aa · 5 turns · $1.10 · 3h ago"),
            "exited detail missing"
        );
        assert!(
            t.contains("… 1 more · /resume lists all"),
            "more line missing"
        );
        assert!(t.contains("Enter starts a session"), "chips missing");
    }

    #[test]
    fn focused_row_sits_on_the_selection_bar() {
        let c = crate::cases::lookup(CASES, "full-sessions").unwrap();
        let buf = buffer(&c, 160, 48);
        let t = text(&c, 160, 48);
        let y = t
            .split('\n')
            .position(|l| l.contains("fix flaky lock test"))
            .unwrap() as u16;
        assert_eq!(buf[(0, y)].bg, SEL, "first live row not on the bar");
        assert_eq!(buf[(0, y + 1)].bg, SEL, "its second line not on the bar");
        let y2 = t
            .split('\n')
            .position(|l| l.contains("review #688"))
            .unwrap() as u16;
        assert_ne!(buf[(0, y2)].bg, SEL, "unfocused row on the bar");
    }

    #[test]
    fn focus_exited_bars_the_exited_row() {
        let c = crate::cases::lookup(CASES, "full-focus-exited").unwrap();
        let buf = buffer(&c, 160, 48);
        let t = text(&c, 160, 48);
        let y = t
            .split('\n')
            .position(|l| l.contains("cut the 0.0.1 release"))
            .unwrap() as u16;
        assert_eq!(buf[(0, y)].bg, SEL, "exited row not on the bar");
    }

    #[test]
    fn empty_names_nothing() {
        let c = crate::cases::lookup(CASES, "full-empty").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("no sessions yet"), "header line wrong");
        assert!(t.contains("No sessions yet."), "middle line wrong");
        assert!(
            t.contains("Type below and press Enter to start one."),
            "prompt line wrong"
        );
        assert!(!t.contains("live ·"), "sections drawn without sessions");
    }

    #[test]
    fn closing_reads_dim() {
        let c = crate::cases::lookup(CASES, "full-closing").unwrap();
        let buf = buffer(&c, 160, 48);
        let t = text(&c, 160, 48);
        let y = t.split('\n').position(|l| l.contains("CLOSING")).unwrap() as u16;
        let mut found = false;
        for x in 0..160 {
            if buf[(x, y)].symbol() == "C" {
                assert!(
                    buf[(x, y)].modifier.contains(Modifier::DIM),
                    "CLOSING not dim at {x}"
                );
                found = true;
                break;
            }
        }
        assert!(found, "no CLOSING row");
        assert!(
            t.contains("fix flaky lock test"),
            "closing row lost its name"
        );
    }

    #[test]
    fn picker_lists_recent_workspaces() {
        let c = crate::cases::lookup(CASES, "full-picker").unwrap();
        let t = text(&c, 160, 48);
        assert!(t.contains("Workspaces"), "picker title missing");
        for ws in ["~/work/fiber", "~/work/beacon", "~/work/pi-rig"] {
            assert!(t.contains(ws), "missing recent {ws}");
        }
    }

    #[test]
    fn rail_still_lifts_the_waiting_card() {
        let c = crate::cases::lookup(CASES, "rail-focused").unwrap();
        let t = text(&c, 160, 48);
        // Compact cards name the waiting session instead of its state word.
        assert!(t.contains("review #688"), "waiting card missing");
        assert!(t.contains("the rail has the keyboard"), "dim input missing");
        assert!(t.contains("⌥1–9"), "foot hint missing");
        assert!(t.contains("back to the session"), "hint label missing");
        // The waiting card is brighter than the working card above it.
        let buf = buffer(&c, 160, 48);
        let y = t
            .split('\n')
            .position(|l| l.contains("review #688"))
            .unwrap() as u16;
        let y0 = t.split('\n').position(|l| l.contains("fix flaky")).unwrap() as u16;
        assert_ne!(buf[(3, y)].bg, buf[(3, y0)].bg, "focused card not lifted");
    }

    #[test]
    fn image_shown_boundaries() {
        let plain = || crate::cases::lookup(CASES, "full-sessions").unwrap();
        // Supported and past the first draw, nothing open: the image shows.
        assert!(image_shown(true, false, &plain(), 160, 48));
        // Without support, or on the first draw, the pixel logo stays.
        assert!(!image_shown(false, false, &plain(), 160, 48));
        assert!(!image_shown(true, true, &plain(), 160, 48));
        // An open overlay covers the logo's cells, so the image hides.
        assert!(!image_shown(
            true,
            false,
            &crate::cases::lookup(CASES, "full-picker").unwrap(),
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
        // The rail still draws no logo at all.
        assert!(!image_shown(
            true,
            false,
            &crate::cases::lookup(CASES, "rail-focused").unwrap(),
            160,
            48
        ));
        // The image needs the four-row logo and its 32 cells.
        assert!(image_shown(true, false, &plain(), 160, tall_min()));
        assert!(!image_shown(true, false, &plain(), 160, tall_min() - 1));
        assert!(image_shown(true, false, &plain(), 80, 48));
        assert!(!image_shown(true, false, &plain(), 66, 48));
    }

    #[test]
    fn image_escapes_sequence() {
        let mut img = Image {
            transmitted: false,
            placed: false,
        };
        assert_eq!(img.escapes(false, 0, 0), "");
        assert_eq!(
            img.escapes(true, 0, 0),
            crate::logo::transmit() + &crate::logo::place(0, 0)
        );
        assert_eq!(img.escapes(true, 0, 0), crate::logo::place(0, 0));
        assert_eq!(img.escapes(false, 0, 0), crate::logo::HIDE);
        assert_eq!(img.escapes(false, 0, 0), "");
        assert_eq!(img.escapes(true, 0, 0), crate::logo::place(0, 0));
    }

    #[test]
    fn wait_for_boundaries() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        assert_eq!(wait_for(None, now), None);
        assert_eq!(
            wait_for(Some(now + Duration::from_millis(30)), now),
            Some(Duration::from_millis(30))
        );
        assert_eq!(wait_for(Some(now), now), Some(Duration::ZERO));
        assert_eq!(
            wait_for(Some(now - Duration::from_millis(5)), now),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn home_blanks_the_logo_under_the_image() {
        let c = crate::cases::lookup(CASES, "full-sessions").unwrap();
        let buf = buffer_image(&c, 160, 48);
        for y in 0..4 {
            for x in 0..32 {
                assert_eq!(buf[(x, y)].symbol(), " ", "cell ({x}, {y})");
            }
        }
        assert!(text(&c, 160, 48).contains("0.0.1"));
    }

    #[test]
    fn arrows_walk_list_entry_and_chips() {
        let mut c = base();
        // ↓ from the entry bar focuses the chip row.
        on_key(&mut c, Key::Down, Mods::default());
        assert_eq!(c.fleet.chip, Some(0));
        // ↓ from a chip focuses the first row.
        on_key(&mut c, Key::Down, Mods::default());
        assert_eq!(c.sel, Some(0));
        assert_eq!(c.fleet.chip, None);
        // ↑ from the first row returns to the entry bar.
        on_key(&mut c, Key::Up, Mods::default());
        assert_eq!(c.sel, None);
        // ↑ from the entry bar focuses the last row.
        on_key(&mut c, Key::Up, Mods::default());
        assert!(c.sel.is_some());
        // ↓ past the last row returns to the entry bar.
        on_key(&mut c, Key::Down, Mods::default());
        assert_eq!(c.sel, None);
    }

    #[test]
    fn enter_opens_and_esc_leaves() {
        let mut c = base();
        c.sel = Some(0);
        c.fleet.sel = Some(0);
        assert!(matches!(
            on_key(&mut c, Key::Enter, Mods::default()),
            Step::Conversation
        ));
        assert!(matches!(
            on_key(&mut c, Key::Esc, Mods::default()),
            Step::Conversation
        ));
        assert!(matches!(
            on_key(&mut c, Key::Right, Mods::default()),
            Step::Conversation
        ));
    }

    #[test]
    fn typing_clears_row_focus_into_the_entry_bar() {
        let mut c = base();
        c.sel = Some(2);
        c.fleet.sel = Some(2);
        on_key(&mut c, Key::Char('f'), Mods::default());
        assert_eq!(c.sel, None);
        assert_eq!(c.fleet.sel, None);
        assert_eq!(c.ui.input, "f");
    }

    #[test]
    fn delete_drops_the_focused_exited_row() {
        let mut c = base();
        c.sel = Some(5);
        c.fleet.sel = Some(5);
        on_key(&mut c, Key::Delete, Mods::default());
        let t = text(&c, 160, 48);
        assert!(
            !t.contains("cut the 0.0.1 release"),
            "deleted row still drawn"
        );
        assert!(t.contains("speed up the render loop"), "wrong row dropped");
    }

    #[test]
    fn no_row_overflows_its_screen() {
        for name in CASES.iter().map(|c| c.name) {
            let c = crate::cases::lookup(CASES, name).unwrap();
            let fr = frame(&c, 160, 48, false);
            assert_eq!(fr.screen.len(), 48, "{name} is not 48 rows");
            for (y, ps) in fr.screen.iter().enumerate() {
                for p in ps {
                    assert!(
                        crate::width(&p.row.spans) <= p.w as usize,
                        "{name} row {y} overflows its {w} columns",
                        w = p.w
                    );
                    assert!(p.x + p.w <= 160, "{name} row {y} runs past the screen edge");
                }
            }
        }
    }
}
