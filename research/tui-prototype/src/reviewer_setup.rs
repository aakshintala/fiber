//! `/reviewer-setup` (#2480): the notes setup with the person, drawn as a
//! centred panel over a dimmed conversation. Fixture data only: the
//! prototype runs no reviewer, so the topics, evidence and every line below
//! are made up. Each case draws one static frame and waits for a key; Esc,
//! q or Ctrl+C quits. The `notice` case draws the notice the terminal shows
//! when the reviewer blocks while both layers are empty, naming
//! `/reviewer-setup`; `question`, `waiting` and `saved` draw the screens
//! around the proposals: the topic's question, the wait for its lines and
//! the final screen after the save.

use super::input::{Ev, Key};
use super::overlays;
use super::{Args, Term};
use super::{BLUE, GREEN, RED, bold, dim, fg, fit, paint, panel, row, sp, width};
use crate::cases::{Case, Surface};
use ratatui::style::{Modifier, Style};
use std::io::{self, Write};

/// Where a proposed line's evidence comes from: the project's own files,
/// the person's answer to one question per topic, or the person's sessions.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    ProjectFiles,
    Answer,
    Session,
}

/// The notes layer a line will go to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Layer {
    PerProject,
    Global,
}

/// Which layer a line goes to: a line from the project's files goes to the
/// per-project notes, an answer about accounts, hosts or tools to the
/// global notes, and a session line to the per-project notes only when
/// every escalation behind it came from this project. Every session has a
/// project: outside a git repository the launch directory is the project.
fn layer(source: Source, all_here: bool) -> Layer {
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
    Notice,
    Question,
    Waiting,
    Saved,
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
        check: "topic `4 of 5 · Routine actions` with its question (`What routine actions …`), the `gh pr merge … --squash` escalation count as evidence and the `[per-project]` line barred; a centred panel with ▄ ▀ edges, `Reviewer setup` bold accent with a dim ✕, and a bold-key legend foot.",
        build: || State { kind: Kind::First },
    },
    Case {
        name: "outside",
        help: "first run from a launch directory outside git: an answer line is global",
        check: "topic `2 of 5 · Internal hosts` with its question (`Which internal hosts …`) and the person's answer as evidence and the `[global]` line barred, with no line about a project; the same panel and legend.",
        build: || State {
            kind: Kind::Outside,
        },
    },
    Case {
        name: "rerun",
        help: "rerun: earlier lines first, to keep or strike",
        check: "topic `1 of 5 · Source control`, two kept `✓` earlier lines under dim `Earlier lines`, then the evidence and the new proposal barred under dim `New proposals`, with no question text; the same panel with a keep-and-strike legend.",
        build: || State { kind: Kind::Rerun },
    },
    Case {
        name: "struck",
        help: "a struck line, kept out of the notes",
        check: "topic `4 of 5 · Routine actions`, then the dim `struck — kept out of the notes` line, then the struck line with strikethrough on the bar, and nothing else; the same panel with a keep-or-back legend.",
        build: || State { kind: Kind::Struck },
    },
    Case {
        name: "flipped",
        help: "a flipped layer, from per-project to global",
        check: "the topic with its question and the same line as `first` barred with the `[global]` tag, under a dim `was per-project` line; the same panel and legend.",
        build: || State {
            kind: Kind::Flipped,
        },
    },
    Case {
        name: "diff",
        help: "the final diff per layer with files named, approve or cancel",
        check: "each layer's notes as a diff against the file as it is now with the file named, the topics' heading lines as saved (`### Source control`), kept bullets as context, struck bullets with `-` in the removed colour, added bullets and new headings with `+` in the added colour, `(no changes)` where the layer is untouched, and a dim reload line; the same panel with an approve-or-cancel legend.",
        build: || State { kind: Kind::Diff },
    },
    Case {
        name: "notice",
        help: "the notice naming /reviewer-setup when the reviewer blocks with both layers empty",
        check: "the notice text naming `/reviewer-setup`, wrapped to the panel; the same panel with an any-key-closes legend.",
        build: || State { kind: Kind::Notice },
    },
    Case {
        name: "question",
        help: "a topic's question with the answer field",
        check: "topic `1 of 5 · Source control` with its question (`Which accounts …`) and a one-line answer field with a block cursor, no layer tag; the same panel with an answer-or-back legend.",
        build: || State {
            kind: Kind::Question,
        },
    },
    Case {
        name: "waiting",
        help: "waiting for the topic's proposed lines",
        check: "topic `1 of 5 · Source control` and a dim `Proposing lines…` row; the same panel with a back-only legend.",
        build: || State {
            kind: Kind::Waiting,
        },
    },
    Case {
        name: "saved",
        help: "after the save: each file, saved or not, and the reload offer",
        check: "each file named with whether it was saved: the global file saved with a dim sync error beside it, the per-project file saved; a bold-key reload-now-or-next-session legend.",
        build: || State { kind: Kind::Saved },
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
        Kind::Notice => panel::footer_legend(&[("Any key", "closes")]),
        Kind::Question => panel::footer_legend(&[("Enter", "answer"), ("Esc", "back")]),
        Kind::Waiting => panel::footer_legend(&[("Esc", "back")]),
        Kind::Saved => panel::footer_legend(&[("r", "reload now"), ("n", "from the next session")]),
    }
}

