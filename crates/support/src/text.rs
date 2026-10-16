//! Text on one line: control characters as spaces.

/// `text` with a `\r\n` pair as one space and every other control
/// character as one space, so a value stays on its lines and no escape
/// sequence reaches the terminal.
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            out.push(' ');
        } else if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
