//! A steering message where it landed in the card, and when
//! (`docs/tui.md`, "Turns").

use jiff::tz::TimeZone;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::format::wrap_joined;
use crate::local_time::time_of_day;
use crate::markdown::style;
use crate::rows::{RowText, Rows};
use crate::theme::Role;

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

    /// A steering message with its image parts: each draws as its one
    /// clickable line under the text (`docs/tui.md`, "Images").
    pub(super) fn with_images(text: String, images: Vec<crate::image::Part>, ts: u64) -> Self {
        let mut steered = Self::new(text, ts);
        steered.images = images;
        steered
    }

    /// A labelled rule (`steer · HH:MM`) over the message's bold text: the
    /// rule fills the width with dashes past one space, and the text wraps
    /// with its joins. A blank message draws the rule alone
    /// (`docs/tui.md`, "Turns"). Each image draws as its one clickable
    /// line under the text (`docs/tui.md`, "Images").
    pub(super) fn rows(
        &self,
        width: u16,
        zone: &TimeZone,
        layout: &crate::image::Layout,
        out: &mut Rows,
    ) {
        let label = match time_of_day(self.ts, zone) {
            Some(time) => format!("steer · {time}"),
            None => "steer".to_owned(),
        };
        let columns = usize::from(width);
        let dashes = columns.saturating_sub(crate::format::width(&label).saturating_add(1));
        let line = if dashes > 0 {
            Line::from(vec![
                Span::styled(label, Style::default().add_modifier(Modifier::DIM)),
                Span::styled(format!(" {}", "─".repeat(dashes)), style(Role::Muted)),
            ])
        } else {
            Line::from(Span::styled(
                label,
                Style::default().add_modifier(Modifier::DIM),
            ))
        };
        let tail = if dashes > 0 {
            dashes.saturating_add(1)
        } else {
            0
        };
        out.push_text(
            (line, None),
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
