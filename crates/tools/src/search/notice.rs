//! The one-line note naming skipped top-level directories
//! (`docs/tools.md`, "Search", "What it skips").
//!
//! When a search finds nothing and skipped ignored directories on the way,
//! one line on standard error names the directories skipped at the top
//! level, so the model can search them by name. A search that prints a
//! result prints no notice.

use std::collections::BTreeSet;
use std::path::Path;

/// The top-level directories of `fs_dir` the walk skipped for ignore
/// reasons: each base name with a trailing slash, sorted, without the
/// version-control directories. Found by comparing a depth-1 walk with the
/// ignore rules on against one with them off; computed only when a search
/// printed nothing, so the second walk costs nothing otherwise.
pub(crate) fn skipped(cwd: &Path, fs_dir: &Path) -> Vec<String> {
    let mut kept = BTreeSet::new();
    let root = super::walk::DirRoot {
        walk: fs_dir.to_path_buf(),
        show: fs_dir.to_path_buf(),
    };
    for entry in super::walk::walk(cwd, &root, Some(1)) {
        let found = match entry {
            Ok(found) => found,
            Err(_) => return Vec::new(),
        };
        if found.depth == 1 {
            kept.insert(found.display);
        }
    }
    let mut skipped = BTreeSet::new();
    let mut builder = ignore::WalkBuilder::new(fs_dir);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(1))
        .current_dir(cwd.to_path_buf())
        .sort_by_file_path(|first, second| first.cmp(second));
    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return Vec::new(),
        };
        if entry.depth() != 1 {
            continue;
        }
        let kind = match entry.file_type() {
            Some(kind) => kind,
            None => continue,
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        // The filtered walk prints below the same root, so names compare.
        if kept.contains(entry.path()) || super::walk::is_vcs(entry.file_name()) {
            continue;
        }
        skipped.insert(entry.file_name().to_owned());
    }
    skipped
        .into_iter()
        .map(|name| format!("{}/", name.to_string_lossy()))
        .collect()
}

/// The `grep` notice: names `skipped`, or nothing when none was skipped.
/// `pattern` is the search pattern as given.
pub(crate) fn grep_line(pattern: &str, skipped: &[String]) -> Option<String> {
    let [first, ..] = skipped else {
        return None;
    };
    Some(format!(
        "grep: no match. Skipped ignored directories: {}. Search one by name, such as `grep -r {pattern} {}`.",
        skipped.join(", "),
        first.trim_end_matches('/')
    ))
}

/// The `find` notice: the same line with `find` in place of `grep`.
pub(crate) fn find_line(skipped: &[String]) -> Option<String> {
    let [first, ..] = skipped else {
        return None;
    };
    Some(format!(
        "find: no match. Skipped ignored directories: {}. Search one by name, such as `find {}`.",
        skipped.join(", "),
        first.trim_end_matches('/')
    ))
}

/// Joins one search's skipped directories: sorted, without repeats.
pub(crate) fn union(first: Vec<String>, second: Vec<String>) -> Vec<String> {
    let mut all: BTreeSet<String> = first.into_iter().collect();
    all.extend(second);
    all.into_iter().collect()
}

#[cfg(test)]
#[path = "notice_tests.rs"]
mod tests;
