//! The text helpers: what a terminal does with a control character, and
//! how wide and how long text is on screen. Text that reaches the screen or
//! the terminal passes through one of these, so a new entry point has one
//! place to sanitise.

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

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
