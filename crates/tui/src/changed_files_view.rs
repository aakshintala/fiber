//! Changed files' ranking, diff command and swapped view (`docs/tui.md`,
//! "Swapped views").

use std::collections::{BTreeMap, BTreeSet};

use contract::events::CommandResult;

use crate::diff::{DiffLine, diff_lines, gutter_prefix, gutter_width, ink_spans, safe_line};
use crate::format;
use crate::swapped::{Chrome, Frame, Ink, List, Span, Spot};
use ratatui::style::{Modifier, Style};

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

/// The shell line for `path`'s diff against HEAD (`docs/tui.md`, "Swapped views"). Single quotes keep paths literal to the shell; embedded quotes
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

/// The shell line for the view-open tracking query over the listed
/// paths: one `git status --porcelain` limited to the listed paths,
/// sent on the same side and channel as the numstat query. Each listed
/// path the query reports unstaged-clean stays silent; anything else
/// reports its class with hub-side line counts for untracked paths, so
/// the fold needs no path parsing: the rows stay absolute card paths.
pub(crate) fn tracking_command(paths: &[&str]) -> String {
    let quoted: Vec<String> = paths
        .iter()
        .map(|path| format!("'{}'", path.replace('\'', "'\\''")))
        .collect();
    let each = quoted.join(" ");
    format!(
        "for p in {each}; do s=$(git --no-optional-locks status --porcelain=v1 --no-renames -- \"$p\" 2>/dev/null | head -c 2); case \"$s\" in '??'*) c=$(wc -l < \"$p\" 2>/dev/null | tr -d '[:space:]'); printf 'untracked\\t%s\\t%s\\0' \"$c\" \"$p\";; A*) printf 'added\\t\\t%s\\0' \"$p\";; *D*) printf 'deleted\\t\\t%s\\0' \"$p\";; esac; done"
    )
}

/// One listed path's tracking class, folded with the figures answer:
/// `untracked` (with hub-side line counts) and `added` (staged-new)
/// draw `A`; `deleted` with a numstat row draws `D`; anything the
/// numstat answer omits outside `untracked` joins no-diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tracking {
    /// Paths git does not track, with their line counts at query time.
    pub(crate) untracked: BTreeSet<String>,
    /// Paths staged new in the index.
    pub(crate) added: BTreeSet<String>,
    /// Paths deleted against `HEAD`.
    pub(crate) deleted: BTreeSet<String>,
    /// Line counts for untracked paths: the answer carries hub-side
    /// counts, since `count_lines` runs only against a local root.
    pub(crate) counts: BTreeMap<String, u64>,
}

