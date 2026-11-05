//! The visibility check (`docs/code-quality.md`, "Visibility"): a
//! `pub(crate)` or `pub(super)` function, const or static no other file of
//! its crate names is wider than any use needs. Use-search is plain text
//! over every other file of the crate, so a match in a comment, a string,
//! a macro body or a `#[cfg]`-gated file still counts, and a match only ever
//! hides a narrowing. Items defined inside an inline `mod` block are
//! skipped — their same-file uses stay invisible to a file-granular search.

use crate::rules::RustFile;

/// A `pub(crate)` or `pub(super)` function, const or static no other file
/// of its crate names, as `<path>:<line>: <name> is pub(crate) but no other
/// file in <crate> names it`. Test files count on both sides; macro bodies
/// count as use-sites.
pub(crate) fn check(files: &[RustFile]) -> Vec<String> {
    let mut failures = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for declaration in declarations(&file.source) {
            let named_elsewhere = files.iter().enumerate().any(|(other, candidate)| {
                other != index
                    && candidate.krate == file.krate
                    && candidate.source.contains(declaration.name.as_str())
            });
            if !named_elsewhere {
                failures.push(format!(
                    "{}:{}: {} is {} but no other file in {} names it",
                    file.path,
                    declaration.line,
                    declaration.name,
                    declaration.visibility,
                    file.krate
                ));
            }
        }
    }
    failures
}

/// A checkable declaration: its name, its 1-based line, and its visibility
/// spelled as written.
struct Declaration {
    name: String,
    line: usize,
    visibility: &'static str,
}

/// Every checkable `pub(crate)`/`pub(super)` function, const or static in
/// `source`, in line order: at most one per line, since a line holds at
/// most one leading
/// visibility qualifier.
fn declarations(source: &str) -> Vec<Declaration> {
    let mut found = Vec::new();
    // The `{` depth before the current line. An inline `mod` region covers
    // the lines strictly deeper than its opener's entry depth; the opener
    // line itself is still checked.
    let mut depth: i32 = 0;
    let mut regions: Vec<i32> = Vec::new();
    let mut macro_body: Option<MacroBody> = None;
    let mut in_block_comment = false;
    // A `}` below depth zero means the braces do not balance; the rest of
    // the file resolves to skip, the safe direction.
    let mut unbalanced = false;
    for (index, raw) in source.lines().enumerate() {
        let code = code_line(raw, &mut in_block_comment);
        let parsed = declaration(&code);
        let hidden = unbalanced
            || macro_body.is_some()
            || regions.first().is_some_and(|&entry| depth > entry);
        if let Some((name, visibility)) = parsed
            && !hidden
        {
            found.push(Declaration {
                name,
                line: index + 1,
                visibility,
            });
        }
        // Every inline `mod` block hides its items, whatever its own
        // visibility: same-file uses of those items stay invisible to a
        // file-granular search. The opener line itself is still checked.
        if opens_inline_mod(&code) {
            regions.push(depth);
        }
        if macro_body.is_none() {
            if let Some(body) = macro_opener(&code)
                && body.balance > 0
            {
                macro_body = Some(body);
            }
        } else if let Some(body) = macro_body.as_mut() {
            let (open, close) = body.delimiter.pair();
            for c in code.chars() {
                if c == open {
                    body.balance += 1;
                } else if c == close {
                    body.balance -= 1;
                }
            }
            if body.balance <= 0 {
                macro_body = None;
            }
        }
        depth += brace_delta(&code);
        if depth < 0 {
            unbalanced = true;
            regions.clear();
        } else {
            while regions.last().is_some_and(|&entry| depth <= entry) {
                regions.pop();
            }
        }
    }
    found
}

/// The `{` opens minus the `}` closes in comment- and string-free code.
fn brace_delta(code: &str) -> i32 {
    let mut delta = 0;
    for c in code.chars() {
        if c == '{' {
            delta += 1;
        } else if c == '}' {
            delta -= 1;
        }
    }
    delta
}

