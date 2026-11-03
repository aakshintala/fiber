//! The docs check (`docs/ci.md`, "The docs check"): relative Markdown links
//! and anchors that do not resolve, section citations naming a heading the
//! file does not have, and backticked repository paths that do not exist.
//! pulldown-cmark reads the Markdown; the citation grammar is Fiber's own.
//! It also serves the code scan, which resolves heading citations through
//! the same index.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{BrokenLink, Event, LinkType, Options, Parser, Tag, TagEnd};

const CHECKED: [&str; 3] = ["GLOSSARY.md", "AGENTS.md", "README.md"];
const PATH_ROOTS: [&str; 5] = ["docs/", "crates/", "scripts/", "research/", ".github/"];

/// What the checks need from one Markdown file. Offsets are into its source.
#[derive(Debug, Default)]
struct Parsed {
    /// Each heading as written, without its `#` marks, and as rendered text,
    /// with the byte offset where the heading starts.
    headings: Vec<(usize, String, String)>,
    /// Inline link and image destinations, and reference definitions.
    links: Vec<(usize, String)>,
    /// Full and collapsed references whose label has no definition.
    broken: Vec<(usize, String)>,
    /// Inline code spans: where each starts and ends, and its text.
    code: Vec<(usize, usize, String)>,
}

fn parse(markdown: &str) -> Parsed {
    let broken = RefCell::new(Vec::new());
    // A shortcut `[x]` with no definition is plain text, not a link.
    let callback = |link: BrokenLink<'_>| {
        if matches!(link.link_type, LinkType::Reference | LinkType::Collapsed) {
            let label = link
                .reference
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            broken.borrow_mut().push((link.span.start, label));
        }
        None
    };
    let parser = Parser::new_with_broken_link_callback(markdown, Options::empty(), Some(callback))
        .into_offset_iter();
    let mut parsed = Parsed {
        links: parser
            .reference_definitions()
            .iter()
            .map(|(_, d)| (d.span.start, d.dest.to_string()))
            .collect(),
        ..Parsed::default()
    };
    let mut heading: Option<(usize, String, String)> = None;
    for (event, range) in parser {
        if let Event::Start(Tag::Heading { .. }) = &event {
            let start = range.start;
            let source = markdown
                .get(range.clone())
                .and_then(|s| s.lines().next())
                .unwrap_or_default();
            let written = source
                .trim()
                .trim_start_matches('#')
                .trim()
                .trim_end_matches('#')
                .trim_end();
            heading = Some((start, written.to_owned(), String::new()));
        } else if let Event::End(TagEnd::Heading(_)) = &event {
            parsed.headings.extend(heading.take());
        } else if let Event::Start(
            Tag::Link {
                link_type: LinkType::Inline,
                dest_url,
                ..
            }
            | Tag::Image {
                link_type: LinkType::Inline,
                dest_url,
                ..
            },
        ) = &event
        {
            parsed.links.push((range.start, dest_url.to_string()));
        } else if let Event::Code(text) = &event {
            parsed.code.push((range.start, range.end, text.to_string()));
        }
        if let (Some((_, _, rendered)), Event::Text(text) | Event::Code(text)) =
            (heading.as_mut(), &event)
        {
            rendered.push_str(text);
        }
    }
    parsed.broken = broken.into_inner();
    parsed
}

/// One parse of a doc: each heading as written, with its 1-based line.
pub(crate) struct HeadingIndex {
    names: Vec<String>,
    lines: Vec<usize>,
}

impl HeadingIndex {
    /// Every heading as written, in document order.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// The nearest heading starting at or above the 1-based line.
    pub(crate) fn above(&self, line: usize) -> Option<&str> {
        let mut out: Option<&str> = None;
        for (name, start) in self.names.iter().zip(self.lines.iter()) {
            if *start <= line {
                out = Some(name.as_str());
            } else {
                break;
            }
        }
        out
    }
}

/// The shared parse of a doc; both the Markdown path and the code scan query
/// this index. The code scan caches one index per target for the run, so a
/// cited doc is read and parsed once there; `check_file` builds one per
/// citation it checks.
pub(crate) fn heading_index(markdown: &str) -> HeadingIndex {
    let parsed = parse(markdown);
    let mut names = Vec::with_capacity(parsed.headings.len());
    let mut lines = Vec::with_capacity(parsed.headings.len());
    for (offset, written, _) in parsed.headings {
        lines.push(line_of(markdown, offset));
        names.push(written);
    }
    HeadingIndex { names, lines }
}