/// Folds a tracking answer into per-path classes. Records are NUL
/// separated `class`, count, path fields; an empty count means the
/// count did not survive, a missing or malformed record rejects the
/// whole answer, and blank fragments (the closing-line strip's
/// trailing newline, an empty answer) are valid: every path is clean.
pub(crate) fn parse_tracking(output: &str) -> Option<Tracking> {
    let mut tracking = Tracking::default();
    for record in output.split('\0') {
        if record.trim().is_empty() {
            continue;
        }
        let mut fields = record.splitn(3, '\t');
        let (Some(class), Some(count), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if path.is_empty() {
            return None;
        }
        let count = if count.trim().is_empty() {
            None
        } else {
            Some(count.trim().parse::<u64>().ok()?)
        };
        match class {
            "untracked" => {
                tracking.untracked.insert(path.to_owned());
                if let Some(lines) = count {
                    tracking.counts.insert(path.to_owned(), lines);
                }
            }
            "added" if count.is_none() => {
                tracking.added.insert(path.to_owned());
            }
            "deleted" if count.is_none() => {
                tracking.deleted.insert(path.to_owned());
            }
            _ => return None,
        }
    }
    Some(tracking)
}

/// Displayed figures for the card's paths, with the paths the
/// numstat answer omits marked untracked for the row form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedFigures {
    pub(crate) figures: BTreeMap<String, (u64, u64)>,
    /// Paths new relative to `HEAD`, per the tracking answer.
    pub(crate) untracked: BTreeSet<String>,
    /// Staged-new paths: `A` with numstat figures.
    pub(crate) added: BTreeSet<String>,
    /// Deleted paths with a numstat row: `D`.
    pub(crate) deleted: BTreeSet<String>,
    /// Tracked paths the numstat answer omits: no-diff candidates the
    /// fold marks at answer time.
    pub(crate) dim: BTreeSet<String>,
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
/// On a remote terminal the card holds hub workspace paths, so a path
/// the launch workspace does not strip also matches by its viewed
/// workspace-relative form.
///
/// The tracking answer classifies each listed path as well: `untracked`
/// paths resolve to their line counts (the answer's hub-side counts
/// first, the local line counts where those never ran); `added` keeps
/// numstat figures with `A`; `deleted` with a numstat row draws `D`.
/// Omitted paths outside `untracked` join `dim` for the no-diff fold —
/// no-diff beats `D`, since only an uncommitted deletion carries a
/// numstat row. Without tracking no class applies.
pub(crate) fn resolve_figures(
    changes: &BTreeMap<String, (u64, u64)>,
    numstat: Option<&str>,
    untracked_lines: &BTreeMap<String, u64>,
    tracking: Option<&Tracking>,
    workspace: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> ResolvedFigures {
    let parsed = numstat.map(parse_numstat).unwrap_or_default();
    let answered = numstat.is_some();
    let mut figures = BTreeMap::new();
    let mut untracked = BTreeSet::new();
    let mut added = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    let mut dim = BTreeSet::new();
    if let Some(tracking) = tracking {
        for path in changes.keys() {
            if tracking.added.contains(path.as_str()) {
                added.insert(path.clone());
            }
            if tracking.deleted.contains(path.as_str()) {
                deleted.insert(path.clone());
            }
        }
    }
    for (path, card) in changes.iter() {
        let figure = match lookup(&parsed, path, workspace, viewed) {
            Some(NumstatFigure::Counted(added_count, removed_count)) => {
                (*added_count, *removed_count)
            }
            Some(NumstatFigure::Binary) => *card,
            None if answered => match tracking {
                Some(tracking) if tracking.untracked.contains(path.as_str()) => {
                    match tracking
                        .counts
                        .get(path.as_str())
                        .or_else(|| untracked_lines.get(path.as_str()))
                    {
                        Some(lines) => {
                            untracked.insert(path.clone());
                            (*lines, 0)
                        }
                        // Undone-new: tracking saw it untracked, but no
                        // count survived, so it joins no-diff, not A.
                        None => {
                            dim.insert(path.clone());
                            *card
                        }
                    }
                }
                Some(_) if added.contains(path.as_str()) => *card,
                Some(_) => {
                    dim.insert(path.clone());
                    *card
                }
                // Without tracking no class applies: the card's figures
                // with no letter and no no-diff marking.
                None => *card,
            },
            None => *card,
        };
        figures.insert(path.clone(), figure);
    }
    ResolvedFigures {
        figures,
        untracked,
        added,
        deleted,
        dim,
    }
}

/// The answer's figure for a card path: the path as listed, its launch
/// workspace-relative form, or its viewed workspace-relative form for
/// git's repository-relative output. The viewed workspace names the hub
/// workspace on a remote terminal, whose card holds hub workspace paths.
pub(crate) fn lookup<'a>(
    parsed: &'a BTreeMap<String, NumstatFigure>,
    path: &str,
    workspace: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> Option<&'a NumstatFigure> {
    if let Some(figure) = parsed.get(path) {
        return Some(figure);
    }
    for root in [workspace, viewed].into_iter().flatten() {
        if let Some(figure) = std::path::Path::new(path)
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.to_str())
            .and_then(|relative| parsed.get(relative))
        {
            return Some(figure);
        }
    }
    None
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

/// Folds a shell answer into a safe diff display. The shell tool
/// appends its closing "Exit code N." line to the command's own output,
/// which is the driver's, not the diff's: strip it like the numstat
/// answer does, so a committed file's reply reads empty and no "Exit
/// code" text reaches any diff.
pub(crate) fn answer(result: Option<&CommandResult>) -> Diff {
    let Some(CommandResult::Shell {
        output,
        artifact,
        process,
    }) = result
    else {
        return failed();
    };
    let body = numstat_body(output, process.exit_code);
    if !body.is_empty() {
        return Diff::Lines {
            lines: body.lines().map(safe_line).collect(),
            cut: artifact.as_deref().map(safe_line),
        };
    }
    if process.signal.is_none() && process.exit_code == Some(0) {
        Diff::Empty
    } else {
        failed()
    }
}

/// The first hunk's new-side start over the shown diff `lines`: the `+c`
/// of the first `@@ -a,b +c,d @@` header, at least 1 (`+0,0` deletes at
/// the file's start, which no editor takes as a line). Anything else — no
/// header, an unparsable one, an empty or binary diff — opens at line 1.
pub(crate) fn first_hunk_line(lines: &[String]) -> u64 {
    lines.iter().find_map(|line| hunk_start(line)).unwrap_or(1)
}

/// One `@@ -a,b +c,d @@` header's new-side start, at least 1; None for
/// any other line, so headers the parse cannot read are skipped.
fn hunk_start(line: &str) -> Option<u64> {
    let header = line.strip_prefix("@@ ")?;
    let new = header.split_whitespace().nth(1)?;
    let start = new.strip_prefix('+')?;
    let start = start.split([',', ' ']).next()?;
    start.parse::<u64>().ok().map(|start| start.max(1))
}

/// One body row of the grouped Changed files list, in display order:
/// each folder holding a listed file with its files under it, then the
/// rule above the totals. Folder and rule rows hold no path; file rows
/// hold their path, so selection, Enter and clicks share this map and
/// never send a diff request for a folder or the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListRow {
    /// A dim folder row with its trailing slash, or `"./"` for files at
    /// the workspace's top level.
    Folder(String),
    /// A file row: its path in the displayed figures.
    File(String),
    /// The rule above the totals.
    Rule,
}

