//! Building a question panel's lines with their styled runs, without
//! hand-counted offsets (`docs/tui.md`, "A question form"): each row is a
//! list of text segments, and one mark per styled segment follows the real
//! byte positions.

use ratatui::style::{Modifier, Style};

use super::form::Spot;
use super::{Mark, Panel, PanelSpot, Title};
use crate::format::{cut, width as cells};
use crate::theme::Role;

/// "Question" in the title: bold in secondary.
const HEAD: Style = Style::new()
    .fg(Role::Secondary.color())
    .add_modifier(Modifier::BOLD);

/// The title's session and question count: in secondary.
const TITLE: Style = Style::new().fg(Role::Secondary.color());

/// The title's place among the requests waiting: bold in secondary.
const PLACE: Style = Style::new()
    .fg(Role::Secondary.color())
    .add_modifier(Modifier::BOLD);

/// The title's right-aligned hint: dim.
const HINT: Style = Style::new().add_modifier(Modifier::DIM);

/// A panel under construction: its lines, their styled runs, its click
/// targets, its cursor line and its text cursor.
pub(super) struct Builder {
    lines: Vec<String>,
    marks: Vec<Mark>,
    spots: Vec<PanelSpot>,
    cursor: Option<usize>,
    caret: Option<(usize, u16)>,
}

impl Builder {
    /// An empty panel.
    pub(super) fn new() -> Builder {
        Builder {
            lines: Vec::new(),
            marks: Vec::new(),
            spots: Vec::new(),
            cursor: None,
            caret: None,
        }
    }

    /// Pushes one row of `parts`: each segment's control characters read
    /// as spaces, concatenated into the line, with one mark per non-empty
    /// segment whose style is not default, at the segment's byte range.
    /// Returns the row's line index.
    pub(super) fn row(&mut self, parts: &[(&str, Style)]) -> usize {
        let segments: Vec<(String, Style)> = parts
            .iter()
            .map(|(text, style)| (crate::text::controls_to_space(text), *style))
            .collect();
        self.emit(&segments)
    }

    /// Pushes a blank row. Returns its line index.
    pub(super) fn blank(&mut self) -> usize {
        self.lines.push(String::new());
        self.lines.len().saturating_sub(1)
    }

    /// Pushes `segments` as one row clipped to `cols` display cells:
    /// the marks of a clipped tail end at the drawn line, so no mark
    /// reaches past it. Returns the row's line index.
    pub(super) fn clipped_row(&mut self, segments: &[(&str, Style)], cols: usize) -> usize {
        let cleaned: Vec<(String, Style)> = segments
            .iter()
            .map(|(text, style)| (crate::text::controls_to_space(text), *style))
            .collect();
        let full: String = cleaned.iter().map(|(text, _)| text.as_str()).collect();
        let drawn = cut(&full, cols);
        let mut rest = drawn.len();
        let mut kept: Vec<(&str, Style)> = Vec::with_capacity(cleaned.len());
        for (text, style) in &cleaned {
            if rest == 0 {
                break;
            }
            let take = text.len().min(rest);
            kept.push((&text[..take], *style));
            rest = rest.saturating_sub(take);
        }
        self.row(&kept)
    }

    /// Pushes one row of `left` and right-aligned `right`: at least one
    /// space separates them, so past the width the right part follows one
    /// space after the left. Returns the row's line index.
    pub(super) fn right_aligned(
        &mut self,
        left: &[(&str, Style)],
        right: &[(&str, Style)],
        width: u16,
    ) -> usize {
        let cleaned = |parts: &[(&str, Style)]| {
            parts
                .iter()
                .map(|(text, style)| (crate::text::controls_to_space(text), *style))
                .collect::<Vec<_>>()
        };
        let left = cleaned(left);
        let right = cleaned(right);
        let cells = |segments: &[(String, Style)]| {
            segments
                .iter()
                .map(|(text, _)| cells(text))
                .fold(0usize, |sum, cells| sum.saturating_add(cells))
        };
        let pad = usize::from(width)
            .saturating_sub(cells(&left).saturating_add(cells(&right)))
            .max(1);
        let mut segments = left;
        segments.push((" ".repeat(pad), Style::default()));
        segments.extend(right);
        self.emit(&segments)
    }

    /// The title row for `title` at `width` columns: "Question" and the
    /// session, the question count on a form, the place among the requests
    /// waiting only when more than one waits, and the hint right-aligned.
    /// Returns the row's line index.
    pub(super) fn title(&mut self, title: &Title, width: u16) -> usize {
        let session = format!(" from {}", title.session);
        let count = title
            .questions
            .map(|questions| format!(" · {questions} questions, any can be skipped"));
        let place = title.place.map(|(k, n)| format!(" · {k} of {n}"));
        let mut left: Vec<(&str, Style)> = vec![("Question", HEAD), (session.as_str(), TITLE)];
        if let Some(count) = count.as_ref() {
            left.push((count.as_str(), TITLE));
        }
        if let Some(place) = place.as_ref() {
            left.push((place.as_str(), PLACE));
        }
        self.right_aligned(&left, &[("Esc: chat about this", HINT)], width)
    }

    /// A click target over the whole `line`.
    pub(super) fn spot(&mut self, line: usize, spot: Spot) {
        self.spots.push(PanelSpot {
            line,
            cols: None,
            spot,
        });
    }

    /// A click target over `cols` display cells of `line`.
    pub(super) fn spot_cols(&mut self, line: usize, cols: (u16, u16), spot: Spot) {
        self.spots.push(PanelSpot {
            line,
            cols: Some(cols),
            spot,
        });
    }

    /// Puts the panel's cursor on `line`.
    pub(super) fn here(&mut self, line: usize) {
        self.cursor = Some(line);
    }

    /// Puts the text cursor at column `col` of `line`.
    pub(super) fn caret(&mut self, line: usize, col: u16) {
        self.caret = Some((line, col));
    }

    /// The panel built. A question is never the alert tint.
    pub(super) fn finish(self) -> Panel {
        Panel {
            lines: self.lines,
            marks: self.marks,
            alert: false,
            spots: self.spots,
            cursor: self.cursor,
            caret: self.caret,
        }
    }

    /// Pushes `segments` as one row with a mark per styled segment.
    fn emit(&mut self, segments: &[(String, Style)]) -> usize {
        let line = self.lines.len();
        let mut text = String::new();
        for (part, style) in segments {
            let start = text.len();
            text.push_str(part);
            if !part.is_empty() && *style != Style::default() {
                self.marks.push(Mark {
                    line,
                    bytes: start..text.len(),
                    style: *style,
                });
            }
        }
        self.lines.push(text);
        line
    }
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
