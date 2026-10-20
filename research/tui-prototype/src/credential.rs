//! Static credential picker looks for #724. No session or clock is involved.
use super::input::{Ev, Key};
use super::{Args, Row, Term, dim, fg, paint, row, sp};
use crate::cases::{Case, Surface};
use ratatui::style::{Modifier, Style};
use std::io::{self, Write};

const FOOTER: &str = "↑↓ move · Enter choose · Ctrl+S this session · Esc close";
const CASES: &[Case<Look>] = &[
    Case {
        name: "credentials",
        help: "fake's labels, home current and selected",
        check: "the header Credentials bold, /credential dim and Esc returns dim right-aligned; default, home and work in order; only home marked ● current in accent and selected in reverse; fake plain above the bottom footer; the footer ↑↓ move · Enter choose · Ctrl+S this session · Esc close dim.",
        build: || Look::Labels,
    },
    Case {
        name: "credentials-reading",
        help: "waiting for the labels",
        check: "the same header and footer, no label rows, and Reading the labels… plain above the bottom footer.",
        build: || Look::Reading,
    },
    Case {
        name: "credentials-none",
        help: "scripted has no credential labels",
        check: "the same header and footer, no label rows, and scripted has no credential labels plain above the bottom footer.",
        build: || Look::Empty,
    },
];

pub(crate) const SURFACE: Surface = Surface {
    flag: "--credential-case",
    file: "credential",
    title: "Credential picker (#724)",
    docs: || crate::cases::docs(CASES),
};

#[derive(Clone, Copy)]
enum Look {
    Labels,
    Reading,
    Empty,
}

fn labels(look: Look) -> &'static [&'static str] {
    match look {
        Look::Labels => &["default", "home", "work"],
        _ => &[],
    }
}
fn selected(look: Look) -> Option<usize> {
    match look {
        Look::Labels => Some(1),
        _ => None,
    }
}
fn body_rows(look: Look, height: usize) -> Vec<Row> {
    let mut out = vec![super::view_header("Credentials", "/credential")];
    for (i, label) in labels(look).iter().enumerate() {
        let style = match selected(look) == Some(i) {
            true => Style::new().add_modifier(Modifier::REVERSED),
            false => Style::new(),
        };
        let mut spans = vec![sp(*label, style)];
        if *label == "home" {
            spans.push(sp("  ● current", style.patch(fg(crate::CYAN))));
        }
        out.push(row(spans));
    }
    let status = match look {
        Look::Labels => "fake",
        Look::Reading => "Reading the labels…",
        Look::Empty => "scripted has no credential labels",
    };
    out.resize_with(height.saturating_sub(2).max(out.len()), Row::default);
    out.push(row(vec![sp(status, Style::new())]));
    out.push(row(vec![sp(FOOTER, dim())]));
    out
}

pub(crate) fn run_credential(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.credential_case.as_deref().unwrap_or_default();
    let look = crate::cases::lookup(CASES, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --credential-case case {name:?}; one of: {}",
                crate::cases::names(CASES)
            ),
        )
    })?;
    draw(term, look)?;
    let mut reader = super::input::Reader::new()?;
    loop {
        let (events, resized) = reader.wait(None)?;
        if resized {
            draw(term, look)?;
        }
        for event in events {
            if quits(event) {
                return Ok(format!("credential {name}\n"));
            }
        }
    }
}

