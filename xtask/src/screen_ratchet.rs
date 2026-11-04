//! The grid-read ratchet (`docs/testing/screens.md`, "Screens"): each file
//! under `crates/main/tests/` holds at most its inline maximum of unmarked
//! grid-read lines, outside `crates/main/tests/support/`. A grid-read line
//! is a source line holding one of `.screen()`, `.cell(`, `.contents` or
//! `.rows`: acquiring a grid, or reading its cells, text or rows. The rule
//! is deliberately outside-a-wait: a line inside a `wait_screen` call's
//! span never counts, because the wait's own check settles the grid and its
//! predicate reads are the assertion, not a read outside a wait. The same
//! holds for a line inside a listed wait helper's call span (`WAIT_HELPERS`):
//! the helper passes its predicate argument to a `wait_screen` call, so the
//! call site's predicate lines are the wait's own check too. A
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
//!
//! The same stdin block carries the mouse count as `mouse <path>
//! <maximum>` lines: each file holds at most its inline maximum of
//! mouse-event lines holding a press, release, drag or motion report
//! outside the frame-locked click helpers' call spans, and a file missing
//! from the mouse entries holds none. Wheel reports stay outside the
//! count, and a full-line `//` comment line never counts.

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
/// Helpers whose call span the ratchet skips like a wait's: a name joins
/// only when the helper passes its predicate argument to a `wait_screen*`
/// call, so the predicate lines at its call sites are the wait's own
/// check. The reviewer checks that against the helper's body before
/// listing it here.
const WAIT_HELPERS: &[&str] = &["feed"];
/// The frame-locked click helpers whose call span the mouse count skips:
/// `mouse_where`, `mouse_where_paused` and `click_where`. A literal
/// inside the call that uses coordinates from outside the closure's
/// argument would slip through the span exemption, so every conversion
/// builds its coordinates from the closure's argument.
const MOUSE: &[&str] = &[".mouse_where", ".click_where"];
/// The mouse reports the count matches: SGR press, release, drag and
/// motion. Wheel reports (`[<64;`, `[<65;`) send fixed cells in a pane
/// and read no row from a frame, so they stay outside it.
const MOUSE_READS: &[&str] = &["[<0;", "[<3;", "[<32;", "[<35;"];

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

/// The two per-file maxima tables the `screen-ratchet` stdin block holds:
/// grid-read lines under plain `<path> <maximum>` entries, and mouse-event
/// lines under `mouse <path> <maximum>` entries. Each file's maxima are
/// independent, and a file missing from a map holds none under it.
#[derive(Debug, PartialEq)]
pub(crate) struct Maxima {
    pub(crate) reads: BTreeMap<String, usize>,
    pub(crate) mouse: BTreeMap<String, usize>,
}

/// Parses the inline maxima table from stdin: one `<path> <maximum>` per
/// line for grid reads, and one `mouse <path> <maximum>` per line for
/// mouse events. Blank lines and `#` comment lines are ignored. A
/// malformed or duplicate `mouse` line fails with its line number.
// debt: keeps separate mouse/plain validate-parse-insert branches to avoid refactoring the line parser now;
// revisit when a third line kind is added.
pub(crate) fn parse_screen_maxima(input: &str) -> Result<Maxima, String> {
    let mut maxima = Maxima {
        reads: BTreeMap::new(),
        mouse: BTreeMap::new(),
    };
    for (index, line) in input.lines().enumerate() {
        let line_number = index.saturating_add(1);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let first = parts.next().unwrap_or_default();
        if first == "mouse" {
            let path = parts.next().unwrap_or_default();
            let maximum = parts.next().unwrap_or_default();
            if path.is_empty() || maximum.is_empty() || parts.next().is_some() {
                return Err(format!(
                    "line {line_number}: expected `mouse <path> <maximum>`, got `{trimmed}`"
                ));
            }
            let maximum = maximum
                .parse::<usize>()
                .map_err(|error| format!("line {line_number}: maximum for {path}: {error}"))?;
            if maxima.mouse.insert(path.to_owned(), maximum).is_some() {
                return Err(format!("line {line_number}: duplicate maximum for {path}"));
            }
            continue;
        }
        let maximum = parts.next().unwrap_or_default();
        if first.is_empty() || maximum.is_empty() || parts.next().is_some() {
            return Err(format!(
                "line {line_number}: expected `<path> <maximum>`, got `{trimmed}`"
            ));
        }
        let maximum = maximum
            .parse::<usize>()
            .map_err(|error| format!("line {line_number}: maximum for {first}: {error}"))?;
        if maxima.reads.insert(first.to_owned(), maximum).is_some() {
            return Err(format!("line {line_number}: duplicate maximum for {first}"));
        }
    }
    Ok(maxima)
}

/// Each file in scope whose unmarked count exceeds its maximum (0 when the
/// table names no maximum for it), in path order. Each failure names the
/// file, the count and the maximum: grid-read lines against the reads map,
/// then mouse-event lines outside the helper against the mouse map.
pub(crate) fn check(files: &[RustFile], maxima: &Maxima) -> Vec<String> {
    let mut scoped: Vec<&RustFile> = files.iter().filter(|file| is_scoped(&file.path)).collect();
    scoped.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    for file in scoped {
        let count = count_unmarked(&file.source);
        let maximum = maxima.reads.get(&file.path).copied().unwrap_or(0);
        if count > maximum {
            failures.push(format!(
                "{} holds {count} unmarked grid-read lines (maximum {maximum})",
                file.path
            ));
        }
        let mouse = count_mouse(&file.source);
        let mouse_maximum = maxima.mouse.get(&file.path).copied().unwrap_or(0);
        if mouse > mouse_maximum {
            failures.push(format!(
                "{} holds {mouse} mouse-event lines outside the helper (maximum {mouse_maximum})",
                file.path
            ));
        }
    }
    failures
}

