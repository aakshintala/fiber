//! Checks that the code matches the rules and tables in the docs: the
//! 800-line cap (`docs/code-quality.md`, "Size"), the `unsafe` table in
//! `docs/code-quality.md`, and the crate list in `docs/dependencies.md`.

use std::collections::BTreeSet;

use crate::select::is_test_file;

const LINE_CAP: usize = 800;

/// A Rust file in a workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustFile {
    pub(crate) krate: String,
    /// Relative to the repository.
    pub(crate) path: String,
    /// Relative to the crate.
    pub(crate) rel: String,
    pub(crate) source: String,
}

pub(crate) fn over_cap(files: &[RustFile]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !is_test_file(&f.rel))
        .filter_map(|f| {
            let lines = f.source.matches('\n').count();
            (lines > LINE_CAP)
                .then(|| format!("{}: {lines} lines, over the {LINE_CAP}-line cap", f.path))
        })
        .collect()
}

/// `source` with comments and literals removed, so what is left is code.
/// It knows every Rust literal form: strings, raw strings, byte and C
/// strings with or without `r`, chars and byte chars, and tells a char from
/// a lifetime. Block comments nest. Every step consumes at least one
/// character.
pub(crate) fn code_only(source: &str) -> String {
    let mut out = String::new();
    let mut it = source.chars();
    let mut prev = ' ';
    while let Some(c) = it.next() {
        if c == '/' && peek(&it, 0) == '/' {
            if it.by_ref().any(|c| c == '\n') {
                out.push('\n');
            }
        } else if c == '/' && peek(&it, 0) == '*' {
            it.next();
            skip_block_comment(&mut it);
            out.push(' ');
        } else if !is_ident(prev)
            && let Some(literal) = literal_start(c, &it)
        {
            match literal {
                Literal::Quoted { opening, quote } => {
                    it.by_ref().take(opening).for_each(drop);
                    skip_quoted(&mut it, quote);
                }
                Literal::Raw { opening, hashes } => {
                    it.by_ref().take(opening).for_each(drop);
                    skip_past(&mut it, &format!("\"{}", "#".repeat(hashes)));
                }
            }
            out.push_str("\"\"");
        } else {
            out.push(c);
        }
        prev = c;
    }
    out
}

/// A literal that starts at the character just taken from `it`. `opening`
/// counts the characters still to consume up to and including the opening
/// quote.
enum Literal {
    /// A string, char, byte or C literal ending at an unescaped `quote`.
    Quoted { opening: usize, quote: char },
    /// A raw literal ending at `"` and `hashes` hashes.
    Raw { opening: usize, hashes: usize },
}

fn literal_start(c: char, it: &std::str::Chars<'_>) -> Option<Literal> {
    let hashes_at = |n: usize| it.clone().skip(n).take_while(|h| *h == '#').count();
    // `n` is where the hashes start, after `c`.
    let raw = |n: usize| {
        let hashes = hashes_at(n);
        (peek(it, n + hashes) == '"').then_some(Literal::Raw {
            opening: n + hashes + 1,
            hashes,
        })
    };
    // A char literal is one character or one escape between quotes; any
    // other `'` starts a lifetime or a label.
    let char_literal = |n: usize| peek(it, n) == '\\' || peek(it, n + 1) == '\'';
    match (c, peek(it, 0)) {
        ('"', _) => Some(Literal::Quoted {
            opening: 0,
            quote: '"',
        }),
        ('\'', _) if char_literal(0) => Some(Literal::Quoted {
            opening: 0,
            quote: '\'',
        }),
        ('b', '\'') => Some(Literal::Quoted {
            opening: 1,
            quote: '\'',
        }),
        ('b' | 'c', '"') => Some(Literal::Quoted {
            opening: 1,
            quote: '"',
        }),
        ('b' | 'c', 'r') => raw(1),
        ('r', _) => raw(0),
        _ => None,
    }
}

fn peek(it: &std::str::Chars<'_>, n: usize) -> char {
    it.clone().nth(n).unwrap_or('\0')
}

/// Consumes `it` through the `*/` that closes a block comment whose `/*`
/// was just consumed, counting nested comments.
fn skip_block_comment(it: &mut std::str::Chars<'_>) {
    let mut depth = 1usize;
    while let Some(c) = it.next() {
        if c == '/' && peek(it, 0) == '*' {
            it.next();
            depth += 1;
        } else if c == '*' && peek(it, 0) == '/' {
            it.next();
            depth -= 1;
            if depth == 0 {
                return;
            }
        }
    }
}

/// Consumes `it` through the first `quote` no backslash escapes.
fn skip_quoted(it: &mut std::str::Chars<'_>, quote: char) {
    while let Some(c) = it.next() {
        if c == '\\' {
            it.next();
        } else if c == quote {
            return;
        }
    }
}

