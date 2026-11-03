//! The census ratchet (ticket #3982): each production file under `crates/`
//! holds at most its inline maximum of string-typed closed-set sites, the
//! #3666 census pattern. A site is one occurrence of `kind: String`,
//! `code: String` or `code: &str`, `.kind.as_str(`-style calls,
//! `.kind == "`-style compares, `["kind"]`-style indexes or
//! `.get("kind")`-style lookups, for each of `kind`, `code`, `view` and
//! `format`. A file missing from the maxima table holds none, and lowering
//! a count never fails: only a rise fails. `scripts/check` keeps the table
//! inline and passes it here on stdin; this module owns site detection and
//! the per-file comparison.
//!
//! The scan runs on the raw line, so a site in a string or a comment
//! counts: over-counting only raises a ceiling, never lets a new site slip
//! past. Occurrences count, not matching lines, so two sites on one line
//! both count. A site split across lines is invisible here, as it is to
//! the census.

use std::collections::BTreeMap;

use crate::rules::RustFile;
use crate::select::is_test_file;

/// The four closed-set names the census tracks.
/// The four closed-set names the census tracks.
const NAMES: &[&str] = &["kind", "code", "view", "format"];
/// Files in scope: Rust files under `crates/`, outside test files.
const SCOPE_PREFIX: &str = "crates/";
/// Suffix of the files the ratchet scans.
const RUST_SUFFIX: &str = ".rs";

/// Occurrences of the six census forms in `source`, summed over its lines.
pub(crate) fn count_sites(source: &str) -> usize {
    source.lines().map(count_line).sum()
}

/// Occurrences of the six census forms on one line, leftmost
/// non-overlapping: a match consumes its text, otherwise the scan advances
/// one character. Every form opens with `k`, `c`, `.` or `[`, so any other
/// position advances without trying a form.
fn count_line(line: &str) -> usize {
    let mut count = 0usize;
    let mut rest = line;
    while !rest.is_empty() {
        let starter = rest.as_bytes().first().copied();
        let matched = if starter == Some(b'k') {
            match_kind_type(rest)
        } else if starter == Some(b'c') {
            match_code_type(rest)
        } else if starter == Some(b'.') {
            match_as_str(rest)
                .or_else(|| match_compare(rest))
                .or_else(|| match_get(rest))
        } else if starter == Some(b'[') {
            match_index(rest)
        } else {
            None
        };
        if let Some(after) = matched {
            count = count.saturating_add(1);
            rest = after;
        } else {
            let Some(first) = rest.chars().next() else {
                break;
            };
            let Some(next) = rest.get(first.len_utf8()..) else {
                break;
            };
            rest = next;
        }
    }
    count
}

/// `rest` past one of the four names at its start, if it opens with one.
fn match_name(rest: &str) -> Option<&str> {
    for name in NAMES {
        if let Some(after) = rest.strip_prefix(name) {
            return Some(after);
        }
    }
    None
}

/// `rest` past its leading whitespace (`\s` in the census pattern).
fn skip_whitespace(rest: &str) -> &str {
    let mut current = rest;
    while let Some(after) = current.strip_prefix(|c: char| c.is_whitespace()) {
        current = after;
    }
    current
}

/// `kind:`, whitespace, `String`.
fn match_kind_type(rest: &str) -> Option<&str> {
    skip_whitespace(rest.strip_prefix("kind:")?).strip_prefix("String")
}

/// `code:`, whitespace, `String` or `&str`.
fn match_code_type(rest: &str) -> Option<&str> {
    let after = skip_whitespace(rest.strip_prefix("code:")?);
    after
        .strip_prefix("String")
        .or_else(|| after.strip_prefix("&str"))
}

/// `.` name `.as_str(`.
fn match_as_str(rest: &str) -> Option<&str> {
    match_name(rest.strip_prefix('.')?)?.strip_prefix(".as_str(")
}

/// `.` name, whitespace, `==` or `!=`, whitespace, `"`.
fn match_compare(rest: &str) -> Option<&str> {
    let after = skip_whitespace(match_name(rest.strip_prefix('.')?)?);
    let operator = after.strip_prefix(['!', '='])?.strip_prefix('=')?;
    skip_whitespace(operator).strip_prefix('"')
}

/// `["` name `"]`.
fn match_index(rest: &str) -> Option<&str> {
    match_name(rest.strip_prefix("[\"")?)?.strip_prefix("\"]")
}

/// `.get("` name `")`.
fn match_get(rest: &str) -> Option<&str> {
    match_name(rest.strip_prefix(".get(\"")?)?.strip_prefix("\")")
}

/// Per-file census counts for the files in scope: Rust files under
/// `crates/`, excluding test files (`crate::select::is_test_file` on the
/// crate-relative path). Only files with a nonzero count appear.
pub(crate) fn counts(files: &[RustFile]) -> BTreeMap<String, usize> {
    let mut table = BTreeMap::new();
    for file in files {
        if !file.path.starts_with(SCOPE_PREFIX) || !file.path.ends_with(RUST_SUFFIX) {
            continue;
        }
        if is_test_file(&file.rel) {
            continue;
        }
        let count = count_sites(&file.source);
        if count > 0 {
            table.insert(file.path.clone(), count);
        }
    }
    table
}

/// Each in-scope file whose count exceeds its maximum (0 when the table
/// names no maximum for it), in path order. Each failure names the file,
/// the count and the maximum.
pub(crate) fn check(files: &[RustFile], maxima: &BTreeMap<String, usize>) -> Vec<String> {
    let mut failures = Vec::new();
    for (path, count) in counts(files) {
        let maximum = maxima.get(&path).copied().unwrap_or(0);
        if count > maximum {
            failures.push(format!(
                "{path} holds {count} string-typed closed-set sites (maximum {maximum})"
            ));
        }
    }
    failures
}

/// The current per-file counts as `<path> <count>` lines, in path order:
/// the seed `scripts/check` carries, and what `--print` prints.
pub(crate) fn table(files: &[RustFile]) -> String {
    counts(files)
        .into_iter()
        .map(|(path, count)| format!("{path} {count}\n"))
        .collect()
}

#[cfg(test)]
#[path = "census_ratchet_tests.rs"]
mod tests;
