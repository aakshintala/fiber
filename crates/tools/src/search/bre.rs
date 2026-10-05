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
/// Nothing when no closer follows, when a nested `[` opens a character
/// class, a collating element or an equivalence element instead of a
/// member, or when `--` appears: an invalid class errors in GNU, a valid
/// element has no escaping that keeps GNU's reading, and a `--` there is
/// a range or an error in GNU, never set difference, so the system grep
/// decides each case.
///
/// Rust reads `&&`, `~~`, `--` and a nested `[` as set operators while
/// GNU reads them as members, so `&` and `~` escape, a `[` outside
/// `[:...:]` escapes, and a class holding `--` hands over to the system
/// grep instead: a `--` there is a range or an error in GNU, never set
/// difference, and no escaping keeps both readings.
fn translate_class(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut text = String::from("[");
    // The previous raw char in the class, for `--`.
    let mut prev = '[';
    // Negation is `^` only: a leading `!` is an ordinary member, as the
    // runner's grep reads it.
    if chars.peek() == Some(&'^') {
        text.push('^');
        chars.next();
        prev = '^';
    }
    if chars.peek() == Some(&'!') {
        text.push_str("\\!");
        chars.next();
        prev = '!';
    }
    // A leading `]` is a member, not the closer.
    if chars.peek() == Some(&']') {
        text.push(']');
        chars.next();
        prev = ']';
    }
    loop {
        let next = chars.next()?;
        if next == '[' {
            // A character class such as `[:alpha:]` copies through; its
            // closer is not the bracket's.
            if let Some(chunk) = class_chunk(chars) {
                text.push('[');
                text.push_str(&chunk);
                for current in chunk.chars() {
                    if current == '-' && prev == '-' {
                        return None;
                    }
                    prev = current;
                }
                continue;
            }
            // A `[` opening a character class (`[:`), a collating
            // element (`[.`) or an equivalence element (`[=`) never reads
            // as a member: the system reports an invalid one and runs a
            // valid element, so it decides either way.
            if matches!(chars.peek(), Some(':') | Some('.') | Some('=')) {
                return None;
            }
            // Any other `[` is a member in GNU but opens a set in Rust:
            // escape it.
            text.push_str("\\[");
            prev = '[';
            continue;
        }
        if next == ']' {
            text.push(']');
            return Some(text);
        }
        // Inside brackets a backslash is ordinary, so escaping it keeps it
        // one: `[a\]` reads `a` and `\`, as the runner's grep does.
        if next == '\\' {
            text.push_str("\\\\");
            prev = '\\';
        } else if next == '&' || next == '~' {
            // Single or doubled, GNU reads these as members.
            text.push('\\');
            text.push(next);
            prev = next;
        } else if next == '-' && prev == '-' {
            return None;
        } else {
            text.push(next);
            prev = next;
        }
    }
}

/// The `:...:]` opening after a `[` inside a bracket expression, through
/// its closer: consumes nothing unless the closer follows, when the
/// consumed text is returned. A `[` with no `:]` after it is left for the
/// caller: an ordinary member, unless it opens a class or an element.
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
/// ripgrep's syntax, except inside `[...]`: there a backslash is an
/// ordinary member, only the first `^` negates, and Rust reads `&&`, `~~`,
/// `--` and a nested `[` as set operators while GNU reads members, so
/// `&` and `~` escape, a `[` outside `[:...:]` escapes, and a class
/// holding `--` hands over to the system grep, as
/// [`translate_class`] does for basic expressions. An unterminated class
/// hands over too: the system reports it.
///
/// `` \` `` and `\'` pass through untouched: ripgrep reads them as literal
/// characters where GNU anchors, a corner too rare to hand over.
pub(crate) fn translate_ere(pattern: &str) -> Option<String> {
    let mut chars = pattern.chars().peekable();
    let mut out = String::new();
    // Whether the scan stands inside a bracket expression.
    let mut class = false;
    // Whether no member was read in the class yet: only there `^` negates.
    let mut first = false;
    // Whether only the negation was read in the class: a `]` here is a
    // member, not the closer.
    let mut fresh = false;
    // The previous raw char in the class, for `--`.
    let mut prev = '\0';
    while let Some(current) = chars.next() {
        if current == '\\' {
            if class {
                // Inside brackets a backslash is an ordinary member, as
                // the runner's grep reads it: keep it, and read what
                // follows as a member on its own pass. A trailing one
                // leaves the bracket open, handing over below.
                out.push_str(r"\\");
                first = false;
                fresh = false;
                prev = '\\';
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
            continue;
        }
        if class {
            if current == ']' {
                out.push(']');
                if fresh {
                    fresh = false;
                    first = false;
                    prev = ']';
                } else {
                    class = false;
                }
                continue;
            }
            if current == '^' && first {
                // Only the first caret negates: a later one is a member,
                // while a `]` stays leading after the negation (`[^]]`).
                out.push('^');
                first = false;
                prev = '^';
                continue;
            }
            if current == '[' {
                // A character class such as `[:alpha:]` copies through;
                // its closer is not the bracket's.
                if let Some(chunk) = class_chunk(&mut chars) {
                    out.push('[');
                    out.push_str(&chunk);
                    for member in chunk.chars() {
                        if member == '-' && prev == '-' {
                            return None;
                        }
                        prev = member;
                    }
                    first = false;
                    fresh = false;
                    continue;
                }
                // As in [`translate_class`]: a class or element opener
                // never reads as a member, so the system decides.
                if matches!(chars.peek(), Some(':') | Some('.') | Some('=')) {
                    return None;
                }
                out.push_str(r"\[");
                first = false;
                fresh = false;
                prev = '[';
                continue;
            }
            if current == '&' || current == '~' {
                out.push('\\');
                out.push(current);
                first = false;
                fresh = false;
                prev = current;
                continue;
            }
            if current == '-' && prev == '-' {
                return None;
            }
            out.push(current);
            first = false;
            fresh = false;
            prev = current;
            continue;
        }
        if current == '[' {
            out.push('[');
            class = true;
            first = true;
            fresh = true;
            prev = '[';
            continue;
        }
        out.push(current);
    }
    // An open bracket never closed hands over: the system reports it.
    if class {
        return None;
    }
    Some(out)
}

#[cfg(test)]
#[path = "bre_tests.rs"]
mod tests;
