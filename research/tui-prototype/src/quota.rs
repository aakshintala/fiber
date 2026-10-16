//! The quota view, for #2526.
//!
//! A throwaway look prototype: each case draws one static frame from the
//! fixtures in this file, reusing the builders in `super` (the palette and
//! the swapped-view `view_header`), and the program waits for a key; Esc, q
//! or Ctrl+C quits. The fixture replay never draws under a case. The `quota`
//! case is the capture: the cached rows with the fetch still in flight, so
//! the top row carries the dim `fetching` mark.

use super::input::{Ev, Key};
use super::{Args, Term};
use super::{BI, bold, dim, fg, paint, row, sp, t};
use crate::RED;
use crate::cases::{Case, Surface};
use ratatui::style::Style;
use std::io::{self, Write};

/// Every `--quota-case` case, named in README.md.
const CASES: &[Case<Look>] = &[
    Case {
        name: "quota",
        help: "the quota view with the fetch in flight over cached rows",
        check: "the `Quota` header with `  /quota` dim, `fetching` dim and `Esc returns` dim right-aligned; `codex · default` with two windows, each a dim name, its percent and its bar, then its reset time dim (`resets in 3h12m`, `resets in 2d4h`); `claude · default` with its last value and `unreachable` in the error colour; nothing clipped.",
        build: || Look {
            kind: Kind::Full,
            fetching: true,
        },
    },
    Case {
        name: "quota-empty",
        help: "the quota view with no entries",
        check: "the same header without `fetching`, and one dim line `No provider reports quota` in place of the rows.",
        build: || Look {
            kind: Kind::Empty,
            fetching: false,
        },
    },
];

/// `--quota-case`, for `--help` and `check/quota.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--quota-case",
    file: "quota",
    title: "Quota view (#2526)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Full,
    Empty,
}

struct Look {
    kind: Kind,
    /// Whether the fetch that fills the view is still in flight: the top
    /// row then carries the dim `fetching` mark over the cached rows.
    fetching: bool,
}

/// A cached quota window: the vendor's name, how much is used, and the
/// reset time in minutes, as the `quota` answer's `resets_at` would give.
struct Window {
    name: &'static str,
    percent: u8,
    resets_in_min: u64,
}

/// The cached rows the capture draws: one provider with two windows and
/// reset times, and one unreachable provider with its last value.
const WINDOWS: &[Window] = &[
    Window {
        name: "5h",
        percent: 62,
        resets_in_min: 192,
    },
    Window {
        name: "weekly",
        percent: 31,
        resets_in_min: 3120,
    },
];
const UNREACHABLE_LAST: u8 = 65;

