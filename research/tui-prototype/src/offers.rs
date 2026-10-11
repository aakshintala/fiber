//! The repository's offer and the one-question forms, for #1636.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! `slab` surface and the swapped-view `view_header`), and the program waits
//! for a key; Esc, q or Ctrl+C quits. The fixture replay never draws under a
//! case. The offer lists every pending item as docs/tui.md, "Approving what
//! a repository ships", describes it; the four forms are the one-question
//! panels docs/tui.md, "A question form", describes, with no tab row and
//! ending in "Chat about this".

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BC, PURPLE};
use super::{dim, fg, paint, row, slab, sp, t, view_header};
use crate::cases::{Case, Surface};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use std::io::{self, Write};

/// Every `--offer-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "offer",
        help: "the repository's offer: an extension, a hook and a changed MCP server with three choices on each",
        check: "the extension, the hook and the MCP server, each with what an install shows and three choices starting at skip, the changed server's diff with its red removal, the TUI-files sentence, and the choose/send/put-aside hint.",
        build: || Look { kind: Kind::Offer },
    },
    Case {
        name: "confirm",
        help: "a one-question form with yes and no",
        check: "the approval slab with its stripe, the title with Esc right-aligned and no tab row, the question, yes and no, and Chat about this last with no Submit row.",
        build: || Look { kind: Kind::Confirm },
    },
    Case {
        name: "select",
        help: "a one-question form with labelled options and descriptions",
        check: "the options with their descriptions and ( )/(•) marks under the cursor, and Chat about this last with no Submit row.",
        build: || Look { kind: Kind::Select },
    },
    Case {
        name: "multi_select",
        help: "a one-question form with toggles and a Submit row",
        check: "choose any, the [ ]/[x] marks, the Submit row, then Chat about this.",
        build: || Look {
            kind: Kind::MultiSelect,
        },
    },
    Case {
        name: "text_input",
        help: "a one-question form answering in words with a Submit row",
        check: "only the answer-in-words row with its cursor, the Submit row, then Chat about this.",
        build: || Look {
            kind: Kind::TextInput,
        },
    },
];

/// `--offer-case`, for `--help` and `check/offers.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--offer-case",
    file: "offers",
    title: "Repository offer and one-question forms (#1636)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Offer,
    Confirm,
    Select,
    MultiSelect,
    TextInput,
}

#[derive(Debug)]
struct Look {
    kind: Kind,
}

/// One panel row: the ▌ stripe and a space before the content, as the
/// question form draws every row.
fn panel_row(spans: Vec<Span<'static>>) -> super::Row {
    let mut v = vec![sp("▌", fg(PURPLE)), sp(" ", Style::new())];
    v.extend(spans);
    row(v)
}

/// A blank panel row keeps its stripe.
fn panel_blank() -> super::Row {
    panel_row(vec![])
}

/// The one-question title: "Question from <session>" with Esc right-aligned,
/// and no tab row under it.
fn title_row() -> super::Row {
    panel_row(vec![
        sp("Question", fg(PURPLE).add_modifier(Modifier::BOLD)),
        sp(" from main", fg(PURPLE)),
        t(),
        sp("Esc: chat about this", dim()),
    ])
}

/// The quiet frame above a form panel: a short transcript stub.
fn stub_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("you · 14:03", dim())]),
        row(vec![sp(
            "Sweep the flaky lock tests before review.",
            Style::new(),
        )]),
        super::Row::default(),
    ]
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

fn body_rows(c: &Look, cols: usize, rows: usize) -> Vec<Placed> {
    let full = cols as u16;
    let blank = || Placed {
        x: 0,
        w: full,
        row: super::Row::default(),
    };
    match c.kind {
        Kind::Offer => {
            let mut out = placed(vec![view_header("Offer", "repository code")], full);
            out.push(blank());
            out
        }
        Kind::Confirm | Kind::Select | Kind::MultiSelect | Kind::TextInput => {
            // The panel sits at the bottom of an otherwise quiet frame.
            let panel = slab(vec![title_row(), panel_blank()], BC, None, cols);
            let stub = stub_rows();
            let fill = rows.saturating_sub(stub.len() + panel.len());
            let mut out = placed(stub, full);
            out.extend(std::iter::repeat_with(blank).take(fill));
            out.extend(placed(panel, full));
            out
        }
    }
}

fn frame(c: &Look, cols: usize, rows: usize) -> Frame {
    let mut body = body_rows(c, cols, rows);
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

/// The `--offer-case` case called `name`, or an error listing every case.
fn find(name: &str) -> io::Result<Look> {
    crate::cases::lookup(CASES, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --offer-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        )
    })
}

/// Draws the `--offer-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_offer(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.offer_case.clone().unwrap_or_default();
    let c = find(&name)?;
    draw(term, &c)?;
    let mut rd = super::input::Reader::new()?;
    loop {
        let (evs, resized) = rd.wait(None)?;
        if resized {
            draw(term, &c)?;
        }
        for ev in evs {
            match ev {
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("offer {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("offer {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("offer {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 5);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn unknown_offer_case_errors_listing_every_case() {
        let e = find("nope").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let msg = e.to_string();
        for c in CASES {
            assert!(msg.contains(c.name), "missing {}", c.name);
        }
    }

    #[test]
    fn frame_is_exactly_rows_high() {
        let c = parse("offer").unwrap();
        let n = body_rows(&c, 160, 48).len();
        for rows in [n - 1, n, n + 1] {
            assert_eq!(frame(&c, 160, rows).screen.len(), rows);
        }
    }
}
