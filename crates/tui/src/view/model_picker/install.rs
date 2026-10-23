//! The picker's not-installed rows, the error under a failed one, and the
//! lines a landed install leaves under the list (`docs/tui.md`, "Swapped
//! views"). Every text here came from an install and is cut to
//! [`TEXT_MAX`] columns, so no row outgrows the picker.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::overlay::Row;
use super::{gutter, id_spans};
use crate::format::width as cells;
use crate::model_picker::{InstallRow, Outcome, Progress, credential_text};
use crate::mouse::TargetId;
use crate::swapped::Spot;
use crate::text::{cut, to_u16};
use crate::theme::Role;

/// The most columns of install text a row draws: with the four columns
/// of indent and "✗ " it stays inside the picker's 96-column content.
pub(super) const TEXT_MAX: usize = 80;

/// The indent of the error and outcome rows, under the name.
const INDENT: &str = "    ";

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// One not-installed row: the gutter, the provider's name in `accent`
/// with each query hit bold and underlined, then " · not installed" dim,
/// or " ⟳ installing" and the spinner in `attention` while its install
/// runs. The name takes clicks as Enter does; the row takes clicks
/// anywhere, focusing it. The focused row sits on the bar.
pub(super) fn install_row(row: &InstallRow, focused: bool, spinner: &str) -> Row {
    let mut spans = vec![gutter(focused)];
    spans.extend(id_spans(&row.provider, &row.hits));
    spans.push(match &row.progress {
        Some(Progress::Installing) => Span::styled(
            format!(" ⟳ installing {spinner}"),
            Style::new().fg(Role::Attention.color()),
        ),
        Some(Progress::Failed(_)) | None => Span::styled(" · not installed".to_owned(), dim()),
    });
    Row {
        spans,
        right: Vec::new(),
        targets: vec![
            (0, u16::MAX, TargetId::View(Spot::Row(row.at))),
            (
                2,
                to_u16(2 + cells(&row.provider)),
                TargetId::View(Spot::Cell(row.at, 0)),
            ),
        ],
        barred: focused,
    }
}

/// The failed install's error under its row, indented four columns: "✗ "
/// and the first line in `error`. A row that did not fail has none.
pub(super) fn error_row(row: &InstallRow) -> Option<Row> {
    let Some(Progress::Failed(message)) = &row.progress else {
        return None;
    };
    Some(Row {
        spans: vec![
            Span::raw(INDENT.to_owned()),
            Span::styled(
                format!("✗ {}", cut(message, TEXT_MAX, "…")),
                Style::new().fg(Role::Error.color()),
            ),
        ],
        right: Vec::new(),
        targets: vec![(0, u16::MAX, TargetId::View(Spot::Row(row.at)))],
        barred: false,
    })
}

/// The rows a landed install leaves under the list, indented four
/// columns: "✓ " in `success` and "installed <name>", each summary line
/// dim, and for each provider needing a key its dim line pointing at
/// `fiber login`, or, for a key that could not be read, "✗ " and the
/// error in `error`.
pub(super) fn outcome_rows(outcomes: &[Outcome]) -> Vec<Row> {
    let mut rows = Vec::new();
    for outcome in outcomes {
        rows.push(Row {
            spans: vec![
                Span::raw(INDENT.to_owned()),
                Span::styled("✓ ".to_owned(), Style::new().fg(Role::Success.color())),
                Span::raw(cut(
                    &format!("installed {}", outcome.provider),
                    TEXT_MAX,
                    "…",
                )),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        });
        for line in &outcome.summary {
            rows.push(plain(cut(line, TEXT_MAX, "…")));
        }
        for need in &outcome.login {
            let text = cut(&credential_text(need), TEXT_MAX, "…");
            rows.push(match &need.error {
                None => plain(text),
                Some(_) => Row {
                    spans: vec![
                        Span::raw(INDENT.to_owned()),
                        Span::styled(format!("✗ {text}"), Style::new().fg(Role::Error.color())),
                    ],
                    right: Vec::new(),
                    targets: Vec::new(),
                    barred: false,
                },
            });
        }
    }
    rows
}

/// One dim outcome row with no bar and no targets.
fn plain(text: String) -> Row {
    Row {
        spans: vec![Span::raw(INDENT.to_owned()), Span::styled(text, dim())],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