/// A reset time as the view draws it: `resets in` then the duration,
/// minutes rounded down, `3h12m`, and days above a day, `2d4h`.
fn resets_in(mins: u64) -> String {
    if mins >= 1440 {
        format!("{}d{}h", mins / 1440, mins % 1440 / 60)
    } else if mins >= 60 {
        format!("{}h{}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    }
}

/// The swapped-view top row: `Quota` bold, `  /quota` dim, `fetching` dim
/// while the fetch is in flight, and `Esc returns` dim right-aligned.
fn quota_header(fetching: bool) -> super::Row {
    let mut spans = vec![
        sp(" ", Style::new()),
        sp("Quota", bold()),
        sp("  /quota", dim()),
    ];
    if fetching {
        spans.push(sp("  fetching", dim()));
    }
    spans.push(t());
    spans.push(sp("Esc returns ", dim()));
    super::Row {
        spans,
        bg: Some(BI),
        act: Some(super::Act::Back),
        ..Default::default()
    }
}

/// The quota bars cap at this width, so a wide screen keeps them readable:
/// a full-width bar would drown the figures.
const BAR_W: usize = 40;

/// One cached window: the dim name, its percent, and its bar in the rail
/// cards' colour, then the reset time dim on the row after it.
fn window_rows(w: &Window, cols: usize) -> Vec<super::Row> {
    let mut out = vec![row(vec![
        sp(w.name, dim()),
        t(),
        sp(format!("{}%", w.percent), Style::new()),
    ])];
    out.push(row(super::bar(
        f64::from(w.percent) / 100.0,
        cols.min(BAR_W),
        super::rail_ctx_color(w.percent),
    )));
    out.push(row(vec![sp(
        format!("resets in {}", resets_in(w.resets_in_min)),
        dim(),
    )]));
    out
}

/// An unreachable entry: its last value and `unreachable` in the error
/// colour, or `unreachable` alone when it has no last value.
fn unreachable_row(last: Option<u8>) -> super::Row {
    match last {
        Some(pct) => row(vec![
            sp(format!("{pct}%"), Style::new()),
            sp(" · ", dim()),
            sp("unreachable", fg(RED)),
        ]),
        None => row(vec![sp("unreachable", fg(RED))]),
    }
}

/// One cached entry's header: the provider bold, its credential label dim.
fn entry_header(provider: &str, credential: &str) -> super::Row {
    row(vec![
        sp(provider, bold()),
        sp(format!(" · {credential}"), dim()),
    ])
}

fn body_rows(c: &Look, cols: usize) -> Vec<super::Row> {
    let mut out = vec![quota_header(c.fetching), super::Row::default()];
    match c.kind {
        Kind::Empty => {
            out.push(row(vec![sp("No provider reports quota", dim())]));
        }
        Kind::Full => {
            out.push(entry_header("codex", "default"));
            for w in WINDOWS {
                out.extend(window_rows(w, cols));
            }
            out.push(super::Row::default());
            out.push(entry_header("claude", "default"));
            out.push(unreachable_row(Some(UNREACHABLE_LAST)));
        }
    }
    out
}

/// A row painted at its own offset and width.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

fn frame(c: &Look, cols: usize, rows: usize) -> Vec<Vec<Placed>> {
    let mut body: Vec<Placed> = body_rows(c, cols)
        .into_iter()
        .map(|row| Placed {
            x: 0,
            w: cols as u16,
            row,
        })
        .collect();
    body.truncate(rows);
    while body.len() < rows {
        body.push(Placed {
            x: 0,
            w: cols as u16,
            row: super::Row::default(),
        });
    }
    body.into_iter().map(|p| vec![p]).collect()
}

fn draw(term: &mut Term, c: &Look) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|fr| {
        let buf = fr.buffer_mut();
        for (y, placements) in frame(c, cols as usize, rows as usize).iter().enumerate() {
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

/// Draws the `--quota-case` case and waits for a key. Mutually exclusive
/// with the conversation view: the fixture replay never draws.
pub(crate) fn run_quota(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.quota_case.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --quota-case case {name:?}; one of: {}",
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => return Ok(format!("quota {name}\n")),
                Ev::Key(Key::Esc, _) => return Ok(format!("quota {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("quota {name}\n"));
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

    fn parse(name: &str) -> Option<Look> {
        crate::cases::lookup(CASES, name)
    }

    fn text(c: &Look, cols: usize, rows: usize) -> String {
        frame(c, cols, rows)
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
        for (y, ps) in frame(c, cols, rows).iter().enumerate() {
            for p in ps {
                crate::paint(&mut buf, p.x, y as u16, p.w, &p.row);
            }
        }
        buf
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
    fn the_reset_time_rounds_minutes_down() {
        assert_eq!(resets_in(59), "59m");
        assert_eq!(resets_in(60), "1h0m");
        assert_eq!(resets_in(61), "1h1m");
        assert_eq!(resets_in(192), "3h12m");
        assert_eq!(resets_in(1439), "23h59m");
        assert_eq!(resets_in(1440), "1d0h");
        assert_eq!(resets_in(1441), "1d0h");
        assert_eq!(resets_in(3120), "2d4h");
    }

    #[test]
    fn the_header_names_the_view_and_its_command() {
        for cols in [160, 100] {
            let buf = buffer(&parse("quota").unwrap(), cols, 48);
            assert_eq!(buf[(1, 0)].symbol(), "Q");
            assert!(
                buf[(1, 0)].modifier.contains(Modifier::BOLD),
                "the name is not bold"
            );
            let line: String = (0..cols as u16)
                .map(|x| buf[(x, 0)].symbol().to_string())
                .collect();
            assert!(line.contains("Quota"), "no name");
            assert!(line.contains("  /quota"), "no command");
            assert!(line.contains("fetching"), "no fetching mark");
            assert!(line.ends_with("Esc returns "), "no Esc hint: {line:?}");
            // The tint runs the whole row, like every swapped view.
            for x in [0, cols as u16 / 2, cols as u16 - 1] {
                assert_eq!(buf[(x, 0)].bg, BI, "no surface tint at {x}");
            }
        }
    }

    #[test]
    fn the_fetching_mark_is_dim() {
        use ratatui::style::Color;
        let buf = buffer(&parse("quota").unwrap(), 160, 48);
        let line: String = (0..160u16)
            .map(|x| buf[(x, 0)].symbol().to_string())
            .collect();
        let at = line.find("fetching").unwrap();
        for i in 0.."fetching".len() {
            let cell = &buf[(at as u16 + i as u16, 0)];
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "fetching is not dim at {}",
                at + i
            );
            assert_ne!(cell.bg, Color::Reset, "fetching left the tint");
        }
    }

    #[test]
    fn the_capture_shows_two_windows_with_reset_times() {
        for cols in [160, 100] {
            let t = text(&parse("quota").unwrap(), cols, 48);
            assert!(t.contains("codex"), "no provider");
            assert!(t.contains("default"), "no credential label");
            assert!(t.contains("5h"), "no first window");
            assert!(t.contains("62%"), "no first figure");
            assert!(t.contains("resets in 3h12m"), "no first reset time");
            assert!(t.contains("weekly"), "no second window");
            assert!(t.contains("31%"), "no second figure");
            assert!(t.contains("resets in 2d4h"), "no second reset time");
            // Each reset time reads on the row after its window's bar.
            let lines: Vec<&str> = t.split('\n').collect();
            let bar = lines.iter().position(|l| l.contains("62%")).unwrap();
            assert!(
                lines[bar + 2].contains("resets in 3h12m"),
                "the reset time is not after its window"
            );
        }
    }

    #[test]
    fn the_capture_shows_the_unreachable_last_value() {
        for cols in [160, 100] {
            let t = text(&parse("quota").unwrap(), cols, 48);
            assert!(t.contains("claude"), "no unreachable provider");
            assert!(t.contains("65%"), "no last value");
            assert!(t.contains("unreachable"), "no unreachable mark");
        }
        // The mark reads in the error colour.
        let buf = buffer(&parse("quota").unwrap(), 160, 48);
        let t = text(&parse("quota").unwrap(), 160, 48);
        let y = t
            .split('\n')
            .position(|l| l.contains("unreachable"))
            .unwrap() as u16;
        let row: String = (0..160u16)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let at = row.find("unreachable").unwrap();
        assert_eq!(buf[(at as u16, y)].fg, RED);
    }

    #[test]
    fn unreachable_without_a_last_value_shows_the_word_alone() {
        let r = unreachable_row(None);
        let plain: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "unreachable");
        assert!(
            r.spans.iter().all(|s| s.style.fg == Some(RED)),
            "the lone mark left the error colour"
        );
    }

    #[test]
    fn unreachable_with_a_last_value_keeps_both() {
        let r = unreachable_row(Some(65));
        let plain: String = r.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "65% · unreachable");
        assert_eq!(r.spans[0].style.fg, None);
        assert_eq!(r.spans[2].style.fg, Some(RED));
    }

    #[test]
    fn empty_names_what_is_missing() {
        for cols in [160, 100] {
            let t = text(&parse("quota-empty").unwrap(), cols, 48);
            assert!(t.contains("Quota"), "no header");
            assert!(t.contains("No provider reports quota"), "no empty line");
            assert!(!t.contains("fetching"), "a settled view still fetches");
        }
        // The empty line is dim.
        let buf = buffer(&parse("quota-empty").unwrap(), 160, 48);
        let t = text(&parse("quota-empty").unwrap(), 160, 48);
        let y = t
            .split('\n')
            .position(|l| l.contains("No provider reports quota"))
            .unwrap() as u16;
        let line: String = (0..160u16)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let at = line.find("No provider").unwrap();
        assert!(
            buf[(at as u16, y)].modifier.contains(Modifier::DIM),
            "the empty line is not dim"
        );
    }

    #[test]
    fn nothing_is_cut() {
        for (name, words) in [
            (
                "quota",
                vec![
                    "Quota", "/quota", "fetching", "Esc", "codex", "weekly", "resets",
                ],
            ),
            ("quota-empty", vec!["Quota", "/quota", "Esc", "No provider"]),
        ] {
            for cols in [160, 100] {
                let t = text(&parse(name).unwrap(), cols, 48);
                assert!(!t.contains("…"), "{name} cuts a row at {cols}");
            }
            // Every fixture word reads whole.
            let t = text(&parse(name).unwrap(), 160, 48);
            for w in words {
                assert!(t.contains(w), "{name} drops {w}");
            }
        }
    }

    #[test]
    fn the_bar_caps_at_forty_cells() {
        for cols in [160, 100] {
            let t = text(&parse("quota").unwrap(), cols, 48);
            let bars: Vec<&str> = t.split('\n').filter(|l| l.contains('▆')).collect();
            assert_eq!(bars.len(), 2, "one bar per window");
            for b in bars {
                assert_eq!(b.chars().count(), BAR_W, "the bar is not capped");
            }
        }
    }

    #[test]
    fn the_frame_fills_the_viewport() {
        assert_eq!(frame(&parse("quota").unwrap(), 160, 48).len(), 48);
        assert_eq!(frame(&parse("quota").unwrap(), 100, 40).len(), 40);
    }
}
