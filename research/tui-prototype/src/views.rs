//! The swapped views, for #1633.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! `slab` surface, its roles and the swapped-view `view_header`), and the
//! program waits for a key; Esc, q or Ctrl+C quits. The fixture replay never
//! draws under a case. The ticket's cases are the minimum; `running` is drawn
//! only as docs/tui.md describes it.
//!
//! Where docs/tui.md names no command for a view (a delegate's or job's view
//! opens from its card row, not a slash command), the header's dim slot
//! carries what opened it, so the top row keeps its shape: name bold,
//! dim context, "Esc returns" dim right-aligned.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BC, BI, BW, RED};
use super::{bold, dim, fg, paint, row, slab, sp, view_header};
use crate::cases::{Case, Surface};
use ratatui::style::Style;
use std::io::{self, Write};

/// Every `--view-case` case, named in README.md.
const CASES: &[Case<Look>] = &[Case {
    name: "delegate-view",
    help: "a delegate's breadcrumb, status card, transcript and input box",
    check: "the breadcrumb `main › ◆ review: …`, a status card naming harness, model, calls, elapsed, session and the stop target, the transcript in turn cards, and the input box sending steering to the delegate.",
    build: || Look {
        kind: Kind::Delegate,
    },
}];

/// `--view-case`, for `--help` and `check/views.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--view-case",
    file: "views",
    title: "Swapped views (#1633)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Delegate,
}

struct Look {
    kind: Kind,
}

/// A fixed tick is unnecessary: no swapped-view still animates.
/// Narrow width the README documents beside the 160x48 captures.
#[cfg(test)]
const NARROW: usize = 100;
#[cfg(test)]
const NARROW_ROWS: usize = 40;

/// A row painted at its own offset and width.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

struct Frame {
    screen: Vec<Vec<Placed>>,
}

/// The delegate's status card: harness, model, calls, elapsed and session,
/// with the stop target that ends it.
fn delegate_status(w: usize) -> Vec<super::Row> {
    slab(
        vec![
            row(vec![
                sp("harness fiber", Style::new()),
                sp(" · model opus-5-5 · high", dim()),
            ]),
            row(vec![sp("12 calls · elapsed 4m · session s_d417a0", dim())]),
            row(vec![sp("■ stop s_d417a0", fg(RED))]),
        ],
        BC,
        None,
        w,
    )
}

/// The delegate's transcript in the conversation's turn cards.
fn delegate_transcript(w: usize) -> Vec<super::Row> {
    slab(
        vec![
            row(vec![sp("◆ review · 14:02", dim())]),
            row(vec![sp(
                "The sweep is done: 3 call sites, all covered.",
                Style::new(),
            )]),
            row(vec![sp("you · 14:03", dim())]),
            row(vec![sp("Ship it.", Style::new())]),
        ],
        BW,
        None,
        w,
    )
}

/// The input box, sending steering messages to the delegate.
fn delegate_input(w: usize) -> Vec<super::Row> {
    slab(
        vec![row(vec![
            sp("› █", Style::new()),
            sp(" steering goes to ◆ review", dim()),
        ])],
        BI,
        None,
        w,
    )
}

fn body_rows(c: &Look, cols: usize) -> Vec<super::Row> {
    match c.kind {
        Kind::Delegate => {
            let mut out = vec![view_header("Delegate", "◆ review")];
            out.push(super::Row::default());
            out.push(row(vec![sp(
                "main › ◆ review: the wait_for_path sweep",
                bold(),
            )]));
            out.push(super::Row::default());
            out.extend(delegate_status(cols));
            out.push(super::Row::default());
            out.extend(delegate_transcript(cols));
            out.push(super::Row::default());
            out.extend(delegate_input(cols));
            out
        }
    }
}

fn frame(c: &Look, cols: usize, rows: usize) -> Frame {
    let mut body = body_rows(c, cols);
    body.truncate(rows);
    while body.len() < rows {
        body.push(super::Row::default());
    }
    Frame {
        screen: body
            .into_iter()
            .map(|r| {
                vec![Placed {
                    x: 0,
                    w: cols as u16,
                    row: r,
                }]
            })
            .collect(),
    }
}

