//! Images and links, for #1637.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! swapped-view `view_header` and the kitty detection in `logo.rs`), and the
//! program waits for a key; Esc, q or Ctrl+C quits. The fixture replay never
//! draws under a case. An image a tool returned draws through kitty's Unicode
//! placeholders where the terminal speaks the protocol, and as its one line
//! everywhere else, as docs/tui.md, "Images", describes.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BLUE, CYAN, dim, fg, paint, row, sp, view_header};
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
    let mut body = placed(body_rows(c, cols, inline), cols as u16);
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

fn draw(term: &mut Term, c: &Look, inline: bool) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let fr = frame(c, cols as usize, rows as usize, inline);
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
    if inline && c.kind == Kind::ToolImage {
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
    draw(term, &c, show)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c, show)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("image {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("image {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("image {name}\n"));
                }
                _ => {}
            }
        }
    }
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
        assert_eq!(CASES.len(), 2);
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
        for name in ["tool_image", "fallback"] {
            let c = parse_case(name);
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
        for name in ["tool_image", "fallback"] {
            let c = parse_case(name);
            let buf = buffer(&c, 160, 48, false);
            for x in [0, 79, 159] {
                assert_eq!(buf[(x, 0)].bg, BI, "no surface tint at {x} in {name}");
            }
        }
    }
}
