//! The reviewer setup's panel (`docs/permissions.md`, "Setting up the
//! notes"; `docs/tui.md`, "Overlays"): one overlay whose body is the screen
//! the setup is on, drawn through the shared frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::configure::FileOutcome;
use crate::format::wrap;
use crate::mouse::{Target, TargetId};
use crate::reviewer_setup::template::{DiffRow, NotesLayer};
use crate::reviewer_setup::{LineRow, LineState, Screen, Setup};
use crate::theme::Role;
use crate::view::overlay::{self, Overlay, Place, Row};
use crate::view::panel::fit_spans;

/// The panel's preferred content width: the question class.
const PREFER: u16 = 71;

/// The width of the layer tags' column: the longer tag.
const TAG_W: usize = 13;

/// The cell the proposal's text starts at: the marker, the tag column and
/// two blank cells.
const HANG: usize = 2 + TAG_W + 2;

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn row(spans: Vec<Span<'static>>) -> Row {
    Row {
        spans,
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

fn dim_row(text: &str) -> Row {
    row(vec![Span::styled(text.to_owned(), dim())])
}

/// A paragraph of `text` as plain rows wrapped at `inner`.
fn paragraph(text: &str, inner: usize, style: Style) -> Vec<Row> {
    wrap(text, inner)
        .into_iter()
        .map(|line| row(vec![Span::styled(line, style)]))
        .collect()
}

/// The topic line: `Topic n of 6 · <heading name>`.
fn topic_row(setup: &Setup) -> Row {
    let name = setup.topic().heading().trim_start_matches("### ");
    dim_row(&format!(
        "Topic {} of {} · {name}",
        setup.number(),
        contract::notes::Topic::ALL.len()
    ))
}

/// What the lines header says on the read-only topic instead of the
/// question: entries are trusted, so the person sees the risk up front.
const RISK_CAPTION: &str =
    "An entry is trusted: a command on it that writes after all writes without review.";

/// One dim row per withheld subject: its subject, its count and the one
/// reason it gets no entry. No checkbox, layer key, cite, save or diff
/// row: the row is evidence context, not a proposal.
fn context_rows(context: &[contract::notes::AllowsTally], inner: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    for tally in context {
        rows.extend(paragraph(
            &format!("{} · {} · {}", tally.subject(), tally.count(), tally.text()),
            inner,
            dim(),
        ));
    }
    rows
}

/// The persistent proposal notice, when the read-only request never gave
/// lines: exactly one dim row naming why.
fn notice_row(setup: &Setup, inner: usize) -> Vec<Row> {
    setup
        .proposal_notice()
        .map(|message| {
            paragraph(
                &format!("Read-only commands: not proposed: {message}"),
                inner,
                dim(),
            )
        })
        .unwrap_or_default()
}

/// The overlay the setup draws at `inner` columns in `height` rows.
pub(crate) fn frame(setup: &Setup, inner: usize, height: u16) -> Overlay {
    let capacity = overlay::question_capacity(height);
    let (body, footer) = match setup.screen() {
        Screen::Question => (question_body(setup, inner), question_legend()),
        Screen::Waiting => (
            vec![topic_row(setup), dim_row("Proposing lines…")],
            overlay::legend(&[("Esc", "back")]),
        ),
        Screen::Lines => lines(setup, inner, capacity),
        Screen::Diff => (diff_body(setup, inner, capacity), diff_legend()),
        Screen::Saved => saved(setup, inner),
        Screen::Notice => (
            paragraph(crate::reviewer_setup::NOTICE, inner, Style::new()),
            overlay::legend(&[("Any key", "closes")]),
        ),
    };
    Overlay {
        title: Some(("Reviewer setup".to_owned(), Some(Span::raw("✕")))),
        close: Some(TargetId::CloseOverlay),
        body,
        footer: Some(footer),
        prefer: PREFER,
    }
}

/// Draws the setup over `area` and returns its slab: empty while it is
/// closed.
pub(crate) fn draw(app: &App, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
    let Some(setup) = app.reviewer_setup().filter(|_| app.reviewer_setup_open()) else {
        return Rect::new(area.x, area.y, 0, 0);
    };
    let framed = frame(setup, overlay::question_inner(area.width), area.height);
    overlay::draw(buf, area, &framed, Place::Centre, targets)
}

// ------------------------------------------------------------- question

fn question_legend() -> Row {
    overlay::legend(&[("Enter", "answer"), ("Esc", "back")])
}

fn question_body(setup: &Setup, inner: usize) -> Vec<Row> {
    let mut rows = vec![topic_row(setup)];
    rows.extend(paragraph(setup.topic().question(), inner, Style::new()));
    let field = format!("{}█", setup.answer());
    for (at, text) in wrap(&field, inner.saturating_sub(2))
        .into_iter()
        .enumerate()
    {
        let lead = if at == 0 {
            Span::styled("› ", bold())
        } else {
            Span::raw("  ")
        };
        rows.push(row(vec![lead, Span::raw(text)]));
    }
    if let Some(message) = setup.error() {
        rows.extend(paragraph(message, inner, dim()));
    }
    rows
}

// ---------------------------------------------------------------- lines

/// A line's rows: the marker, the layer tag and the text wrapped under
/// itself, its first row barred while it has the focus. A line the person
/// kept, and the struck line on the bar, read as the tag and the text one
/// space apart; every other line keeps the tag in its own column.
fn line_rows(line: &LineRow<'_>, inner: usize) -> Vec<Row> {
    let struck = line.state == LineState::Struck;
    let marker = if line.focused {
        let mut style = bold();
        if struck {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        Span::styled("› ", style)
    } else if line.state == LineState::Kept {
        Span::styled("✓ ", Style::new().fg(Role::Success.color()))
    } else {
        Span::raw("  ")
    };
    let text_style = match (line.focused, line.state) {
        (_, LineState::Struck) => dim().add_modifier(Modifier::CROSSED_OUT),
        (true, _) => bold(),
        (false, LineState::Kept) => dim(),
        (false, LineState::Open) => Style::new(),
    };
    let tag_style = if struck {
        Style::new().add_modifier(Modifier::CROSSED_OUT)
    } else if line.focused {
        bold()
    } else {
        Style::new()
    };
    let shown = line
        .editing
        .map_or_else(|| line.text.to_owned(), |field| format!("{field}█"));
    let tight = if line.focused {
        struck
    } else {
        line.state == LineState::Kept
    };
    let mut rows = Vec::new();
    if tight {
        let spans = [
            Span::styled(line.layer.tag().to_owned(), tag_style),
            Span::styled(format!(" {shown}"), text_style),
        ];
        for (at, mut pieces) in wrap_spans(&spans, inner).into_iter().enumerate() {
            if at == 0 {
                pieces.insert(0, marker.clone());
            }
            let mut made = row(pieces);
            made.barred = line.focused && at == 0;
            rows.push(made);
        }
        return rows;
    }
    let tag = format!("{:<TAG_W$}", line.layer.tag());
    let text = wrap(&shown, inner.saturating_sub(HANG).max(1));
    for (at, piece) in text.into_iter().enumerate() {
        let mut spans = if at == 0 {
            vec![
                marker.clone(),
                Span::styled(tag.clone(), tag_style),
                Span::raw("  "),
            ]
        } else {
            vec![Span::raw(" ".repeat(HANG))]
        };
        spans.push(Span::styled(piece, text_style));
        let mut made = row(spans);
        made.barred = line.focused && at == 0;
        rows.push(made);
    }
    rows
}

/// One line's block: the rows that come before it, its own and the ones
/// after it.
struct Block {
    rows: Vec<Row>,
    /// Where the focused line's own rows start in `rows`.
    own: usize,
}

fn legend_for(lines: &[LineRow<'_>]) -> Row {
    let Some(focus) = lines.iter().find(|line| line.focused) else {
        return overlay::legend(&[("Enter", "next topic"), ("Esc", "back")]);
    };
    if focus.editing.is_some() {
        return overlay::legend(&[("Enter", "confirm"), ("Esc", "cancel")]);
    }
    match (focus.earlier, focus.state) {
        (_, LineState::Struck) => overlay::legend(&[("Enter", "keep"), ("Esc", "back")]),
        (true, _) => overlay::legend(&[("Enter", "keep"), ("x", "strike"), ("Esc", "back")]),
        (false, _) => overlay::legend(&[
            ("Tab", "flip layer"),
            ("Enter", "confirm"),
            ("e", "edit"),
            ("x", "strike"),
            ("Esc", "back"),
        ]),
    }
}

fn lines(setup: &Setup, inner: usize, capacity: usize) -> (Vec<Row>, Row) {
    let items = setup.line_rows();
    let legend = legend_for(&items);
    let has_earlier = items.iter().any(|line| line.earlier);
    let any_open = items
        .iter()
        .any(|line| !line.earlier && line.state == LineState::Open);

    // The question reads while the person is still deciding and no earlier
    // line stands first; the read-only topic shows the risk caption
    // instead of a question it never asks.
    let mut header = vec![topic_row(setup)];
    if setup.topic() == contract::notes::Topic::ReadOnlyCommands {
        header.extend(paragraph(RISK_CAPTION, inner, Style::new()));
    } else if !has_earlier && any_open {
        header.extend(paragraph(setup.topic().question(), inner, Style::new()));
    }
    header.extend(notice_row(setup, inner));
    if let Some(message) = setup.error() {
        header.extend(paragraph(message, inner, dim()));
    }
    let context = context_rows(&setup.evidence_context(), inner);
    if items.is_empty() {
        header.push(dim_row("No lines for this topic"));
        header.extend(context);
        return (header, legend);
    }

    let mut first_earlier = true;
    let mut first_proposal = true;
    let mut blocks: Vec<Block> = Vec::new();
    for line in &items {
        let mut rows = Vec::new();
        if line.earlier && std::mem::take(&mut first_earlier) {
            rows.push(dim_row("Earlier lines"));
        }
        if !line.earlier && std::mem::take(&mut first_proposal) {
            if has_earlier {
                rows.push(dim_row("New proposals"));
            }
            if any_open {
                for text in setup.evidence() {
                    rows.extend(paragraph(text, inner, Style::new()));
                }
            }
        }
        if line.focused && line.state == LineState::Struck {
            // Entries never enter the notes, so striking one only skips
            // its save.
            let struck = if setup.topic() == contract::notes::Topic::ReadOnlyCommands {
                "struck — will not be saved"
            } else {
                "struck — kept out of the notes"
            };
            rows.push(dim_row(struck));
        }
        let own = rows.len();
        rows.extend(line_rows(line, inner));
        if line.focused
            && let Some(was) = line.flipped_from
        {
            rows.push(dim_row(&format!(
                "was {} · Tab flipped it",
                was.tag().trim_matches(['[', ']'])
            )));
        }
        blocks.push(Block { rows, own });
    }

    let focus = items.iter().position(|line| line.focused).unwrap_or(0);
    let focused_len = blocks.get(focus).map_or(0, |block| block.rows.len());
    // The header stays while it and the focused line fit together;
    // otherwise the focused line takes the whole body.
    let keep_header = header.len() + focused_len <= capacity;
    let room = if keep_header {
        capacity - header.len()
    } else {
        capacity
    };
    let len = |block: &Block| block.rows.len();
    // The window begins where it last did and moves down past each block
    // while the run from there to the focus does not fit; the focus alone
    // when none fits.
    let start = setup.top().get().min(focus);
    let top = start
        + (start..focus)
            .take_while(|from| {
                blocks
                    .iter()
                    .take(focus + 1)
                    .skip(*from)
                    .map(len)
                    .sum::<usize>()
                    > room
            })
            .count();
    setup.top().set(top);

    let mut body = if keep_header { header } else { Vec::new() };
    let mut left = room;
    for (at, block) in blocks.into_iter().enumerate().skip(top) {
        if left == 0 {
            break;
        }
        let mut rows = block.rows;
        if at == focus && rows.len() > left {
            // A line taller than what is left shows its start and ends in …;
            // the labels before it give way first.
            let skip = block.own.min(rows.len() - left);
            rows.drain(..skip);
            clip(&mut rows, left, inner);
        }
        left = left.saturating_sub(rows.len());
        body.extend(rows);
    }
    body.extend(context);
    (body, legend)
}

/// Keeps the first `max` rows and ends the last in `…`.
fn clip(rows: &mut Vec<Row>, max: usize, inner: usize) {
    rows.truncate(max);
    if let Some(last) = rows.last_mut() {
        let style = last.spans.last().map_or_else(Style::new, |span| span.style);
        let mut spans = std::mem::take(&mut last.spans);
        spans.push(Span::styled("…", style));
        last.spans = fit_spans(spans, inner);
    }
}

// ----------------------------------------------------------------- diff

fn diff_legend() -> Row {
    overlay::legend(&[("Enter", "approve"), ("Esc", "cancel")])
}

fn diff_rows_of(rows: &[DiffRow], inner: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for diff in rows {
        let (marker, text, style) = match diff {
            DiffRow::Context(text) => ("  ", text, dim()),
            DiffRow::Removed(text) => ("- ", text, Style::new().fg(Role::Removed.color())),
            DiffRow::Added(text) => ("+ ", text, Style::new().fg(Role::Added.color())),
        };
        let pieces = wrap(text, inner.saturating_sub(2).max(1));
        let pieces = if pieces.is_empty() {
            vec![String::new()]
        } else {
            pieces
        };
        for (at, piece) in pieces.into_iter().enumerate() {
            let lead = if at == 0 { marker } else { "  " };
            out.push(row(vec![Span::styled(format!("{lead}{piece}"), style)]));
        }
    }
    out
}

/// Every row the diff screen has: each layer's file and its changed
/// topics, then the proposed config sections, the notice and error rows,
/// and the reload offer.
fn diff_all(setup: &Setup, inner: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    for (at, layer) in setup.layers().into_iter().enumerate() {
        if at > 0 {
            rows.push(row(Vec::new()));
        }
        let name = match layer.layer {
            NotesLayer::Global => "Global notes",
            NotesLayer::PerProject => "Per-project notes",
        };
        rows.extend(paragraph(
            &format!("{name} · {}", layer.file.display()),
            inner,
            dim(),
        ));
        rows.push(dim_row("  reviewer.context:"));
        if layer.rows.is_empty() {
            rows.push(dim_row("    (no changes)"));
        } else {
            rows.extend(diff_rows_of(layer.rows, inner));
        }
    }
    for section in setup.config_sections().into_iter() {
        if section.entries.is_empty() {
            continue;
        }
        rows.push(row(Vec::new()));
        rows.extend(paragraph(
            &format!("shell.read_only · {}", section.file.display()),
            inner,
            dim(),
        ));
        rows.extend(diff_rows_of(
            &section
                .entries
                .iter()
                .map(|entry| {
                    DiffRow::Added(contract::notes::render_read_only_entry(
                        &entry.command,
                        &entry.flags,
                    ))
                })
                .collect::<Vec<_>>(),
            inner,
        ));
    }
    rows.push(row(Vec::new()));
    rows.extend(notice_row(setup, inner));
    if let Some(message) = setup.error() {
        rows.extend(paragraph(message, inner, dim()));
    }
    rows.push(row(vec![
        Span::styled("Approving then offers ", dim()),
        Span::styled("r", bold()),
        Span::styled(" reload now or ", dim()),
        Span::styled("n", bold()),
        Span::styled(" from the next session.", dim()),
    ]));
    rows
}

fn diff_body(setup: &Setup, inner: usize, capacity: usize) -> Vec<Row> {
    let all = diff_all(setup, inner);
    setup.note_viewport(all.len(), capacity);
    let start = setup.scroll().min(all.len().saturating_sub(capacity));
    all.into_iter().skip(start).take(capacity).collect()
}

// ---------------------------------------------------------------- saved

fn saved(setup: &Setup, inner: usize) -> (Vec<Row>, Row) {
    let mut rows = Vec::new();
    for file in setup.saved_files() {
        let path = file.file.display();
        let (state, error) = match &file.outcome {
            FileOutcome::Saved => ("saved", None),
            FileOutcome::SavedUnsynced(error) => ("saved", Some(error.message.as_str())),
            FileOutcome::NotSaved(error) => ("not saved", Some(error.message.as_str())),
        };
        let mut spans = vec![Span::raw(format!("{path} · {state}"))];
        if let Some(message) = error {
            spans.push(Span::styled(format!(" · {message}"), dim()));
        }
        rows.extend(wrap_spans(&spans, inner).into_iter().map(row));
    }
    let legend = if setup.any_saved() {
        overlay::legend(&[("r", "reload now"), ("n", "from the next session")])
    } else {
        overlay::legend(&[("Esc", "closes")])
    };
    (rows, legend)
}

/// `spans` wrapped as one paragraph at `inner`, each row after the first
/// hanging two cells under the first: the style follows each character.
fn wrap_spans(spans: &[Span<'static>], inner: usize) -> Vec<Vec<Span<'static>>> {
    let whole: String = spans.iter().map(|span| span.content.as_ref()).collect();
    let mut chars = spans
        .iter()
        .flat_map(|span| span.content.chars().map(|ch| (ch, span.style)))
        .peekable();
    let mut rows = Vec::new();
    for (at, text) in wrap(&whole, inner.saturating_sub(2).max(1))
        .into_iter()
        .enumerate()
    {
        // The wrap dropped the spaces and newlines it broke at.
        while chars.next_if(|(ch, _)| matches!(ch, ' ' | '\n')).is_some() {}
        let mut out = if at > 0 {
            vec![Span::raw("  ")]
        } else {
            Vec::new()
        };
        let mut run = String::new();
        let mut run_style = Style::new();
        for ch in text.chars() {
            let style = chars.next().map_or(run_style, |(_, style)| style);
            if style != run_style && !run.is_empty() {
                out.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
            run.push(ch);
        }
        if !run.is_empty() {
            out.push(Span::styled(run, run_style));
        }
        rows.push(out);
    }
    rows
}

#[cfg(test)]
#[path = "reviewer_setup_tests.rs"]
mod tests;
