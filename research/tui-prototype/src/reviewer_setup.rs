//! `/reviewer-setup` (#2480): the notes setup with the person, drawn as a
//! centred panel over a dimmed conversation. Fixture data only: the
//! prototype runs no reviewer, so the topics, evidence and every line below
//! are made up. Each case draws one static frame and waits for a key; Esc,
//! q or Ctrl+C quits. The notice the terminal shows when the reviewer blocks
//! while both layers are empty reuses `--overlay notice`: it is not redrawn
//! here.

use super::input::{Ev, Key};
use super::overlays;
use super::{Args, Term};
use super::{dim, fit, paint, panel, sp, width};
use crate::cases::{Case, Surface};
use std::io::{self, Write};

/// Where a proposed line's evidence comes from: the project's own files,
/// the person's answer to one question per topic, or the person's sessions.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    ProjectFiles,
    Answer,
    Session,
}

/// The notes layer a line will go to.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Layer {
    PerProject,
    Global,
}

/// Which layer a line goes to: a line from the project's files goes to the
/// per-project notes, an answer about accounts, hosts or tools to the
/// global notes, and a session line to the per-project notes only when
/// every escalation behind it came from this project. Outside a project,
/// every line goes to the global notes.
#[allow(dead_code)]
fn layer(source: Source, all_here: bool, in_project: bool) -> Layer {
    if !in_project {
        return Layer::Global;
    }
    match source {
        Source::ProjectFiles => Layer::PerProject,
        Source::Answer => Layer::Global,
        Source::Session => {
            if all_here {
                Layer::PerProject
            } else {
                Layer::Global
            }
        }
    }
}

/// The layer's tag on a proposed line.
#[allow(dead_code)]
fn tag(layer: Layer) -> &'static str {
    match layer {
        Layer::PerProject => "[per-project]",
        Layer::Global => "[global]",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    First,
    Outside,
    Rerun,
    Struck,
    Flipped,
    Diff,
}

#[derive(Debug)]
pub struct State {
    kind: Kind,
}

/// Every `--reviewer-setup` case.
pub(crate) const CASES: &[Case<State>] = &[
    Case {
        name: "first",
        help: "first run in a project: one topic at a time with evidence and layer",
        check: "topic `4 of 5 · Routine actions` with the `gh pr merge … --squash` escalation count as evidence and the `[per-project]` line barred; a centred panel with ▄ ▀ edges, `Reviewer setup` bold accent with a dim ✕, and a bold-key legend foot.",
        build: || State { kind: Kind::First },
    },
    Case {
        name: "outside",
        help: "first run outside a project: every line global",
        check: "topic `2 of 5 · Internal hosts` with the person's answer as evidence and the `[global]` line barred, under a dim `Outside a project` line; the same panel and legend.",
        build: || State {
            kind: Kind::Outside,
        },
    },
    Case {
        name: "rerun",
        help: "rerun: earlier lines first, to keep or strike",
        check: "two kept `✓` earlier lines under dim `Earlier lines`, then the new proposal barred under dim `New proposals`; the same panel with a keep-and-strike legend.",
        build: || State { kind: Kind::Rerun },
    },
    Case {
        name: "struck",
        help: "a struck line, kept out of the notes",
        check: "the struck line dim with strikethrough on the bar, under a dim `struck` line; the same panel with a keep-or-back legend.",
        build: || State { kind: Kind::Struck },
    },
    Case {
        name: "flipped",
        help: "a flipped layer, from per-project to global",
        check: "the same line as `first` barred with the `[global]` tag, under a dim `was per-project` line; the same panel and legend.",
        build: || State {
            kind: Kind::Flipped,
        },
    },
    Case {
        name: "diff",
        help: "the final diff per layer with files named, approve or cancel",
        check: "each layer's notes as a diff against the file as it is now with the file named, added lines `+` in the added colour, `(no changes)` where the layer is untouched, and a dim reload line; the same panel with an approve-or-cancel legend.",
        build: || State { kind: Kind::Diff },
    },
];

/// `--reviewer-setup`, for `--help` and `check/reviewer-setup.md`.
pub(crate) const SURFACE: Surface = Surface {
    flag: "--reviewer-setup",
    file: "reviewer-setup",
    title: "Reviewer setup (#2480)",
    docs: || crate::cases::docs(CASES),
};

#[cfg(test)]
pub fn for_case(case: &str) -> State {
    crate::cases::lookup(CASES, case).unwrap_or_else(|| {
        panic!(
            "--reviewer-setup {}",
            crate::cases::names(CASES).replace(", ", "|")
        )
    })
}

/// The foot legend for the case: keys bold, labels muted.
fn footer(s: &State) -> super::Row {
    match s.kind {
        Kind::First | Kind::Outside | Kind::Flipped => panel::footer_legend(&[
            ("Tab", "flip layer"),
            ("Enter", "confirm"),
            ("e", "edit"),
            ("x", "strike"),
            ("Esc", "back"),
        ]),
        Kind::Rerun => panel::footer_legend(&[("Enter", "keep"), ("x", "strike"), ("Esc", "back")]),
        Kind::Struck => panel::footer_legend(&[("Enter", "keep"), ("Esc", "back")]),
        Kind::Diff => panel::footer_legend(&[("Enter", "approve"), ("Esc", "cancel")]),
    }
}

/// The panel's body rows at an inner width. Task 2 draws them.
fn body(_s: &State, _inner: usize) -> Vec<super::Row> {
    vec![]
}