/// The number of mouse-event lines in `source`: a source line holding a
/// press, release, drag or motion report, outside the frame-locked click
/// helpers' call spans. A line whose trimmed start begins `//` never
/// counts. The match is conservative like the grid-read count: a report
/// inside a string literal or a block comment counts.
pub(crate) fn count_mouse(source: &str) -> usize {
    let in_span = spans(source, MOUSE);
    source
        .lines()
        .zip(in_span)
        .filter(|(line, span)| {
            !span
                && !line.trim_start().starts_with("//")
                && MOUSE_READS.iter().any(|read| line.contains(read))
        })
        .count()
}

/// The number of grid-read lines in `source` with no literal `// settled:`
/// text on the read line or the line before it. A line whose trimmed start
/// begins `//` never counts, and neither does a line inside a
/// `wait_screen` call's span.
pub(crate) fn count_unmarked(source: &str) -> usize {
    let mut opens = vec![WAIT];
    opens.extend(WAIT_HELPERS.iter().copied());
    let in_wait = spans(source, &opens);
    let mut previous_settled = false;
    let mut count = 0usize;
    for (line, wait) in source.lines().zip(in_wait) {
        let has_read = READS.iter().any(|read| line.contains(read));
        let is_comment = line.trim_start().starts_with("//");
        let has_settled = line.contains(MARKER);
        if has_read && !is_comment && !has_settled && !previous_settled && !wait {
            count = count.saturating_add(1);
        }
        previous_settled = has_settled;
    }
    count
}

/// One in-span flag per line of `source`: whether the line sits inside a
/// call opened by one of `opens`. The opening and closing lines are both
/// inside: the base is set on the opening line at its depth, and clears
/// after the line where the depth falls back to it.
fn spans(source: &str, opens: &[&str]) -> Vec<bool> {
    let mut depth = 0i32;
    let mut comment = 0u32;
    let mut base: Option<i32> = None;
    let mut flags = Vec::new();
    for line in source.lines() {
        let (code, unclosed) = code_part(line, comment);
        comment = unclosed;
        if base.is_none() && opens.iter().any(|open| opens_call(&code, open)) {
            base = Some(depth);
        }
        flags.push(base.is_some());
        depth += delimiter_depth(&code);
        if let Some(open) = base
            && depth <= open
        {
            base = None;
        }
    }
    flags
}

/// Whether `code` — a line with strings and comments stripped — opens a
/// call to `prefix`: the prefix, then any `[A-Za-z0-9_]` suffix a method
/// name carries (so `.wait_screen` covers `wait_screen_leading`, and
/// `.mouse_where` covers `mouse_where_paused`), then optional space, then
/// `(`. A bare helper name opens only as a whole word, never in its `fn`
/// definition: a dotted prefix carries its own dot, so `feed` still
/// rejects `xfeed` and `feed_more` while `.wait_screen` still opens on any
/// receiver.
fn opens_call(code: &str, prefix: &str) -> bool {
    let mut search = code;
    while let Some(at) = search.find(prefix) {
        let before = &search[..at];
        let after = &search[at + prefix.len()..];
        let word_edge =
            |side: Option<char>| side.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let whole_word = prefix.starts_with('.') || !word_edge(before.chars().next_back());
        let after_name = if prefix.starts_with('.') {
            after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
        } else {
            after
        };
        if whole_word
            && (prefix.starts_with('.') || !word_edge(after.chars().next()))
            && !before.trim_end().ends_with("fn")
            && after_name.trim_start().starts_with('(')
        {
            return true;
        }
        search = after;
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
/// the count stays a conservative upper bound. `comment` threads the open
/// block-comment depth from the previous line — Rust block comments nest,
/// so only a `*/` with no unclosed `/*` before it leaves comment mode;
/// the return carries the depth that stays open.
fn code_part(line: &str, mut comment: u32) -> (String, u32) {
    let mut code = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        let before = rest.len();
        if comment > 0 {
            // The earlier of the two comment tokens decides. The needles
            // differ in their first byte, so their positions can never tie
            // and no comparison between them is needed: branch on which
            // token starts at the earlier position instead.
            let next = rest.find("/*").into_iter().chain(rest.find("*/")).min();
            match next {
                Some(at) if rest[at..].starts_with("/*") => {
                    comment += 1;
                    rest = &rest[at + 2..];
                }
                Some(at) => {
                    comment -= 1;
                    rest = &rest[at + 2..];
                }
                None => return (code, comment),
            }
        } else if rest.starts_with("//") {
            break;
        } else if rest.starts_with("/*") {
            rest = &rest[2..];
            comment += 1;
        } else if let Some(after) = strip_raw_string(rest) {
            rest = after;
            code.push(' ');
        } else if rest.starts_with('"') {
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
        } else if rest.starts_with('\'') {
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
        } else {
            let len = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            code.push_str(&rest[..len]);
            rest = &rest[len..];
        }
        // Every branch above strictly shortens `rest` on well-formed input;
        // a helper that ever stops shrinking it (only a mutant can) ends the
        // line here instead of spinning forever.
        if rest.len() >= before {
            break;
        }
    }
    (code, comment)
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
