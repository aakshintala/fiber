//! The bare screen-call ratchet (`docs/testing/screens.md`, "Screens"): each
//! file under `crates/main/tests/` holds at most its inline maximum of
//! unmarked `.screen()` call-form lines, outside
//! `crates/main/tests/support/`. Only a real `// settled:` line comment on
//! the call line or the line before it exempts a call: the marker inside a
//! string literal or a block comment exempts nothing. A file missing from
//! the maxima table holds none. `scripts/check` keeps the table inline and
//! passes it here on stdin; this module owns call detection, comment
//! recognition and the per-file comparison.

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
/// The call form the ratchet counts; a `.screen` field access is not a call.
const CALL: &str = ".screen()";

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
                "{} holds {count} unmarked .screen() call lines (maximum {maximum})",
                file.path
            ));
        }
    }
    failures
}

/// The number of `.screen()` call-form lines in `source` with no real
/// `// settled:` comment on the call line or the line before it.
pub(crate) fn count_unmarked(source: &str) -> usize {
    let mut depth = 0u32;
    let mut in_string = false;
    let mut raw_hashes: Option<usize> = None;
    let mut previous_settled = false;
    let mut count = 0usize;
    for line in source.lines() {
        let (has_call, has_settled, next_depth, next_string, next_raw) =
            scan_line(line, depth, in_string, raw_hashes);
        depth = next_depth;
        in_string = next_string;
        raw_hashes = next_raw;
        if has_call && !has_settled && !previous_settled {
            count = count.saturating_add(1);
        }
        previous_settled = has_settled;
    }
    count
}

/// Whether a byte can continue a Rust identifier.
fn is_ident_byte(byte: Option<&u8>) -> bool {
    matches!(byte, Some(seen) if seen.is_ascii_alphanumeric() || *seen == b'_')
}

/// Scans one line: whether its code holds a `.screen()` call and whether
/// its line comment holds the marker, plus the lexer state the next line
/// starts in (block-comment depth, open string, open raw string with its
/// hash count). Strings, character literals, raw strings and block comments
/// hide both markers; a line comment ends the scan.
fn scan_line(
    line: &str,
    mut depth: u32,
    mut in_string: bool,
    mut raw_hashes: Option<usize>,
) -> (bool, bool, u32, bool, Option<usize>) {
    let bytes = line.as_bytes();
    let mut index = 0usize;
    let mut has_call = false;
    while index < bytes.len() {
        if let Some(hashes) = raw_hashes {
            if bytes.get(index) == Some(&b'"') {
                let mut closed = true;
                for offset in 0..hashes {
                    if bytes.get(index.saturating_add(1).saturating_add(offset)) != Some(&b'#') {
                        closed = false;
                        break;
                    }
                }
                if closed {
                    raw_hashes = None;
                    index = index.saturating_add(1).saturating_add(hashes);
                    continue;
                }
            }
            index = index.saturating_add(1);
            continue;
        }
        if in_string {
            if bytes.get(index) == Some(&b'\\') {
                index = index.saturating_add(2);
                continue;
            }
            if bytes.get(index) == Some(&b'"') {
                in_string = false;
            }
            index = index.saturating_add(1);
            continue;
        }
        if depth > 0 {
            if bytes.get(index) == Some(&b'/') && bytes.get(index.saturating_add(1)) == Some(&b'*')
            {
                depth = depth.saturating_add(1);
                index = index.saturating_add(2);
                continue;
            }
            if bytes.get(index) == Some(&b'*') && bytes.get(index.saturating_add(1)) == Some(&b'/')
            {
                depth = depth.saturating_sub(1);
                index = index.saturating_add(2);
                continue;
            }
            index = index.saturating_add(1);
            continue;
        }
        if bytes.get(index) == Some(&b'r') {
            let mut hashes_end = index.saturating_add(1);
            while bytes.get(hashes_end) == Some(&b'#') {
                hashes_end = hashes_end.saturating_add(1);
            }
            if bytes.get(hashes_end) == Some(&b'"') {
                let prev_is_ident = index > 0 && is_ident_byte(bytes.get(index.saturating_sub(1)));
                let prev_is_byte_prefix = prev_is_ident
                    && bytes.get(index.saturating_sub(1)) == Some(&b'b')
                    && (index < 2 || !is_ident_byte(bytes.get(index.saturating_sub(2))));
                if !prev_is_ident || prev_is_byte_prefix {
                    raw_hashes = Some(hashes_end.saturating_sub(index.saturating_add(1)));
                    index = hashes_end.saturating_add(1);
                    continue;
                }
            }
        }
        if bytes.get(index) == Some(&b'"') {
            in_string = true;
            index = index.saturating_add(1);
            continue;
        }
        if bytes.get(index) == Some(&b'\'') {
            if bytes.get(index.saturating_add(1)) == Some(&b'\\') {
                let mut close = index.saturating_add(2);
                while close < bytes.len()
                    && close < index.saturating_add(12)
                    && bytes.get(close) != Some(&b'\'')
                {
                    close = close.saturating_add(1);
                }
                if bytes.get(close) == Some(&b'\'') {
                    index = close.saturating_add(1);
                } else {
                    index = index.saturating_add(1);
                }
                continue;
            }
            if bytes.get(index.saturating_add(2)) == Some(&b'\'') {
                index = index.saturating_add(3);
                continue;
            }
            index = index.saturating_add(1);
            continue;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index.saturating_add(1)) == Some(&b'*') {
            depth = depth.saturating_add(1);
            index = index.saturating_add(2);
            continue;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index.saturating_add(1)) == Some(&b'/') {
            let comment = line.get(index..).unwrap_or_default();
            return (
                has_call,
                comment.contains(MARKER),
                depth,
                in_string,
                raw_hashes,
            );
        }
        if bytes.get(index) == Some(&b'.') {
            let end = index.saturating_add(CALL.len());
            if end <= line.len() && line.get(index..end) == Some(CALL) {
                has_call = true;
                index = end;
                continue;
            }
        }
        index = index.saturating_add(1);
    }
    (has_call, false, depth, in_string, raw_hashes)
}

#[cfg(test)]
#[path = "screen_ratchet_tests.rs"]
mod tests;
