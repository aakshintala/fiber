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
    BI, BLUE, CYAN, HD, ORANGE, SEL, SX_KW, SX_STR, bold, dim, fg, fit, lift, paint, row, slab, sp,
    t,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

/// Every `--home` case, named in README.md.
pub(crate) const CASES: &[&str] = &[
    "empty",
    "sessions",
    "hover-workspace",
    "hover-worktree",
    "hover-model",
    "hover-thinking",
    "worktree-on",
    "worktree-off",
    "picker-recent",
    "picker-typed",
];

/// The home input box and session list width at 160 columns.
const HOME_W: usize = 84;
/// The picker's width.
const PICK_W: usize = 60;
/// The fixture's version, as docs/tui.md's logo reads.
const VERSION: &str = "0.0.1";

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

struct Case {
    sessions: bool,
    hover: Option<Hover>,
    worktree: bool,
    picker: Option<Picker>,
}

fn parse(name: &str) -> Option<Case> {
    let base = || Case {
        sessions: true,
        hover: None,
        worktree: true,
        picker: None,
    };
    match name {
        "empty" => Some(Case {
            sessions: false,
            ..base()
        }),
        "sessions" => Some(base()),
        "hover-workspace" => Some(Case {
            hover: Some(Hover::Workspace),
            ..base()
        }),
        "hover-worktree" => Some(Case {
            hover: Some(Hover::Worktree),
            ..base()
        }),
        "hover-model" => Some(Case {
            hover: Some(Hover::Model),
            ..base()
        }),
        "hover-thinking" => Some(Case {
            hover: Some(Hover::Thinking),
            ..base()
        }),
        "worktree-on" => Some(base()),
        "worktree-off" => Some(Case {
            worktree: false,
            ..base()
        }),
        "picker-recent" => Some(Case {
            picker: Some(Picker::Recent),
            ..base()
        }),
        "picker-typed" => Some(Case {
            picker: Some(Picker::Typed),
            ..base()
        }),
        _ => None,
    }
}

/// An exited session: the name, or the first prompt when it has none; the
/// spend; and the workspace's last path segment, for a session outside the
/// launch project (`~/work/fiber`). `ws: None` is the launch project.
struct Exited {
    name: &'static str,
    prompt: &'static str,
    spend: &'static str,
    ws: Option<&'static str>,
}

const SESSIONS: &[Exited] = &[
    Exited {
        name: "fix flaky lock test",
        prompt: "",
        spend: "$0.42",
        ws: None,
    },
    Exited {
        name: "docs: rail spec",
        prompt: "",
        spend: "$1.10",
        ws: None,
    },
    Exited {
        name: "migrate every provider adapter to the new streaming contract",
        prompt: "",
        spend: "$2.31",
        ws: Some("pi-rig"),
    },
    Exited {
        name: "",
        prompt: "how do I backfill embeddings for old sessions?",
        spend: "$0.87",
        ws: Some("fiber-worktrees"),
    },
    Exited {
        name: "bump ratatui",
        prompt: "",
        spend: "$0.08",
        ws: None,
    },
    Exited {
        name: "rewrite onboarding tour",
        prompt: "",
        spend: "$0.35",
        ws: Some("beacon"),
    },
];

/// Recent workspaces, from `recent.jsonl`.
const RECENTS: &[&str] = &[
    "~/work/fiber",
    "~/work/fiber-worktrees",
    "~/work/beacon",
    "~/work/pi-rig",
];
/// Every directory the typed-path row can complete to.
const ALL_DIRS: &[&str] = &[
    "~/work/fiber",
    "~/work/fiber-worktrees",
    "~/work/beacon",
    "~/work/pi-rig",
];
/// The fixture's typed prefix for the `picker-typed` case.
const TYPED: &str = "~/work/fi";

/// Directories `typed` completes to, in order. An exact match is already
/// complete, so it is left out; empty typed lists everything.
pub(crate) fn complete_dirs<'a>(typed: &str, dirs: &[&'a str]) -> Vec<&'a str> {
    dirs.iter()
        .copied()
        .filter(|d| d.len() > typed.len() && d.starts_with(typed))
        .collect()
}