/// The centred panel rows at an area this wide: a bold accent title with
/// a dim ✕, the body, and the legend foot.
pub fn view(s: &State, w: usize) -> Vec<super::Row> {
    let natural = body(s, 10_000)
        .iter()
        .map(|r| width(&r.spans))
        .max()
        .unwrap_or(0)
        .max(width(&footer(s).spans));
    // The legend always fits: the preferred width stretches past the usual
    // cap rather than cutting the foot.
    let prefer = w.saturating_sub(4).min(96).max(width(&footer(s).spans));
    let panel_w = panel::fit_width(natural, prefer, w);
    let inner = panel::inner_w(panel_w);
    let rows = panel::frame(
        Some(panel::title_row("Reviewer setup", Some(sp("✕", dim())))),
        body(s, inner),
        Some(footer(s)),
        panel_w,
    );
    panel::centre(rows, panel_w, w)
}

// ============================================================ frame
/// A row painted at its own offset and width, so the panel's background
/// stays within its edges instead of extending across the screen.
struct Placed {
    x: u16,
    w: u16,
    row: super::Row,
}

fn frame(s: &State, cols: usize, rows: usize) -> Vec<Vec<Placed>> {
    let mut screen: Vec<Vec<Placed>> = overlays::backdrop(cols, rows)
        .into_iter()
        .map(|r| {
            vec![Placed {
                x: 0,
                w: cols as u16,
                row: r,
            }]
        })
        .collect();
    // A small panel floats centred over the dimmed conversation, which
    // reads above and below it.
    let ov = view(s, cols);
    let y0 = rows.saturating_sub(ov.len()) / 2;
    for (k, r) in ov.into_iter().enumerate() {
        if y0 + k >= screen.len() {
            break;
        }
        screen[y0 + k] = vec![Placed {
            x: 0,
            w: cols as u16,
            row: super::Row {
                spans: fit(&r.spans, cols),
                ..r
            },
        }];
    }
    screen
}

fn draw(term: &mut Term, s: &State) -> io::Result<()> {
    let size = term.size()?;
    let (cols, rows) = (size.width.max(1), size.height.max(1));
    term.backend_mut().write_all(b"\x1b[?2026h")?;
    term.draw(|fr| {
        let buf = fr.buffer_mut();
        for (y, placements) in frame(s, cols as usize, rows as usize).iter().enumerate() {
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

/// Draws the `--reviewer-setup` case and waits for a key. Mutually exclusive
/// with the conversation view: the fixture replay never draws.
pub(crate) fn run_reviewer_setup(a: &Args, term: &mut Term) -> io::Result<String> {
    let name = a.reviewer_setup.clone().unwrap_or_default();
    let Some(c) = crate::cases::lookup(CASES, &name) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "unknown --reviewer-setup case {name:?}; one of: {}",
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
                Ev::Key(Key::Char('c'), m) if m.ctrl => {
                    return Ok(format!("reviewer-setup {name}\n"));
                }
                Ev::Key(Key::Esc, _) => return Ok(format!("reviewer-setup {name}\n")),
                Ev::Key(Key::Char('q'), m) if !m.ctrl && !m.alt && !m.sup => {
                    return Ok(format!("reviewer-setup {name}\n"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plain;

    fn text(s: &State, cols: usize, rows: usize) -> String {
        frame(s, cols, rows)
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

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(CASES.len(), 6);
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    #[test]
    fn lines_go_to_the_layer_their_evidence_names() {
        // Project files land per-project; answers land global.
        assert_eq!(layer(Source::ProjectFiles, true, true), Layer::PerProject);
        assert_eq!(layer(Source::Answer, true, true), Layer::Global);
        // A session line lands per-project only when every escalation
        // behind it came from this project.
        assert_eq!(layer(Source::Session, true, true), Layer::PerProject);
        assert_eq!(layer(Source::Session, false, true), Layer::Global);
        // Outside a project every line goes global, whatever its source.
        for source in [Source::ProjectFiles, Source::Answer, Source::Session] {
            assert_eq!(layer(source, true, false), Layer::Global);
            assert_eq!(layer(source, false, false), Layer::Global);
        }
    }

    #[test]
    fn the_tags_name_the_layers() {
        assert_eq!(tag(Layer::PerProject), "[per-project]");
        assert_eq!(tag(Layer::Global), "[global]");
    }

    #[test]
    fn each_case_renders_without_panicking_at_80_and_120() {
        for c in CASES {
            let s = for_case(c.name);
            for cols in [80usize, 120] {
                let rows = view(&s, cols);
                assert!(!rows.is_empty(), "{} draws nothing at {cols}", c.name);
                for r in &rows {
                    assert!(
                        crate::width(&r.spans) <= cols,
                        "{} overflows at {cols}",
                        c.name
                    );
                }
                let t = text(&s, cols, 40);
                assert!(
                    t.contains("Reviewer setup"),
                    "{} lost its title at {cols}",
                    c.name
                );
                let panel_text = rows.iter().map(plain).collect::<Vec<_>>().join("\n");
                assert!(
                    panel_text.contains("Reviewer setup"),
                    "{} lost its title in the panel at {cols}",
                    c.name
                );
            }
        }
    }

    #[test]
    fn unknown_case_names_every_case() {
        let err = std::panic::catch_unwind(|| for_case("nope")).unwrap_err();
        let msg = err.downcast_ref::<String>().map(String::as_str).unwrap();
        for c in CASES {
            assert!(msg.contains(c.name), "the listing drops {}", c.name);
        }
    }
}
