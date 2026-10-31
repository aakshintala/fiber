//! The question panel's stateless text rows: counting cells, moving a
//! character cursor, the one shared single-line editor for every text row
//! in an overlay, and the words row shared by forms and one-question
//! forms (`docs/tui.md`, "A question form").
//!
//! `input.rs`'s `Draft` does not fit these rows: it is a multi-line
//! piece list with paste tokens, images, a selection and serials, while
//! these rows hold one `String` with a character caret and draw it
//! through `clipped_row`. The editor below is the smallest thing that
//! gives all three rows the same word and line keys.

use crate::format::width as measure;
use crate::keys::Edit;

/// The byte where the character `chars` characters into `text` starts, or
/// the end.
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| at)
}

/// Inserts `ch` at the character cursor and moves it past the new character.
pub(super) fn insert_at(text: &mut String, caret: &mut usize, ch: char) {
    text.insert(byte_at(text, *caret), ch);
    *caret = (*caret).saturating_add(1);
}

/// Deletes the character before the cursor, if any, and moves the cursor left.
pub(super) fn delete_before(text: &mut String, caret: &mut usize) {
    if let Some(before) = caret.checked_sub(1) {
        let at = byte_at(text, before);
        text.remove(at);
        *caret = before;
    }
}

/// A word is a run of alphanumeric characters or `_`, as in the draft's
/// pieces: anything else is a gap the cursor steps over.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Applies one editing key to a single-line row: arrows and word moves,
/// word and line-start deletes, line start and end. `ShiftEnter`, `CtrlJ`,
/// forward `Delete` and `Paste` do nothing here (a paste is typed by the
/// caller) and return `false`.
pub(super) fn edit_line(text: &mut String, caret: &mut usize, edit: &Edit) -> bool {
    match edit {
        Edit::Left => {
            move_caret(text, caret, false);
            true
        }
        Edit::Right => {
            move_caret(text, caret, true);
            true
        }
        Edit::WordLeft => {
            word_left(text, caret);
            true
        }
        Edit::WordRight => {
            word_right(text, caret);
            true
        }
        Edit::DeleteWord => {
            delete_word(text, caret);
            true
        }
        Edit::DeleteToLineStart => {
            delete_to_line_start(text, caret);
            true
        }
        Edit::LineStart => {
            line_start(caret);
            true
        }
        Edit::LineEnd => {
            line_end(text, caret);
            true
        }
        Edit::ShiftEnter | Edit::CtrlJ | Edit::Delete | Edit::Paste(_) => false,
    }
}

/// Moves the character cursor to the start of the previous word: back
/// over gaps, then over one run of word characters.
pub(super) fn word_left(text: &str, caret: &mut usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut at = (*caret).min(chars.len());
    while at > 0
        && !chars
            .get(at.saturating_sub(1))
            .is_some_and(|ch| is_word_char(*ch))
    {
        at = at.saturating_sub(1);
    }
    while at > 0
        && chars
            .get(at.saturating_sub(1))
            .is_some_and(|ch| is_word_char(*ch))
    {
        at = at.saturating_sub(1);
    }
    *caret = at;
}

/// Moves the character cursor to the end of the next word: forward over
/// gaps, then over one run of word characters.
pub(super) fn word_right(text: &str, caret: &mut usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut at = (*caret).min(chars.len());
    while at < chars.len() && !chars.get(at).is_some_and(|ch| is_word_char(*ch)) {
        at = at.saturating_add(1);
    }
    while at < chars.len() && chars.get(at).is_some_and(|ch| is_word_char(*ch)) {
        at = at.saturating_add(1);
    }
    *caret = at;
}

/// Deletes back to the start of the previous word. A single-line row has
/// no line breaks, so its line starts at 0, which `word_left` never
/// passes.
pub(super) fn delete_word(text: &mut String, caret: &mut usize) {
    let mut target = *caret;
    word_left(text, &mut target);
    let start = byte_at(text, target);
    let end = byte_at(text, *caret);
    text.drain(start..end);
    *caret = target;
}

/// Deletes from the row's start to the cursor; at the start, nothing.
pub(super) fn delete_to_line_start(text: &mut String, caret: &mut usize) {
    let end = byte_at(text, *caret);
    text.drain(..end);
    *caret = 0;
}

/// Moves the character cursor to the row's start.
pub(super) fn line_start(caret: &mut usize) {
    *caret = 0;
}

/// Moves the character cursor to the row's end.
pub(super) fn line_end(text: &str, caret: &mut usize) {
    *caret = text.chars().count();
}

/// Moves the character cursor one position, clamped to the text's ends.
pub(super) fn move_caret(text: &str, caret: &mut usize, right: bool) {
    *caret = if right {
        (*caret).saturating_add(1).min(text.chars().count())
    } else {
        (*caret).saturating_sub(1)
    };
}

/// How many display cells `text` takes, as a screen column.
pub(super) fn cells(text: &str) -> u16 {
    u16::try_from(measure(text)).unwrap_or(u16::MAX)
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