fn draw(term: &mut Term, c: &Look) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    let fr = frame(c, cols as usize, rows as usize);
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
    be.write_all(b"\x1b[?2026l")?;
    be.flush()?;
    Ok(())
}

/// Draws the `--view-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_view(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.view_case.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --view-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("view {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("view {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("view {name}\n"));
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
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
        frame(c, cols, rows)
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
    fn buffer(c: &Look, cols: usize, rows: usize) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
        let fr = frame(c, cols, rows);
        for (y, ps) in fr.screen.iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
    }

    /// The swapped-view top row: the surface tint, the name bold, the command
    /// dim, and "Esc returns" dim right-aligned.
    fn assert_header(buf: &Buffer, case: &str, name: &str, cmd: &str, cols: usize) {
        let spans = &frame(&parse_case(case), cols, 48).screen[0][0].row.spans;
        let _ = spans;
        // The surface tint runs the whole row.
        for x in [0, cols as u16 / 2, cols as u16 - 1] {
            assert_eq!(buf[(x, 0)].bg, BI, "no surface tint at {x}");
        }
        // The name is bold from column 1; the command follows it, dim.
        let nx = 1u16;
        for (i, ch) in name.chars().enumerate() {
            let cell = &buf[(nx + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::BOLD),
                "the name is not bold at {}",
                nx as usize + i
            );
        }
        let cx = nx + name.width() as u16 + 2;
        for (i, ch) in cmd.chars().enumerate() {
            let cell = &buf[(cx + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "the command is not dim at {}",
                cx as usize + i
            );
        }
        // "Esc returns" ends the row, dim.
        let ex = cols as u16 - "Esc returns ".width() as u16;
        assert_eq!(buf[(ex, 0)].symbol(), "E");
        for (i, ch) in "Esc returns ".chars().enumerate() {
            let cell = &buf[(ex + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string().as_str());
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "Esc is not dim at {}",
                ex as usize + i
            );
        }
    }

    fn parse_case(name: &str) -> Look {
        parse(name).unwrap()
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 1);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn delegate_view_header_row() {
        for cols in [160, NARROW] {
            let c = parse_case("delegate-view");
            let buf = buffer(&c, cols, 48);
            assert_header(&buf, "delegate-view", "Delegate", "◆ review", cols);
        }
    }

    #[test]
    fn delegate_view_shows_breadcrumb_status_transcript_and_input() {
        for cols in [160, NARROW] {
            let c = parse_case("delegate-view");
            let t = text(&c, cols, 48);
            assert!(t.contains("main › ◆ review:"), "no breadcrumb");
            assert!(t.contains("harness fiber"), "no harness");
            assert!(t.contains("opus-5-5"), "no model");
            assert!(t.contains("12 calls"), "no calls");
            assert!(t.contains("4m"), "no elapsed");
            assert!(t.contains("s_d417a0"), "no session");
            assert!(t.contains("■ stop"), "no stop target");
            assert!(t.contains("The sweep is done"), "no transcript");
            assert!(t.contains("steering goes to ◆ review"), "no input box");
        }
    }

    #[test]
    fn no_row_overflows_its_screen() {
        use crate::width;
        for name in CASES.iter().map(|c| c.name) {
            for (cols, rows) in [(160, 48), (NARROW, NARROW_ROWS)] {
                let c = crate::cases::lookup(CASES, name).unwrap();
                let fr = frame(&c, cols, rows);
                assert_eq!(fr.screen.len(), rows, "{name} is not {rows} rows");
                for (y, ps) in fr.screen.iter().enumerate() {
                    for p in ps {
                        assert!(
                            width(&p.row.spans) <= p.w as usize,
                            "{name} row {y} overflows its {w} columns",
                            w = p.w
                        );
                        assert!(
                            p.x + p.w <= cols as u16,
                            "{name} row {y} runs past the edge"
                        );
                    }
                }
            }
        }
    }

}
