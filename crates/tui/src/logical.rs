//! A page's rows joined back into the lines they wrap (`docs/tui.md`,
//! "Selection and copy": a selection copies text unwrapped; "History and
//! paging": every row has a stable index). Each row's [`RowText`] says how
//! it joins the row before and which of its cells are decoration; rows are
//! never joined blindly.

use ratatui::text::Line;

use crate::rows::{Join, RowText};
use crate::turn::Row;

/// One logical line: its text, and for each of its chars the row it came
/// from, by index in the page's rows, and its byte offset in that row's
/// `line.to_string()`; `None` for the space a soft wrap dropped. `row` is
/// the row it starts on, so a blank line, which has no char, has a place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Logical {
    pub(crate) text: String,
    pub(crate) from: Vec<Option<(usize, usize)>>,
    pub(crate) row: usize,
}

/// The logical lines `rows` draw, each row's text joined to the line
/// before as its `texts` entry says. A decoration row adds nothing.
pub(crate) fn logical(rows: &[Row], texts: &[RowText]) -> Vec<Logical> {
    let mut out: Vec<Logical> = Vec::new();
    for (at, ((line, _), text)) in rows.iter().zip(texts).enumerate() {
        if text.decoration {
            continue;
        }
        let (start, body) = line_text(line, text);
        let current = match (text.join, out.last_mut()) {
            (Join::Wrap | Join::WrapSpace, Some(current)) => {
                if text.join == Join::WrapSpace && !current.text.is_empty() && !body.is_empty() {
                    current.text.push(' ');
                    current.from.push(None);
                }
                current
            }
            (Join::Break, _) | (Join::Wrap | Join::WrapSpace, None) => {
                out.push(Logical {
                    row: at,
                    ..Logical::default()
                });
                let Some(current) = out.last_mut() else {
                    continue;
                };
                current
            }
        };
        for (offset, ch) in body.char_indices() {
            current.text.push(ch);
            current.from.push(Some((at, start.saturating_add(offset))));
        }
    }
    out
}

/// The row's text after its first `text.skip` cells, trailing whitespace
/// trimmed, and the byte in `line.to_string()` it starts at.
pub(crate) fn line_text(line: &Line<'_>, text: &RowText) -> (usize, String) {
    let whole = line.to_string();
    let mut used = 0u16;
    let mut start = whole.len();
    for (at, ch) in whole.char_indices() {
        if used >= text.skip {
            start = at;
            break;
        }
        let mut buf = [0u8; 4];
        let cells = crate::format::width(ch.encode_utf8(&mut buf));
        used = used.saturating_add(u16::try_from(cells).unwrap_or(u16::MAX));
    }
    let body = whole.get(start..).unwrap_or_default().trim_end().to_owned();
    (start, body)
}

#[cfg(test)]
#[path = "logical_tests.rs"]
mod tests;
