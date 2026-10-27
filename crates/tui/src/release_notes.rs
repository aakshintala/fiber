//! The `/release-notes` view: the running binary's embedded changelog
//! section for its own version, wrapped into physical rows
//! (`docs/tui.md`, "Swapped views"). Pure: the caller passes the
//! embedded source and the launch version, so no test reads a file and
//! nothing here touches the network.

use crate::swapped::{Frame, Ink, List, Spot, rows_height};
use crate::theme::Role;

/// The bullet's marker: accent, as reply bullets draw
/// (`docs/tui.md`, "Markdown in replies").
const BULLET: &str = "• ";
/// The hanging indent wrapped bullet lines keep: two columns.
const HANG: &str = "  ";
/// A source line's bullet marker: only a dash and a space starts one.
const SOURCE_BULLET: &str = "- ";

/// The changelog section headed by exactly `## <version>`: the lines
/// after that heading through the next raw `## ` heading or the end of
/// the file. The heading comparison is raw, with no trim: trailing
/// whitespace does not match. `None` when no raw exact heading names
/// the version.
pub(crate) fn section<'a>(changelog: &'a str, version: &str) -> Option<&'a str> {
    let heading = format!("## {version}");
    let mut start = None;
    let mut end = changelog.len();
    let mut offset = 0;
    for line in changelog.split_inclusive('\n') {
        let text = line.strip_suffix('\n').unwrap_or(line);
        if start.is_none() {
            if text == heading {
                start = Some(offset + line.len());
            }
        } else if text.starts_with("## ") {
            end = offset;
            break;
        }
        offset += line.len();
    }
    start.map(|from| &changelog[from..end])
}

/// The notes frame at `width` by `height`: the section's rows, or the
/// no-notes message with the releases page when the version has no
/// section. Long lines wrap at the width instead of clipping: the
/// shared renderer cuts each physical row, so a long logical line
/// passed through would lose its tail. The copied list is clamped to
/// the wrapped rows, so a widened redraw cannot strand it past its
/// last row.
pub(crate) fn frame(
    changelog: &str,
    version: &str,
    list: List,
    width: u16,
    height: usize,
) -> Frame {
    let width = usize::from(width).max(1);
    let mut rows = Vec::new();
    match section(changelog, version) {
        Some(body) => {
            for line in body.split('\n') {
                push_line(&mut rows, line, width);
            }
        }
        None => {
            push_line(&mut rows, "No notes for this version.", width);
            push_line(&mut rows, contract::releases::RELEASES, width);
        }
    }
    let mut frame = Frame {
        title: "Release notes".to_owned(),
        command: "/release-notes".to_owned(),
        rows,
        list,
        below: Vec::new(),
        field: None,
        footer: "↑↓ scroll · PgUp/PgDn page · Esc close".to_owned(),
    };
    let visible = rows_height(&frame, height);
    let last = frame.rows.len();
    frame.list.select(frame.list.selected(), last, visible);
    frame
}

/// Wraps one logical `line` into physical rows: a dash-space line draws
/// its bullet marker in accent with its wrapped lines hanging two
/// columns in (`docs/tui.md`, "Markdown in replies"); any other line
/// wraps at the width, blank lines staying blank.
fn push_line(rows: &mut Vec<Vec<(String, Option<Spot>, Ink)>>, line: &str, width: usize) {
    if line.is_empty() {
        rows.push(vec![(String::new(), None, Ink::Plain)]);
        return;
    }
    if let Some(body) = line.strip_prefix(SOURCE_BULLET) {
        // The marker's display width, not its bytes: `•` takes one cell.
        let body_width = width.saturating_sub(crate::format::width(BULLET)).max(1);
        let mut wrapped = crate::format::wrap(body, body_width).into_iter();
        match wrapped.next() {
            Some(first) => {
                rows.push(vec![
                    (BULLET.to_owned(), None, Ink::Role(Role::Accent)),
                    (first, None, Ink::Plain),
                ]);
                for rest in wrapped {
                    rows.push(vec![
                        (HANG.to_owned(), None, Ink::Plain),
                        (rest, None, Ink::Plain),
                    ]);
                }
            }
            None => {
                rows.push(vec![(BULLET.to_owned(), None, Ink::Role(Role::Accent))]);
            }
        }
        return;
    }
    for row in crate::format::wrap(line, width) {
        rows.push(vec![(row, None, Ink::Plain)]);
    }
}

#[cfg(test)]
#[path = "release_notes_tests.rs"]
mod tests;