fn quits(event: Ev) -> bool {
    match event {
        Ev::Key(Key::Esc, _) => true,
        Ev::Key(Key::Char('c'), mods) => mods.ctrl,
        Ev::Key(Key::Char('q'), mods) => !(mods.ctrl || mods.alt || mods.sup),
        _ => false,
    }
}
fn draw(term: &mut Term, look: Look) -> io::Result<()> {
    let size = term.size()?;
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|frame| {
        for (y, line) in body_rows(look, usize::from(size.height))
            .iter()
            .take(size.height as usize)
            .enumerate()
        {
            paint(frame.buffer_mut(), 0, y as u16, size.width, line);
        }
    })?;
    term.backend_mut().write_all(b"\x1b[?2026l")?;
    term.backend_mut().flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(look: Look) -> Vec<String> {
        body_rows(look, 24)
            .iter()
            .filter(|r| !r.spans.is_empty())
            .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }
    #[test]
    fn shared_frame_places_labels_after_header_and_status_above_bottom_footer() {
        let rows: Vec<String> = body_rows(Look::Labels, 24)
            .iter()
            .map(|r| r.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(rows.len(), 24);
        assert_eq!(rows[1], "default");
        assert_eq!(rows[2], "home  ● current");
        assert_eq!(rows[3], "work");
        assert_eq!(rows[22], "fake");
        assert_eq!(rows[23], FOOTER);
    }
    #[test]
    fn labels_are_ordered_and_only_home_is_current_and_selected() {
        assert_eq!(
            text(Look::Labels),
            [
                " Credentials  /credential\tEsc returns ",
                "default",
                "home  ● current",
                "work",
                "fake",
                FOOTER
            ]
        );
        assert_eq!(selected(Look::Labels), Some(1));
        let rows = body_rows(Look::Labels, 24);
        for (i, row) in rows[1..4].iter().enumerate() {
            for span in &row.spans {
                assert_eq!(span.style.add_modifier.contains(Modifier::REVERSED), i == 1);
            }
        }
        assert_eq!(rows[2].spans[1].style.fg, Some(crate::CYAN));
    }
    #[test]
    fn painted_header_is_right_aligned_and_rows_match_the_look() {
        use ratatui::{buffer::Buffer, layout::Rect};
        for width in [80, 160] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, width, 48));
            for (y, line) in body_rows(Look::Labels, 48).iter().enumerate() {
                paint(&mut buffer, 0, y as u16, width, line);
            }
            let line = |y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            assert!(line(0).ends_with("Esc returns "));
            assert_eq!(line(1).trim_end(), "default");
            assert_eq!(line(2).trim_end(), "home  ● current");
            assert_eq!(line(3).trim_end(), "work");
            assert_eq!(line(46).trim_end(), "fake");
            assert_eq!(line(47).trim_end(), FOOTER);
            assert!(buffer[(0, 2)].modifier.contains(Modifier::REVERSED));
            assert_eq!(buffer[(6, 2)].fg, crate::CYAN);
        }
    }
    #[test]
    fn reading_has_no_rows_or_selection() {
        assert_eq!(
            text(Look::Reading),
            [
                " Credentials  /credential\tEsc returns ",
                "Reading the labels…",
                FOOTER
            ]
        );
        assert_eq!(selected(Look::Reading), None);
        assert!(labels(Look::Reading).is_empty());
    }
    #[test]
    fn scripted_has_no_rows_or_selection() {
        assert_eq!(
            text(Look::Empty),
            [
                " Credentials  /credential\tEsc returns ",
                "scripted has no credential labels",
                FOOTER
            ]
        );
        assert_eq!(selected(Look::Empty), None);
        assert!(labels(Look::Empty).is_empty());
    }
    #[test]
    fn quit_keys_ignore_other_input_and_modified_q() {
        use super::super::input::Mods;
        let plain = Mods::default();
        assert!(quits(Ev::Key(Key::Esc, plain)));
        assert!(quits(Ev::Key(Key::Char('q'), plain)));
        assert!(quits(Ev::Key(
            Key::Char('c'),
            Mods {
                ctrl: true,
                ..plain
            }
        )));
        assert!(!quits(Ev::Key(Key::Char('c'), plain)));
        assert!(!quits(Ev::Key(Key::Char('x'), plain)));
        assert!(!quits(Ev::Da1));
        for mods in [
            Mods {
                ctrl: true,
                ..plain
            },
            Mods { alt: true, ..plain },
            Mods { sup: true, ..plain },
        ] {
            assert!(!quits(Ev::Key(Key::Char('q'), mods)));
        }
    }
    #[test]
    fn cases_and_unknown_are_pinned() {
        assert_eq!(
            crate::cases::names(CASES),
            "credentials, credentials-reading, credentials-none"
        );
        for case in CASES {
            assert!(crate::cases::lookup(CASES, case.name).is_some());
        }
        assert!(crate::cases::lookup(CASES, "unknown").is_none());
    }
    #[test]
    fn header_and_under_row_and_footer_have_their_styles() {
        for look in [Look::Labels, Look::Reading, Look::Empty] {
            let rows = body_rows(look, 24);
            assert_eq!(rows[0].bg, Some(crate::BI));
            assert!(rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
            for i in [2, 4] {
                assert!(rows[0].spans[i].style.add_modifier.contains(Modifier::DIM));
            }
            assert_eq!(rows[22].spans[0].style, Style::new());
            assert!(rows[23].spans[0].style.add_modifier.contains(Modifier::DIM));
        }
    }
}