/// The grouped list's body rows for `changes`: each folder holding a
/// listed file in path order with its files under it in name order,
/// then the rule above the totals. Folders group the workspace-relative
/// display paths; file rows hold the card's own path, which Enter, the
/// editor and the figures read. With no files the plan holds no rule,
/// since the totals have no last file to follow.
pub(crate) fn list_rows(
    changes: &BTreeMap<String, (u64, u64)>,
    workspace: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> Vec<ListRow> {
    let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in changes.keys() {
        folders
            .entry(folder_of(display_path(path, workspace, viewed)))
            .or_default()
            .push(path.clone());
    }
    let mut rows = Vec::new();
    for (folder, mut paths) in folders {
        rows.push(ListRow::Folder(folder));
        paths.sort();
        rows.extend(paths.into_iter().map(ListRow::File));
    }
    if rows.iter().any(|row| matches!(row, ListRow::File(_))) {
        rows.push(ListRow::Rule);
    }
    rows
}

/// The display rows holding files: the rows selection, Enter and clicks
/// move over, so folder and rule rows never send a diff request.
pub(crate) fn file_takes(
    changes: &BTreeMap<String, (u64, u64)>,
    workspace: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> Vec<usize> {
    list_rows(changes, workspace, viewed)
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, ListRow::File(_)))
        .map(|(at, _)| at)
        .collect()
}

/// The grouped row's path: the file's path on a file row, `None` on a
/// folder or rule row, which Enter and clicks leave alone.
pub(crate) fn row_path(rows: &[ListRow], at: usize) -> Option<&str> {
    match rows.get(at) {
        Some(ListRow::File(path)) => Some(path),
        _ => None,
    }
}

/// The folder row a path groups under: its directory with the trailing
/// slash, or `"./"` for a file at the workspace's top level.
fn folder_of(path: &str) -> String {
    match path.rfind('/') {
        Some(at) => path[..=at].to_owned(),
        None => "./".to_owned(),
    }
}

/// The file name a file row shows: the path past its last slash.
fn name_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(at) => &path[at + 1..],
        None => path,
    }
}

/// The path the list shows: workspace-relative, since the card holds
/// absolute workspace paths (`docs/tui.md`, "Swapped views"). Strips
/// the first root that prefixes it — the local launch workspace, else
/// the viewed one — like the figures lookup does; anything else, such
/// as a path outside the workspace, shows whole.
fn display_path<'a>(
    path: &'a str,
    workspace: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> &'a str {
    for root in [workspace, viewed].into_iter().flatten() {
        if let Some(relative) = std::path::Path::new(path)
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.to_str())
        {
            return relative;
        }
    }
    path
}

