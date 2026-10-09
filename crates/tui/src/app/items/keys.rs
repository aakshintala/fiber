//! Typing into a running `tty` job's view: every key but Esc and Ctrl+C is
//! sent as `job_input` to the attached session, which owns the job
//! (`docs/tui.md`, "Swapped views": the input box types into the job as raw
//! keys). A `tty` job is the one whose record holds a screen grid
//! ([`Output::tty`](crate::tty_screen::Output)).

use crate::keys::{Edit, Key};
use crate::tty_screen::Output;

use super::JobRecord;

/// The bytes `key` types into the job: the key as typed, escape sequences
/// included. `None` for Esc, which closes the view, for Ctrl+C, which keeps
/// the app's quit gesture, and for keys the [`Key`] enum cannot name, which
/// are not sent. The keys that arrive as [`Edit`] go through
/// [`encode_edit`].
pub(super) fn encode(key: &Key) -> Option<String> {
    let text = match key {
        Key::Char(ch) => ch.to_string(),
        Key::Enter => "\r".to_owned(),
        Key::Backspace => "\x7f".to_owned(),
        Key::Tab => "\t".to_owned(),
        Key::Up => "\x1b[A".to_owned(),
        Key::Down => "\x1b[B".to_owned(),
        Key::End => "\x1b[F".to_owned(),
        Key::PageUp => "\x1b[5~".to_owned(),
        Key::PageDown => "\x1b[6~".to_owned(),
        Key::CtrlO => "\x0f".to_owned(),
        Key::CtrlG => "\x07".to_owned(),
        Key::CtrlR => "\x12".to_owned(),
        Key::CtrlF => "\x06".to_owned(),
        Key::CtrlV => "\x16".to_owned(),
        Key::CtrlL => "\x0c".to_owned(),
        Key::Esc
        | Key::CtrlC
        | Key::BackTab
        | Key::F1
        | Key::AltA
        | Key::AltUp
        | Key::AltDown
        | Key::AltX
        | Key::AltP
        | Key::AltR
        | Key::AltDigit(_) => return None,
    };
    Some(text)
}

/// The bytes `edit` types into the job: the edit as typed, escape
/// sequences included. Every variant sends; a pasted line break goes as a
/// carriage return, as Enter does.
pub(super) fn encode_edit(edit: &Edit) -> Option<String> {
    let text = match edit {
        Edit::Left => "\x1b[D".to_owned(),
        Edit::Right => "\x1b[C".to_owned(),
        Edit::WordLeft => "\x1b[1;5D".to_owned(),
        Edit::WordRight => "\x1b[1;5C".to_owned(),
        Edit::LineStart => "\x1b[H".to_owned(),
        Edit::LineEnd => "\x1b[F".to_owned(),
        Edit::Delete => "\x1b[3~".to_owned(),
        Edit::ShiftEnter => "\r".to_owned(),
        Edit::CtrlJ => "\n".to_owned(),
        Edit::DeleteWord => "\x1b\x7f".to_owned(),
        Edit::Paste(text) => text.replace('\n', "\r"),
    };
    Some(text)
}

/// Whether `record` is a `tty` job: the one whose output is a screen grid.
pub(super) fn is_tty(record: &JobRecord) -> bool {
    matches!(record.output, Some(Output::Grid(..)))
}

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
