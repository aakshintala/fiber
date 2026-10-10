//! Checks that the code matches the rules and tables in the docs: the
//! 800-line report (`docs/code-quality.md`, "Size"), the `unsafe` table in
//! `docs/code-quality.md`, the crate list in `docs/dependencies.md`, and
//! that process signals stay in the guarded helpers.

use std::collections::BTreeSet;

use proc_macro2::{TokenStream, TokenTree};

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
            (lines > LINE_CAP).then(|| format!("{}: {lines} lines, over {LINE_CAP}", f.path))
        })
        .collect()
}

/// Crates only some workspace members may link.
pub(crate) struct Isolation {
    /// The crates kept out of every other member's normal dependency tree.
    pub(crate) crates: &'static [&'static str],
    /// The members that may link them.
    pub(crate) members: &'static [&'static str],
    /// What a failure says after the crate it found.
    pub(crate) why: &'static str,
}

/// The crates only the image child links (`docs/dependencies.md`, "Crates
/// used only by the image child").
pub(crate) const IMAGE: Isolation = Isolation {
    crates: &["image", "fast_image_resize", "lopdf"],
    members: &["picture", "main"],
    why: "only the image child links image code",
};

/// The crates admitted only for `tui` (`docs/dependencies.md`, "Admitting
/// a crate"): no other Fiber crate depends on them.
pub(crate) const TUI: Isolation = Isolation {
    crates: &["ratatui", "crossterm"],
    members: &["tui", "main"],
    why: "only the terminal links terminal UI code",
};

/// Each failure where a workspace member's normal dependency tree names
/// one of `rule`'s crates. `trees` pairs a member with the output of `cargo
/// tree -p MEMBER -e normal --prefix none`: one line per crate, the crate's
/// name first. `rule`'s members are exempt.
pub(crate) fn leaks(trees: &[(String, String)], rule: &Isolation) -> Vec<String> {
    let mut failures = BTreeSet::new();
    for (member, tree) in trees {
        if rule.members.contains(&member.as_str()) {
            continue;
        }
        for name in tree
            .lines()
            .filter_map(|line| line.split_whitespace().next())
        {
            if rule.crates.contains(&name) {
                failures.insert(format!(
                    "{member}: its normal dependency tree holds {name}; {}",
                    rule.why
                ));
            }
        }
    }
    failures.into_iter().collect()
}

/// Whether Rust `source` uses the `unsafe` keyword. proc-macro2 tokenises
/// it, so comments, literals and doc comments are never read as code.
pub(crate) fn uses_unsafe(source: &str) -> Result<bool, proc_macro2::LexError> {
    fn walk(stream: TokenStream) -> bool {
        stream.into_iter().any(|tree| match tree {
            TokenTree::Ident(ident) => ident == "unsafe",
            TokenTree::Group(group) => walk(group.stream()),
            TokenTree::Punct(_) | TokenTree::Literal(_) => false,
        })
    }
    Ok(walk(source.parse()?))
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
    let mut used = BTreeSet::new();
    for f in files {
        // A file that does not tokenise could hide `unsafe`, so it fails the
        // check rather than being skipped.
        if uses_unsafe(&f.source)
            .map_err(|e| format!("{}: does not tokenise as Rust: {e}", f.path))?
        {
            used.insert((f.krate.clone(), f.path.clone()));
        }
    }
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

/// Files that may send a process signal. An entry must refuse an id of 1 or
/// less before anything runs: group -1 reaches every process the user owns.
const SIGNAL_ALLOWLIST: &[&str] = &[
    "crates/extensions/src/host/exec.rs",
    "crates/fakes/src/process_group.rs",
    "crates/cli/src/extension_test.rs",
    "crates/jobs/src/delegate/group.rs",
    "crates/main/src/switch/read.rs",
    "crates/mcp/src/registry.rs",
    "crates/tui/src/paste_image.rs",
    "crates/tools/src/shell/process_group.rs",
    "crates/support/src/group.rs",
];

/// What counts as a process signal: the pattern, and whether it only counts
/// beside the word kill, on its own line or in the backslash-continued
/// lines around it. A bare double dash also starts an operand such as
/// find's `-delete`, so the shell fragments need their kill. The strings
/// are joined from halves so this file holds no pattern itself; otherwise
/// the check would fail on its own source.
const SIGNAL_PATTERNS: &[(&str, bool)] = &[
    (concat!("Command::", "new(\"kill\")"), false),
    (concat!("--", " -"), true),
    (concat!("\"-", "$"), true),
    (concat!("kill_process_", "group"), false),
    (concat!("kill_process", "("), false),
    (concat!("kill", "pg"), false),
    (concat!("libc::", "kill"), false),
    (concat!("nix::sys::", "signal"), false),
];

pub(crate) fn signal_sites(files: &[RustFile]) -> Vec<String> {
    let mut failures = Vec::new();
    for f in files {
        if SIGNAL_ALLOWLIST.contains(&f.path.as_str()) {
            continue;
        }
        let lines: Vec<&str> = f.source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for &(pattern, needs_kill) in SIGNAL_PATTERNS {
                if !line.contains(pattern) {
                    continue;
                }
                if needs_kill && !chain_has_kill(&lines, index) {
                    continue;
                }
                failures.push(format!("{}:{}: {pattern}", f.path, index + 1));
            }
        }
    }
    failures
}

/// Whether `line` continues a shell command from the line before it: the
/// earlier text ends with a backslash, possibly followed by the closing
/// quote or a newline escape.
fn continues(line: &str) -> bool {
    let trimmed = line.trim_end();
    let trimmed = trimmed.strip_suffix('"').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("\\n").unwrap_or(trimmed);
    trimmed.ends_with('\\')
}

/// Whether the word kill is on the fragment's own line or on an earlier line
/// the fragment continues through backslashes. A command names `kill` before
/// its arguments, so only earlier lines count.
fn chain_has_kill(lines: &[&str], index: usize) -> bool {
    let (earlier, rest) = lines.split_at(index.min(lines.len()));
    let earlier = earlier.iter().rev().take_while(|line| continues(line));
    rest.iter()
        .take(1)
        .chain(earlier)
        .any(|line| line.contains("kill"))
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
