//! Retries, failed turns, crash recovery and rewind, for #1635.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette, the
//! `slab` surface and the swapped-view `view_header`), and the program waits
//! for a key; Esc, q or Ctrl+C quits. The fixture replay never draws under a
//! case. The wording is docs/tui.md at origin/main: "The working line",
//! "Errors and retries", "Handoff", "A dropped connection", "After a crash"
//! and "Rewind"; the code names are docs/errors.md's.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BH, BLUE, CYAN, ORANGE, PURPLE, RED};
use super::{bold, dim, fg, paint, row, slab, sp, t, view_header};
use crate::cases::{Case, Surface};
use ratatui::style::{Modifier, Style};
use std::io::{self, Write};

/// Every `--failure-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "retry",
        help: "a pending retry on the working line, and 1 failed model call on the group line",
        check: "the working line reading ↻ Retrying in 4s · rate_limited · attempt 2 of 4 with its attention mark, and the group line above counting 1 failed model call.",
        build: || Look { kind: Kind::Retry },
    },
    Case {
        name: "failed",
        help: "a failed turn: the ✗ line with its code, the provider's words, then ▣ failed",
        check: "the ✗ line with the message and provider_unavailable in the error colour, the provider's words dim under it, then the dim ▣ failed line with its duration and tool calls.",
        build: || Look { kind: Kind::Failed },
    },
    Case {
        name: "auth_failed",
        help: "a turn failed with authentication_failed, offering log in",
        check: "the ✗ line with the message and authentication_failed, and log in as its own target.",
        build: || Look {
            kind: Kind::AuthFailed,
        },
    },
    Case {
        name: "nudge",
        help: "the handoff nudge: one dim line where it happened",
        check: "the single dim ◔ Context at 268k line sitting between the group lines where it happened.",
        build: || Look { kind: Kind::Nudge },
    },
    Case {
        name: "connection_lost",
        help: "the dropped-connection banner in place of the working line",
        check: "the conversation stub above, the Connection lost · reconnecting (attempt 2)… banner where the working line was, and the draft row kept under it.",
        build: || Look {
            kind: Kind::ConnectionLost,
        },
    },
    Case {
        name: "crashed",
        help: "a crashed turn: ▣ cut short, the may-have-run row, the ↺ band, the orphaned-jobs line",
        check: "the dim ▣ cut short line with Fiber stopped at 14:18, the ledger row reading ? may have run; not run again, the ↺ resumed band on the handoff tint, and the one line naming every orphaned job.",
        build: || Look {
            kind: Kind::Crashed,
        },
    },
    Case {
        name: "rewind",
        help: "the /rewind view with turns and step boundaries",
        check: "the Rewind header, one before: row per turn, the chosen turn opened into its step boundaries, the files and calls since with none of it undone, the job with stop then adopt, and the Rewind row last.",
        build: || Look { kind: Kind::Rewind },
    },
    Case {
        name: "rewound",
        help: "the ⟲ line after a rewind",
        check: "the ⟲ line naming the new session, what was not restored, and which jobs stopped or were adopted.",
        build: || Look {
            kind: Kind::Rewound,
        },
    },
];

/// `--failure-case`, for `--help` and `check/failures.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--failure-case",
    file: "failures",
    title: "Retries, failed turns, crash recovery and rewind (#1635)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Retry,
    Failed,
    AuthFailed,
    Nudge,
    ConnectionLost,
    Crashed,
    Rewind,
    Rewound,
}

#[derive(Debug)]
struct Look {
    kind: Kind,
}

/// The quiet frame above a failure: who asked, what they asked, and the last
/// finished group's summary line.
fn stub_rows(summary: &str) -> Vec<super::Row> {
    vec![
        row(vec![sp("you · 14:03", dim())]),
        row(vec![sp(
            "Cut a release build and run the smoke tests.",
            Style::new(),
        )]),
        super::Row::default(),
        row(vec![sp(summary, dim())]),
    ]
}

