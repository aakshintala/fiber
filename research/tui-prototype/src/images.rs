//! Images and links, for #1637.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! swapped-view `view_header` and the kitty detection in `logo.rs`), and the
//! program waits for a key; Esc, q or Ctrl+C quits. The fixture replay never
//! draws under a case. An image a tool returned draws through kitty's Unicode
//! placeholders where the terminal speaks the protocol, and as its one line
//! everywhere else, as docs/tui.md, "Images", describes.

use super::input::{Ev, Key, Mouse};
use super::{Args, Fold, Term};
use super::{BLUE, BU, CYAN, SEL, bold, dim, fg, lift, paint, row, slab, sp, t, view_header};
use crate::cases::{Case, Surface};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::io::{self, Write};

/// Every `--image-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "tool_image",
        help: "a ledger row for a tool that returned an image, inline through kitty placeholders or as its one line",
        check: "the read ledger row with its blue tick and dimensions, the image below it through kitty placeholders where the terminal speaks the protocol and its one line everywhere else.",
        build: || Look {
            kind: Kind::ToolImage,
        },
    },
    Case {
        name: "fallback",
        help: "an image's one line, and one that cannot show with its notice",
        check: "the one-line image row in the link look, and a second row for an image that cannot show with the notice saying why.",
        build: || Look {
            kind: Kind::Fallback,
        },
    },
    Case {
        name: "pasted",
        help: "a pasted image in the prompt bubble, inline through kitty placeholders or as its one line",
        check: "the prompt bubble with [Image #1] and the image through kitty placeholders where the terminal speaks the protocol and its one line everywhere else.",
        build: || Look { kind: Kind::Pasted },
    },
    Case {
        name: "links",
        help: "a reply with a focused and a hovered link, and a table with a focused and a hovered link cell",
        check: "the reply with its focused and hovered links, the table with its focused and hovered link cells, the focused link on the selection tint with the Enter-opens hint.",
        build: || Look { kind: Kind::Links },
    },
    Case {
        name: "screen_reader",
        help: "the fixture drawn flat: one line per item in reading order, state in words",
        check: "the flat frame with no tints, edges, stripes or animation: one line per item in reading order, state in words not glyphs, the image as its one line.",
        build: || Look {
            kind: Kind::ScreenReader,
        },
    },
    Case {
        name: "title_approval",
        help: "the terminal title while an approval waits",
        check: "the labelled OSC 2 row for `! fiber · approval`, written to the terminal and restored to `fiber` on exit.",
        build: || Look {
            kind: Kind::Title(TitleKind::Approval),
        },
    },
    Case {
        name: "title_question",
        help: "the terminal title while a question waits",
        check: "the labelled OSC 2 row for `! fiber · question`, written to the terminal and restored to `fiber` on exit.",
        build: || Look {
            kind: Kind::Title(TitleKind::Question),
        },
    },
    Case {
        name: "title_waiting",
        help: "the terminal title while the session waits",
        check: "the labelled OSC 2 row for `! fiber · waiting`, written to the terminal and restored to `fiber` on exit.",
        build: || Look {
            kind: Kind::Title(TitleKind::Waiting),
        },
    },
    Case {
        name: "title_finished",
        help: "the terminal title after a finished turn",
        check: "the labelled OSC 2 row for `✓ fiber · finished`, written to the terminal and restored to `fiber` on exit.",
        build: || Look {
            kind: Kind::Title(TitleKind::Finished),
        },
    },
];

/// `--image-case`, for `--help` and `check/images.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--image-case",
    file: "images",
    title: "Images and links (#1637)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    ToolImage,
    Fallback,
    Pasted,
    Links,
    ScreenReader,
    Title(TitleKind),
}

/// One attention state, naming the terminal title docs/tui.md lists.
#[derive(Clone, Copy, PartialEq, Debug)]
enum TitleKind {
    Approval,
    Question,
    Waiting,
    Finished,
}

/// The exact title string docs/tui.md, "attention", gives each state.
fn title_of(k: TitleKind) -> &'static str {
    match k {
        TitleKind::Approval => "! fiber · approval",
        TitleKind::Question => "! fiber · question",
        TitleKind::Waiting => "! fiber · waiting",
        TitleKind::Finished => "✓ fiber · finished",
    }
}

/// The title on the way out: a normal title again.
const NORMAL_TITLE: &str = "fiber";

/// Sets the terminal title through OSC 2.
fn title_esc(title: &str) -> String {
    format!("\x1b]2;{title}\x1b\\")
}

#[derive(Debug)]
struct Look {
    kind: Kind,
}

// ============================================================ the kitty placeholders
// The escape fields below (`a=T,U=1`, `f=32`, `s`, `v`, `i`, `c`, `r`,
// `q=2`, `m`, 4096-byte chunks) and the placeholder cells (U+10EEEE with
// the row and column diacritics from the rowcolumn-diacritics table, the
// image id in the foreground colour) come from the kitty graphics protocol
// specification, not from a probe of it. They are provisional until the
// owner's live run in Ghostty, as in logo.rs.

/// This surface's image id, distinct from logo.rs's id 1.
const IMG_ID: u32 = 7;
/// The fixture's pixels: a generated gradient, so no binary is checked in.
const IMG_W: usize = 96;
const IMG_H: usize = 72;
/// One placeholder cell is this base character with its row and column
/// diacritics after it.
const PH: char = '\u{10EEEE}';
/// The row/column diacritics: the first 64 entries of the specification's
/// rowcolumn-diacritics table, in order.
const DIA: [char; 64] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}',
    '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
];

/// The diacritic for row or column `n`.
fn dia(n: usize) -> char {
    DIA[n]
}

/// One placeholder cell for (`row`, `col`): the base character with the
/// row diacritic then the column diacritic.
fn cell(row: usize, col: usize) -> String {
    format!("{PH}{}{}", dia(row), dia(col))
}

/// The `cols`-by-`rows` placeholder grid as one styled cell per position,
/// the image id in each cell's foreground colour.
fn grid(cols: usize, rows: usize, id: u32) -> Vec<Vec<Span<'static>>> {
    (0..rows)
        .map(|r| {
            (0..cols)
                .map(|c| Span::styled(cell(r, c), Style::new().fg(Color::Indexed(id as u8))))
                .collect()
        })
        .collect()
}

/// Fits a desired `cols`-by-`rows` image into the conversation `conv_w`
/// cells wide: at most 40% of its width and 12 rows tall, as docs/tui.md,
/// "Images", describes.
fn clamp(cols: usize, rows: usize, conv_w: usize) -> (usize, usize) {
    (cols.min(conv_w * 40 / 100).min(DIA.len()), rows.min(12))
}

/// The fixture's cell size in a conversation `conv_w` cells wide, keeping
/// its proportions: the fixture is 4:3 and a cell is about twice as tall
/// as it is wide, so 12 rows want 32 columns.
fn fit(conv_w: usize) -> (usize, usize) {
    clamp(12 * 8 / 3, 12, conv_w)
}

