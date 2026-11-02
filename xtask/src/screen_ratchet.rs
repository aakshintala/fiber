//! The grid-read ratchet (`docs/testing/screens.md`, "Screens"): each file
//! under `crates/main/tests/` holds at most its inline maximum of unmarked
//! grid-read lines, outside `crates/main/tests/support/`. A grid-read line
//! is a source line holding one of `.screen()`, `.cell(`, `.contents` or
//! `.rows`: acquiring a grid, or reading its cells, text or rows. The rule
//! is deliberately outside-a-wait: a line inside a `wait_screen` call's
//! span never counts, because the wait's own check settles the grid and its
//! predicate reads are the assertion, not a read outside a wait. A
//! predicate that derives its expected value from the grid it asserts on
//! sits inside the wait, where a line ratchet cannot tell it from the
//! wait's own probe, so that shape stays outside this check.
//!
//! The count is conservative: a line counts when it holds the read text,
//! even inside a string literal or a block comment, and only a full-line
//! `//` comment line or the literal text `// settled:` on the read line or
//! the line before it exempts it. Literal false positives are safe because
//! the ratchet is an upper bound: over-counting a read inside a string, or
//! exempting on marker text inside a string, never lets a real unsettled
//! read slip past. A file missing from the maxima table holds none.
//! `scripts/check` keeps the table inline and passes it here on stdin; this
//! module owns read detection, wait-span tracking, comment recognition and
//! the per-file comparison.

use std::collections::BTreeMap;

use crate::rules::RustFile;

/// Files in scope: Rust files under `crates/main/tests/`, outside `support/`.
const SCOPE_PREFIX: &str = "crates/main/tests/";
/// The driver and its helpers live here; the rule does not apply to them.
const SUPPORT_PREFIX: &str = "crates/main/tests/support/";
/// Suffix of the files the ratchet scans.
const RUST_SUFFIX: &str = ".rs";
/// The exemption marker, as written in a Rust line comment.
const MARKER: &str = "// settled:";
/// The source texts that read the grid: `.screen()` acquires one (a
/// `.screen` field access without the call parens is not a call),
/// `.cell(` reads cells exactly, and `.contents` / `.rows` read the
/// grid's text and rows.
const READS: &[&str] = &[".screen()", ".cell(", ".contents", ".rows"];
/// The waits whose call span the ratchet skips: `wait_screen`,
/// `wait_screen_leading`, `wait_screen_reporting`.
const WAIT: &str = ".wait_screen";

/// Parses the inline maxima table from stdin: one `<path> <maximum>` per
/// line. Blank lines and `#` comment lines are ignored.
pub(crate) fn parse_maxima(input: &str) -> Result<BTreeMap<String, usize>, String> {
    let mut maxima = BTreeMap::new();
    for (index, line) in input.lines().enumerate() {
        let line_number = index.saturating_add(1);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let path = parts.next().unwrap_or_default();
        let maximum = parts.next().unwrap_or_default();
        if path.is_empty() || maximum.is_empty() || parts.next().is_some() {
            return Err(format!(
                "line {line_number}: expected `<path> <maximum>`, got `{trimmed}`"
            ));
        }
        let maximum = maximum
            .parse::<usize>()
            .map_err(|error| format!("line {line_number}: maximum for {path}: {error}"))?;
        if maxima.insert(path.to_owned(), maximum).is_some() {
            return Err(format!("line {line_number}: duplicate maximum for {path}"));
        }
    }
    Ok(maxima)
}

/// Whether `path` is a file the ratchet scans.
fn is_scoped(path: &str) -> bool {
    path.starts_with(SCOPE_PREFIX)
        && path.ends_with(RUST_SUFFIX)
        && !path.starts_with(SUPPORT_PREFIX)
}

/// Each file in scope whose unmarked count exceeds its maximum (0 when the
/// table names no maximum for it), in path order. Each failure names the
/// file, the count and the maximum.
pub(crate) fn check(files: &[RustFile], maxima: &BTreeMap<String, usize>) -> Vec<String> {
    let mut scoped: Vec<&RustFile> = files.iter().filter(|file| is_scoped(&file.path)).collect();
    scoped.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    for file in scoped {
        let count = count_unmarked(&file.source);
        let maximum = maxima.get(&file.path).copied().unwrap_or(0);
        if count > maximum {
            failures.push(format!(
                "{} holds {count} unmarked grid-read lines (maximum {maximum})",
                file.path
            ));
        }
    }
    failures
}