/// The fixture lines both proposals and the diff share.
const SQUASH: &str = "squash-merging my own pull requests once CI passes";
const SQUASH_EV: &str = "14 escalations answered allow for `gh pr merge … --squash`";
const HOSTS: &str = "corp.example hosts and registries count as inside";
const HOSTS_EV: &str = "answer: only corp.example hosts and registries count as inside";
const OURS: &str = "github.com/acme/* are ours";
const MINE: &str = "my account amogh-dev is mine";
const LABS: &str = "github.com/acme-labs/* are ours";
const LABS_EV: &str = "its git remotes: upstream github.com/acme-labs/fiber";
/// The answer typed so far on the question frame.
const ANSWER: &str = "github.com/acme, and my account amogh-dev";
const OLD: &str = "github.com/acme-old/* are ours";
/// The question the person answers for each topic drawn here, in the words
/// of docs/permissions.md, "What the person tells it".
const ROUTINE_Q: &str = "What routine actions, such as squash-merging your own pull requests once CI passes, should the reviewer allow?";
const HOSTS_Q: &str = "Which internal hosts, registries and services count as inside?";
const SOURCE_Q: &str =
    "Which accounts, organisations and repositories are your own or your organisation's?";
/// The notice the terminal shows the first time in a session that the
/// reviewer blocks a call while both layers of the notes are empty.
const NOTICE_TEXT: &str = "The reviewer blocked a call while both layers of the notes are empty. Run /reviewer-setup to set them up.";
/// Fiber home holds both layers: the global file at its top and the
/// per-project file under `projects/<key>` (docs/configuration.md, "Layers").
const GLOBAL_FILE: &str = "~/.fiber/config.json";
/// The sync error the saved frame shows beside the global file.
const SYNC_ERROR: &str = "could not sync the directory: the new file is in place";
const PROJECT_FILE: &str = "~/.fiber/projects/fiber/config.json";

/// A topic heading: which of the five topics this screen proposes.
fn topic(name: &str) -> super::Row {
    row(vec![sp(name, dim())])
}

/// An evidence line, wrapped to the inner width.
fn evidence(text: &str, inner: usize) -> Vec<super::Row> {
    super::wrap_rows(vec![sp(text, Style::new())], inner, vec![], vec![])
}

