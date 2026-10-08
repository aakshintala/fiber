//! Bare URLs in conversation rows (`docs/tui.md`, "Links"): `http://`
//! and `https://` URLs drawn as plain text are links too.

use std::ops::Range;

/// Trailing punctuation no URL keeps.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '\'', '"'];

/// The bare `http://` and `https://` URLs `text` holds, as byte ranges:
/// each starts at its scheme and runs to the next whitespace or control,
/// with trailing punctuation and an unbalanced `)` or `]` cut off
/// (`docs/tui.md`, "Links").
pub(crate) fn urls(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while at < text.len() {
        let rest = &text[at..];
        let lower = rest.to_ascii_lowercase();
        let prefix = if lower.starts_with("https://") {
            "https://".len()
        } else if lower.starts_with("http://") {
            "http://".len()
        } else {
            // One char forward; a multibyte char is never a scheme start.
            let next = rest.chars().next().map_or(1, |ch| ch.len_utf8());
            at = at.saturating_add(next);
            continue;
        };
        let mut end = at.saturating_add(prefix);
        while let Some(ch) = text.get(end..).and_then(|rest| rest.chars().next()) {
            if ch.is_whitespace() || ch.is_control() {
                break;
            }
            end = end.saturating_add(ch.len_utf8());
        }
        let mut range = at..end;
        trim(&mut range, text);
        if range.end.saturating_sub(range.start) > prefix {
            out.push(range.clone());
            at = range.end;
        } else {
            // `http://` alone is no link; move past its scheme.
            at = at.saturating_add(prefix);
        }
        let _ = bytes;
    }
    out
}

/// Cuts trailing punctuation and an unbalanced `)` or `]` off `range`.
fn trim(range: &mut Range<usize>, text: &str) {
    loop {
        let Some(tail) = text.get(range.clone()).and_then(|url| url.chars().next_back()) else {
            return;
        };
        if TRAILING.contains(&tail) {
            range.end = range.end.saturating_sub(tail.len_utf8());
            continue;
        }
        if tail == ')' || tail == ']' {
            let open = if tail == ')' { '(' } else { '[' };
            let url = text.get(range.clone()).unwrap_or_default();
            let opens = url.chars().filter(|ch| *ch == open).count();
            let closes = url.chars().filter(|ch| *ch == tail).count();
            if closes > opens {
                range.end = range.end.saturating_sub(1);
                continue;
            }
        }
        return;
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