// ============================================================ pieces
/// A pixel letter: `#` a filled cell, `o` its shaded counter, `.` empty.
type Glyph = (&'static [&'static str], Color);
const LETTERS: &[Glyph] = &[
    (&[".###", "#...", "###.", "#..."], HD),     // f: heading
    (&["#", " ", "#", "#"], BLUE),               // i: accent
    (&["#   ", "#   ", "####", "# o#"], SX_STR), // b: string
    (&[" ###", "#   ", "##o#", " ###"], CYAN),   // e: type
    (&["### ", "#  #", "#   ", "#   "], SX_KW),  // r: keyword
];

/// The logo: `⌇ fiber 0.0.1`, four rows tall in half-block pixel letters, the
/// ⌇ in accent, the name in the accent gradient, the version dim.
fn logo(w: usize) -> Vec<Vec<Span<'static>>> {
    let mut out = vec![vec![]; 4];
    for r in 0..4 {
        let mut line = vec![sp("⌇ ", fg(BLUE))];
        for (g, c) in LETTERS {
            for ch in g[r].chars() {
                match ch {
                    '#' => line.push(sp("█", fg(*c))),
                    'o' => line.push(sp("░", fg(*c).add_modifier(Modifier::DIM))),
                    _ => line.push(sp(" ", Style::new())),
                }
            }
            line.push(sp(" ", Style::new()));
        }
        line.push(sp(" ", Style::new()));
        if r == 3 {
            line.push(sp(VERSION, dim()));
        }
        out[r] = fit(&line, w);
    }
    out
}

/// One chip: bracketed text on the raised surface, lighter under the pointer.
fn chip(text: &str, st: Style, hovered: bool) -> Span<'static> {
    let bg = if hovered { lift(SEL) } else { SEL };
    sp(format!("[{text}]"), st.bg(bg))
}

fn chip_row(c: &Case) -> Vec<Span<'static>> {
    let h = |k: Hover| c.hover == Some(k);
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
    s.push(chip("gpt-6.1-sol", fg(CYAN), h(Hover::Model)));
    s.push(sp(" ", Style::new()));
    s.push(chip("medium", fg(ORANGE), h(Hover::Thinking)));
    s.push(t());
    s.push(sp("enter starts a session ", dim()));
    s
}

/// The large input box: the `/? for shortcuts` placeholder over the chip row,
/// with the prototype's ▌ stripe.
fn input_box(c: &Case, w: usize) -> Vec<super::Row> {
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

fn session_rows(w: usize) -> Vec<Vec<Span<'static>>> {
    SESSIONS
        .iter()
        .map(|s| {
            let mut line = vec![
                sp("○ ", dim()),
                sp(
                    if s.name.is_empty() { s.prompt } else { s.name },
                    Style::new(),
                ),
                sp(" · ", dim()),
                sp(s.spend, dim()),
            ];
            if let Some(ws) = s.ws {
                line.push(sp(" · ", dim()));
                line.push(sp(ws, dim()));
            }
            fit(&line, w)
        })
        .collect()
}

