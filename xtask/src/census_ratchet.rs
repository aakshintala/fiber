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

/// The four closed-set names the census tracks.
const NAMES: &[&str] = &["kind", "code", "view", "format"];

/// Occurrences of the six census forms in `source`, summed over its lines.
pub(crate) fn count_sites(source: &str) -> usize {
    source.lines().map(count_line).sum()
}

/// Occurrences of the six census forms on one line, leftmost
/// non-overlapping: a match consumes its text, otherwise the scan advances
/// one character.
fn count_line(line: &str) -> usize {
    let mut count = 0usize;
    let mut rest = line;
    while !rest.is_empty() {
        if let Some(after) = match_site(rest) {
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

/// The remainder past a census site at the start of `rest`, if one of the
/// six forms opens there.
fn match_site(rest: &str) -> Option<&str> {
    match_kind_type(rest)
        .or_else(|| match_code_type(rest))
        .or_else(|| match_as_str(rest))
        .or_else(|| match_compare(rest))
        .or_else(|| match_index(rest))
        .or_else(|| match_get(rest))
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

#[cfg(test)]
#[path = "census_ratchet_tests.rs"]
mod tests;
