//! The question panel's stateless text rows: counting cells, moving a
//! character cursor, and the words row shared by forms and one-question
//! forms (`docs/tui.md`, "A question form").

use crate::format::width as measure;

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