/// The workspace picker over home: recent workspaces, or the typed-path row
/// with its completions over the recents.
fn picker(p: Picker, w: usize) -> Vec<super::Row> {
    let mut inner = vec![row(vec![sp(" Workspaces", bold())])];
    if p == Picker::Typed {
        inner.push(row(vec![
            sp("  ", Style::new()),
            sp("› ", fg(CYAN)),
            sp(TYPED, Style::new()),
            sp("█", dim()),
        ]));
        for (i, d) in complete_dirs(TYPED, ALL_DIRS).iter().enumerate() {
            let rest = d.strip_prefix("~/work/").unwrap_or(d);
            if i == 0 {
                inner.push(super::Row {
                    spans: vec![sp("▌ ", fg(BLUE)), sp("~/work/", dim()), sp(rest, bold())],
                    bg: Some(lift(SEL)),
                    ..Default::default()
                });
            } else {
                inner.push(row(vec![
                    sp("  ", Style::new()),
                    sp("~/work/", dim()),
                    sp(rest.to_string(), Style::new()),
                ]));
            }
        }
        inner.push(row(vec![sp("  ── recents ──", dim())]));
    }
    for (i, r) in RECENTS.iter().enumerate() {
        let selected = p == Picker::Recent && i == 0;
        if selected {
            inner.push(super::Row {
                spans: vec![sp("▌ ", fg(BLUE)), sp(r.to_string(), Style::new())],
                bg: Some(lift(SEL)),
                ..Default::default()
            });
        } else {
            inner.push(row(vec![
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
    slab(inner, SEL, None, w)
}

// ============================================================ frame
/// A full-screen row: `spans` centred at the content width.
fn full(spans: Vec<Span<'static>>, x0: usize) -> super::Row {
    let mut s = vec![sp(" ".repeat(x0), Style::new())];
    s.extend(spans);
    row(s)
}

fn frame(c: &Case, cols: usize, rows: usize) -> Vec<super::Row> {
    let w = HOME_W.min(cols.saturating_sub(4)).max(20);
    let x0 = cols.saturating_sub(w) / 2;
    let mut screen: Vec<super::Row> = (0..rows).map(|_| row(vec![])).collect();
    let mut y = 2;
    for l in logo(w) {
        if y < rows {
            screen[y] = full(l, x0);
        }
        y += 1;
    }
    y += 1;
    for r in input_box(c, w) {
        if y < rows {
            let bg = r.bg;
            screen[y] = full(r.spans, x0);
            screen[y].bg = bg;
        }
        y += 1;
    }
    y += 1;
    if c.sessions {
        for l in session_rows(w) {
            if y < rows {
                screen[y] = full(l, x0);
            }
            y += 1;
        }
    } else {
        let line = fit(
            &[sp(
                "No sessions yet — type a prompt above and press Enter.",
                dim(),
            )],
            w,
        );
        if y < rows {
            screen[y] = full(line, x0);
        }
        y += 1;
    }
    let _ = y;
    let hint = "enter starts a session · ↑↓ select · q quits";
    if rows >= 2 {
        let pad = cols.saturating_sub(hint.width()) / 2;
        screen[rows - 2] = row(vec![sp(" ".repeat(pad), Style::new()), sp(hint, dim())]);
    }
    if let Some(p) = c.picker {
        let pw = PICK_W.min(cols.saturating_sub(4)).max(20);
        let px0 = cols.saturating_sub(pw) / 2;
        for (k, r) in picker(p, pw).into_iter().enumerate() {
            let py = 12 + k;
            if py < rows {
                screen[py] = full(r.spans, px0);
                screen[py].bg = r.bg;
            }
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
        for (y, r) in frame(c, cols as usize, rows as usize).iter().enumerate() {
            paint(buf, 0, y as u16, cols, r);
        }
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--home` case and waits for a key. Mutually exclusive with the
/// conversation view: the fixture replay never draws.
pub(crate) fn run_home(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.home.clone().unwrap_or_default();
    let Some(c) = parse(&name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --home case {name:?}; one of: {}", CASES.join(", ")),
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("home {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("home {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
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
    fn empty_typed_lists_everything() {
        assert_eq!(complete_dirs("", ALL_DIRS), ALL_DIRS);
    }

    #[test]
    fn prefix_keeps_order_and_drops_the_rest() {
        assert_eq!(
            complete_dirs("~/work/fi", ALL_DIRS),
            vec!["~/work/fiber", "~/work/fiber-worktrees"]
        );
    }

    #[test]
    fn exact_prefix_of_a_longer_dir_still_completes() {
        assert_eq!(complete_dirs("~/work/fiber", ALL_DIRS), vec!["~/work/fiber-worktrees"]);
    }

    #[test]
    fn exact_typed_with_no_longer_match_completes_nothing() {
        assert!(complete_dirs("~/work/pi-rig", ALL_DIRS).is_empty());
    }

    #[test]
    fn longer_than_every_dir_matches_nothing() {
        assert!(complete_dirs("~/work/fiber-worktrees/x", ALL_DIRS).is_empty());
    }

    #[test]
    fn no_prefix_match_matches_nothing() {
        assert!(complete_dirs("~/play", ALL_DIRS).is_empty());
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert!(complete_dirs("~/WORK/fi", ALL_DIRS).is_empty());
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(CASES.iter().all(|n| parse(n).is_some()));
        assert_eq!(CASES.len(), 10);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn logo_is_four_rows_tall() {
        assert_eq!(logo(HOME_W).len(), 4);
    }
}
