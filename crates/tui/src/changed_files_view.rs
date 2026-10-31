//! Changed files' ranking, diff command and swapped view (`docs/tui.md`,
//! "Swapped views").

use std::collections::{BTreeMap, BTreeSet};

use contract::events::CommandResult;

use crate::diff::{DiffLine, diff_lines, gutter_prefix, gutter_width, ink_spans, safe_line};
use crate::format;
use crate::swapped::{Frame, Ink, List, Spot};

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

/// The shell line for the view-open figures query (`docs/tui.md`,
/// "Swapped views"): one `git diff --numstat HEAD` limited to the
/// listed paths. Single quotes keep paths literal to the shell;
/// embedded quotes close, escape a quote, and reopen the word, as in
/// `diff_command`. A missing git, a non-repository cwd, a non-zero
/// exit or unparsable output all mean the same to the caller: no
/// answer, so the rows keep the card's figures.
pub(crate) fn numstat_command(paths: &[&str]) -> String {
    let quoted: Vec<String> = paths
        .iter()
        .map(|path| format!("'{}'", path.replace('\'', "'\\''")))
        .collect();
    format!(
        "git --no-optional-locks --literal-pathspecs diff --numstat --no-color --no-ext-diff HEAD -- {}",
        quoted.join(" ")
    )
}

/// One numstat row's figure: counted lines, or a binary path git
/// reports as dashes in both columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumstatFigure {
    Counted(u64, u64),
    Binary,
}

/// Folds `git diff --numstat HEAD` output into per-path figures.
/// Binary rows and unparsable lines are dropped: binary paths keep
/// the card's figures and garbage never renames a row.
pub(crate) fn parse_numstat(output: &str) -> BTreeMap<String, NumstatFigure> {
    let mut figures = BTreeMap::new();
    for line in output.lines() {
        let mut columns = line.split('\t');
        let (Some(added), Some(removed), Some(path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let figure = match (added.parse::<u64>(), removed.parse::<u64>()) {
            (Ok(added), Ok(removed)) => NumstatFigure::Counted(added, removed),
            _ if added == "-" && removed == "-" => NumstatFigure::Binary,
            _ => continue,
        };
        figures.insert(path.to_owned(), figure);
    }
    figures
}

/// The answer's numstat lines: the shell tool appends its closing
/// "Exit code N." line to the command's own output, which is the
/// driver's, not git's, and never parses. Strips that closing line
/// only, when it names the answer's own exit code; anything else
/// parses or rejects as it reads.
pub(crate) fn numstat_body(output: &str, exit_code: Option<i32>) -> &str {
    let Some(code) = exit_code else {
        return output;
    };
    let closing = format!("Exit code {code}.");
    let trimmed = output.strip_suffix('\n').unwrap_or(output);
    match trimmed.strip_suffix(&closing) {
        Some(body) => body,
        None => output,
    }
}

/// Whether every line of a numstat answer parses (empty lines
/// aside): anything else is not an answer the rows may show, so
/// the fold stores nothing and the rows keep the card's figures.
pub(crate) fn numstat_fully_parsed(output: &str) -> bool {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .all(|line| !parse_numstat(line).is_empty())
}

/// The answered figures query: git's raw numstat output plus the
/// workspace line counts for the paths it omits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NumstatAnswer {
    pub(crate) raw: String,
    pub(crate) lines: BTreeMap<String, u64>,
}

/// Displayed figures for the card's paths, with the paths the
/// numstat answer omits marked untracked for the row form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedFigures {
    pub(crate) figures: BTreeMap<String, (u64, u64)>,
    pub(crate) untracked: BTreeSet<String>,
}

/// Counts lines for `paths` under `root`: the added figure for
/// paths git does not track. Unreadable files are skipped, so they
/// keep the card's figures and the view still opens.
pub(crate) fn count_lines(root: &std::path::Path, paths: &[&str]) -> BTreeMap<String, u64> {
    paths
        .iter()
        .filter_map(|path| {
            let text = std::fs::read(root.join(path)).ok()?;
            let text = String::from_utf8(text).ok()?;
            Some((
                (*path).to_owned(),
                u64::try_from(text.lines().count()).unwrap_or(u64::MAX),
            ))
        })
        .collect()
}