/// The fixture's bytes: a straight RGBA gradient, row-major, generated in
/// code so no binary is checked in.
fn rgba() -> Vec<u8> {
    let mut out = Vec::with_capacity(IMG_W * IMG_H * 4);
    for y in 0..IMG_H {
        for x in 0..IMG_W {
            out.extend_from_slice(&[
                (x * 255 / (IMG_W - 1)) as u8,
                (y * 255 / (IMG_H - 1)) as u8,
                128,
                255,
            ]);
        }
    }
    out
}

/// Sends the fixture with its virtual placement: RGBA `f=32`, combined
/// transmit and `U=1` placement of `cols` by `rows`, base64 in chunks of
/// at most 4096 bytes, `m=1` on all but the last chunk.
fn transmit(cols: usize, rows: usize) -> String {
    const CHUNK: usize = 4096;
    let b64 = super::b64(&rgba());
    let mut out = String::new();
    let mut it = b64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(chunk) = it.next() {
        let last = it.peek().is_none();
        let m = if last { 0 } else { 1 };
        if first {
            out.push_str(&format!(
                "\x1b_Ga=T,U=1,f=32,s={IMG_W},v={IMG_H},i={IMG_ID},c={cols},r={rows},q=2,m={m};{}",
                String::from_utf8_lossy(chunk)
            ));
            first = false;
        } else {
            out.push_str(&format!("\x1b_Gm={m};{}", String::from_utf8_lossy(chunk)));
        }
        out.push_str("\x1b\\");
    }
    out
}

/// A link's look: the accent colour, underlined, as the prototype styles
/// its focused rows.
fn link_sp(text: String) -> Span<'static> {
    sp(text, fg(BLUE).add_modifier(Modifier::UNDERLINED))
}

/// An image's one line, clickable: `▣ screenshot.png · 1280×800`.
fn one_line(name: &str, dims: &str) -> super::Row {
    row(vec![
        link_sp(format!("▣ {name}")),
        sp(format!(" · {dims}"), dim()),
    ])
}

/// Whether the image draws inline on this run: the terminal speaks kitty
/// graphics (logo.rs's detection, never under tmux) and no screen reader
/// asked for the flat form.
pub(crate) fn inline(screen_reader: bool) -> bool {
    !screen_reader && super::logo::supported(|k| std::env::var(k).ok())
}

// ============================================================ frames
/// A ledger row for a tool that returned an image: the step gutter, the
/// blue tick, the kind in its colour, the target and the dimensions, as
/// the conversation's ledger draws each call.
fn tool_row() -> super::Row {
    row(vec![
        sp("    1 ", dim()),
        sp("✓", fg(BLUE)),
        sp(" ", Style::new()),
        sp(format!("{:<9}", "read"), fg(CYAN)),
        sp("screenshot.png", Style::new()),
        sp("  ", Style::new()),
        sp("1280×800", dim()),
    ])
}

/// The image under its ledger row: the placeholder grid where the
/// terminal speaks kitty graphics, its one line everywhere else.
fn tool_image_rows(inline: bool, conv_w: usize) -> Vec<super::Row> {
    if !inline {
        return vec![prefixed_one_line(one_line("screenshot.png", "1280×800"))];
    }
    let (cols, rows) = fit(conv_w);
    grid(cols, rows, IMG_ID)
        .into_iter()
        .map(|cells| {
            let mut spans = vec![sp("        ", Style::new())];
            spans.extend(cells);
            row(spans)
        })
        .collect()
}

/// An indented one-line row, sitting where the image would.
fn prefixed_one_line(r: super::Row) -> super::Row {
    let mut spans = vec![sp("        ", Style::new())];
    spans.extend(r.spans);
    super::Row { spans, ..r }
}

/// A pasted image in the prompt bubble: `[Image #1]` with the image
/// through placeholders where the terminal speaks kitty graphics, its
/// one line everywhere else.
fn pasted_rows(inline: bool, conv_w: usize) -> Vec<super::Row> {
    let mut inner = vec![row(vec![sp("[Image #1]", bold())])];
    if inline {
        let (cols, rows) = fit(conv_w);
        inner.extend(grid(cols, rows, IMG_ID).into_iter().map(row));
    } else {
        inner.push(one_line("pasted.png", "1280×800"));
    }
    slab(inner, BU, None, conv_w)
}

/// The focused link: the link look on the selection tint.
fn focused_link(text: &str) -> Span<'static> {
    sp(
        text.to_string(),
        fg(BLUE).add_modifier(Modifier::UNDERLINED).bg(SEL),
    )
}

/// The hovered link: the link look brightened the way `--hover` lifts
/// the cells under the pointer.
fn hovered_link(text: &str) -> Span<'static> {
    sp(
        text.to_string(),
        fg(BLUE)
            .add_modifier(Modifier::UNDERLINED)
            .bg(lift(Color::Reset)),
    )
}

/// A reply with a focused link and a hovered link, then a table with a
/// focused and a hovered link cell, each link in the clickable look.
fn links_rows() -> Vec<super::Row> {
    vec![
        row(vec![
            sp("The new spec lives at ", Style::new()),
            focused_link("https://example.com/spec"),
            sp(",", Style::new()),
        ]),
        row(vec![
            sp("and the fix touches ", Style::new()),
            hovered_link("crates/log/tests/lock.rs"),
            sp(".", Style::new()),
        ]),
        row(vec![sp("Enter opens · y copies", dim())]),
        super::Row::default(),
        row(vec![sp("name  link", bold())]),
        row(vec![sp("────  ──────────────────────────", fg(SEL))]),
        row(vec![
            sp("spec  ", Style::new()),
            focused_link("https://example.com/spec"),
        ]),
        row(vec![
            sp("fix   ", Style::new()),
            hovered_link("crates/log/tests/lock.rs"),
        ]),
    ]
}

/// Collapses whitespace to single spaces, so one flat item reads as one line.
fn one(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One flat line: the default style on every span and no background. The
/// canned `screen_reader` case and the `--screen-reader --static` replay
/// share it, so both frames carry no tint on any cell.
fn flat_line(s: String) -> super::Row {
    row(vec![sp(s, Style::new())])
}

/// The canned flat frame, sharing `flat_line` with the replay.
fn screen_reader_rows() -> Vec<super::Row> {
    vec![
        row(vec![
            sp("Screen reader", Style::new()),
            t(),
            sp("flat", Style::new()),
        ]),
        super::Row::default(),
        flat_line("you 14:03: Sweep the flaky lock tests before review.".into()),
        flat_line("reply: The lock helper now retries three times.".into()),
        flat_line("thought: check the flaky test first".into()),
        flat_line("tool read src/main.rs: 120 lines".into()),
        flat_line("image: ▣ screenshot.png · 1280×800".into()),
        flat_line("tool shell: exit 0, 5 lines".into()),
        flat_line("waiting: approval for shell rm -rf /tmp/x".into()),
        flat_line("running: shell, elapsed 12s".into()),
        flat_line("done: completed, 2 tool calls".into()),
    ]
}

/// A call's result in words, from the ledger's own result text.
fn flat_result(c: &super::Call) -> String {
    super::result_spans(c)
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<String>()
}

/// One flat line for a call: its kind, its target and its result in words.
fn flat_call(c: &super::Call) -> String {
    let kind = super::kind_of(c);
    let tgt = super::target(c);
    let what = if tgt.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} {tgt}")
    };
    format!("tool {what}: {}", one(&flat_result(c)))
}