/// A proposed line as the focused choice: the layer tag in the key column
/// and the line beside it, the whole row barred.
fn barred(layer: Layer, line: &str, inner: usize) -> Vec<super::Row> {
    let key_w = panel::key_width(&[tag(Layer::PerProject), tag(Layer::Global)]);
    panel::bar(panel::choice_row(true, tag(layer), line, key_w, inner))
}

/// A proposal from its evidence: the layer comes from the rule, so the
/// screens cannot tag a line against it.
fn proposal(source: Source, all_here: bool, line: &str, inner: usize) -> Vec<super::Row> {
    barred(layer(source, all_here), line, inner)
}

/// An earlier line the rerun kept: a tick in the success colour, no bar.
/// The layer comes from the rule, as on a proposal.
fn kept(source: Source, all_here: bool, line: &str) -> super::Row {
    let layer = layer(source, all_here);
    row(vec![
        sp("\u{2713} ", fg(BLUE)),
        sp(tag(layer), Style::new()),
        sp(format!(" {line}"), dim()),
    ])
}

/// A struck line: struck through even on its bar, so it reads as removed.
fn struck_row(source: Source, all_here: bool, line: &str, inner: usize) -> Vec<super::Row> {
    let off = Style::new().add_modifier(Modifier::CROSSED_OUT);
    panel::bar(super::wrap_rows(
        vec![
            sp(tag(layer(source, all_here)), off),
            sp(format!(" {line}"), off),
        ],
        inner,
        vec![sp("\u{203a} ", bold().add_modifier(Modifier::CROSSED_OUT))],
        vec![sp("  ", Style::new())],
    ))
}

/// A diff row: a two-cell marker and the saved line. Added lines read in
/// the added colour, removed ones in the removed colour, context dim.
fn diff_row(marker: &str, line: &str) -> super::Row {
    let style = match marker {
        "+" => fg(GREEN),
        "-" => fg(RED),
        _ => dim(),
    };
    row(vec![sp(format!("{marker} {line}"), style)])
}

/// A file's name row and the `reviewer.context:` row under it.
fn file_rows(label: &str, file: &str) -> Vec<super::Row> {
    vec![
        row(vec![sp(format!("{label} · {file}"), dim())]),
        row(vec![sp("  reviewer.context:", dim())]),
    ]
}

/// The notice's whole text, wrapped to the inner width.
fn notice_body(inner: usize) -> Vec<super::Row> {
    super::wrap_rows(vec![sp(NOTICE_TEXT, Style::new())], inner, vec![], vec![])
}

/// The topic line, then the question the person answers.
fn asked(topic_line: &str, question: &str, inner: usize) -> Vec<super::Row> {
    [vec![topic(topic_line)], evidence(question, inner)].concat()
}

