//! A steering message where it landed in the card, and when
//! (`docs/tui.md`, "Turns").

use jiff::tz::TimeZone;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ext::Input;
use crate::format::wrap_joined;
use crate::local_time::time_of_day;
use crate::rows::{RowText, Rows};
use crate::theme::{Role, style};

/// A steering message where it landed in the card, and when
/// (`docs/tui.md`, "Turns").
#[derive(Debug, Clone)]
pub(crate) struct Steered {
    text: String,
    images: Vec<crate::image::Part>,
    ts: u64,
}

impl Steered {
    pub(super) fn new(text: String, ts: u64) -> Self {
        Self {
            text,
            images: Vec::new(),
            ts,
        }
    }

    /// Its card entry: the text with its images, and when it landed.
    pub(crate) fn turn_entry(&self) -> crate::ext::TurnEntry<'_> {
        crate::ext::TurnEntry::Steer {
            text: &self.text,
            images: self
                .images
                .iter()
                .map(|part| crate::ext::PromptImage {
                    path: &part.path,
                    width: part.width,
                    height: part.height,
                })
                .collect(),
            ts: self.ts,
        }
    }

    /// A steering message with its image parts: each draws as its one
    /// clickable line under the text (`docs/tui.md`, "Images").
    pub(super) fn with_images(text: String, images: Vec<crate::image::Part>, ts: u64) -> Self {
        let mut steered = Self::new(text, ts);
        steered.images = images;
        steered
    }

    /// A labelled rule (`steer · HH:MM`) over the message's bold text:
    /// "steer" in the attention tint and bold, the time dim, and the rule
    /// in the rule tint filling the width past one space. A blank message
    /// draws the rule alone (`docs/tui.md`, "Turns"). Each image draws
    /// as its one clickable line under the text (`docs/tui.md`, "Images").
    /// A bound `steer` renderer replaces the rule, the text and the image
    /// lines: the image targets sit on the image lines they open, so no
    /// replacement line carries one.
    pub(super) fn rows(
        &self,
        width: u16,
        zone: &TimeZone,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        let flat = out.seam.as_ref().is_some_and(|seam| seam.flat);
        let input = crate::ext::Steer {
            text: &self.text,
            images: self
                .images
                .iter()
                .map(|part| crate::ext::PromptImage {
                    path: &part.path,
                    width: part.width,
                    height: part.height,
                })
                .collect(),
            ts: self.ts,
            flat,
        };
        let wrapped = Input::Steer(input);
        let builtin = |input: &Input<'_>, w: u16| {
            if !matches!(input, Input::Steer(_)) {
                return Vec::new();
            }
            let mut scratch = Rows::default();
            self.builtin_rows(w, zone, layout, &mut scratch);
            let (rows, _) = scratch.into_parts();
            rows.into_iter()
                .map(|(line, _)| crate::ext::spans::from_ratatui(&line.spans))
                .collect()
        };
        let rendered = match out.fills_for("steer") {
            crate::ext::SlotSelection::Draw { extension } => {
                out.ext_lines("steer", &extension, &wrapped, width, &builtin)
            }
            crate::ext::SlotSelection::Absent | crate::ext::SlotSelection::Suppressed => None,
        };
        match rendered {
            Some(fitted) => {
                for line in fitted.iter() {
                    out.push_fitted(line, None);
                }
            }
            None => self.builtin_rows(width, zone, layout, out),
        }
    }

    /// The rule over the message's bold text with each image's line
    /// under it, as above.
    fn builtin_rows(
        &self,
        width: u16,
        zone: &TimeZone,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        let middle = match time_of_day(self.ts, zone) {
            Some(time) => format!(" · {time} "),
            None => " ".to_owned(),
        };
        let columns = usize::from(width);
        let dashes = columns.saturating_sub(5 + crate::format::width(&middle));
        let mut spans = vec![
            Span::styled("steer", style(Role::Attention).add_modifier(Modifier::BOLD)),
            Span::styled(middle, Style::default().add_modifier(Modifier::DIM)),
        ];
        if dashes > 0 {
            spans.push(Span::styled("─".repeat(dashes), style(Role::Rule)));
        }
        let tail = if dashes > 0 {
            dashes.saturating_add(1)
        } else {
            0
        };
        out.push_text(
            (Line::from(spans), None),
            RowText {
                tail: u16::try_from(tail).unwrap_or(u16::MAX),
                ..RowText::plain()
            },
        );
        if !self.text.trim().is_empty() {
            for (row, join) in wrap_joined(&self.text, columns) {
                out.push_text(
                    (
                        Line::from(Span::styled(
                            row,
                            Style::default().add_modifier(Modifier::BOLD),
                        )),
                        None,
                    ),
                    RowText {
                        join,
                        ..RowText::plain()
                    },
                );
            }
        }
        super::images::steer_rows(&self.images, width, layout, out);
    }
}

#[cfg(test)]
#[path = "steer_tests.rs"]
mod tests;