/// Consumes `it` through the first `close`.
fn skip_past(it: &mut std::str::Chars<'_>, close: &str) {
    let mut seen = String::new();
    for c in it.by_ref() {
        seen.push(c);
        if seen.ends_with(close) {
            return;
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether Rust `source` uses `unsafe` in code.
pub(crate) fn uses_unsafe(source: &str) -> bool {
    let code = code_only(source);
    code.match_indices("unsafe").any(|(i, _)| {
        let before = code
            .get(..i)
            .and_then(|s| s.chars().next_back())
            .unwrap_or(' ');
        let after = code
            .get(i + "unsafe".len()..)
            .and_then(|s| s.chars().next())
            .unwrap_or(' ');
        !is_ident(before) && !is_ident(after)
    })
}

/// The body of the section under `heading`, up to the next heading of the
/// same or a higher level.
pub(crate) fn section<'a>(markdown: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let level = |line: &str| {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        (hashes > 0 && line.get(hashes..).is_some_and(|rest| rest.starts_with(' ')))
            .then_some(hashes)
    };
    let mut lines = markdown.lines();
    let start = lines.by_ref().find_map(|line| {
        let hashes = level(line)?;
        (line.get(hashes..).map(str::trim) == Some(heading)).then_some(hashes)
    })?;
    Some(
        lines
            .take_while(|line| level(line).is_none_or(|l| l > start))
            .collect(),
    )
}

fn is_separator(line: &str) -> bool {
    line.contains('-')
        && line.starts_with('|')
        && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The cells of every table body row in `lines`.
pub(crate) fn table_rows(lines: &[&str]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in lines.iter().map(|l| l.trim()) {
        if is_separator(line) {
            rows.pop(); // the header row
        } else if let Some(inner) = line.strip_prefix('|') {
            let inner = inner.strip_suffix('|').unwrap_or(inner);
            rows.push(
                inner
                    .split('|')
                    .map(|cell| cell.trim().to_owned())
                    .collect(),
            );
        }
    }
    rows
}

fn unticked(cell: &str) -> String {
    cell.trim_matches('`').to_owned()
}

/// (crate, file) pairs the `unsafe` table lists.
pub(crate) fn unsafe_table(code_quality: &str) -> Result<BTreeSet<(String, String)>, String> {
    let lines =
        section(code_quality, "`unsafe`").ok_or("docs/code-quality.md has no `unsafe` section")?;
    Ok(table_rows(&lines)
        .iter()
        .filter_map(|cells| match cells.as_slice() {
            [krate, file, ..] if krate != "none yet" => Some((unticked(krate), unticked(file))),
            _ => None,
        })
        .collect())
}

pub(crate) fn unsafe_mismatches(
    files: &[RustFile],
    code_quality: &str,
) -> Result<Vec<String>, String> {
    let used: BTreeSet<(String, String)> = files
        .iter()
        .filter(|f| uses_unsafe(&f.source))
        .map(|f| (f.krate.clone(), f.path.clone()))
        .collect();
    let listed = unsafe_table(code_quality)?;
    let mut failures: Vec<String> = used
        .difference(&listed)
        .map(|(_, path)| {
            format!("{path}: uses unsafe, but the table in docs/code-quality.md does not list it")
        })
        .collect();
    failures.extend(listed.difference(&used).map(|(krate, path)| {
        format!("{path}: listed for {krate} in docs/code-quality.md, but uses no unsafe")
    }));
    Ok(failures)
}

/// Crate names in the tables of "Runtime dependencies" and "Tests and
/// development tools".
pub(crate) fn admitted(dependencies: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for heading in ["Runtime dependencies", "Tests and development tools"] {
        let lines = section(dependencies, heading)
            .ok_or(format!("docs/dependencies.md has no \"{heading}\" section"))?;
        for cells in table_rows(&lines) {
            let first = cells.first().map_or("", String::as_str);
            names.extend(first.split(',').map(unticked_trimmed).filter(|w| {
                !w.is_empty()
                    && w.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            }));
        }
    }
    Ok(names)
}

fn unticked_trimmed(part: &str) -> String {
    unticked(part.trim())
}

/// Failures for each (crate, dependency) pair whose dependency is not listed.
pub(crate) fn unlisted(
    dependencies: &BTreeSet<(String, String)>,
    markdown: &str,
) -> Result<Vec<String>, String> {
    let listed = admitted(markdown)?;
    Ok(dependencies
        .iter()
        .filter(|(_, dep)| !listed.contains(dep))
        .map(|(krate, dep)| {
            format!("{krate} depends on {dep}, which docs/dependencies.md does not list")
        })
        .collect())
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
