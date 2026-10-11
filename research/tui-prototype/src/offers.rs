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
use super::{BC, BK, PURPLE, RED};
use super::{bold, dim, fg, paint, row, slab, sp, t, view_header};
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

/// The three choices on one offer item: approve, skip for this session,
/// or never. Every item starts at skip; the chosen answer reads bold with
/// its mark in secondary.
fn offer_choices() -> super::Row {
    row(vec![
        sp("  ", Style::new()),
        sp("( ) ", dim()),
        sp("approve", Style::new()),
        sp("   ", Style::new()),
        sp("(•) ", fg(PURPLE)),
        sp("skip for this session", bold()),
        sp("   ", Style::new()),
        sp("( ) ", dim()),
        sp("never", Style::new()),
    ])
}

/// The changed MCP server's diff, reusing the changed-files hunk look:
/// `@@` dim, the removal red, the addition default, on the hunk surface.
fn offer_diff_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("@@ -2,5 +2,5 @@ servers.linear", dim())]),
        row(vec![sp("     \"command\": \"linear-mcp\",", Style::new())]),
        row(vec![sp(
            "-    \"args\": [\"--stdio\", \"--scope\", \"read\"],",
            fg(RED),
        )]),
        row(vec![sp(
            "+    \"args\": [\"--stdio\", \"--scope\", \"write\"],",
            Style::new(),
        )]),
        row(vec![sp("     \"timeout_ms\": 5000,", Style::new())]),
    ]
}

/// Every pending item with what an install shows: its name, what it runs
/// or declares, and its source path.
fn offer_rows(cols: usize) -> Vec<super::Row> {
    let mut out = vec![
        row(vec![
            sp("▸ ", fg(PURPLE)),
            sp("Extension notes · v0.3", Style::new()),
        ]),
        row(vec![sp(
            "  from .fiber/extensions/notes · loads in this project only",
            dim(),
        )]),
        row(vec![sp(
            "  runs notes-server --serve · install: npm ci",
            dim(),
        )]),
        row(vec![sp("  carries the review skill", dim())]),
        offer_choices(),
        super::Row::default(),
        row(vec![sp(
            "  Hook tidy · before_tool on read, shell",
            Style::new(),
        )]),
        row(vec![sp(
            "  runs scripts/tidy.sh · timeout 10s · on failure refuse",
            dim(),
        )]),
        offer_choices(),
        super::Row::default(),
        row(vec![sp(
            "  MCP server linear · declared in .fiber/mcp.json",
            Style::new(),
        )]),
        row(vec![sp(
            "  runs linear-mcp --stdio · the declaration changed",
            dim(),
        )]),
    ];
    out.extend(slab(offer_diff_rows(), BK, None, cols));
    out.extend([
        offer_choices(),
        super::Row::default(),
        row(vec![sp(
            "A package's TUI files are never installed from a repository.",
            dim(),
        )]),
        super::Row::default(),
        row(vec![sp(
            "↑↓ choose an item · ←→ its answer · Send answers them all · Esc puts it aside",
            dim(),
        )]),
    ]);
    out
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
            out.extend(placed(offer_rows(cols), full));
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
    use crate::BI;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn parse_case(name: &str) -> Look {
        parse(name).unwrap()
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
    fn assert_header(buf: &Buffer, name: &str, cmd: &str, cols: usize) {
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
        let cx = nx + name.len() as u16 + 2;
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
        let ex = cols as u16 - "Esc returns ".len() as u16;
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

    #[test]
    fn offer_header_row() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("offer");
            let buf = buffer(&c, cols, rows);
            assert_header(&buf, "Offer", "repository code", cols);
        }
    }

    #[test]
    fn offer_lists_three_items_starting_at_skip() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("offer");
            let t = text(&c, cols, rows);
            for item in ["Extension notes", "Hook tidy", "MCP server linear"] {
                assert!(t.contains(item), "no {item}");
            }
            // What an install shows per item: the name, what it
            // runs or declares, and its source path.
            for detail in [
                ".fiber/extensions/notes",
                "notes-server --serve",
                "review skill",
                "before_tool",
                "scripts/tidy.sh",
                ".fiber/mcp.json",
                "linear-mcp --stdio",
            ] {
                assert!(t.contains(detail), "no {detail}");
            }
            // Every item starts at skip, and nothing starts approved or never.
            assert_eq!(t.matches("(•) skip for this session").count(), 3);
            assert!(!t.contains("(•) approve"), "an item starts approved");
            assert!(!t.contains("(•) never"), "an item starts at never");
            // The cursor sits on exactly one item, the first.
            assert_eq!(t.matches('▸').count(), 1, "no single cursor");
            let cursor = t.lines().find(|l| l.contains('▸')).unwrap();
            assert!(cursor.contains("Extension notes"), "cursor not on {cursor}");
            // A package's TUI files are never installed from a repository.
            assert!(
                t.contains(
                    "A package's TUI files are never installed from a repository."
                ),
                "no TUI-files sentence"
            );
            assert!(
                t.contains(
                    "↑↓ choose an item · ←→ its answer · Send answers them all · Esc puts it aside"
                ),
                "no footer hint"
            );
            // Painted rows never run wider than the frame: fit() would cut
            // them with …, and every choice label reads whole at 100 cols.
            let buf = buffer(&c, cols, rows);
            let painted = buf_text(&buf, cols, rows);
            assert!(!painted.contains('…'), "truncated at {cols}");
            for label in ["approve", "skip for this session", "never"] {
                assert!(painted.contains(label), "no whole {label} at {cols}");
            }
        }
    }

    #[test]
    fn offer_choices_mark_only_skip() {
        let r = offer_choices();
        let style_of = |want: &str| {
            r.spans
                .iter()
                .find(|s| s.content == want)
                .unwrap_or_else(|| panic!("no {want}"))
                .style
        };
        assert_eq!(style_of("(•) "), fg(PURPLE));
        assert_eq!(style_of("skip for this session"), bold());
        assert!(
            r.spans.iter().filter(|s| s.content == "( ) ").count() == 2,
            "two unchosen marks"
        );
        for s in r.spans.iter().filter(|s| s.content == "( ) ") {
            assert_eq!(s.style, dim());
        }
        assert_eq!(style_of("approve"), Style::new());
        assert_eq!(style_of("never"), Style::new());
    }

    #[test]
    fn offer_diff_marks_removal_red() {
        let rows = offer_diff_rows();
        assert_eq!(rows.len(), 5);
        let plain = |r: &crate::Row| {
            r.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
        };
        let removal = rows.iter().find(|r| plain(r).contains("read")).unwrap();
        assert_eq!(removal.spans[0].style.fg, Some(RED));
        let addition = rows.iter().find(|r| plain(r).contains("write")).unwrap();
        assert_eq!(addition.spans[0].style.fg, None);
        let hunk = rows.iter().find(|r| plain(r).starts_with("@@")).unwrap();
        assert_eq!(hunk.spans[0].style, dim());
        // The hunk slab reads as one surface across the whole row.
        let c = parse_case("offer");
        let buf = buffer(&c, 160, 48);
        let painted = buf_text(&buf, 160, 48);
        let y = painted
            .lines()
            .position(|l| l.contains("\"read\""))
            .unwrap() as u16;
        assert_eq!(buf[(0, y)].symbol(), "-");
        assert_eq!(buf[(0, y)].fg, RED);
        for x in [0, 79, 159] {
            assert_eq!(buf[(x, y)].bg, BK, "no hunk tint at {x}");
        }
    }
}