/// Displayed figures for the card's paths: git's where the numstat
/// answer counts the path, the card's where git calls it binary, the
/// untracked line count where the answer omits the path, and the
/// card's everywhere else. `None` is no answer yet: until the
/// numstat answer arrives, and whenever it never does, every row
/// shows the card's figures. An answered path git omits is
/// untracked, per the doc: numstat never lists one.
///
/// Git prints paths relative to the repository root while the card
/// holds workspace paths, so a card path also matches by its
/// workspace-relative form; anything else keeps the card's figures.
pub(crate) fn resolve_figures(
    changes: &BTreeMap<String, (u64, u64)>,
    numstat: Option<&str>,
    untracked_lines: &BTreeMap<String, u64>,
    workspace: Option<&std::path::Path>,
) -> ResolvedFigures {
    let parsed = numstat.map(parse_numstat).unwrap_or_default();
    let answered = numstat.is_some();
    let mut figures = BTreeMap::new();
    let mut untracked = BTreeSet::new();
    for (path, card) in changes.iter() {
        let figure = match lookup(&parsed, path, workspace) {
            Some(NumstatFigure::Counted(added, removed)) => (*added, *removed),
            Some(NumstatFigure::Binary) => *card,
            None if answered => match untracked_lines.get(path) {
                Some(lines) => {
                    untracked.insert(path.clone());
                    (*lines, 0)
                }
                None => *card,
            },
            None => *card,
        };
        figures.insert(path.clone(), figure);
    }
    ResolvedFigures { figures, untracked }
}

/// The answer's figure for a card path: the path as listed, or its
/// workspace-relative form for git's repository-relative output.
pub(crate) fn lookup<'a>(
    parsed: &'a BTreeMap<String, NumstatFigure>,
    path: &str,
    workspace: Option<&std::path::Path>,
) -> Option<&'a NumstatFigure> {
    if let Some(figure) = parsed.get(path) {
        return Some(figure);
    }
    let relative = std::path::Path::new(path).strip_prefix(workspace?).ok()?;
    parsed.get(relative.to_str()?)
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

/// Builds the file list or the selected file's diff frame. The list
/// rows show `figures` where answered, the card's figures until
/// then; an untracked path draws its line count with
/// "· untracked, wholly added" dim.
pub(crate) fn frame(
    changes: &BTreeMap<String, (u64, u64)>,
    figures: Option<&ResolvedFigures>,
    chosen: Option<(&str, &Diff)>,
    list: List,
) -> Frame {
    if let Some((path, diff)) = chosen {
        return diff_frame(path, diff, list);
    }
    let resolved;
    let figures = match figures {
        Some(figures) => figures,
        None => {
            resolved = ResolvedFigures {
                figures: changes.clone(),
                untracked: BTreeSet::new(),
            };
            &resolved
        }
    };
    let paths = ranked(&figures.figures);
    let rows = paths
        .iter()
        .map(|(path, added, removed)| {
            if figures.untracked.contains(*path) {
                return vec![
                    (safe_line(path), None, Ink::Heading),
                    (" · ".to_owned(), None, Ink::Muted),
                    ("untracked, wholly added".to_owned(), None, Ink::Muted),
                ];
            }
            vec![
                (safe_line(path), None, Ink::Heading),
                (" · ".to_owned(), None, Ink::Muted),
                (
                    format!("+{added}"),
                    None,
                    Ink::Role(crate::theme::Role::Added),
                ),
                (" ".to_owned(), None, Ink::Plain),
                (
                    format!("−{removed}"),
                    None,
                    Ink::Role(crate::theme::Role::Removed),
                ),
            ]
        })
        .collect();
    let (files, added, removed) = totals(&figures.figures);
    Frame {
        joins: Vec::new(),
        title: "Changed files".to_owned(),
        command: "diff against HEAD".to_owned(),
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
        Diff::Lines { lines, cut } => {
            let drawn = diff_lines(&lines.join("\n"));
            let gutter = gutter_width(drawn.iter().filter_map(|line| line.number));
            (
                drawn
                    .into_iter()
                    .map(|line| hunk_row(line, gutter))
                    .collect(),
                cut.iter()
                    .map(|artifact| format!("Cut: the whole diff is in {artifact}"))
                    .collect(),
            )
        }
        Diff::Empty => (
            vec![vec![(
                "No changes against HEAD.".to_owned(),
                None,
                Ink::Plain,
            )]],
            Vec::new(),
        ),
        Diff::Failed(message) => (
            vec![vec![(safe_line(message), None, Ink::Plain)]],
            Vec::new(),
        ),
    };
    Frame {
        joins: Vec::new(),
        title: format!("Changed files › {} · diff against HEAD", safe_line(path)),
        command: "diff against HEAD".to_owned(),
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

/// One diff line as swapped cells: the dim gutter, then the text
/// under its ink. The gutter fits the widest number in the diff.
fn hunk_row(line: DiffLine, gutter: usize) -> Vec<(String, Option<Spot>, Ink)> {
    let mut cells = Vec::new();
    if let Some(prefix) = gutter_prefix(line.number, gutter) {
        cells.push((prefix, None, Ink::Muted));
    }
    cells.extend(
        ink_spans(&line)
            .into_iter()
            .map(|(text, ink)| (text, None, ink)),
    );
    cells
}

/// The standard unreadable-diff answer.
fn failed() -> Diff {
    Diff::Failed("The diff could not be read.".to_owned())
}

#[cfg(test)]
#[path = "changed_files_view_tests.rs"]
mod tests;