/// The line with comments, string literals and character literals replaced
/// by spaces, so brace counting and declaration matching never see a brace
/// or keyword inside them. `in_block_comment` threads a `/* ... */` comment
/// across lines; an unclosed string runs to the end of its line.
fn code_line(line: &str, in_block_comment: &mut bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    if *in_block_comment {
        match block_end(&chars, 0) {
            Some(after) => {
                blanks(&mut out, after);
                index = after;
                *in_block_comment = false;
            }
            None => {
                blanks(&mut out, chars.len());
                return out;
            }
        }
    }
    while index < chars.len() {
        if chars.get(index) == Some(&'/') && chars.get(index + 1) == Some(&'/') {
            blanks(&mut out, chars.len() - index);
            break;
        }
        if chars.get(index) == Some(&'/') && chars.get(index + 1) == Some(&'*') {
            match block_end(&chars, index + 2) {
                Some(after) => {
                    blanks(&mut out, after - index);
                    index = after;
                    continue;
                }
                None => {
                    blanks(&mut out, chars.len() - index);
                    *in_block_comment = true;
                    break;
                }
            }
        }
        if let Some(after) = raw_string_end(&chars, index) {
            blanks(&mut out, after - index);
            index = after;
            continue;
        }
        if chars.get(index) == Some(&'"')
            || (chars.get(index) == Some(&'b') && chars.get(index + 1) == Some(&'"'))
        {
            let mut cursor = index + 1 + usize::from(chars.get(index) == Some(&'b'));
            while cursor < chars.len() && chars.get(cursor) != Some(&'"') {
                let escaped = usize::from(
                    chars.get(cursor) == Some(&'\\') && chars.get(cursor + 1).is_some(),
                );
                cursor += 1 + escaped;
            }
            if cursor < chars.len() {
                cursor += 1;
            }
            blanks(&mut out, cursor - index);
            index = cursor;
            continue;
        }
        if chars.get(index) == Some(&'\'') {
            // A character literal is exactly `'x'` or an escape; anything
            // else is a lifetime, which stays: it holds no braces.
            if let Some(after) = char_end(&chars, index) {
                blanks(&mut out, after - index);
                index = after;
                continue;
            }
            out.push('\'');
            index += 1;
            continue;
        }
        if let Some(c) = chars.get(index) {
            out.push(*c);
        }
        index += 1;
    }
    out
}

/// `count` spaces, keeping token separation where text was stripped.
fn blanks(out: &mut String, count: usize) {
    for _ in 0..count {
        out.push(' ');
    }
}