/// The replayed fixture drawn flat, as docs/tui.md, "Screen readers",
/// describes: one default-style line per item in reading order, state in
/// words, never glyphs. The fixture carries no images; the canned
/// `screen_reader` case covers the image's one line.
pub(crate) fn flat_rows(f: &Fold) -> Vec<super::Row> {
    let mut out = vec![
        flat_line("Screen reader flat".into()),
        super::Row::default(),
    ];
    for t in &f.turns {
        out.push(flat_line(format!(
            "you {}: {}",
            super::clock(t.ts),
            one(&t.prompt)
        )));
        for b in &t.blocks {
            match b {
                super::Block::Text(s) => {
                    out.push(flat_line(format!("reply: {}", one(s))));
                }
                super::Block::Group(g) => {
                    for i in &g.items {
                        match i {
                            super::Item::R(r) => {
                                out.push(flat_line(format!("thought: {}", one(&r.text))))
                            }
                            super::Item::C(c) => out.push(flat_line(flat_call(c))),
                        }
                    }
                }
                super::Block::Steer(s, ts) => out.push(flat_line(format!(
                    "steer {}: {}",
                    super::clock(*ts),
                    one(s)
                ))),
                super::Block::Mcp(m) => {
                    out.push(flat_line(format!("notice: {}", one(m))));
                }
                super::Block::Done(s) => {
                    out.push(flat_line(format!("done: {}", one(s))));
                }
                super::Block::Answer(rows, note, ts, declined) => {
                    let when = super::clock(*ts);
                    if *declined {
                        out.push(flat_line(format!("you answered {when}: declined")));
                    } else {
                        let mut parts: Vec<String> = rows
                            .iter()
                            .map(|(h, a, skipped)| {
                                if *skipped {
                                    format!("{h} = skipped")
                                } else {
                                    format!("{h} = {a}")
                                }
                            })
                            .collect();
                        if let Some(n) = note {
                            parts.push(format!("note = \u{201c}{n}\u{201d}"));
                        }
                        out.push(flat_line(format!(
                            "you answered {when}: {}",
                            parts.join("; ")
                        )));
                    }
                }
                super::Block::Handoff(h) => out.push(flat_line(format!(
                    "handoff {}: {}",
                    one(&h.trigger),
                    h.outcome.as_deref().unwrap_or("running")
                ))),
            }
        }
    }
    out
}

/// Moves a scroll offset by `d` rows over `len` rows in a view `h` high.
fn scroll(top: usize, d: isize, len: usize, h: usize) -> usize {
    let max = len.saturating_sub(h);
    if d < 0 {
        top.saturating_sub((-d) as usize)
    } else {
        (top + d as usize).min(max)
    }
}