/// The working line's pending retry, in its place under the conversation:
/// the attention mark, the countdown, and the code with the attempt dim.
fn retry_row() -> super::Row {
    row(vec![
        sp("  ", Style::new()),
        sp("↻ ", fg(ORANGE)),
        sp("Retrying in 4s", Style::new()),
        sp(" · rate_limited · attempt 2 of 4", dim()),
    ])
}

/// A failed turn's ✗ line: the message and the code in the error colour.
fn error_row(message: &str, code: &str) -> super::Row {
    row(vec![sp(format!("✗ {message} ({code})"), fg(RED))])
}

/// A turn's ▣ end line, as the fold draws every finished card.
fn end_row(outcome: &str) -> super::Row {
    row(vec![sp(outcome, dim())])
}

/// The dropped-connection banner, standing where the working line was.
fn connection_row() -> super::Row {
    row(vec![
        sp("  ", Style::new()),
        sp("⚠ ", fg(ORANGE)),
        sp("Connection lost", Style::new()),
        sp(" · reconnecting (attempt 2)…", dim()),
    ])
}

/// One ledger row for a call that started and never completed, after a
/// crash: its step gutter, the `?` mark, its kind and target, and the
/// may-have-run result dim.
fn crashed_call_row() -> super::Row {
    row(vec![
        sp("    3 ", dim()),
        sp("?", fg(ORANGE)),
        sp(" ", Style::new()),
        sp(format!("{:<9}", "shell"), fg(CYAN)),
        sp("./smoke.sh", Style::new()),
        sp("  ? may have run; not run again", dim()),
    ])
}

/// The ↺ resumed band on the handoff tint, as the handoff band draws its own.
fn resumed_band(cols: usize) -> Vec<super::Row> {
    slab(
        vec![row(vec![
            sp(" ↺ ", fg(PURPLE)),
            sp("resumed", fg(PURPLE).add_modifier(Modifier::BOLD)),
            t(),
            sp("14:19 ", dim()),
        ])],
        BH,
        None,
        cols,
    )
}

/// The `/rewind` view's rows under its header: one `before:` row per turn,
/// the chosen turn opened into every step boundary, what is still pending,
/// and Rewind last.
fn rewind_rows() -> Vec<super::Row> {
    vec![
        row(vec![sp("before: Fix the flaky lock test", dim())]),
        row(vec![
            sp("▸ ", fg(PURPLE)),
            sp("before: Cut a release build", bold()),
        ]),
        row(vec![sp(
            "    after your message · after step 1: read 2 files · after step 2: ran 3 commands",
            dim(),
        )]),
        super::Row::default(),
        row(vec![
            sp("Files Fiber wrote since: ", Style::new()),
            sp("smoke.log", fg(CYAN)),
            sp(" — none of it is undone", dim()),
        ]),
        row(vec![sp(
            "Calls that may have changed files: edit build/config · shell ./smoke",
            dim(),
        )]),
        row(vec![
            sp("Jobs since: ", Style::new()),
            sp("smoke", Style::new()),
            sp(" still runs (", dim()),
            sp("stop", bold()),
            sp(", the default · ", dim()),
            sp("adopt", Style::new()),
            sp(")", dim()),
        ]),
        super::Row::default(),
        row(vec![sp(
            "Summary: the release build is green; the smoke test flaked once on retry.",
            Style::new(),
        )]),
        super::Row::default(),
        row(vec![sp(
            " Rewind ",
            Style::new().add_modifier(Modifier::REVERSED),
        )]),
    ]
}

