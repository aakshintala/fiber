//! The question panel's stateless text rows: cleaning model text, counting
//! cells, moving a character cursor, and the words row shared by forms and
//! one-question forms (`docs/tui.md`, "A question form").

use crate::format::{cut, width as measure};

/// The words row at `cols` columns and, when the cursor is on it (`here`),
/// the text cursor's column. The row never wraps: it shows the words from
/// their first character, clipped at the width, unless the text cursor
/// would reach the width, when it shows them from the first character that
/// puts the text cursor on the last column.
pub(super) fn words_row(words: &str, caret: usize, here: bool, cols: u16) -> (String, Option<u16>) {
    let prefix = format!("{} ✎ ", if here { '›' } else { ' ' });
    if words.is_empty() {
        return (
            format!("{prefix}answer in words"),
            here.then(|| cells(&prefix)),
        );
    }
    let words: Vec<char> = clean(words).chars().collect();
    let before =
        |start: usize| -> String { words.get(start..caret).unwrap_or_default().iter().collect() };
    let fits = |start: usize| measure(&prefix) + measure(&before(start)) < usize::from(cols);
    let start = if here {
        (0..caret).find(|start| fits(*start)).unwrap_or(caret)
    } else {
        0
    };
    let shown: String = words.get(start..).unwrap_or_default().iter().collect();
    let line = cut(&format!("{prefix}{shown}"), usize::from(cols));
    let caret = here.then(|| cells(&prefix).saturating_add(cells(&before(start))));
    (line, caret)
}

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

/// `text` with every control character as a space, so model text stays on
/// its line and sends the terminal nothing.
pub(super) fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