/// The index just past the `*/` closing the block comment opened before
/// `from`, counting nested openers the way Rust does.
fn block_end(chars: &[char], from: usize) -> Option<usize> {
    let mut depth = 1;
    let mut cursor = from;
    while cursor + 1 < chars.len() {
        if chars.get(cursor) == Some(&'/') && chars.get(cursor + 1) == Some(&'*') {
            depth += 1;
            cursor += 2;
        } else if chars.get(cursor) == Some(&'*') && chars.get(cursor + 1) == Some(&'/') {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Some(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

/// The index just past a raw string opening at `index`, if one opens there:
/// `r"..."`, `br"..."` or `rb"..."`, each with any number of `#`.
fn raw_string_end(chars: &[char], index: usize) -> Option<usize> {
    let mut cursor = index;
    if chars.get(cursor) == Some(&'b') && chars.get(cursor + 1) == Some(&'r') {
        cursor += 2;
    } else if chars.get(cursor) == Some(&'r') {
        cursor += 1;
        if chars.get(cursor) == Some(&'b') {
            cursor += 1;
        }
    } else {
        return None;
    }
    let mut hashes = 0;
    while chars.get(cursor) == Some(&'#') {
        hashes += 1;
        cursor += 1;
    }
    if chars.get(cursor) != Some(&'"') {
        return None;
    }
    cursor += 1;
    while cursor < chars.len() {
        if chars.get(cursor) == Some(&'"') {
            let mut closing = cursor + 1;
            while closing < chars.len()
                && chars.get(closing) == Some(&'#')
                && closing - cursor - 1 < hashes
            {
                closing += 1;
            }
            if closing - cursor - 1 == hashes {
                return Some(closing);
            }
        }
        cursor += 1;
    }
    None
}

/// The index just past a character literal opening at `index` (`'x'`, an
/// escape, or `\u{...}`), or `None` for a lifetime or anything else.
fn char_end(chars: &[char], index: usize) -> Option<usize> {
    let mut cursor = index + 1;
    if chars.get(cursor) == Some(&'\\') {
        cursor += 1;
        if chars.get(cursor) == Some(&'u') && chars.get(cursor + 1) == Some(&'{') {
            cursor += 2;
            while cursor < chars.len() && chars.get(cursor) != Some(&'}') {
                cursor += 1;
            }
            if chars.get(cursor) != Some(&'}') {
                return None;
            }
            cursor += 1;
        } else if chars.get(cursor).is_some() {
            cursor += 1;
        } else {
            return None;
        }
    } else if chars.get(cursor).is_some_and(|c| *c != '\'') {
        cursor += 1;
    } else {
        return None;
    }
    if chars.get(cursor) == Some(&'\'') {
        Some(cursor + 1)
    } else {
        None
    }
}

/// Which delimiter a `macro_rules!` body uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delimiter {
    Brace,
    Paren,
    Bracket,
}

impl Delimiter {
    fn pair(self) -> (char, char) {
        if self == Delimiter::Brace {
            ('{', '}')
        } else if self == Delimiter::Paren {
            ('(', ')')
        } else {
            ('[', ']')
        }
    }
}

/// A `macro_rules!` body being skipped: its delimiter and the opens minus
/// closes since its opener. Only the outermost tracks; nested differences
/// balance out inside it.
struct MacroBody {
    delimiter: Delimiter,
    balance: i32,
}

/// The `macro_rules!` body opening on comment- and string-free `code`, if
/// the line opens one: the delimiter and the balance after the opener,
/// counted over the rest of the line.
fn macro_opener(code: &str) -> Option<MacroBody> {
    let chars: Vec<char> = code.chars().collect();
    let keyword: Vec<char> = "macro_rules".chars().collect();
    let mut cursor = 0;
    while cursor < chars.len() {
        if starts_word(&chars, cursor, &keyword) {
            let mut after = cursor + keyword.len();
            skip_ws(&chars, &mut after);
            if chars.get(after) != Some(&'!') {
                cursor += 1;
                continue;
            }
            after += 1;
            skip_ws(&chars, &mut after);
            // A raw macro name is vanishingly rare, but cheap to accept.
            if chars.get(after) == Some(&'r')
                && chars.get(after + 1) == Some(&'#')
                && chars.get(after + 2).is_some_and(|c| is_ident_start(*c))
            {
                after += 3;
            }
            let name_start = after;
            while chars.get(after).is_some_and(|c| is_ident_continue(*c)) {
                after += 1;
            }
            if after == name_start {
                cursor += 1;
                continue;
            }
            skip_ws(&chars, &mut after);
            let delimiter = if chars.get(after) == Some(&'{') {
                Delimiter::Brace
            } else if chars.get(after) == Some(&'(') {
                Delimiter::Paren
            } else if chars.get(after) == Some(&'[') {
                Delimiter::Bracket
            } else {
                cursor += 1;
                continue;
            };
            let (open, close) = delimiter.pair();
            let mut balance = 1;
            if let Some(tail) = chars.get(after + 1..) {
                for c in tail {
                    if *c == open {
                        balance += 1;
                    } else if *c == close {
                        balance -= 1;
                    }
                }
            }
            return Some(MacroBody { delimiter, balance });
        }
        cursor += 1;
    }
    None
}

/// Whether `word` starts at `at` with identifier boundaries on both sides.
fn starts_word(chars: &[char], at: usize, word: &[char]) -> bool {
    if at > 0 && chars.get(at - 1).is_some_and(|c| is_ident_continue(*c)) {
        return false;
    }
    let Some(rest) = chars.get(at..) else {
        return false;
    };
    if rest.len() < word.len() {
        return false;
    }
    let (head, tail) = rest.split_at(word.len());
    head == word && tail.first().is_none_or(|c| !is_ident_continue(*c))
}

/// Past the whitespace at `cursor`.
fn skip_ws(chars: &[char], cursor: &mut usize) {
    while chars.get(*cursor).is_some_and(|c| c.is_whitespace()) {
        *cursor += 1;
    }
}

/// Whether the character can start an identifier.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Whether the character can continue an identifier.
fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether the line opens an inline `mod` region: a `mod` declaration
/// with any visibility (or none) followed by `{`, or by neither `{` nor
/// `;`, which only an inline opener missing its brace on this line can be.
fn opens_inline_mod(code: &str) -> bool {
    let mut rest = code.trim_start();
    if let Some(after) = strip_word(rest, "pub") {
        rest = after;
        if rest.starts_with('(') {
            let Some(close) = rest.find(')') else {
                return false;
            };
            rest = rest.get(close + 1..).unwrap_or("").trim_start();
        }
    }
    let Some(after) = strip_word(rest, "mod") else {
        return false;
    };
    let Some((_, rest)) = leading_ident(after) else {
        return false;
    };
    let rest = rest.trim_start();
    if rest.starts_with('{') {
        return true;
    }
    if rest.starts_with(';') {
        return false;
    }
    true
}

/// The declaration a line holds, if its first token is a `pub(crate)` or
/// `pub(super)` qualifier followed by a `fn`, `const` or `static` item:
/// the name and the visibility as written. Only functions, consts and
/// statics are candidates: a type's visibility is pinned by any
/// wider-or-equal interface that mentions it (E0446), and a type's use can
/// reach another module through inference without being spelled, so a
/// syntactic check cannot see it (`crates/fakes/src/http_server/server.rs:150`
/// keeps `TunnelState` visible through its `pub(crate)` field;
/// `workers::run` in `crates/log/src/search/workers.rs:28` uses `Searched`
/// without naming it). Structs, enums, unions, traits, type aliases,
/// modules, imports and fields resolve to skip, which under-flags.
fn declaration(code: &str) -> Option<(String, &'static str)> {
    let rest = code.trim_start();
    let (visibility, mut rest) = split_visibility(rest)?;
    // `const fn`, `unsafe fn`, `async fn`: qualifiers before the item keyword.
    loop {
        if let Some(after) = strip_word(rest, "unsafe") {
            rest = after;
        } else if let Some(after) = strip_word(rest, "async") {
            rest = after;
        } else if let Some(after) = strip_word(rest, "default") {
            rest = after;
        } else if let Some(after) = strip_word(rest, "extern") {
            rest = after;
        } else if let Some(after) = strip_word(rest, "const")
            && starts_with_word(after, "fn")
        {
            rest = after;
        } else {
            break;
        }
    }
    for keyword in ["fn", "const", "static"] {
        if let Some(after) = strip_word(rest, keyword) {
            if keyword == "static" {
                let body = strip_word(after, "mut").unwrap_or(after);
                return leading_ident(body).map(|(name, _)| (name, visibility));
            }
            return leading_ident(after).map(|(name, _)| (name, visibility));
        }
    }
    None
}

/// The `pub(crate)` or `pub(super)` qualifier opening `s`, spelled as
/// written, and the text after it.
fn split_visibility(s: &str) -> Option<(&'static str, &str)> {
    let rest = strip_word(s, "pub")?;
    let rest = rest.strip_prefix('(')?.trim_start();
    for (word, visibility) in [("crate", "pub(crate)"), ("super", "pub(super)")] {
        if let Some(rest) = strip_word(rest, word) {
            let rest = rest.strip_prefix(')')?;
            if rest.is_empty() {
                return None;
            }
            if !rest.starts_with(|c: char| c.is_whitespace()) {
                return None;
            }
            return Some((visibility, rest.trim_start()));
        }
    }
    None
}

/// `s` without its leading `word`, when `word` ends on an identifier
/// boundary.
fn strip_word<'s>(s: &'s str, word: &str) -> Option<&'s str> {
    let rest = s.strip_prefix(word)?;
    if rest.starts_with(|c: char| is_ident_continue(c)) {
        return None;
    }
    Some(rest.trim_start())
}

/// Whether `s` starts with `word` on an identifier boundary.
fn starts_with_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word)
        .is_some_and(|rest| !rest.starts_with(|c: char| is_ident_continue(c)))
}

/// The identifier starting `s` (with an `r#` prefix stripped in the return)
/// and the text after it.
fn leading_ident(s: &str) -> Option<(String, &str)> {
    let (body, skipped) = match s.strip_prefix("r#") {
        Some(body) if body.starts_with(|c: char| is_ident_start(c)) => (body, "r#".len()),
        _ if s.starts_with(|c: char| is_ident_start(c)) => (s, 0),
        _ => return None,
    };
    let end = body
        .find(|c: char| !is_ident_continue(c))
        .unwrap_or(body.len());
    let rest = s.get(skipped + end..)?;
    Some((body.get(..end)?.to_owned(), rest))
}

#[cfg(test)]
#[path = "visibility_tests.rs"]
mod tests;
