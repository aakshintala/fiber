//! The text helpers: what a terminal does with a control character, and
//! how wide and how long text is on screen. Text that reaches the screen or
//! the terminal passes through one of these, so a new entry point has one
//! place to sanitise.

use ratatui::style::Style;
use ratatui::text::Span;

/// Whether the terminal draws `ch` rather than acting on it.
pub(crate) fn printable(ch: char) -> bool {
    !ch.is_control()
}

/// `text` without a control character (C0, C1 or DEL), so it can end no
/// escape sequence and start none.
pub(crate) fn strip_controls(text: &str) -> String {
    text.chars().filter(|&ch| printable(ch)).collect()
}

/// `text` with each control character as one space, so model text stays on
/// its line and sends the terminal nothing.
pub(crate) fn controls_to_space(text: &str) -> String {
    text.chars()
        .map(|ch| if printable(ch) { ch } else { ' ' })
        .collect()
}

/// Pasted text: `\r\n` and `\r` become `\n`, and every control character
/// but `\n` and `\t` goes. Cleaning clean text changes nothing.
pub(crate) fn paste_clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&ch| matches!(ch, '\n' | '\t') || printable(ch))
        .collect()
}

/// How many columns `text` takes on screen.
pub(crate) fn width(text: &str) -> usize {
    Span::raw(text).width()
}

/// How many columns `ch` takes on screen.
pub(crate) fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    width(ch.encode_utf8(&mut buf))
}

/// Each drawn piece of `text`, from left to right: every grapheme
/// whole, with the control characters `Span::styled_graphemes` skips kept
/// one by one, so they measure exactly as when the text is walked one
/// char at a time. Stops early when `emit` says so. `emit` also takes
/// each piece's byte offset in `text`, so a caller can keep ranges
/// without holding the borrow the piece itself carries.
pub(crate) fn each_piece(text: &str, mut emit: impl FnMut(&str, usize) -> bool) {
    let mut start: Option<usize> = None;
    for (at, ch) in text.char_indices() {
        if printable(ch) {
            if start.is_none() {
                start = Some(at);
            }
            continue;
        }
        if let Some(from) = start.take()
            && !emit_graphemes(&text[from..at], from, &mut emit)
        {
            return;
        }
        if !emit(&text[at..at + ch.len_utf8()], at) {
            return;
        }
    }
    if let Some(from) = start {
        emit_graphemes(&text[from..], from, &mut emit);
    }
}

/// Every grapheme of `run` to `emit`; false when `emit` stops the walk.
/// `run` holds no control character, so no grapheme is skipped. `base`
/// is `run`'s byte offset in the walked text.
fn emit_graphemes(run: &str, base: usize, emit: &mut impl FnMut(&str, usize) -> bool) -> bool {
    let mut at = base;
    for grapheme in Span::raw(run).styled_graphemes(Style::default()) {
        if !emit(grapheme.symbol, at) {
            return false;
        }
        at = at.saturating_add(grapheme.symbol.len());
    }
    true
}

/// How many columns one drawn piece takes, counted as width work:
/// every piece `cut` or `wrap_joined` sizes is measured once.
pub(crate) fn measured(piece: &str) -> usize {
    crate::work::add(|work| work.grapheme_widths += 1);
    width(piece)
}

/// `text` cut to at most `max` columns. With an `ellipsis`, text wider
/// than `max` ends in it, inside `max`, and is empty when `max` is
/// narrower than the ellipsis. A grapheme that does not fit is dropped
/// whole, never split.
pub(crate) fn cut(text: &str, max: usize, ellipsis: &str) -> String {
    if ellipsis.is_empty() {
        return cut_to(text, max);
    }
    if width(text) <= max {
        return text.to_owned();
    }
    let Some(room) = max.checked_sub(width(ellipsis)) else {
        return String::new();
    };
    let mut out = cut_to(text, room);
    out.push_str(ellipsis);
    out
}

/// The leading pieces of `text` that fit in `max` columns.
fn cut_to(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut taken = 0usize;
    // The width so far is carried, so each piece is measured once: the
    // kept pieces and at most the first rejected one, since the walk
    // stops there.
    each_piece(text, |piece, _| {
        let cell = measured(piece);
        if taken.saturating_add(cell) > max {
            return false;
        }
        out.push_str(piece);
        taken = taken.saturating_add(cell);
        true
    });
    out
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
