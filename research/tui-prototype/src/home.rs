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
    BI, BLUE, CYAN, ORANGE, SEL, bold, dim, fg, fit, lift, paint, row, slab, sp,
    t,
};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use crate::cases::{Case, Surface};
use std::io::{self, Write};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// Every `--home` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case { name: "empty", help: "no sessions yet: logo, input box, chips", check: "the logo should read as pixel letters four rows tall (⌇ in accent, the name in the accent gradient, `0.0.1` dim on the last row); under it the large input box with `/? for shortcuts`, the chip row and `enter starts a session`; under the box one dim `No sessions yet` line; the key hint at the foot.", build: || Look { sessions: false, ..base() } },
    Case { name: "sessions", help: "six exited sessions listed", check: "six exited rows, each `○ name · spend`, the three outside the launch project with their workspace's last segment; the long pi-rig name should fit without pushing the spend off the row.", build: || base() },
    Case { name: "hover-workspace", help: "the workspace chip hovered", check: "the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Workspace), ..base() } },
    Case { name: "hover-worktree", help: "the worktree switch hovered", check: "the switch chip should sit lighter with its ● still blue.", build: || Look { hover: Some(Hover::Worktree), ..base() } },
    Case { name: "hover-model", help: "the model chip hovered", check: "the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Model), ..base() } },
    Case { name: "hover-thinking", help: "the thinking chip hovered", check: "the one chip should sit lighter than its neighbours while keeping its own text colour.", build: || Look { hover: Some(Hover::Thinking), ..base() } },
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

struct Look {
    sessions: bool,
    hover: Option<Hover>,
    worktree: bool,
    picker: Option<Picker>,
}

fn base() -> Look {
    Look { sessions: true, hover: None, worktree: true, picker: None }
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
    y += 1;
    if c.sessions {
        for l in session_rows(w) {
            put(&mut screen, y, x0, w, l, None);
            y += 1;
        }
    } else {
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
        y += 1;
    }
    let _ = y;
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
        assert_eq!(CASES.len(), 10);
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
