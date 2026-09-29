//! The docs check (`docs/ci.md`, "The docs check"): relative Markdown links
//! and anchors that do not resolve, section citations naming a heading the
//! file does not have, and backticked repository paths that do not exist.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

const CHECKED: [&str; 3] = ["CONTEXT.md", "AGENTS.md", "README.md"];
const PATH_ROOTS: [&str; 5] = ["docs/", "crates/", "scripts/", "research/", ".github/"];

/// `markdown` with fenced code blocks blanked, line numbers kept.
pub(crate) fn prose(markdown: &str) -> String {
    let mut fenced = false;
    let lines: Vec<&str> = markdown
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
                ""
            } else if fenced {
                ""
            } else {
                line
            }
        })
        .collect();
    lines.join("\n")
}

/// The text of every heading outside fenced code.
pub(crate) fn headings(markdown: &str) -> Vec<String> {
    prose(markdown)
        .lines()
        .filter_map(|line| {
            let hashes = line.chars().take_while(|c| *c == '#').count();
            let rest = line.get(hashes..)?;
            ((1..=6).contains(&hashes) && rest.starts_with([' ', '\t']))
                .then(|| rest.trim().trim_end_matches('#').trim_end().to_owned())
        })
        .collect()
}

/// GitHub's anchor for a heading.
pub(crate) fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

pub(crate) fn anchors(markdown: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    headings(markdown)
        .iter()
        .map(|heading| {
            let base = slug(heading);
            let count = seen.entry(base.clone()).or_insert(0);
            let anchor = if *count == 0 {
                base
            } else {
                format!("{base}-{count}")
            };
            *count += 1;
            anchor
        })
        .collect()
}

/// Inline code spans: (offset of the opening backtick, content). A span
/// whose content `accept` rejects is not consumed, so its closing backtick
/// can open the next one.
fn code_spans(text: &str, accept: impl Fn(&str) -> bool) -> Vec<(usize, &str)> {
    let mut ticks = text.match_indices('`').map(|(i, _)| i).peekable();
    let mut spans = Vec::new();
    while let Some(open) = ticks.next() {
        let Some(&close) = ticks.peek() else { break };
        let content = text.get(open + 1..close).unwrap_or_default();
        if accept(content) {
            spans.push((open, content));
            ticks.next();
        }
    }
    spans
}

/// The quoted heading after a cited file: `, "Heading"`, whitespace (line
/// breaks included) normalised.
fn cited_heading(after: &str) -> Option<String> {
    let rest = after.strip_prefix(',')?;
    let quoted = rest.trim_start();
    if quoted.len() == rest.len() {
        return None;
    }
    let inner = quoted.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(
        inner
            .get(..end)?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Inline link targets: (offset, target) for each `](target)`.
fn inline_links(text: &str) -> Vec<(usize, &str)> {
    text.match_indices("](")
        .filter_map(|(i, _)| {
            let rest = text.get(i + 2..)?;
            let end = rest.find(|c: char| c == ')' || c.is_whitespace())?;
            Some((i, rest.get(..end)?))
        })
        .filter(|(_, target)| !target.is_empty())
        .collect()
}

fn normalise_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Reference definitions, `[label]: target` at the start of a line:
/// (offset, normalised label, target).
fn reference_definitions(text: &str) -> Vec<(usize, String, &str)> {
    let mut offset = 0;
    let mut found = Vec::new();
    for line in text.split('\n') {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if let Some(rest) = line.trim_start_matches(' ').strip_prefix('[')
            && indent <= 3
            && let Some((label, after)) = rest.split_once("]:")
            && let Some(target) = after.split_whitespace().next()
        {
            found.push((
                offset + indent,
                normalise_label(label),
                target.trim_start_matches('<').trim_end_matches('>'),
            ));
        }
        offset += line.len() + 1;
    }
    found
}

/// Full and collapsed reference links, `[text][label]` and `[text][]`:
/// (offset, normalised label).
fn reference_uses(text: &str) -> Vec<(usize, String)> {
    text.match_indices("][")
        .filter_map(|(i, _)| {
            let label = text.get(i + 2..)?;
            let end = label.find(']')?;
            let label = label.get(..end)?;
            if label.contains('\n') || label.contains('[') {
                return None;
            }
            if !label.trim().is_empty() {
                return Some((i, normalise_label(label)));
            }
            let before = text.get(..i)?;
            let open = before.rfind('[')?;
            Some((i, normalise_label(before.get(open + 1..)?)))
        })
        .collect()
}

fn is_external(target: &str) -> bool {
    let scheme: String = target
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        .collect();
    !scheme.is_empty()
        && target
            .get(scheme.len()..)
            .is_some_and(|rest| rest.starts_with(':'))
}

/// `base` joined with `rel`, with `.` and `..` resolved.
fn join(base: &Path, rel: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in base.join(rel).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(component),
        }
    }
    out
}

fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map_or(0, |before| before.matches('\n').count())
        + 1
}

/// Failures in the Markdown file at `path`, relative to `root`.
pub(crate) fn check_file(root: &Path, path: &str) -> Result<Vec<String>, String> {
    let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let file = root.join(path);
    let dir = file.parent().unwrap_or(root).to_path_buf();
    let text = prose(&read(&file)?);
    let mut failures = Vec::new();
    let mut fail = |offset: usize, message: String| {
        failures.push(format!("{path}:{}: {message}", line_of(&text, offset)))
    };

    let definitions = reference_definitions(&text);
    let labels: BTreeSet<&str> = definitions
        .iter()
        .map(|(_, label, _)| label.as_str())
        .collect();
    for (offset, label) in reference_uses(&text) {
        if !labels.contains(label.as_str()) {
            fail(offset, format!("reference [{label}]: no definition"));
        }
    }
    let links = inline_links(&text).into_iter().chain(
        definitions
            .iter()
            .map(|(offset, _, target)| (*offset, *target)),
    );
    for (offset, target) in links {
        if is_external(target) {
            continue;
        }
        let (file_part, anchor) = target.split_once('#').unwrap_or((target, ""));
        let resolved = if file_part.is_empty() {
            file.clone()
        } else {
            join(&dir, file_part)
        };
        if !resolved.exists() {
            fail(offset, format!("link to {target}: no such file"));
        } else if !anchor.is_empty()
            && resolved.extension().is_some_and(|e| e == "md")
            && !anchors(&read(&resolved)?).contains(anchor)
        {
            fail(offset, format!("link to {target}: no such anchor"));
        }
    }

    let md_span = |s: &str| s.ends_with(".md") && !s.contains(char::is_whitespace);
    for (offset, cited) in code_spans(&text, md_span) {
        let after = text.get(offset + cited.len() + 2..).unwrap_or_default();
        let Some(heading) = cited_heading(after) else {
            continue;
        };
        let target = [root.join(cited), join(&dir, cited)]
            .into_iter()
            .find(|c| c.is_file());
        match target {
            None => fail(offset, format!("citation of {cited}: no such file")),
            Some(target) if !headings(&read(&target)?).contains(&heading) => {
                fail(
                    offset,
                    format!("citation of {cited}, \"{heading}\": no such heading"),
                );
            }
            Some(_) => {}
        }
    }

    let repo_path = |s: &str| PATH_ROOTS.iter().any(|r| s.starts_with(r));
    for (offset, cited) in code_spans(&text, repo_path) {
        let placeholder =
            cited.contains(|c: char| matches!(c, '<' | '>' | '*' | '{' | '}') || c.is_whitespace());
        if !placeholder && !root.join(cited).exists() {
            fail(offset, format!("path {cited}: does not exist"));
        }
    }
    Ok(failures)
}

/// The files the docs check covers, relative to `root`.
pub(crate) fn checked_files(root: &Path) -> Result<Vec<String>, String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else if path.extension().is_some_and(|e| e == "md")
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(&root.join("docs"), root, &mut files)?;
    files.sort();
    files.extend(
        CHECKED
            .iter()
            .filter(|f| root.join(f).exists())
            .map(|f| (*f).to_owned()),
    );
    Ok(files)
}

#[cfg(test)]
#[path = "docs_tests.rs"]
mod tests;