/// Paints the flat rows at a scroll offset, as `draw` does.
fn draw_flat(term: &mut Term, rows: &[super::Row], top: usize) -> io::Result<()> {
    let size = term.size()?;
    let (cols, h) = (size.width.max(1), size.height.max(1));
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|f| {
        let buf = f.buffer_mut();
        for y in 0..h {
            let r = rows.get(top + y as usize).cloned().unwrap_or_default();
            paint(buf, 0, y, cols, &r);
        }
    })?;
    let be = term.backend_mut();
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--screen-reader --static` replay: the folded fixture flat,
/// scrolled with arrows or the wheel. Mutually exclusive with the
/// conversation view, like the `--image-case` cases: the fixture replay
/// never draws. Esc, q or Ctrl+C quits.
pub(crate) fn run_flat(a: &Args, term: &mut Term, f: &Fold) -> io::Result<String> {
    let rows = flat_rows(f);
    let mut top = 0usize;
    draw_flat(term, &rows, top)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        let h = term.size()?.height.max(1) as usize;
        if resized {
            top = scroll(top, 0, rows.len(), h);
            draw_flat(term, &rows, top)?;
        }
        for ev in evs {
            match flat_key(top, ev, a.wheel, rows.len(), h) {
                None => return Ok("screen reader\n".into()),
                Some(n) if n != top => {
                    top = n;
                    draw_flat(term, &rows, top)?;
                }
                Some(_) => {}
            }
        }
    }
}

/// One attention state's title: the header with a small labelled row
/// carrying the exact OSC 2 string the run writes.
fn title_rows(k: TitleKind) -> Vec<super::Row> {
    vec![
        view_header("Title", "attention"),
        super::Row::default(),
        row(vec![sp("OSC 2: ", dim()), sp(title_of(k), bold())]),
    ]
}

/// The fallback rows: one image's one line, then an image that cannot
/// show with the notice saying why.
fn fallback_rows() -> Vec<super::Row> {
    vec![
        one_line("screenshot.png", "1280×800"),
        super::Row::default(),
        one_line("avatar.png", "800×600"),
        row(vec![sp(
            "image unavailable: file missing; not asked for again",
            dim(),
        )]),
    ]
}

fn body_rows(c: &Look, cols: usize, inline: bool) -> Vec<super::Row> {
    match c.kind {
        Kind::ToolImage => {
            let mut out = vec![view_header("Images", "tool result"), super::Row::default()];
            out.push(tool_row());
            out.extend(tool_image_rows(inline, cols));
            out
        }
        Kind::Fallback => {
            let mut out = vec![view_header("Images", "one line"), super::Row::default()];
            out.extend(fallback_rows());
            out
        }
        Kind::Pasted => {
            let mut out = vec![view_header("Images", "pasted"), super::Row::default()];
            out.extend(pasted_rows(inline, cols));
            out
        }
        Kind::Links => {
            let mut out = vec![view_header("Links", "reply"), super::Row::default()];
            out.extend(links_rows());
            out
        }
        Kind::ScreenReader => screen_reader_rows(),
        Kind::Title(k) => title_rows(k),
    }
}

/// A row painted at its own offset and width.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

struct Frame {
    screen: Vec<Vec<Placed>>,
}

/// Rows painted at `w`: the terminal width.
fn placed(rows: Vec<super::Row>, w: u16) -> Vec<Placed> {
    rows.into_iter()
        .map(|row| Placed { x: 0, w, row })
        .collect()
}

fn frame(c: &Look, cols: usize, rows: usize, inline: bool) -> Frame {
    frame_with(c, cols, rows, inline, None)
}

/// What Enter opens, `y` copies and a click opens: the case's actionable
/// targets in focus order. The image that cannot show has none: it is
/// never asked for again.
fn targets(c: &Look) -> Vec<&'static str> {
    match c.kind {
        Kind::ToolImage => vec!["screenshot.png"],
        Kind::Fallback => vec!["screenshot.png"],
        Kind::Pasted => vec!["pasted.png"],
        Kind::Links => vec!["https://example.com/spec", "crates/log/tests/lock.rs"],
        Kind::ScreenReader | Kind::Title(_) => vec![],
    }
}

/// The target a body row's text shows, if it shows one.
fn target_at(kind: Kind, text: &str) -> Option<&'static str> {
    match kind {
        Kind::ToolImage if text.contains("screenshot.png") || text.contains(PH) => {
            Some("screenshot.png")
        }
        Kind::Fallback if text.contains("screenshot.png") => Some("screenshot.png"),
        Kind::Pasted
            if text.contains("pasted.png") || text.contains("[Image #1]") || text.contains(PH) =>
        {
            Some("pasted.png")
        }
        Kind::Links => {
            if text.contains("https://example.com/spec") {
                Some("https://example.com/spec")
            } else if text.contains("crates/log/tests/lock.rs") {
                Some("crates/log/tests/lock.rs")
            } else {
                None
            }
        }
        Kind::ScreenReader | Kind::Title(_) => None,
        _ => None,
    }
}

/// The dim status row an open leaves: what would have happened, since the
/// prototype spawns no process.
fn open_status(t: &str) -> String {
    format!("→ would open {t}")
}

/// The dim status row a copy leaves.
fn copy_status(t: &str) -> String {
    format!("→ copied {t}")
}

/// The status an Enter or `y` press leaves on this case: the focused
/// target, if the case shows one.
fn key_status(c: &Look, ev: Ev) -> Option<String> {
    let first = *targets(c).first()?;
    match ev {
        Ev::Key(Key::Enter, _) => Some(open_status(first)),
        Ev::Key(Key::Char('y'), m) if !m.ctrl && !m.alt && !m.sup => Some(copy_status(first)),
        _ => None,
    }
}

/// The status a click on a body row's text leaves, if the row shows a target.
fn click_status(c: &Look, text: &str) -> Option<String> {
    target_at(c.kind, text).map(open_status)
}

/// The status a click at body row `y` leaves, if the row shows a target.
fn click_row(c: &Look, cols: usize, inline: bool, y: u16) -> Option<String> {
    let text: String = body_rows(c, cols, inline)
        .get(y as usize)
        .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect())
        .unwrap_or_default();
    click_status(c, &text)
}

/// The flat view's answer to an event: a new scroll offset, `None` to quit.
/// Any other event keeps the offset.
fn flat_key(top: usize, ev: Ev, wheel: usize, len: usize, h: usize) -> Option<usize> {
    match ev {
        Ev::Key(Key::Char('c'), m) if m.ctrl => None,
        Ev::Key(Key::Esc, _) => None,
        Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => None,
        Ev::Key(Key::Up, _) => Some(scroll(top, -1, len, h)),
        Ev::Key(Key::Down, _) => Some(scroll(top, 1, len, h)),
        Ev::Key(Key::PageUp, _) => Some(scroll(top, -(h as isize), len, h)),
        Ev::Key(Key::PageDown, _) => Some(scroll(top, h as isize, len, h)),
        Ev::Mouse(Mouse::WheelUp, _, _, _) => Some(scroll(top, -(wheel as isize), len, h)),
        Ev::Mouse(Mouse::WheelDown, _, _, _) => Some(scroll(top, wheel as isize, len, h)),
        _ => Some(top),
    }
}

/// The frame with a dim status row under the body, like the rail's
/// "would send" row: what Enter, `y` or a click would have done.
fn frame_with(c: &Look, cols: usize, rows: usize, inline: bool, status: Option<&str>) -> Frame {
    let mut body = placed(body_rows(c, cols, inline), cols as u16);
    if let Some(s) = status {
        body.push(Placed {
            x: 0,
            w: cols as u16,
            row: row(vec![sp(format!("  {s}"), dim())]),
        });
    }
    body.truncate(rows);
    while body.len() < rows {
        body.push(Placed {
            x: 0,
            w: cols as u16,
            row: super::Row::default(),
        });
    }
    Frame {
        screen: body.into_iter().map(|p| vec![p]).collect(),
    }
}

fn draw(term: &mut Term, c: &Look, inline: bool, status: Option<&str>) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let fr = match status {
        Some(s) => frame_with(c, cols as usize, rows as usize, inline, Some(s)),
        None => frame(c, cols as usize, rows as usize, inline),
    };
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|f| {
        let buf = f.buffer_mut();
        for (y, placements) in fr.screen.iter().enumerate() {
            for p in placements {
                paint(buf, p.x, y as u16, p.w, &p.row);
            }
        }
    })?;
    let be = term.backend_mut();
    // The placeholders are text, so a redraw moves the image; only the
    // bytes go out of band, sized to this draw.
    if inline && matches!(c.kind, Kind::ToolImage | Kind::Pasted) {
        let (gw, gh) = fit(cols as usize);
        be.write_all(transmit(gw, gh).as_bytes())?;
    }
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// The `--image-case` case called `name`, or an error listing every case.
fn find(name: &str) -> io::Result<Look> {
    crate::cases::lookup(CASES, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --image-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        )
    })
}

/// Draws the `--image-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_images(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.image_case.clone().unwrap_or_default();
    let c = find(&name)?;
    let show = inline(a.screen_reader);
    let mut status: Option<String> = None;
    draw(term, &c, show, status.as_deref())?;
    // A title case owns the terminal title while it is up, and leaves a
    // normal title behind.
    let title = match c.kind {
        Kind::Title(k) => Some(title_of(k)),
        _ => None,
    };
    if let Some(t) = title {
        let be = term.backend_mut();
        be.write_all(title_esc(t).as_bytes())?;
        be.flush()?;
    }
    let mut rd = super::input::Reader::new()?;
    let done = 'run: loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c, show, status.as_deref())?;
            if let Some(t) = title {
                // A redraw clears nothing, but the title stays owned.
                let be = term.backend_mut();
                be.write_all(title_esc(t).as_bytes())?;
                be.flush()?;
            }
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => break 'run format!("image {name}\n"),
                Ev::Key(Key::Esc, _) => break 'run format!("image {name}\n"),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    break 'run format!("image {name}\n");
                }
                // A focused link opens with Enter and copies with `y`,
                // as docs/tui.md, "Links", says; a click on a row that
                // shows a target opens it. Nothing is spawned: the dim
                // status row says what would have happened.
                Ev::Mouse(Mouse::Down, _, y, _) => {
                    let size = term.size()?;
                    let cols = size.width.max(1) as usize;
                    if let Some(s) = click_row(&c, cols, show, y) {
                        status = Some(s);
                        draw(term, &c, show, status.as_deref())?;
                    }
                }
                ev => {
                    if let Some(s) = key_status(&c, ev) {
                        status = Some(s);
                        draw(term, &c, show, status.as_deref())?;
                    }
                }
            }
        }
    };
    if title.is_some() {
        let be = term.backend_mut();
        be.write_all(title_esc(NORMAL_TITLE).as_bytes())?;
        be.flush()?;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BI;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn parse_case(name: &str) -> Look {
        parse(name).unwrap()
    }

    fn text(c: &Look, cols: usize, rows: usize, inline: bool) -> String {
        frame(c, cols, rows, inline)
            .screen
            .into_iter()
            .map(|ps| {
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

    /// Paints a case into a buffer, as `draw` does.
    fn buffer(c: &Look, cols: usize, rows: usize, inline: bool) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        let fr = frame(c, cols, rows, inline);
        for (y, ps) in fr.screen.iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    /// The buffer's text, one line per row, as `draw` painted it.
    fn buf_text(buf: &Buffer, cols: usize, rows: usize) -> String {
        (0..rows as u16)
            .map(|y| {
                (0..cols as u16)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 9);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn unknown_image_case_errors_listing_every_case() {
        let e = find("nope").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let msg = e.to_string();
        for c in CASES {
            assert!(msg.contains(c.name), "missing {}", c.name);
        }
    }

    #[test]
    fn frame_is_exactly_rows_high() {
        for c in CASES {
            let c = parse_case(c.name);
            for inline in [false, true] {
                let n = body_rows(&c, 160, inline).len();
                for rows in [n - 1, n, n + 1] {
                    assert_eq!(frame(&c, 160, rows, inline).screen.len(), rows);
                }
            }
        }
    }

    #[test]
    fn rowcolumn_diacritics_match_the_spec() {
        // The specification's example: 0 is U+0305, 1 is U+030D, 2 is U+030E.
        assert_eq!(dia(0), '\u{305}');
        assert_eq!(dia(1), '\u{30D}');
        assert_eq!(dia(2), '\u{30E}');
        assert_eq!(DIA.len(), 64);
    }

    #[test]
    fn placeholder_cells_carry_row_then_column() {
        assert_eq!(cell(0, 0), "\u{10EEEE}\u{305}\u{305}");
        assert_eq!(cell(1, 2), "\u{10EEEE}\u{30D}\u{30E}");
        let g = grid(4, 3, IMG_ID);
        assert_eq!(g.len(), 3);
        for (r, line) in g.iter().enumerate() {
            assert_eq!(line.len(), 4);
            for (col, s) in line.iter().enumerate() {
                assert_eq!(s.content, cell(r, col));
                assert_eq!(s.style.fg, Some(Color::Indexed(IMG_ID as u8)));
            }
        }
    }

    #[test]
    fn clamp_holds_40_percent_and_12_rows_at_the_edges() {
        // 40% of 100 columns is 40: 39 and 40 pass, 41 gives way.
        assert_eq!(clamp(39, 12, 100), (39, 12));
        assert_eq!(clamp(40, 12, 100), (40, 12));
        assert_eq!(clamp(41, 12, 100), (40, 12));
        // 11 and 12 rows pass, 13 gives way.
        assert_eq!(clamp(32, 11, 160), (32, 11));
        assert_eq!(clamp(32, 12, 160), (32, 12));
        assert_eq!(clamp(32, 13, 160), (32, 12));
        // The fixture wants 32 by 12: whole at 160, narrower at 79.
        assert_eq!(fit(160), (32, 12));
        assert_eq!(fit(100), (32, 12));
        assert_eq!(fit(79), (31, 12));
    }

    #[test]
    fn rgba_is_a_generated_gradient() {
        let bytes = rgba();
        assert_eq!(bytes.len(), IMG_W * IMG_H * 4);
        assert_eq!(&bytes[0..4], &[0, 0, 128, 255]);
        let at = ((IMG_H - 1) * IMG_W + (IMG_W - 1)) * 4;
        assert_eq!(&bytes[at..at + 4], &[255, 255, 128, 255]);
    }

    #[test]
    fn transmit_carries_placement_and_chunks() {
        let t = transmit(32, 12);
        assert!(
            t.starts_with("\x1b_Ga=T,U=1,f=32,s=96,v=72,i=7,c=32,r=12,q=2,m=1;"),
            "no combined transmit and placement: {t:?}"
        );
        let mut parts: Vec<&str> = t.split("\x1b\\").collect();
        assert_eq!(parts.pop(), Some(""));
        assert!(parts[0].starts_with("\x1b_Ga=T,"), "first: {:?}", parts[0]);
        for c in &parts[1..parts.len() - 1] {
            assert!(c.starts_with("\x1b_Gm=1;"), "middle: {c:?}");
        }
        let last = parts[parts.len() - 1];
        assert!(last.starts_with("\x1b_Gm=0;"), "last: {last:?}");
        let payloads: Vec<&str> = parts
            .iter()
            .map(|c| c.split(';').next_back().unwrap())
            .collect();
        for p in &payloads[..payloads.len() - 1] {
            assert!(p.len() <= 4096, "a chunk carries more than 4096 bytes");
            assert_eq!(p.len() % 4, 0, "a chunk is not base64-aligned");
        }
        let joined: String = payloads.concat();
        assert_eq!(joined, crate::b64(&rgba()));
    }

    #[test]
    fn tool_image_ledger_row_and_image() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("tool_image");
            // Where the terminal speaks kitty graphics the image draws
            // below its ledger row through placeholders.
            let t = text(&c, cols, rows, true);
            assert!(t.contains("screenshot.png"), "no target");
            assert!(t.contains("1280×800"), "no dimensions");
            assert!(t.contains("read"), "no kind");
            assert!(t.contains('✓'), "no tick");
            assert_eq!(t.matches(PH).count(), 32 * 12, "no 32-by-12 grid at {cols}");
            let buf = buffer(&c, cols, rows, true);
            let painted = buf_text(&buf, cols, rows);
            assert_eq!(painted.matches(PH).count(), 32 * 12);
            // The grid's first cell carries row 0, column 0 and the image id.
            let y = painted.lines().position(|l| l.contains(PH)).unwrap() as u16;
            let x = painted.lines().nth(y as usize).unwrap().find(PH).unwrap() as u16;
            assert_eq!(buf[(x, y)].symbol(), cell(0, 0).as_str());
            assert_eq!(buf[(x, y)].fg, Color::Indexed(IMG_ID as u8));
            // Everywhere else the ledger row keeps its one line.
            let t = text(&c, cols, rows, false);
            assert!(t.contains("▣ screenshot.png · 1280×800"), "no one line");
            assert_eq!(t.matches(PH).count(), 0, "placeholders without kitty");
        }
    }

    #[test]
    fn fallback_rows_read_as_links_with_a_notice() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("fallback");
            let t = text(&c, cols, rows, false);
            assert!(
                t.contains("▣ screenshot.png · 1280×800"),
                "no first one line"
            );
            assert!(t.contains("▣ avatar.png · 800×600"), "no second one line");
            assert!(
                t.contains("image unavailable: file missing; not asked for again"),
                "no notice"
            );
            // The one lines read as links: accent, underlined.
            let buf = buffer(&c, cols, rows, false);
            let painted = buf_text(&buf, cols, rows);
            let y = painted
                .lines()
                .position(|l| l.contains("screenshot.png"))
                .unwrap() as u16;
            let cell = &buf[(0, y)];
            assert_eq!(cell.fg, BLUE);
            assert!(
                cell.modifier.contains(Modifier::UNDERLINED),
                "the one line is not underlined"
            );
        }
    }

    #[test]
    fn headers_carry_the_surface_tint() {
        for name in [
            "tool_image",
            "fallback",
            "pasted",
            "links",
            "title_approval",
            "title_question",
            "title_waiting",
            "title_finished",
        ] {
            let c = parse_case(name);
            let buf = buffer(&c, 160, 48, false);
            for x in [0, 79, 159] {
                assert_eq!(buf[(x, 0)].bg, BI, "no surface tint at {x} in {name}");
            }
        }
        // The screen reader draws flat: no tint anywhere, not even a header.
        let c = parse_case("screen_reader");
        let buf = buffer(&c, 160, 48, false);
        for y in 0..48 {
            for x in [0, 79, 159] {
                assert_eq!(buf[(x, y)].bg, Color::Reset, "a tint at ({x}, {y})");
            }
        }
    }

    #[test]
    fn pasted_sits_in_the_prompt_bubble() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("pasted");
            let t = text(&c, cols, rows, true);
            assert!(t.contains("[Image #1]"), "no pasted mark");
            assert_eq!(t.matches(PH).count(), 32 * 12, "no 32-by-12 grid at {cols}");
            // The bubble tints the whole grid width.
            let buf = buffer(&c, cols, rows, true);
            let painted = buf_text(&buf, cols, rows);
            let y = painted.lines().position(|l| l.contains(PH)).unwrap() as u16;
            assert_eq!(buf[(0, y)].bg, BU, "no bubble tint");
            assert_eq!(buf[(0, y)].fg, Color::Indexed(IMG_ID as u8), "no image id");
            let t = text(&c, cols, rows, false);
            assert!(t.contains("[Image #1]"), "no pasted mark");
            assert!(t.contains("▣ pasted.png · 1280×800"), "no fallback line");
            assert_eq!(t.matches(PH).count(), 0, "placeholders without kitty");
        }
    }

    #[test]
    fn links_focus_and_hover() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("links");
            // The test reads the rendered buffer, not the intended case.
            let buf = buffer(&c, cols, rows, false);
            let painted = buf_text(&buf, cols, rows);
            for url in ["https://example.com/spec", "crates/log/tests/lock.rs"] {
                assert!(painted.contains(url), "no {url}");
            }
            assert!(
                painted.contains("Enter opens · y copies"),
                "no focused hint"
            );
            // The focused link sits on the selection tint, underlined;
            // the hovered one wears the hover brighten, underlined.
            let reply_y = painted
                .lines()
                .position(|l| l.contains("lives at"))
                .unwrap() as u16;
            let fx = painted
                .lines()
                .nth(reply_y as usize)
                .unwrap()
                .find("https://")
                .unwrap() as u16;
            assert_eq!(buf[(fx, reply_y)].bg, SEL, "no selection tint");
            assert_eq!(buf[(fx, reply_y)].fg, BLUE, "no link colour");
            assert!(
                buf[(fx, reply_y)].modifier.contains(Modifier::UNDERLINED),
                "the focused link is not underlined"
            );
            let hover_y = painted.lines().position(|l| l.contains("touches")).unwrap() as u16;
            let hx = painted
                .lines()
                .nth(hover_y as usize)
                .unwrap()
                .find("crates/")
                .unwrap() as u16;
            assert_eq!(
                buf[(hx, hover_y)].bg,
                lift(Color::Reset),
                "no hover brighten"
            );
            assert_eq!(buf[(hx, hover_y)].fg, BLUE, "no link colour");
            assert!(
                buf[(hx, hover_y)].modifier.contains(Modifier::UNDERLINED),
                "the hovered link is not underlined"
            );
            // The table carries one focused and one hovered link cell.
            let table_y = painted
                .lines()
                .position(|l| l.contains("spec  https://"))
                .unwrap() as u16;
            let tx = painted
                .lines()
                .nth(table_y as usize)
                .unwrap()
                .find("https://")
                .unwrap() as u16;
            assert_eq!(buf[(tx, table_y)].bg, SEL, "no focused table cell");
            let table_hy = painted
                .lines()
                .position(|l| l.contains("fix   crates/"))
                .unwrap() as u16;
            let thx = painted
                .lines()
                .nth(table_hy as usize)
                .unwrap()
                .find("crates/")
                .unwrap() as u16;
            assert_eq!(
                buf[(thx, table_hy)].bg,
                lift(Color::Reset),
                "no hovered table cell"
            );
        }
    }

    #[test]
    fn screen_reader_is_flat_words_in_order() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("screen_reader");
            let buf = buffer(&c, cols, rows, false);
            // No tints: every cell keeps the default colours.
            for y in 0..rows as u16 {
                for x in 0..cols as u16 {
                    assert_eq!(buf[(x, y)].bg, Color::Reset, "a tint at ({x}, {y})");
                    assert_eq!(buf[(x, y)].fg, Color::Reset, "a tint at ({x}, {y})");
                }
            }
            let painted = buf_text(&buf, cols, rows);
            // No half-block edges, stripes or animation glyphs, and state
            // reads in words, never glyphs.
            for g in ['▄', '▀', '█', '▌', '▐', '✓', '✗', '○', '●', '▸', '▾', '⠋']
            {
                assert!(!painted.contains(g), "a {g} in the flat frame");
            }
            // One line per item, in reading order, the image as its one line.
            let first = painted.lines().find(|l| !l.trim().is_empty()).unwrap();
            assert!(first.contains("Screen reader"), "no flat header");
            for item in [
                "you 14:03:",
                "reply:",
                "thought:",
                "tool read src/main.rs: 120 lines",
                "image: ▣ screenshot.png · 1280×800",
                "tool shell: exit 0, 5 lines",
                "waiting:",
                "running:",
                "done:",
            ] {
                assert!(painted.contains(item), "no {item}");
            }
            let order = |mark: &str| painted.find(mark).unwrap();
            assert!(order("you 14:03:") < order("reply:"));
            assert!(order("reply:") < order("image: ▣"));
            assert!(order("image: ▣") < order("waiting:"));
            assert!(order("waiting:") < order("done:"));
        }
    }

    #[test]
    fn title_strings_are_exact() {
        assert_eq!(title_of(TitleKind::Approval), "! fiber · approval");
        assert_eq!(title_of(TitleKind::Question), "! fiber · question");
        assert_eq!(title_of(TitleKind::Waiting), "! fiber · waiting");
        assert_eq!(title_of(TitleKind::Finished), "✓ fiber · finished");
        assert_eq!(
            title_esc("! fiber · approval"),
            "\x1b]2;! fiber · approval\x1b\\"
        );
        assert_eq!(title_esc(NORMAL_TITLE), "\x1b]2;fiber\x1b\\");
    }

    /// Paints a case with a status row into a buffer, as `draw` does.
    fn buffer_with(
        c: &Look,
        cols: usize,
        rows: usize,
        inline: bool,
        status: Option<&str>,
    ) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        let fr = frame_with(c, cols, rows, inline, status);
        for (y, ps) in fr.screen.iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    /// Paints flat rows into a buffer, as `draw_flat` does.
    fn flat_buffer(rows: &[crate::Row], cols: usize, h: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, h as u16));
        for y in 0..h as u16 {
            let r = rows.get(y as usize).cloned().unwrap_or_default();
            crate::paint(&mut buf, 0, y, cols as u16, &r);
        }
        buf
    }

    #[test]
    fn screen_reader_spans_are_all_default() {
        // The canned case shares `flat_line` with the replay: flipping any
        // span back to a tint or an attribute fails here.
        for r in screen_reader_rows() {
            assert_eq!(r.bg, None, "a tint on {:?}", r.spans);
            for s in &r.spans {
                assert_eq!(s.style, Style::new(), "a style on {:?}", s.content);
            }
        }
    }

    /// A fold with one of every block, so the flat replay test reads every branch.
    fn every_block_fold() -> Fold {
        let ts = (14 * 3600 + 3 * 60) * 1000;
        let mut f = Fold::default();
        f.turns.push(crate::Turn {
            prompt: "Sweep the flaky lock tests".into(),
            ts,
            blocks: vec![
                crate::Block::Text("The lock helper\nnow retries.".into()),
                crate::Block::Group(crate::Group {
                    items: vec![
                        crate::Item::R(crate::Reason {
                            text: "check the flaky test first".into(),
                            start: ts,
                            end: None,
                            step: 1,
                        }),
                        crate::Item::C(crate::Call {
                            name: "read".into(),
                            args: serde_json::json!({"path": "src/main.rs"}),
                            st: crate::St::Completed,
                            start: ts,
                            end: Some(ts),
                            err: None,
                            lines: 120,
                            exit: None,
                            changes: vec![],
                            content: String::new(),
                            step: 1,
                            asked: None,
                        }),
                        crate::Item::C(crate::Call {
                            name: "jobs".into(),
                            args: serde_json::json!({}),
                            st: crate::St::Completed,
                            start: ts,
                            end: Some(ts),
                            err: None,
                            lines: 0,
                            exit: None,
                            changes: vec![],
                            content: "3 jobs".into(),
                            step: 1,
                            asked: None,
                        }),
                        crate::Item::C(crate::Call {
                            name: "shell".into(),
                            args: serde_json::json!({"command": "cargo test"}),
                            st: crate::St::Pending,
                            start: ts,
                            end: None,
                            err: None,
                            lines: 0,
                            exit: None,
                            changes: vec![],
                            content: String::new(),
                            step: 1,
                            asked: None,
                        }),
                    ],
                    steps: 1,
                    step_closed: false,
                    open: false,
                    last_ts: ts,
                }),
                crate::Block::Steer("hold on".into(), ts),
                crate::Block::Mcp("linear down".into()),
                crate::Block::Done("completed · 1 · 2 tool calls".into()),
                crate::Block::Answer(
                    vec![
                        ("scope".into(), "lock".into(), false),
                        ("extra".into(), "skipped".into(), true),
                    ],
                    Some("ok".into()),
                    ts,
                    false,
                ),
                crate::Block::Answer(vec![], None, ts, true),
                crate::Block::Handoff(crate::Handoff {
                    trigger: "compact".into(),
                    before: 0,
                    after: None,
                    ts,
                    outcome: None,
                }),
                crate::Block::Handoff(crate::Handoff {
                    trigger: "compact".into(),
                    before: 0,
                    after: Some(1),
                    ts,
                    outcome: Some("completed".into()),
                }),
            ],
            ..Default::default()
        });
        f
    }

    #[test]
    fn flat_rows_read_flat_in_order() {
        let rows = flat_rows(&every_block_fold());
        // Every span keeps the default style and no background.
        for r in &rows {
            assert_eq!(r.bg, None, "a tint on {:?}", r.spans);
            for s in &r.spans {
                assert_eq!(s.style, Style::new(), "a style on {:?}", s.content);
            }
        }
        // The test reads the rendered buffer, not the intended case.
        let buf = flat_buffer(&rows, 160, 48);
        let painted = buf_text(&buf, 160, 48);
        for y in 0..48u16 {
            for x in [0, 79, 159] {
                assert_eq!(buf[(x, y)].bg, Color::Reset, "a tint at ({x}, {y})");
                assert_eq!(buf[(x, y)].fg, Color::Reset, "a tint at ({x}, {y})");
            }
        }
        for g in ['▄', '▀', '█', '▌', '▐', '✓', '✗', '○', '●', '▸', '▾', '⠋']
        {
            assert!(!painted.contains(g), "a {g} in the flat replay");
        }
        // One line per item, in reading order, state in words.
        let reply = painted.lines().find(|l| l.contains("reply:")).unwrap();
        assert!(
            reply.contains("The lock helper now retries."),
            "the reply wraps instead of reading as one line"
        );
        for item in [
            "Screen reader flat",
            "you 14:03: Sweep the flaky lock tests",
            "reply: The lock helper now retries.",
            "thought: check the flaky test first",
            "tool read src/main.rs: 120 lines",
            "tool shell cargo test: running",
            "tool jobs: 3 jobs",
            "steer 14:03: hold on",
            "notice: linear down",
            "done: completed · 1 · 2 tool calls",
            "you answered 14:03: scope = lock; extra = skipped",
            "you answered 14:03: declined",
            "handoff compact: running",
            "handoff compact: completed",
        ] {
            assert!(painted.contains(item), "no {item}");
        }
        let order = |mark: &str| painted.find(mark).unwrap();
        assert!(order("you 14:03:") < order("reply:"));
        assert!(order("reply:") < order("thought:"));
        assert!(order("thought:") < order("tool read"));
        assert!(order("tool shell") < order("steer 14:03:"));
        assert!(order("done:") < order("you answered"));
        assert!(order("declined") < order("handoff compact: running"));
    }

    #[test]
    fn flat_key_maps_every_event() {
        // Ten rows in a four-high view: Down from 0 goes to 1, a page to 4.
        let key = |ev| flat_key(0, ev, 1, 10, 4);
        assert_eq!(
            key(Ev::Key(
                Key::Char('c'),
                crate::input::Mods {
                    ctrl: true,
                    ..Default::default()
                }
            )),
            None
        );
        assert_eq!(key(Ev::Key(Key::Esc, Default::default())), None);
        assert_eq!(key(Ev::Key(Key::Char('q'), Default::default())), None);
        // `q` with a modifier is not the quit key.
        assert_eq!(
            key(Ev::Key(
                Key::Char('q'),
                crate::input::Mods {
                    ctrl: true,
                    ..Default::default()
                }
            )),
            Some(0)
        );
        assert_eq!(key(Ev::Key(Key::Up, Default::default())), Some(0));
        assert_eq!(key(Ev::Key(Key::Down, Default::default())), Some(1));
        assert_eq!(key(Ev::Key(Key::PageUp, Default::default())), Some(0));
        assert_eq!(key(Ev::Key(Key::PageDown, Default::default())), Some(4));
        assert_eq!(
            key(Ev::Mouse(Mouse::WheelUp, 0, 0, Default::default())),
            Some(0)
        );
        assert_eq!(
            key(Ev::Mouse(Mouse::WheelDown, 0, 0, Default::default())),
            Some(1)
        );
        // Any other event keeps the offset.
        assert_eq!(
            flat_key(3, Ev::Key(Key::Enter, Default::default()), 1, 10, 4),
            Some(3)
        );
        assert_eq!(
            flat_key(
                3,
                Ev::Mouse(Mouse::Move, 0, 0, Default::default()),
                1,
                10,
                4
            ),
            Some(3)
        );
    }

    #[test]
    fn scroll_clamps_to_the_view() {
        // Ten rows in a four-high view show 0 through 6.
        assert_eq!(scroll(0, -1, 10, 4), 0);
        assert_eq!(scroll(0, 3, 10, 4), 3);
        assert_eq!(scroll(3, 10, 10, 4), 6);
        assert_eq!(scroll(6, 1, 10, 4), 6);
        assert_eq!(scroll(6, -6, 10, 4), 0);
        // A short list never scrolls.
        assert_eq!(scroll(0, 1, 3, 4), 0);
        assert_eq!(scroll(0, -1, 3, 4), 0);
    }

    #[test]
    fn enter_opens_and_y_copies_the_focused_link() {
        let c = parse_case("links");
        assert_eq!(
            key_status(&c, Ev::Key(Key::Enter, Default::default())),
            Some("→ would open https://example.com/spec".into())
        );
        assert_eq!(
            key_status(&c, Ev::Key(Key::Char('y'), Default::default())),
            Some("→ copied https://example.com/spec".into())
        );
        // The test reads the rendered buffer: the status row sits dim
        // under the body, and the body above it does not move.
        let status = "→ would open https://example.com/spec";
        let buf = buffer_with(&c, 160, 48, false, Some(status));
        let painted = buf_text(&buf, 160, 48);
        let y = painted.lines().position(|l| l.contains(status)).unwrap() as u16;
        assert_eq!(y, 10, "the status is not under the body");
        let x = painted.lines().nth(y as usize).unwrap().find('→').unwrap() as u16;
        assert!(
            buf[(x, y)].modifier.contains(Modifier::DIM),
            "the status row is not dim"
        );
        assert!(
            painted.lines().next().unwrap().contains("Links"),
            "the body moved"
        );
    }

    #[test]
    fn keys_without_targets_stay_quiet() {
        let links = parse_case("links");
        // `y` with a modifier is not the copy key.
        assert_eq!(
            key_status(
                &links,
                Ev::Key(
                    Key::Char('y'),
                    crate::input::Mods {
                        ctrl: true,
                        ..Default::default()
                    }
                )
            ),
            None
        );
        assert_eq!(
            key_status(&links, Ev::Key(Key::Char('n'), Default::default())),
            None
        );
        // The flat frame and the titles show no target.
        for name in [
            "screen_reader",
            "title_approval",
            "title_question",
            "title_waiting",
            "title_finished",
        ] {
            let c = parse_case(name);
            assert_eq!(targets(&c), Vec::<&str>::new(), "a target in {name}");
            assert_eq!(
                key_status(&c, Ev::Key(Key::Enter, Default::default())),
                None,
                "Enter opens in {name}"
            );
            assert_eq!(
                key_status(&c, Ev::Key(Key::Char('y'), Default::default())),
                None,
                "y copies in {name}"
            );
            assert_eq!(
                click_status(&c, "OSC 2: title"),
                None,
                "a click opens in {name}"
            );
        }
    }

    #[test]
    fn clicks_open_the_row_they_land_on() {
        // The fallback's first one line opens; the missing file's row and
        // the notice never ask again.
        let c = parse_case("fallback");
        let buf = buffer(&c, 160, 48, false);
        let painted = buf_text(&buf, 160, 48);
        let lines: Vec<&str> = painted.lines().collect();
        let shot = lines.iter().find(|l| l.contains("screenshot.png")).unwrap();
        assert_eq!(
            click_status(&c, shot),
            Some("→ would open screenshot.png".into())
        );
        // The row wiring agrees with the text wiring, and rows past the
        // body stay quiet.
        let y = lines
            .iter()
            .position(|l| l.contains("screenshot.png"))
            .unwrap() as u16;
        assert_eq!(
            click_row(&c, 160, false, y),
            Some("→ would open screenshot.png".into())
        );
        assert_eq!(click_row(&c, 160, false, 47), None);
        assert_eq!(click_row(&c, 160, false, 480), None);
        let missing = lines.iter().find(|l| l.contains("avatar.png")).unwrap();
        assert_eq!(click_status(&c, missing), None);
        let notice = lines
            .iter()
            .find(|l| l.contains("image unavailable"))
            .unwrap();
        assert_eq!(click_status(&c, notice), None);
        assert_eq!(click_status(&c, ""), None);
        // The ledger row and the placeholders open the tool's image.
        let c = parse_case("tool_image");
        let buf = buffer(&c, 160, 48, true);
        let painted = buf_text(&buf, 160, 48);
        let grid = painted.lines().find(|l| l.contains(PH)).unwrap();
        assert_eq!(
            click_status(&c, grid),
            Some("→ would open screenshot.png".into())
        );
        let buf = buffer(&c, 160, 48, false);
        let painted = buf_text(&buf, 160, 48);
        let line = painted.lines().find(|l| l.contains("▣")).unwrap();
        assert_eq!(
            click_status(&c, line),
            Some("→ would open screenshot.png".into())
        );
        // The pasted mark and its image open the pasted file.
        let c = parse_case("pasted");
        let buf = buffer(&c, 160, 48, true);
        let painted = buf_text(&buf, 160, 48);
        let mark = painted.lines().find(|l| l.contains("[Image #1]")).unwrap();
        assert_eq!(
            click_status(&c, mark),
            Some("→ would open pasted.png".into())
        );
        // Either table cell opens its own link.
        let c = parse_case("links");
        let buf = buffer(&c, 160, 48, false);
        let painted = buf_text(&buf, 160, 48);
        let spec = painted.lines().find(|l| l.contains("spec  https")).unwrap();
        assert_eq!(
            click_status(&c, spec),
            Some("→ would open https://example.com/spec".into())
        );
        let fix = painted
            .lines()
            .find(|l| l.contains("fix   crates"))
            .unwrap();
        assert_eq!(
            click_status(&c, fix),
            Some("→ would open crates/log/tests/lock.rs".into())
        );
        let hint = painted.lines().find(|l| l.contains("Enter opens")).unwrap();
        assert_eq!(click_status(&c, hint), None);
    }

    #[test]
    fn title_cases_label_their_osc2_row() {
        for (name, title) in [
            ("title_approval", "! fiber · approval"),
            ("title_question", "! fiber · question"),
            ("title_waiting", "! fiber · waiting"),
            ("title_finished", "✓ fiber · finished"),
        ] {
            for (cols, rows) in [(160, 48), (100, 40)] {
                let c = parse_case(name);
                let t = text(&c, cols, rows, false);
                assert!(
                    t.contains(&format!("OSC 2: {title}")),
                    "no labelled row in {name}"
                );
            }
        }
    }
}
