//! GNU grep's patterns in ripgrep's syntax (`docs/tools.md`, "Search", "Flags").
//!
//! Without `-E` or `-F` a pattern is a basic regular expression, translated
//! here before the search. With `-E` only the GNU extensions `\<`, `\>` and
//! `\1` to `\9` need a pass; with `-F` the searcher takes the pattern as a
//! fixed string itself. Corner behavior below was probed against the
//! runner's own grep before it was pinned in tests.

/// Translates a GNU basic regular expression to ripgrep's syntax, or nothing
/// when the system grep must run instead: a back-reference, a `*` after a
/// zero-width assertion, a non-ASCII or control escape, or a trailing
/// backslash.
pub(crate) fn translate_bre(pattern: &str) -> Option<String> {
    let mut chars = pattern.chars().peekable();
    let mut out = String::new();
    // Whether the pattern's start was just read: only there `^` anchors.
    let mut at_start = true;
    // Whether the next `(` opens a group: only right after `\(` or `\|`.
    let mut group_open = false;
    // What `*` means where the scan stands.
    let mut star = Star::Literal;
    while let Some(current) = chars.next() {
        let first = at_start;
        at_start = false;
        if current == '\\' {
            let Some(next) = chars.next() else {
                // A trailing backslash: the system reports it.
                return None;
            };
            match next {
                '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                    out.push(next);
                    group_open = next == '(' || next == '|';
                    star = if group_open {
                        Star::Literal
                    } else {
                        Star::Repeat
                    };
                }
                '<' => {
                    out.push_str(r"\b{start}");
                    group_open = false;
                    star = Star::Invalid;
                }
                '>' => {
                    out.push_str(r"\b{end}");
                    group_open = false;
                    star = Star::Invalid;
                }
                '`' => {
                    out.push_str(r"\A");
                    group_open = false;
                    star = Star::Invalid;
                }
                '\'' => {
                    out.push_str(r"\z");
                    group_open = false;
                    star = Star::Invalid;
                }
                '1'..='9' => return None,
                'w' | 'W' | 's' | 'S' => {
                    out.push('\\');
                    out.push(next);
                    group_open = false;
                    star = Star::Repeat;
                }
                // A quantified assertion is never what GNU means: the
                // runner's grep matches nothing for `\<*`, so the system
                // decides what follows one.
                'b' | 'B' => {
                    out.push('\\');
                    out.push(next);
                    group_open = false;
                    star = Star::Invalid;
                }
                _ => {
                    if next.is_ascii_alphanumeric() {
                        out.push(next);
                    } else if next.is_ascii_graphic() || next == ' ' {
                        out.push('\\');
                        out.push(next);
                    } else {
                        return None;
                    }
                    group_open = false;
                    star = Star::Repeat;
                }
            }
            continue;
        }
        if current == '[' {
            let Some(text) = translate_class(&mut chars) else {
                // Unbalanced: the system reports it.
                return None;
            };
            out.push_str(&text);
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        if current == '*' {
            match star {
                // Probed on the runner's grep: `^*` reads a literal
                // asterisk, so an anchor leaves the scan fresh.
                Star::Literal => out.push_str(r"\*"),
                Star::Repeat => out.push('*'),
                Star::Invalid => return None,
            }
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        if current == '^' {
            if first || group_open {
                out.push('^');
                star = Star::Literal;
            } else {
                out.push_str(r"\^");
                star = Star::Repeat;
            }
            group_open = false;
            continue;
        }
        if current == '$' {
            if dollar_anchor_ahead(&mut chars) {
                out.push('$');
                star = Star::Invalid;
            } else {
                out.push_str(r"\$");
                star = Star::Repeat;
            }
            group_open = false;
            continue;
        }
        if matches!(current, '(' | ')' | '{' | '}' | '|' | '+' | '?') {
            out.push('\\');
            out.push(current);
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        out.push(current);
        group_open = false;
        star = Star::Repeat;
    }
    Some(out)
}

/// What `*` means where the scan stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Star {
    /// At the start, or right after `\(`, `\|` or `^`: an ordinary `*`.
    Literal,
    /// Anywhere else: repetition.
    Repeat,
    /// Right after a zero-width assertion: the system decides.
    Invalid,
}

/// Whether the `$` just read anchors: at the end, or right before `\)`
/// or `\|`. What follows is only peeked at, never consumed.
fn dollar_anchor_ahead(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> bool {
    let mut rest = chars.clone();
    match rest.next() {
        None => true,
        Some('\\') => matches!(rest.next(), Some(')') | Some('|')),
        _ => false,
    }
}

/// Translates the bracket expression whose opening `[` was just read:
/// consumes through the closer from `chars`. Backslashes inside are
/// escaped, a leading `]` is kept, and `[:class:]` names pass through.
/// Nothing when no closer follows.
fn translate_class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut text = String::from("[");
    // Negation is `^` only: a leading `!` is an ordinary member, as the
    // runner's grep reads it.
    if chars.peek() == Some(&'^') {
        text.push('^');
        chars.next();
    }
    if chars.peek() == Some(&'!') {
        text.push_str("\\!");
        chars.next();
    }
    // A leading `]` is a member, not the closer.
    if chars.peek() == Some(&']') {
        text.push(']');
        chars.next();
    }
    loop {
        let next = chars.next()?;
        if next == '[' {
            // A character class such as `[:alpha:]` copies through; its
            // closer is not the bracket's.
            if let Some(chunk) = class_chunk(chars) {
                text.push('[');
                text.push_str(&chunk);
                continue;
            }
        }
        if next == ']' {
            text.push(']');
            return Some(text);
        }
        // Inside brackets a backslash is ordinary, so escaping it keeps it
        // one: `[a\]` reads `a` and `\`, as the runner's grep does.
        if next == '\\' {
            text.push_str("\\\\");
        } else {
            text.push(next);
        }
    }
}

/// The `:...:]` opening after a `[` inside a bracket expression, through
/// its closer: consumes nothing unless the closer follows, when the
/// consumed text is returned. A `[` with no `:]` after it stays an
/// ordinary member.
fn class_chunk(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    if chars.peek() != Some(&':') {
        return None;
    }
    // The probe finds the closer before anything is consumed, so a
    // broken step cannot spin: the drain below runs once per chunk char.
    let mut probe = chars.clone();
    let mut chunk = String::new();
    loop {
        let current = probe.next()?;
        chunk.push(current);
        if current == ':' && probe.peek() == Some(&']') {
            // Peeked above: the closer follows.
            chunk.push(']');
            probe.next();
            break;
        }
    }
    for _ in chunk.chars() {
        chars.next();
    }
    Some(chunk)
}

/// Translates GNU extensions in an extended regular expression: `\<` and
/// `\>` become word boundaries, `\1` to `\9` fall back. The rest is already
/// ripgrep's syntax.
///
/// `` \` `` and `\'` pass through untouched: ripgrep reads them as literal
/// characters where GNU anchors, a corner too rare to hand over.
pub(crate) fn translate_ere(pattern: &str) -> Option<String> {
    let mut chars = pattern.chars().peekable();
    let mut out = String::new();
    while let Some(current) = chars.next() {
        if current != '\\' {
            out.push(current);
            continue;
        }
        let next = chars.next()?;
        match next {
            '<' => out.push_str(r"\b{start}"),
            '>' => out.push_str(r"\b{end}"),
            '1'..='9' => return None,
            _ => {
                out.push('\\');
                out.push(next);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "bre_tests.rs"]
mod tests;