/// The number of grid-read lines in `source` with no literal `// settled:`
/// text on the read line or the line before it. A line whose trimmed start
/// begins `//` never counts, and neither does a line inside a
/// `wait_screen` call's span.
pub(crate) fn count_unmarked(source: &str) -> usize {
    let mut depth = 0i32;
    let mut in_block = false;
    let mut wait_base: Option<i32> = None;
    let mut previous_settled = false;
    let mut count = 0usize;
    for line in source.lines() {
        let (code, open) = code_part(line, in_block);
        in_block = open;
        if wait_base.is_none() && opens_wait(&code) {
            wait_base = Some(depth);
        }
        let in_wait = wait_base.is_some();
        let has_read = READS.iter().any(|read| line.contains(read));
        let is_comment = line.trim_start().starts_with("//");
        let has_settled = line.contains(MARKER);
        if has_read && !is_comment && !has_settled && !previous_settled && !in_wait {
            count = count.saturating_add(1);
        }
        previous_settled = has_settled;
        depth += delimiter_depth(&code);
        if let Some(base) = wait_base {
            if depth <= base {
                wait_base = None;
            }
        }
    }
    count
}

/// Whether `code` — a line with strings and comments stripped — opens a
/// grid wait: a `.wait_screen*(` method call.
fn opens_wait(code: &str) -> bool {
    let mut search = code;
    while let Some(at) = search.find(WAIT) {
        let after = search[at + WAIT.len()..]
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if after.trim_start().starts_with('(') {
            return true;
        }
        search = &search[at + WAIT.len()..];
    }
    false
}

/// The net delimiter depth change of `code`: `(` `{` `[` open, `)` `}`
/// `]` close. `code` carries no strings or comments, so every delimiter
/// here is real code.
fn delimiter_depth(code: &str) -> i32 {
    code.chars().fold(0, |depth, c| match c {
        '(' | '{' | '[' => depth + 1,
        ')' | '}' | ']' => depth - 1,
        _ => depth,
    })
}

/// The code part of `line`: string and character literals and comments
/// removed, for wait detection and delimiter depth only, so a
/// `.wait_screen` inside a string or a comment opens no wait and quoted
/// delimiters move no depth. Pattern matching still runs on the raw line:
/// the count stays a conservative upper bound. `in_block` threads an open
/// block comment from the previous line; the return carries whether one
/// stays open.
fn code_part(line: &str, mut in_block: bool) -> (String, bool) {
    let mut code = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        if in_block {
            match rest.find("*/") {
                Some(end) => {
                    rest = &rest[end + 2..];
                    in_block = false;
                }
                None => return (code, true),
            }
            continue;
        }
        if rest.starts_with("//") {
            break;
        }
        if rest.starts_with("/*") {
            rest = &rest[2..];
            in_block = true;
            continue;
        }
        if let Some(after) = strip_raw_string(rest) {
            rest = after;
            code.push(' ');
            continue;
        }
        if rest.starts_with('"') {
            match cooked_end(&rest[1..]) {
                Some(after) => {
                    rest = after;
                    code.push(' ');
                }
                None => {
                    code.push(' ');
                    break;
                }
            }
            continue;
        }
        if rest.starts_with('\'') {
            match char_end(&rest[1..]) {
                Some(len) => {
                    rest = &rest[1 + len..];
                    code.push(' ');
                }
                None => {
                    code.push('\'');
                    rest = &rest[1..];
                }
            }
            continue;
        }
        let len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        code.push_str(&rest[..len]);
        rest = &rest[len..];
    }
    (code, in_block)
}

/// The remainder after the raw-string literal at the start of `rest`
/// (`r"..."`, `r#"..."#`, with an optional `b` prefix), if `rest` opens
/// one.
fn strip_raw_string(rest: &str) -> Option<&str> {
    let mut body = rest.strip_prefix('b').unwrap_or(rest);
    body = body.strip_prefix('r')?;
    let hashes = body.chars().take_while(|&c| c == '#').count();
    body = body.get(hashes..)?;
    body = body.strip_prefix('"')?;
    let closer = format!("\"{}", "#".repeat(hashes));
    let end = body.find(&closer)?;
    Some(&body[end + closer.len()..])
}

/// The remainder after the cooked string body at the start of `body`
/// (past its opening quote), honouring backslash escapes, or `None` when
/// no closing quote follows on this line.
fn cooked_end(body: &str) -> Option<&str> {
    let mut escaped = false;
    for (at, c) in body.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            return Some(&body[at + 1..]);
        }
    }
    None
}

/// The byte length of the character literal body at the start of `body`
/// (past its opening quote), including its closing quote, or `None` when
/// `body` holds no complete literal (a lifetime or a broken literal stays
/// code).
fn char_end(body: &str) -> Option<usize> {
    let mut chars = body.char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        chars.next()?;
    }
    let (at, quote) = chars.next()?;
    if quote != '\'' {
        return None;
    }
    Some(at + 1)
}

#[cfg(test)]
#[path = "screen_ratchet_tests.rs"]
mod tests;