/// A file row's letter: `A` in `added` for a path the answered figures
/// count as untracked, `D` in `removed` for a path missing from the
/// local workspace root at frame time, `M` in `attention` otherwise,
/// including on a remote terminal with no local root to stat.
fn letter(
    path: &str,
    untracked: &BTreeSet<String>,
    added: &BTreeSet<String>,
    deleted: &BTreeSet<String>,
    root: Option<&std::path::Path>,
) -> (String, Ink) {
    if untracked.contains(path) || added.contains(path) {
        ("A  ".to_owned(), Ink::Role(crate::theme::Role::Added))
    } else if deleted.contains(path) || root.is_some_and(|root| !root.join(path).exists()) {
        // A query-reported deletion and a frame-time absence draw the
        // same D; each side has its own test.
        ("D  ".to_owned(), Ink::Role(crate::theme::Role::Removed))
    } else {
        ("M  ".to_owned(), Ink::Role(crate::theme::Role::Attention))
    }
}

/// A figured row's figures: git's where the answer counts the path, the
/// card's everywhere else.
fn display_figures(
    figures: &ResolvedFigures,
    changes: &BTreeMap<String, (u64, u64)>,
    path: &str,
) -> (u64, u64) {
    figures
        .figures
        .get(path)
        .or_else(|| changes.get(path))
        .copied()
        .unwrap_or((0, 0))
}

/// Builds the file list or the selected file's diff frame. The list
/// groups the displayed figures' paths by folder, each file row a
/// letter, its name and its figures right-aligned in one column; a
/// no-diff path stays listed with "no changes" dim. Ctrl+G edits the
/// shown file on a local terminal, and fetches the hub file to edit on
/// a remote one: either way the footer keeps the hint.
pub(crate) fn frame(
    changes: &BTreeMap<String, (u64, u64)>,
    figures: Option<&ResolvedFigures>,
    chosen: Option<(&str, &Diff)>,
    list: List,
    no_diff: &BTreeSet<String>,
    root: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
) -> Frame {
    if let Some((path, diff)) = chosen {
        return diff_frame(path, diff, list);
    }
    // Until the figures answer arrives the rows show the card's figures
    // with M: the presence read stays off, so nothing draws D early.
    let answered = figures.is_some();
    let resolved;
    let figures = match figures {
        Some(figures) => figures,
        None => {
            resolved = ResolvedFigures {
                figures: changes.clone(),
                untracked: BTreeSet::new(),
                added: BTreeSet::new(),
                deleted: BTreeSet::new(),
                dim: BTreeSet::new(),
            };
            &resolved
        }
    };
    let presence = if answered { root } else { None };
    let plan = list_rows(changes, root, viewed);
    // The figures column's widths: every figured row right-aligns its
    // counts in them, and the totals reuse them below.
    let mut added_w = 0;
    let mut removed_w = 0;
    for row in &plan {
        let ListRow::File(path) = row else {
            continue;
        };
        if no_diff.contains(path.as_str()) {
            continue;
        }
        let (added, removed) = display_figures(figures, changes, path);
        added_w = added_w.max(crate::text::width(&format!("+{added}")));
        removed_w = removed_w.max(crate::text::width(&format!("−{removed}")));
    }
    // The content's width so far: the rule spans it, since the frame
    // knows no terminal width to span instead.
    let mut content_w = 0;
    let mut rows = Vec::new();
    for (at, row) in plan.iter().enumerate() {
        match row {
            ListRow::Folder(folder) => {
                let folder = safe_line(folder);
                content_w = content_w.max(crate::text::width(&folder));
                rows.push(vec![(folder, None, Ink::Muted)]);
            }
            ListRow::Rule => {
                rows.push(vec![(
                    "─".repeat(content_w),
                    None,
                    Ink::Role(crate::theme::Role::Rule),
                )]);
            }
            ListRow::File(path) => {
                // The focused name draws bold in accent only for the
                // accent list: any other list keeps its plain names, so
                // the focus stays scoped to the Changed files list.
                let row = file_row(
                    path,
                    figures,
                    changes,
                    no_diff,
                    root,
                    viewed,
                    presence,
                    at == list.selected() && list.is_accent(),
                    added_w,
                    removed_w,
                );
                content_w = content_w.max(row_width(&row));
                rows.push(row);
            }
        }
    }
    let (files, added, removed) = totals(&figures.figures);
    let below = format!(
        "{}  {:>added_w$} {:>removed_w$}",
        format::count(files, "file", "files"),
        format!("+{added}"),
        format!("−{removed}"),
    );
    Frame {
        joins: Vec::new(),
        chrome: Some(Chrome {
            top: Vec::new(),
            draft: None,
            footer: footer_spans(),
        }),
        title: "Changed files".to_owned(),
        command: "diff against HEAD".to_owned(),
        rows,
        list,
        below: vec![below],
        field: None,
        footer: "↑↓ file · Ctrl+G open in editor".to_owned(),
    }
}