fn body_rows(c: &Look, cols: usize) -> Vec<super::Row> {
    match c.kind {
        Kind::Retry => {
            let mut out = stub_rows("Read 2 files, ran 3 commands, 1 failed model call · 41s ▸");
            out.push(retry_row());
            out
        }
        Kind::Failed => {
            let mut out = stub_rows("Read 2 files, ran 3 commands · 41s ▸");
            out.push(error_row("The model is overloaded", "provider_unavailable"));
            out.push(row(vec![sp(
                "  anthropic said HTTP 529: “Overloaded”",
                dim(),
            )]));
            out.push(end_row("▣ failed · 3m 12s · 5 tool calls"));
            out
        }
        Kind::AuthFailed => {
            let mut out = stub_rows("Read 2 files · 18s ▸");
            out.push(row(vec![
                sp(
                    "✗ The provider rejected the credential (authentication_failed) · ",
                    fg(RED),
                ),
                sp("log in", fg(BLUE).add_modifier(Modifier::UNDERLINED)),
            ]));
            out.push(end_row("▣ failed · 22s · 2 tool calls"));
            out
        }
        Kind::Nudge => {
            let mut out = stub_rows("Read 2 files · 18s ▸");
            out.push(row(vec![sp(
                "◔ Context at 268k, two thirds of the way to the 400k handoff. The model was told a handoff keeps the work going.",
                dim(),
            )]));
            out.push(row(vec![sp("Ran 3 commands · 41s ▸", dim())]));
            out
        }
        Kind::ConnectionLost => {
            let mut out = stub_rows("Read 2 files, ran 3 commands · 41s ▸");
            out.push(connection_row());
            out.push(row(vec![sp(
                "  › smoke test logs look green, ship it█",
                dim(),
            )]));
            out
        }
        Kind::Crashed => {
            let mut out = stub_rows("Read 2 files, ran 1 command · 41s ▸");
            out.extend([crashed_call_row(), super::Row::default()]);
            out.push(end_row("▣ cut short: Fiber stopped at 14:18"));
            out.extend(resumed_band(cols));
            out.push(row(vec![
                sp("  ", Style::new()),
                sp("⚙ ", fg(ORANGE)),
                sp("2 orphaned jobs: ", Style::new()),
                sp("smoke, lint", fg(CYAN)),
                sp(" · click to read each job's message", dim()),
            ]));
            out
        }
        Kind::Rewind => {
            let mut out = vec![view_header("Rewind", "/rewind"), super::Row::default()];
            out.extend(rewind_rows());
            out
        }
        Kind::Rewound => {
            let mut out = stub_rows("Read 2 files, ran 3 commands · 41s ▸");
            out.push(end_row("▣ failed · 3m 12s · 5 tool calls"));
            out.push(row(vec![
                sp("⟲ ", fg(PURPLE)),
                sp("rewound to after step 1 · now s_b31f", Style::new()),
                sp(" · files not restored · stopped smoke, adopted lint", dim()),
            ]));
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

fn frame(c: &Look, cols: usize, rows: usize) -> Frame {
    let mut body = placed(body_rows(c, cols), cols as u16);
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

/// The `--failure-case` case called `name`, or an error listing every case.
fn find(name: &str) -> io::Result<Look> {
    crate::cases::lookup(CASES, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --failure-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        )
    })
}

/// Draws the `--failure-case` case and waits for a key. Mutually exclusive with
/// the conversation view: the fixture replay never draws.
pub(crate) fn run_failures(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.failure_case.clone().unwrap_or_default();
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("failures {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("failures {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("failures {name}\n"));
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
        assert_eq!(CASES.len(), 8);
        assert!(parse("nope").is_none());
    }

    #[test]
    fn unknown_failure_case_errors_listing_every_case() {
        let e = find("nope").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        let msg = e.to_string();
        for c in CASES {
            assert!(msg.contains(c.name), "missing {}", c.name);
        }
    }

    #[test]
    fn frame_is_exactly_rows_high() {
        for name in [
            "retry",
            "failed",
            "auth_failed",
            "nudge",
            "connection_lost",
            "crashed",
            "rewound",
        ] {
            let c = parse_case(name);
            let n = body_rows(&c, 160).len();
            for rows in [n - 1, n, n + 1] {
                assert_eq!(frame(&c, 160, rows).screen.len(), rows, "{name}");
            }
        }
    }

    /// Each case's headline element, short enough to survive the 100-column cut.
    fn probes(name: &str) -> &'static [&'static str] {
        match name {
            "retry" => &["Retrying in 4s", "1 failed model call"],
            "failed" => &["✗ The model is overloaded", "▣ failed"],
            "auth_failed" => &["authentication_failed", "log in"],
            "nudge" => &["◔ Context at 268k"],
            "connection_lost" => &["Connection lost"],
            "crashed" => &["cut short", "resumed", "orphaned jobs"],
            "rewind" => &["Rewind", "before:"],
            _ => &["⟲ rewound"],
        }
    }

    #[test]
    fn every_case_renders_at_both_sizes() {
        for c in CASES {
            for (cols, rows) in [(160, 48), (100, 40)] {
                let look = parse_case(c.name);
                let buf = buffer(&look, cols, rows);
                let painted = buf_text(&buf, cols, rows);
                for p in probes(c.name) {
                    assert!(painted.contains(p), "no {p} in {} at {cols}", c.name);
                }
            }
        }
    }

    #[test]
    fn retry_marks_the_working_line_and_counts_the_failed_call() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("retry");
            let t = text(&c, cols, rows);
            assert!(
                t.contains("↻ Retrying in 4s · rate_limited · attempt 2 of 4"),
                "no retry line"
            );
            assert!(t.contains("1 failed model call"), "no failed-call count");
            let buf = buffer(&c, cols, rows);
            let y = buf_text(&buf, cols, rows)
                .lines()
                .position(|l| l.contains("Retrying"))
                .unwrap() as u16;
            assert_eq!(buf[(2, y)].symbol(), "↻");
            assert_eq!(buf[(2, y)].fg, ORANGE);
        }
    }

    #[test]
    fn failed_ends_with_the_code_words_and_failed() {
        let c = parse_case("failed");
        let t = text(&c, 160, 48);
        assert!(
            t.contains("✗ The model is overloaded (provider_unavailable)"),
            "no ✗ line"
        );
        assert!(
            t.contains("anthropic said HTTP 529: “Overloaded”"),
            "no provider words"
        );
        assert!(t.contains("▣ failed"), "no ▣ failed");
        let buf = buffer(&c, 160, 48);
        let painted = buf_text(&buf, 160, 48);
        let y = painted.lines().position(|l| l.contains('✗')).unwrap() as u16;
        assert_eq!(buf[(0, y)].fg, RED);
        let words = painted
            .lines()
            .find(|l| l.contains("anthropic said"))
            .unwrap();
        let wx = words.find("anthropic").unwrap() as u16;
        let wy = painted
            .lines()
            .position(|l| l.contains("anthropic said"))
            .unwrap() as u16;
        assert!(
            buf[(wx, wy)].modifier.contains(Modifier::DIM),
            "provider words not dim"
        );
    }

    #[test]
    fn auth_failed_offers_log_in() {
        let c = parse_case("auth_failed");
        let t = text(&c, 160, 48);
        assert!(t.contains("authentication_failed"), "no code");
        assert!(t.contains("log in"), "no log in");
        let buf = buffer(&c, 160, 48);
        let painted = buf_text(&buf, 160, 48);
        let y = painted.lines().position(|l| l.contains("log in")).unwrap() as u16;
        let x = painted
            .lines()
            .find(|l| l.contains("log in"))
            .unwrap()
            .find("log in")
            .unwrap() as u16;
        let cell = &buf[(x, y)];
        assert_eq!(cell.fg, BLUE);
        assert!(cell.modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn nudge_is_one_dim_line_where_it_happened() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("nudge");
            let t = text(&c, cols, rows);
            assert!(t.contains("◔ Context at 268k"), "no nudge");
            let buf = buffer(&c, cols, rows);
            let painted = buf_text(&buf, cols, rows);
            let y = painted.lines().position(|l| l.contains('◔')).unwrap() as u16;
            assert!(
                buf[(0, y)].modifier.contains(Modifier::DIM),
                "nudge not dim"
            );
            // it sits between the finished group and the next one
            let lines: Vec<&str> = painted.lines().collect();
            assert!(lines[y as usize - 1].contains("Read 2 files"));
            assert!(lines[y as usize + 1].contains("Ran 3 commands"));
        }
    }

    #[test]
    fn connection_lost_replaces_the_working_line_and_keeps_the_draft() {
        let c = parse_case("connection_lost");
        let t = text(&c, 160, 48);
        assert!(
            t.contains("Connection lost · reconnecting (attempt 2)…"),
            "no banner"
        );
        assert!(!t.contains("Working"), "the working line is still there");
        assert!(t.contains("ship it"), "no draft");
    }

    #[test]
    fn crashed_closes_marks_resumes_and_names_orphans() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("crashed");
            let t = text(&c, cols, rows);
            assert!(
                t.contains("▣ cut short: Fiber stopped at 14:18"),
                "no cut-short line"
            );
            assert!(
                t.contains("? may have run; not run again"),
                "no may-have-run row"
            );
            assert!(t.contains("resumed"), "no resumed band");
            assert!(t.contains("smoke, lint"), "no orphaned names");
            let buf = buffer(&c, cols, rows);
            let painted = buf_text(&buf, cols, rows);
            let y = painted.lines().position(|l| l.contains("resumed")).unwrap() as u16;
            assert_eq!(buf[(0, y)].bg, BH, "no handoff tint at 0");
            assert_eq!(buf[(cols as u16 / 2, y)].bg, BH, "no handoff tint mid-row");
            let cy = painted
                .lines()
                .position(|l| l.contains("cut short"))
                .unwrap() as u16;
            assert!(
                buf[(0, cy)].modifier.contains(Modifier::DIM),
                "cut short not dim"
            );
        }
    }

    #[test]
    fn rewind_opens_the_chosen_turn_and_ends_in_rewind() {
        for (cols, rows) in [(160, 48), (100, 40)] {
            let c = parse_case("rewind");
            let buf = buffer(&c, cols, rows);
            // the swapped-view header tint runs the whole first row
            for x in [0, cols as u16 / 2, cols as u16 - 1] {
                assert_eq!(buf[(x, 0)].bg, BI, "no header tint at {x}");
            }
            let t = text(&c, cols, rows);
            assert!(t.contains("Rewind"), "no header");
            assert!(
                t.contains("before: Fix the flaky lock test"),
                "no older turn"
            );
            assert!(t.contains("before: Cut a release build"), "no chosen turn");
            assert!(t.contains("after step 1"), "no step boundary");
            assert!(t.contains("none of it is undone"), "no undone note");
            assert!(t.contains("stop"), "no stop");
            assert!(t.contains("adopt"), "no adopt");
            let rewind = t.rfind(" Rewind ").unwrap();
            let chat_after = t[rewind..].lines().skip(1).all(|l| l.trim().is_empty());
            assert!(chat_after, "a row after Rewind");
        }
    }

    #[test]
    fn rewound_names_the_session_and_the_jobs() {
        let c = parse_case("rewound");
        let t = text(&c, 160, 48);
        assert!(
            t.contains("⟲ rewound to after step 1 · now s_b31f"),
            "no ⟲ line"
        );
        assert!(t.contains("files not restored"), "no unrestored note");
        assert!(t.contains("stopped smoke, adopted lint"), "no job fates");
        let buf = buffer(&c, 160, 48);
        let painted = buf_text(&buf, 160, 48);
        let y = painted.lines().position(|l| l.contains('⟲')).unwrap() as u16;
        assert_eq!(buf[(0, y)].fg, PURPLE);
    }
}