/// Every heading as written, which is what a citation quotes.
pub(crate) fn headings(markdown: &str) -> Vec<String> {
    heading_index(markdown).names().to_vec()
}

/// The failure for a heading citation, shared by the Markdown path and the
/// code scan so the two message texts exist once. `None` found means the
/// cited file is absent.
pub(crate) fn heading_failure(
    cited: &str,
    heading: &str,
    found: Option<&[String]>,
) -> Option<String> {
    match found {
        None => Some(format!("citation of {cited}: no such file")),
        Some(names) if !names.contains(&heading.to_owned()) => Some(format!(
            "citation of {cited}, \"{heading}\": no such heading"
        )),
        Some(_) => None,
    }
}

/// GitHub's anchor for a heading's rendered text.
pub(crate) fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

pub(crate) fn anchors(markdown: &str) -> BTreeSet<String> {
    heading_ids(markdown).into_iter().collect()
}

/// GitHub's anchor for each heading, in document order, with the same
/// dedup suffixes `anchors` collects, so a rendered id never 404s.
pub(crate) fn heading_ids(markdown: &str) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    parse(markdown)
        .headings
        .iter()
        .map(|(_, _, rendered)| {
            let base = slug(rendered);
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

/// The quoted heading after a cited file: `, "Heading"`, whitespace (line
/// breaks included) normalised. Returns the heading and the byte length of
/// `after` consumed through the closing quote, so the code scan can cut the
/// key from the source.
pub(crate) fn cited_heading(after: &str) -> Option<(String, usize)> {
    let rest = after.strip_prefix(',')?;
    let quoted = rest.trim_start();
    let spaces = rest.len() - quoted.len();
    if spaces == 0 {
        return None;
    }
    let inner = quoted.strip_prefix('"')?;
    let end = inner.find('"')?;
    let heading = inner
        .get(..end)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some((heading, 1 + spaces + 1 + end + 1))
}

pub(crate) fn is_external(target: &str) -> bool {
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
pub(crate) fn join(base: &Path, rel: &str) -> PathBuf {
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

/// Failures in the Markdown file at `path`, relative to `root`, in source
/// order.
pub(crate) fn check_file(root: &Path, path: &str) -> Result<Vec<String>, String> {
    let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let file = root.join(path);
    let dir = file.parent().unwrap_or(root).to_path_buf();
    let text = read(&file)?;
    let parsed = parse(&text);
    let mut failures = Vec::new();
    let mut fail = |offset: usize, message: String| {
        failures.push((
            offset,
            format!("{path}:{}: {message}", line_of(&text, offset)),
        ));
    };

    for (offset, label) in &parsed.broken {
        fail(*offset, format!("reference [{label}]: no definition"));
    }
    for (offset, target) in &parsed.links {
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
            fail(*offset, format!("link to {target}: no such file"));
        } else if !anchor.is_empty()
            && resolved.extension().is_some_and(|e| e == "md")
            && !anchors(&read(&resolved)?).contains(anchor)
        {
            fail(*offset, format!("link to {target}: no such anchor"));
        }
    }

    for (start, end, cited) in &parsed.code {
        let is_markdown_file = cited.ends_with(".md") && !cited.contains(char::is_whitespace);
        if is_markdown_file
            && let Some((heading, _)) = cited_heading(text.get(*end..).unwrap_or_default())
        {
            let target = [root.join(cited), join(&dir, cited)]
                .into_iter()
                .find(|c| c.is_file());
            let found = match &target {
                None => None,
                Some(target) => Some(heading_index(&read(target)?).names().to_vec()),
            };
            if let Some(message) = heading_failure(cited, &heading, found.as_deref()) {
                fail(*start, message);
            }
        }
        let placeholder =
            cited.contains(|c: char| matches!(c, '<' | '>' | '*' | '{' | '}') || c.is_whitespace());
        if PATH_ROOTS.iter().any(|r| cited.starts_with(r))
            && !placeholder
            && !root.join(cited).exists()
        {
            fail(*start, format!("path {cited}: does not exist"));
        }
    }
    failures.sort_by_key(|(offset, _)| *offset);
    Ok(failures.into_iter().map(|(_, line)| line).collect())
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
