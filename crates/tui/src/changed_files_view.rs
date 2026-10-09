//! Changed files' ranking, diff command and swapped view (`docs/tui.md`,
//! "Swapped views").

use std::collections::BTreeMap;

use contract::events::CommandResult;

use crate::format;
use crate::swapped::{Frame, Ink, List};

/// Every changed path, most lines changed first, ties by path ascending.
pub(crate) fn ranked(changes: &BTreeMap<String, (u64, u64)>) -> Vec<(&str, u64, u64)> {
    let mut paths: Vec<(&str, u64, u64)> = changes
        .iter()
        .map(|(path, (added, removed))| (path.as_str(), *added, *removed))
        .collect();
    paths.sort_by(|a, b| {
        b.1.saturating_add(b.2)
            .cmp(&a.1.saturating_add(a.2))
            .then_with(|| a.0.cmp(b.0))
    });
    paths
}

/// The shell line for `path`'s diff against HEAD (docs/tui.md, "Swapped
/// views"). Single quotes keep paths literal to the shell; embedded quotes
/// close, escape a quote, and reopen the word.
pub(crate) fn diff_command(path: &str) -> String {
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    format!(
        "git --literal-pathspecs ls-files --error-unmatch -- {quoted} >/dev/null 2>&1 && git --no-optional-locks --literal-pathspecs diff --no-color --no-ext-diff HEAD -- {quoted} || git --no-optional-locks diff --no-color --no-ext-diff --no-index -- /dev/null {quoted}"
    )
}

/// The diff currently being read, read, empty or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Diff {
    /// A shell command is outstanding.
    Reading,
    /// Output lines and the full-output artifact, if cut.
    Lines {
        lines: Vec<String>,
        cut: Option<String>,
    },
    /// The command succeeded and found no diff.
    Empty,
    /// The command could not be read, or its rejection message.
    Failed(String),
}

/// Folds a shell answer into a safe diff display.
pub(crate) fn answer(result: Option<&CommandResult>) -> Diff {
    let Some(CommandResult::Shell {
        output,
        artifact,
        process,
    }) = result
    else {
        return failed();
    };
    if !output.is_empty() {
        return Diff::Lines {
            lines: output.lines().map(safe_line).collect(),
            cut: artifact.as_deref().map(safe_line),
        };
    }
    if process.signal.is_none() && process.exit_code == Some(0) {
        Diff::Empty
    } else {
        failed()
    }
}

/// Builds the file list or the selected file's diff frame.
pub(crate) fn frame(
    changes: &BTreeMap<String, (u64, u64)>,
    chosen: Option<(&str, &Diff)>,
    list: List,
) -> Frame {
    if let Some((path, diff)) = chosen {
        return diff_frame(path, diff, list);
    }
    let paths = ranked(changes);
    let rows = paths
        .iter()
        .map(|(path, added, removed)| {
            vec![(format!("{}  +{added} −{removed}", safe_line(path)), None, Ink::Plain)]
        })
        .collect();
    let (files, added, removed) = totals(changes);
    Frame {
        title: "Changed files".to_owned(),
        rows,
        list,
        below: vec![format!(
            "{} changed  +{added} −{removed}",
            format::count(files, "file", "files")
        )],
        field: None,
        footer: "↑↓ move · Enter show diff · Esc close".to_owned(),
    }
}

/// The current diff frame, with its file in the title.
fn diff_frame(path: &str, diff: &Diff, list: List) -> Frame {
    let (rows, below) = match diff {
        Diff::Reading => (
            vec![vec![("Reading the diff…".to_owned(), None, Ink::Plain)]],
            Vec::new(),
        ),
        Diff::Lines { lines, cut } => (
            lines
                .iter()
                .map(|line| vec![(line.clone(), None, Ink::Plain)])
                .collect(),
            cut.iter()
                .map(|artifact| format!("Cut: the whole diff is in {artifact}"))
                .collect(),
        ),
        Diff::Empty => (
            vec![vec![("No changes against HEAD.".to_owned(), None, Ink::Plain)]],
            Vec::new(),
        ),
        Diff::Failed(message) => (vec![vec![(safe_line(message), None, Ink::Plain)]], Vec::new()),
    };
    Frame {
        title: format!("Changed files › {} · diff against HEAD", safe_line(path)),
        rows,
        list,
        below,
        field: None,
        footer: "↑↓ scroll · ← files · Esc close".to_owned(),
    }
}

/// Counts all changed paths and their added and removed lines.
fn totals(changes: &BTreeMap<String, (u64, u64)>) -> (u64, u64, u64) {
    changes
        .values()
        .fold((0u64, 0u64, 0u64), |(files, added, removed), (a, r)| {
            (
                files.saturating_add(1),
                added.saturating_add(*a),
                removed.saturating_add(*r),
            )
        })
}

/// Removes terminal controls from a line; tabs use four spaces so file
/// content cannot send terminal escapes through the view.
fn safe_line(line: &str) -> String {
    let mut safe = String::with_capacity(line.len());
    for ch in line.chars() {
        if ch == '\t' {
            safe.push_str("    ");
        } else if !ch.is_control() {
            safe.push(ch);
        }
    }
    safe
}

/// The standard unreadable-diff answer.
fn failed() -> Diff {
    Diff::Failed("The diff could not be read.".to_owned())
}

#[cfg(test)]
#[path = "changed_files_view_tests.rs"]
mod tests;
