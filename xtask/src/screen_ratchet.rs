//! The bare screen-call ratchet (`docs/testing/screens.md`, "Screens"): each
//! file under `crates/main/tests/` holds at most its inline maximum of
//! unmarked `.screen()` call-form lines, outside
//! `crates/main/tests/support/`. The count is conservative: a line counts
//! when it holds the text `.screen()`, even inside a string literal or a
//! block comment, and only a full-line `//` comment line or the literal
//! text `// settled:` on the call line or the line before it exempts it.
//! Literal false positives are safe because the ratchet is an upper bound:
//! over-counting a `.screen()` inside a string, or exempting on marker
//! text inside a string, never lets a real bare read slip past. A file
//! missing from the maxima table holds none. `scripts/check` keeps the
//! table inline and passes it here on stdin; this module owns call
//! detection, comment recognition and the per-file comparison.

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

/// The number of `.screen()` call-form lines in `source` with no literal
/// `// settled:` text on the call line or the line before it. A line
/// whose trimmed start begins `//` never counts.
pub(crate) fn count_unmarked(source: &str) -> usize {
    let mut previous_settled = false;
    let mut count = 0usize;
    for line in source.lines() {
        let has_call = line.contains(CALL);
        let is_comment = line.trim_start().starts_with("//");
        let has_settled = line.contains(MARKER);
        if has_call && !is_comment && !has_settled && !previous_settled {
            count = count.saturating_add(1);
        }
        previous_settled = has_settled;
    }
    count
}

#[cfg(test)]
#[path = "screen_ratchet_tests.rs"]
mod tests;