/// One file row: the indent, the letter, the workspace-relative name,
/// then the figures right-aligned in the column's widths. The card's
/// own path drives the letter, the figures and the no-diff set; only
/// the shown name relativizes. A no-diff path draws wholly dim with
/// "no changes" where the figures go; the focused name draws bold in
/// accent. The presence root stays off until the figures answer
/// arrives, so no row draws D early.
#[allow(
    clippy::too_many_arguments,
    reason = "the row reads the figures, the no-diff set, both workspace roots, the presence read, the focus and the column widths"
)]
fn file_row(
    path: &str,
    figures: &ResolvedFigures,
    changes: &BTreeMap<String, (u64, u64)>,
    no_diff: &BTreeSet<String>,
    root: Option<&std::path::Path>,
    viewed: Option<&std::path::Path>,
    presence: Option<&std::path::Path>,
    focused: bool,
    added_w: usize,
    removed_w: usize,
) -> Vec<(String, Option<Spot>, Ink)> {
    let shown = display_path(path, root, viewed);
    if no_diff.contains(path) {
        return vec![
            ("    ".to_owned(), None, Ink::Muted),
            ("·  ".to_owned(), None, Ink::Muted),
            (safe_line(name_of(shown)), None, Ink::Muted),
            ("  ".to_owned(), None, Ink::Muted),
            ("no changes".to_owned(), None, Ink::Muted),
        ];
    }
    let (letter, letter_ink) = letter(
        path,
        &figures.untracked,
        &figures.added,
        &figures.deleted,
        presence,
    );
    let name_ink = if focused {
        Ink::Bold(crate::theme::Role::Accent)
    } else {
        Ink::Heading
    };
    let (added, removed) = display_figures(figures, changes, path);
    vec![
        ("    ".to_owned(), None, Ink::Plain),
        (letter, None, letter_ink),
        (safe_line(name_of(shown)), None, name_ink),
        ("  ".to_owned(), None, Ink::Plain),
        (
            format!("{:>added_w$}", format!("+{added}")),
            None,
            Ink::Role(crate::theme::Role::Added),
        ),
        (" ".to_owned(), None, Ink::Plain),
        (
            format!("{:>removed_w$}", format!("−{removed}")),
            None,
            Ink::Role(crate::theme::Role::Removed),
        ),
    ]
}

/// A row's drawn width: its cells' display widths joined.
fn row_width(row: &[(String, Option<Spot>, Ink)]) -> usize {
    row.iter()
        .map(|(text, _, _)| crate::text::width(text))
        .sum()
}

/// The list footer's styled keys: the keys bold, what they do dim.
fn footer_spans() -> Vec<Span> {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    vec![
        ("↑↓".to_owned(), bold),
        (" file · ".to_owned(), Ink::Muted.style()),
        ("Ctrl+G".to_owned(), bold),
        (" open in editor".to_owned(), Ink::Muted.style()),
    ]
}

/// The current diff frame, with its file in the title. Above the
/// hunks, the path row names the file bold with the editor target in
/// `secondary`: a click on it runs the same editor path as Ctrl+G.
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
                "No changes against HEAD: committed or undone since Fiber changed it.".to_owned(),
                None,
                Ink::Muted,
            )]],
            Vec::new(),
        ),
        Diff::Failed(message) => (
            vec![vec![(safe_line(message), None, Ink::Plain)]],
            Vec::new(),
        ),
    };
    let mut framed = vec![vec![
        (safe_line(path), None, Ink::Heading),
        ("  ".to_owned(), None, Ink::Plain),
        (
            "Open in editor ›".to_owned(),
            Some(Spot::Cell(0, 1)),
            Ink::Role(crate::theme::Role::Secondary),
        ),
    ]];
    framed.extend(rows);
    Frame {
        joins: Vec::new(),
        chrome: None,
        title: format!("Changed files › {} · diff against HEAD", safe_line(path)),
        command: "diff against HEAD".to_owned(),
        rows: framed,
        list,
        below,
        field: None,
        footer: "↑↓ scroll · Ctrl+G edits · ← files · Esc close".to_owned(),
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
