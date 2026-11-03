//! The stale-wait ratchet (#3664): each file under `crates/main/tests/`
//! holds at most its inline maximum of stale-wait occurrences, outside
//! `crates/main/tests/support/`. A stale-wait occurrence is a source line
//! holding the identifier `recv_reply`, the text `fn subscribe(` or the
//! text `fn subscribe_full(`, other than a line whose trimmed start is a
//! line comment.
//!
//! The count is conservative: an occurrence counts even inside a string
//! literal or a block comment, and only a full-line `//` comment line
//! exempts it. Literal false positives are safe because the ratchet is an
//! upper bound: over-counting a use inside a string never lets a real
//! `recv_reply` or local `fn subscribe` slip past. A file missing from the
//! maxima table holds none. `scripts/check` keeps the table inline and
//! passes it here on stdin; this module owns occurrence detection and the
//! per-file comparison.

use std::collections::BTreeMap;

use crate::rules::RustFile;

/// Files in scope: Rust files under `crates/main/tests/`, outside `support/`.
const SCOPE_PREFIX: &str = "crates/main/tests/";
/// The driver and its helpers live here; the rule does not apply to them.
const SUPPORT_PREFIX: &str = "crates/main/tests/support/";
/// Suffix of the files the ratchet scans.
const RUST_SUFFIX: &str = ".rs";
/// The stale acknowledgement the shared `recv_answer` replaced.
const REPLY: &str = "recv_reply";
/// The local subscribe copies the shared helpers replaced, under either name.
const SUBSCRIBE: &str = "fn subscribe(";
const SUBSCRIBE_FULL: &str = "fn subscribe_full(";

/// Whether `path` is a file the ratchet scans.
fn is_scoped(path: &str) -> bool {
    path.starts_with(SCOPE_PREFIX)
        && path.ends_with(RUST_SUFFIX)
        && !path.starts_with(SUPPORT_PREFIX)
}

/// Each file in scope holding more stale-wait occurrences than its maximum
/// (0 when the table names no maximum for it), in path order. Each failure
/// names the file, the count and the maximum.
pub(crate) fn check(files: &[RustFile], maxima: &BTreeMap<String, usize>) -> Vec<String> {
    let mut scoped: Vec<&RustFile> = files.iter().filter(|file| is_scoped(&file.path)).collect();
    scoped.sort_by(|a, b| a.path.cmp(&b.path));
    let mut failures = Vec::new();
    for file in scoped {
        let count = count_stale(&file.source);
        let maximum = maxima.get(&file.path).copied().unwrap_or(0);
        if count > maximum {
            failures.push(format!(
                "{} holds {count} stale-wait occurrences (maximum {maximum})",
                file.path
            ));
        }
    }
    failures
}

/// Whether a byte can sit beside an identifier: letters, digits and `_`.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether the byte at `at` in `bytes` is an identifier character: past
/// either end never is, so an edge match keeps its outer side.
fn is_word_at(bytes: &[u8], at: usize) -> bool {
    bytes.get(at).is_some_and(|byte| is_word_byte(*byte))
}

/// The occurrences of the identifier `needle` in `line`: a match counts
/// only when the characters either side are not identifier characters, so
/// a longer name never counts.
fn count_ident(line: &str, needle: &str) -> usize {
    let bytes = line.as_bytes();
    let mut count = 0;
    let mut offset = 0;
    while let Some(at) = line[offset..].find(needle) {
        let start = offset + at;
        let end = start + needle.len();
        if (start == 0 || !is_word_at(bytes, start - 1)) && !is_word_at(bytes, end) {
            count += 1;
        }
        offset = end;
    }
    count
}

/// The stale-wait occurrences in `source`: per line, the whole-identifier
/// `recv_reply` uses plus the `fn subscribe(` and `fn subscribe_full(`
/// definitions. A line whose trimmed start is a line comment never
/// counts; a use inside a string literal or a block comment still does,
/// keeping the count a conservative upper bound.
pub(crate) fn count_stale(source: &str) -> usize {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            count_ident(line, REPLY)
                + line.matches(SUBSCRIBE).count()
                + line.matches(SUBSCRIBE_FULL).count()
        })
        .sum()
}

#[cfg(test)]
#[path = "stale_wait_ratchet_tests.rs"]
mod tests;