/// The panel's body rows at an inner width.
fn body(s: &State, inner: usize) -> Vec<super::Row> {
    match s.kind {
        Kind::First => [
            asked("Topic 4 of 5 · Routine actions", ROUTINE_Q, inner),
            evidence(SQUASH_EV, inner),
            proposal(Source::Session, true, SQUASH, inner),
        ]
        .concat(),
        Kind::Outside => [
            asked("Topic 2 of 5 · Internal hosts", HOSTS_Q, inner),
            evidence(HOSTS_EV, inner),
            proposal(Source::Answer, true, HOSTS, inner),
        ]
        .concat(),
        // Earlier lines show, so the question is left out.
        Kind::Rerun => [
            vec![topic("Topic 1 of 5 · Source control")],
            vec![row(vec![sp("Earlier lines", dim())])],
            vec![
                kept(Source::ProjectFiles, true, OURS),
                kept(Source::Answer, true, MINE),
            ],
            vec![row(vec![sp("New proposals", dim())])],
            evidence(LABS_EV, inner),
            proposal(Source::ProjectFiles, true, LABS, inner),
        ]
        .concat(),
        Kind::Struck => vec![
            topic("Topic 4 of 5 · Routine actions"),
            row(vec![sp("struck — kept out of the notes", dim())]),
        ]
        .into_iter()
        .chain(struck_row(Source::Session, true, SQUASH, inner))
        .collect(),
        Kind::Flipped => [
            asked("Topic 4 of 5 · Routine actions", ROUTINE_Q, inner),
            evidence(SQUASH_EV, inner),
            // The person flipped this line off its rule-given layer.
            barred(Layer::Global, SQUASH, inner),
            vec![row(vec![sp("was per-project · Tab flipped it", dim())])],
        ]
        .concat(),
        // The rows are the line diff of the file as it is against the text
        // the save writes: headings as saved, bullets under them.
        Kind::Diff => [
            file_rows("Global notes", GLOBAL_FILE),
            vec![row(vec![sp("    (no changes)", dim())]), row(vec![])],
            file_rows("Per-project notes", PROJECT_FILE),
            vec![
                diff_row(" ", "### Source control"),
                diff_row(" ", &format!("- {OURS}")),
                diff_row("-", &format!("- {OLD}")),
                diff_row("+", &format!("- {LABS}")),
                diff_row("+", "### Routine actions"),
                diff_row("+", &format!("- {SQUASH}")),
                row(vec![]),
                row(vec![sp(
                    "Approving then offers `r` reload now or `n` from the next session.",
                    dim(),
                )]),
            ],
        ]
        .concat(),
        Kind::Notice => notice_body(inner),
        Kind::Question => [
            asked("Topic 1 of 5 · Source control", SOURCE_Q, inner),
            vec![row(vec![
                sp("› ", bold()),
                sp(ANSWER, Style::new()),
                sp("█", Style::new()),
            ])],
        ]
        .concat(),
        Kind::Waiting => vec![
            topic("Topic 1 of 5 · Source control"),
            row(vec![sp("Proposing lines…", dim())]),
        ],
        // The sync error sits beside its file and wraps under it.
        Kind::Saved => [
            super::wrap_rows(
                vec![
                    sp(format!("{GLOBAL_FILE} · saved"), Style::new()),
                    sp(format!(" · {SYNC_ERROR}"), dim()),
                ],
                inner,
                vec![],
                vec![sp("  ", Style::new())],
            ),
            vec![row(vec![sp(
                format!("{PROJECT_FILE} · saved"),
                Style::new(),
            )])],
        ]
        .concat(),
    }
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
    // A question-sized panel: docs/tui.md wraps questions at a preferred
    // width of 71. The legend always fits: the preferred width stretches
    // past it rather than cutting the foot.
    let prefer = w.saturating_sub(4).min(71).max(width(&footer(s).spans));
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

    /// The panel's rows as plain text lines, without the backdrop.
    fn panel_lines(case: &str, cols: usize) -> Vec<String> {
        view(&for_case(case), cols).iter().map(plain).collect()
    }

    fn position(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row holds {needle:?}"))
    }

    #[test]
    fn every_case_parses_and_unknown_does_not() {
        assert!(
            CASES
                .iter()
                .all(|c| crate::cases::lookup(CASES, c.name).is_some())
        );
        assert_eq!(
            crate::cases::names(CASES),
            "first, outside, rerun, struck, flipped, diff, notice, question, waiting, saved"
        );
        assert!(crate::cases::lookup(CASES, "nope").is_none());
    }

    #[test]
    fn lines_go_to_the_layer_their_evidence_names() {
        // Project files land per-project; answers land global.
        assert_eq!(layer(Source::ProjectFiles, true), Layer::PerProject);
        assert_eq!(layer(Source::Answer, true), Layer::Global);
        // A session line lands per-project only when every escalation
        // behind it came from this project.
        assert_eq!(layer(Source::Session, true), Layer::PerProject);
        assert_eq!(layer(Source::Session, false), Layer::Global);
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
    fn first_proposes_one_line_with_evidence_and_layer() {
        let t = text(&for_case("first"), 120, 40);
        assert!(
            t.contains("Topic 4 of 5 · Routine actions"),
            "missing the topic line"
        );
        assert!(
            t.contains("14 escalations answered allow"),
            "missing the session evidence"
        );
        assert!(t.contains("gh pr merge"), "missing the evidence prefix");
        assert!(t.contains("[per-project]"), "missing the layer tag");
        assert!(
            t.contains("squash-merging my own pull requests once CI passes"),
            "missing the proposed line"
        );
        assert!(
            t.contains("Tab flip layer · Enter confirm · e edit · x strike · Esc back"),
            "missing the legend"
        );
    }

    #[test]
    fn outside_runs_global_and_says_nothing_about_a_project() {
        let t = text(&for_case("outside"), 120, 40);
        assert!(
            t.contains("Topic 2 of 5 · Internal hosts"),
            "missing the topic line"
        );
        assert!(t.contains("answer:"), "missing the answer evidence");
        assert!(t.contains("[global]"), "the line is not global");
        assert!(!t.contains("[per-project]"), "a project tag leaks outside");
        assert!(
            !t.contains("Outside a project"),
            "every session has a project, so the screen never says otherwise"
        );
    }

    #[test]
    fn rerun_lists_earlier_lines_before_new_proposals_under_its_topic() {
        let lines = panel_lines("rerun", 120);
        let topic = position(&lines, "Topic 1 of 5 · Source control");
        let earlier = position(&lines, "Earlier lines");
        let kept = position(&lines, "are ours");
        let new = position(&lines, "New proposals");
        let proposal = position(&lines, "acme-labs");
        assert!(
            topic < earlier && earlier < kept && kept < new && new < proposal,
            "rows read out of order: {lines:#?}"
        );
        // The question text is left out once earlier lines show.
        assert!(
            !lines.iter().any(|l| l.contains("Which accounts")),
            "the question reads on a rerun"
        );
        let t = lines.join("\n");
        assert!(t.contains("✓"), "an earlier line is not ticked");
        assert!(
            t.contains("Enter keep · x strike · Esc back"),
            "missing the legend"
        );
    }

    #[test]
    fn labels_belong_to_the_rerun_alone() {
        for c in ["first", "outside", "struck", "flipped"] {
            let t = panel_lines(c, 120).join("\n");
            assert!(!t.contains("Earlier lines"), "{c} labels earlier lines");
            assert!(!t.contains("New proposals"), "{c} labels new proposals");
        }
    }

    #[test]
    fn every_lines_frame_opens_on_its_topic_line() {
        for c in ["first", "outside", "rerun", "struck", "flipped"] {
            let lines = panel_lines(c, 120);
            let title = position(&lines, "Reviewer setup");
            let topic = position(&lines, "Topic ");
            assert!(
                lines[title + 1..topic].iter().all(|l| l.trim().is_empty()),
                "{c}: something reads between the title and the topic line"
            );
            assert!(lines[topic].contains(" of 5 · "), "{c}: {}", lines[topic]);
        }
    }

    #[test]
    fn struck_keeps_the_line_out_of_the_notes_and_shows_nothing_else() {
        let lines = panel_lines("struck", 120);
        let t = lines.join("\n");
        assert!(
            t.contains("squash-merging my own pull requests once CI passes"),
            "missing the struck line"
        );
        assert!(
            t.contains("struck — kept out of the notes"),
            "missing the struck state"
        );
        assert!(t.contains("Enter keep · Esc back"), "missing the legend");
        // The topic line, the label and the barred row only.
        assert!(
            !t.contains("14 escalations"),
            "evidence reads on a struck line"
        );
        assert!(
            !t.contains("What routine actions"),
            "the question reads on a struck line"
        );
        assert!(
            position(&lines, "Topic 4 of 5") < position(&lines, "struck —")
                && position(&lines, "struck —") < position(&lines, "squash-merging"),
            "rows read out of order"
        );
        // The struck line reads struck through, even on its bar.
        use ratatui::style::Modifier;
        let rows = view(&for_case("struck"), 120);
        let line = rows
            .iter()
            .find(|r| plain(r).contains("squash-merging"))
            .unwrap();
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::CROSSED_OUT)),
            "the struck line is not struck through"
        );
    }

    #[test]
    fn flipped_moves_the_line_to_global() {
        let t = text(&for_case("flipped"), 120, 40);
        assert!(
            t.contains("squash-merging my own pull requests once CI passes"),
            "missing the flipped line"
        );
        assert!(t.contains("[global]"), "the tag did not flip");
        assert!(
            !t.contains("[per-project]"),
            "the old tag is still on the line"
        );
        assert!(
            t.contains("was per-project · Tab flipped it"),
            "missing the flip history"
        );
    }

    #[test]
    fn diff_shows_the_saved_text_of_each_file() {
        let lines = panel_lines("diff", 120);
        let t = lines.join("\n");
        assert!(
            t.contains("~/.fiber/config.json"),
            "the global file is unnamed"
        );
        assert!(
            t.contains("~/.fiber/projects/fiber/config.json"),
            "the per-project file is unnamed"
        );
        assert!(
            t.contains("(no changes)"),
            "the untouched layer says nothing"
        );
        assert!(
            t.contains("Approving then offers `r` reload now or `n` from the next session."),
            "missing the reload step"
        );
        assert!(
            t.contains("Enter approve · Esc cancel"),
            "missing the legend"
        );
        // Headings read as saved, not as `Source control:`.
        assert!(
            t.contains("### Source control"),
            "the heading is not as saved"
        );
        assert!(
            t.contains("### Routine actions"),
            "the heading is not as saved"
        );
        assert!(!t.contains("Source control:"), "a heading reads unsaved");
        // A new heading is itself an added line; an existing one is context.
        assert!(
            t.contains("+ ### Routine actions"),
            "a new heading is not added"
        );
        assert!(
            !t.contains("+ ### Source control"),
            "an existing heading is added"
        );
        assert!(
            t.contains("+ - squash-merging my own pull requests once CI passes"),
            "missing an added bullet"
        );
        // Bullets sit under their topic's heading.
        let source = position(&lines, "### Source control");
        let ours = position(&lines, "+ - github.com/acme-labs/* are ours");
        let routine = position(&lines, "### Routine actions");
        let squash = position(&lines, "+ - squash-merging");
        assert!(
            source < ours && ours < routine && routine < squash,
            "a bullet reads outside its topic's heading: {lines:#?}"
        );
        // Added reads in the added colour, removed in the removed colour.
        let rows = view(&for_case("diff"), 120);
        let colour = |needle: &str| {
            rows.iter()
                .find(|r| plain(r).contains(needle))
                .unwrap_or_else(|| panic!("no row holds {needle:?}"))
                .spans
                .iter()
                .any(|s| {
                    s.style.fg
                        == Some(if needle.starts_with("- ") {
                            crate::RED
                        } else {
                            crate::GREEN
                        })
                })
        };
        assert!(
            colour("+ - squash-merging"),
            "an added line is not the added colour"
        );
        assert!(
            colour("+ ### Routine actions"),
            "an added heading is not the added colour"
        );
        let removed = lines
            .iter()
            .find(|l| l.contains("- - "))
            .expect("a removed bullet");
        assert!(removed.contains("acme-old"), "{removed}");
        assert!(
            rows.iter()
                .find(|r| plain(r).contains("acme-old"))
                .unwrap()
                .spans
                .iter()
                .any(|s| s.style.fg == Some(crate::RED)),
            "a removed line is not the removed colour"
        );
    }

    #[test]
    fn proposal_cases_ask_the_topic_question() {
        for (c, head, tail) in [
            ("first", "What routine actions", "should the reviewer allow"),
            ("outside", "Which internal hosts", HOSTS_Q),
            (
                "flipped",
                "What routine actions",
                "should the reviewer allow",
            ),
        ] {
            let t = text(&for_case(c), 120, 40);
            assert!(t.contains(head), "{c} is missing its topic question");
            // The tail may wrap onto the next row; its words still read.
            for w in tail.split(' ') {
                assert!(t.contains(w), "{c} question lost {w:?}");
            }
            let lines: Vec<&str> = t.split('\n').collect();
            let topic = lines.iter().position(|l| l.contains("of 5")).unwrap();
            let question = lines.iter().position(|l| l.contains(head)).unwrap();
            assert!(topic < question, "{c}: the question reads above its topic");
        }
    }

    #[test]
    fn notice_names_reviewer_setup_and_closes_on_any_key() {
        let t = text(&for_case("notice"), 120, 40);
        assert!(
            t.contains("/reviewer-setup"),
            "the notice does not name the command"
        );
        assert!(
            t.contains("both layers of the notes are empty"),
            "the notice does not say when it shows"
        );
        assert!(t.contains("Any key closes"), "missing the legend");
        assert!(
            !t.contains("Esc closes"),
            "the legend still names Esc alone"
        );
    }

    #[test]
    fn question_asks_for_an_answer() {
        let lines = panel_lines("question", 120);
        let t = lines.join("\n");
        let topic = position(&lines, "Topic 1 of 5 · Source control");
        let question = position(&lines, "Which accounts");
        let field = position(&lines, "█");
        assert!(
            topic < question && question < field,
            "rows read out of order"
        );
        assert!(t.contains("Enter answer · Esc back"), "missing the legend");
        assert!(
            !t.contains("[per-project]") && !t.contains("[global]"),
            "a question carries no layer"
        );
    }

    #[test]
    fn waiting_says_it_is_proposing() {
        let lines = panel_lines("waiting", 120);
        let t = lines.join("\n");
        assert!(t.contains("Proposing lines…"), "missing the waiting row");
        assert!(t.contains("Esc back"), "missing the legend");
        assert!(!t.contains("Enter"), "waiting takes only Esc: {t}");
        let rows = view(&for_case("waiting"), 120);
        let row = rows
            .iter()
            .find(|r| plain(r).contains("Proposing"))
            .unwrap();
        assert!(
            row.spans.iter().all(|s| s.content.trim().is_empty()
                || s.style.add_modifier.contains(ratatui::style::Modifier::DIM)),
            "the waiting row is not dim"
        );
    }

    #[test]
    fn saved_names_each_file_and_offers_a_reload() {
        let lines = panel_lines("saved", 120);
        let t = lines.join("\n");
        let global = position(&lines, "~/.fiber/config.json");
        let project = position(&lines, "~/.fiber/projects/fiber/config.json");
        assert!(global < project, "the global file reads first");
        assert!(lines[global].contains("saved"), "{}", lines[global]);
        assert!(
            t.contains("sync"),
            "the global file's sync error is missing"
        );
        assert!(lines[project].contains("saved"), "{}", lines[project]);
        assert!(
            t.contains("r reload now · n from the next session"),
            "missing the reload offer"
        );
    }

    #[test]
    fn lines_frames_bar_one_row_and_the_rest_none() {
        for (c, bars) in [
            ("first", 1),
            ("outside", 1),
            ("rerun", 1),
            ("struck", 1),
            ("flipped", 1),
            ("diff", 0),
            ("notice", 0),
            ("question", 0),
            ("waiting", 0),
            ("saved", 0),
        ] {
            let rows = view(&for_case(c), 120);
            let barred = rows
                .iter()
                .filter(|r| r.spans.iter().any(|s| s.style.bg == Some(crate::BLUE)))
                .count();
            assert_eq!(barred, bars, "{c}: wrong number of barred rows");
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
