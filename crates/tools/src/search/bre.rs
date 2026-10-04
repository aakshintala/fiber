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
#[allow(dead_code, reason = "the grep search translates patterns with task 4")]
pub(crate) fn translate_bre(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    // Whether the next `(` opens a group: only right after `\(` or `\|`.
    let mut group_open = false;
    // What `*` means where the scan stands.
    let mut star = Star::Literal;
    while let Some(current) = chars.get(index) {
        if *current == '\\' {
            let Some(next) = chars.get(index + 1) else {
                // A trailing backslash: the system reports it.
                return None;
            };
            index += 2;
            match next {
                '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                    out.push(*next);
                    group_open = *next == '(' || *next == '|';
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
                    out.push(*next);
                    group_open = false;
                    star = Star::Repeat;
                }
                // A quantified assertion is never what GNU means: the
                // runner's grep matches nothing for `\<*`, so the system
                // decides what follows one.
                'b' | 'B' => {
                    out.push('\\');
                    out.push(*next);
                    group_open = false;
                    star = Star::Invalid;
                }
                _ => {
                    if next.is_ascii_alphanumeric() {
                        out.push(*next);
                    } else if next.is_ascii_graphic() || *next == ' ' {
                        out.push('\\');
                        out.push(*next);
                    } else {
                        return None;
                    }
                    group_open = false;
                    star = Star::Repeat;
                }
            }
            continue;
        }
        if *current == '[' {
            let Some((text, next)) = translate_class(&chars, index) else {
                // Unbalanced: the system reports it.
                return None;
            };
            out.push_str(&text);
            index = next;
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        if *current == '*' {
            match star {
                // Probed on the runner's grep: `^*` reads a literal
                // asterisk, so an anchor leaves the scan fresh.
                Star::Literal => out.push_str(r"\*"),
                Star::Repeat => out.push('*'),
                Star::Invalid => return None,
            }
            index += 1;
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        if *current == '^' {
            if index == 0 || group_open {
                out.push('^');
                star = Star::Literal;
            } else {
                out.push_str(r"\^");
                star = Star::Repeat;
            }
            index += 1;
            group_open = false;
            continue;
        }
        if *current == '$' {
            if dollar_anchor_ahead(&chars, index) {
                out.push('$');
                star = Star::Invalid;
            } else {
                out.push_str(r"\$");
                star = Star::Repeat;
            }
            index += 1;
            group_open = false;
            continue;
        }
        if matches!(*current, '(' | ')' | '{' | '}' | '|' | '+' | '?') {
            out.push('\\');
            out.push(*current);
            index += 1;
            group_open = false;
            star = Star::Repeat;
            continue;
        }
        out.push(*current);
        index += 1;
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

/// Whether the `$` at `index` anchors: at the end, or right before `\)` or
/// `\|`.
fn dollar_anchor_ahead(chars: &[char], index: usize) -> bool {
    let mut rest = chars.iter().skip(index + 1);
    match rest.next() {
        None => true,
        Some('\\') => matches!(rest.next(), Some(')') | Some('|')),
        _ => false,
    }
}

/// Translates the bracket expression opening at `open`: backslashes inside
/// are escaped, a leading `]` is kept, and `[:class:]` names pass through.
/// Nothing when no closer follows.
fn translate_class(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut text = String::from("[");
    let mut index = open + 1;
    // Negation is `^` only: a leading `!` is an ordinary member, as the
    // runner's grep reads it.
    if chars.get(index) == Some(&'^') {
        text.push('^');
        index += 1;
    }
    if chars.get(index) == Some(&'!') {
        text.push_str("\\!");
        index += 1;
    }
    // A leading `]` is a member, not the closer.
    if chars.get(index) == Some(&']') {
        text.push(']');
        index += 1;
    }
    loop {
        let next = chars.get(index)?;
        index += 1;
        if *next == '[' {
            // A character class such as `[:alpha:]` copies through; its
            // closer is not the bracket's.
            if let Some(end) = class_end(chars, index) {
                for member in chars.iter().skip(index - 1).take(end - (index - 1)) {
                    text.push(*member);
                }
                index = end;
                continue;
            }
        }
        if *next == ']' {
            text.push(']');
            return Some((text, index));
        }
        // Inside brackets a backslash is ordinary, so escaping it keeps it
        // one: `[a\]` reads `a` and `\`, as the runner's grep does.
        if *next == '\\' {
            text.push_str("\\\\");
        } else {
            text.push(*next);
        }
    }
}

/// The index past the `:]` closing a `[:class:]` whose `:` sits at `colon`,
/// or nothing when none follows.
fn class_end(chars: &[char], colon: usize) -> Option<usize> {
    if chars.get(colon) != Some(&':') {
        return None;
    }
    ((colon + 1)..chars.len())
        .find(|index| chars.get(*index) == Some(&':') && chars.get(index + 1) == Some(&']'))
        .map(|index| index + 2)
}

/// Translates GNU extensions in an extended regular expression: `\<` and
/// `\>` become word boundaries, `\1` to `\9` fall back. The rest is already
/// ripgrep's syntax.
///
/// `` \` `` and `\'` pass through untouched: ripgrep reads them as literal
/// characters where GNU anchors, a corner too rare to hand over.
#[allow(dead_code, reason = "the grep search translates patterns with task 4")]
pub(crate) fn translate_ere(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while let Some(current) = chars.get(index) {
        if *current != '\\' {
            out.push(*current);
            index += 1;
            continue;
        }
        let next = chars.get(index + 1)?;
        index += 2;
        match next {
            '<' => out.push_str(r"\b{start}"),
            '>' => out.push_str(r"\b{end}"),
            '1'..='9' => return None,
            _ => {
                out.push('\\');
                out.push(*next);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "bre_tests.rs"]
mod tests;
