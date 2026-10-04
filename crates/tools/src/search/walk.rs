//! The directory walk behind `grep -r` and `find` (`docs/tools.md`, "Search").
//!
//! One builder serves both: what the workspace's ignore files exclude is
//! skipped, the version-control directories always are, hidden files are
//! searched, and links the walk finds are listed but never followed. A path
//! the caller names is always entered, even when it is ignored.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// Directories a walk never discovers, however the ignore files read
/// (`docs/tools.md`, "Search", "What it skips").
const VCS: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];

/// How a named path is visited.
#[derive(Debug)]
pub(crate) enum Root {
    /// Read the file. A link is read through its target.
    File(FileRoot),
    /// Walk the directory. A link to a directory is walked through its
    /// target while printed paths keep the link.
    Dir(DirRoot),
}

/// A file to read: `read` is opened, paths print as `show`.
#[derive(Debug)]
pub(crate) struct FileRoot {
    /// The file read.
    pub read: PathBuf,
    /// The path printed.
    pub show: PathBuf,
    /// The entry's type, without following a link.
    pub file_type: std::fs::FileType,
}

/// A directory to walk: `walk` is read, paths print below `show`. The two
/// differ only when the path as given is a link.
#[derive(Debug, Clone)]
pub(crate) struct DirRoot {
    /// The directory read.
    pub walk: PathBuf,
    /// The prefix printed.
    pub show: PathBuf,
}

/// Sorts `root` into how it is visited: files and links are read
/// themselves, directories are walked. `root` prints as given and reads
/// below `cwd`. Anything else that is neither file nor directory, such as
/// a device, is read as a file and fails when opened, as GNU does.
pub(crate) fn root_of(cwd: &Path, root: &Path) -> io::Result<Root> {
    let from = cwd.join(root);
    let upper = std::fs::symlink_metadata(&from)?;
    if upper.is_dir() {
        return Ok(Root::Dir(DirRoot {
            walk: from,
            show: root.to_path_buf(),
        }));
    }
    if upper.is_symlink() && std::fs::metadata(&from).is_ok_and(|lower| lower.is_dir()) {
        return Ok(Root::Dir(DirRoot {
            walk: from,
            show: root.to_path_buf(),
        }));
    }
    Ok(Root::File(FileRoot {
        read: from,
        show: root.to_path_buf(),
        file_type: upper.file_type(),
    }))
}

/// One entry a walk found.
#[derive(Debug)]
pub(crate) struct Found {
    /// The path as printed: the search root as given joined to the entry's
    /// relative path.
    pub display: PathBuf,
    /// The entry's type. A link the walk finds is reported as a link.
    pub file_type: std::fs::FileType,
    /// How far below the search root the entry sits; the root is 0.
    pub depth: usize,
}

/// One entry a walk could not read.
#[derive(Debug)]
pub(crate) struct WalkError {
    /// The path as printed.
    pub display: PathBuf,
    /// The reason, such as `Permission denied`.
    pub message: String,
}

/// Walks the directory `root`, yielding what it found and what it could not
/// read in sorted file-name order, the root first. `cwd` is the directory
/// the search runs in. `max_depth` caps how far below the root the walk
/// goes, counting the root as 0. What the walk skips is the module's
/// opening paragraph.
pub(crate) fn walk(
    cwd: &Path,
    root: &DirRoot,
    max_depth: Option<usize>,
) -> Vec<Result<Found, WalkError>> {
    let mut builder = ignore::WalkBuilder::new(&root.walk);
    builder
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // The listed files count outside a repository too, so temp trees
        // and plain directories search as a workspace does.
        .require_git(false)
        .follow_links(false)
        .max_depth(max_depth)
        .current_dir(cwd.to_path_buf())
        .sort_by_file_path(|first, second| first.cmp(second));
    let walk = root.walk.clone();
    builder.filter_entry(move |entry| {
        entry.path() == walk
            || !(entry.file_type().is_some_and(|kind| kind.is_dir()) && is_vcs(entry.file_name()))
    });
    let mut out = Vec::new();
    for result in builder.build() {
        match result {
            Ok(entry) => {
                // Only stdin entries lack a type, and the walk never reads
                // stdin; anything else carries what the walk stat'ed.
                let Some(file_type) = entry.file_type() else {
                    continue;
                };
                out.push(Ok(Found {
                    display: rebased(root, entry.path()),
                    file_type,
                    depth: entry.depth(),
                }));
            }
            Err(error) => out.push(Err(walk_error(root, error))),
        }
    }
    out
}

/// Reports what the walk could not read: the path as printed and the reason.
fn walk_error(root: &DirRoot, error: ignore::Error) -> WalkError {
    match error {
        ignore::Error::WithPath { path, err } => WalkError {
            display: rebased(root, &path),
            message: fault_message(&err),
        },
        ignore::Error::Partial(_)
        | ignore::Error::WithLineNumber { .. }
        | ignore::Error::WithDepth { .. }
        | ignore::Error::Loop { .. }
        | ignore::Error::Io(_)
        | ignore::Error::Glob { .. }
        | ignore::Error::UnrecognizedFileType(_)
        | ignore::Error::InvalidDefinition => WalkError {
            display: root.show.clone(),
            message: fault_message(&error),
        },
    }
}

/// Prints `path` below the root as given. The walker builds its paths below
/// the directory read, which is the path as given except for a link.
fn rebased(root: &DirRoot, path: &Path) -> PathBuf {
    if path == root.walk {
        return root.show.clone();
    }
    if root.walk == root.show {
        return path.to_path_buf();
    }
    // The walker only yields the root and paths below it, so the prefix
    // always strips; anything else keeps its own path.
    path.strip_prefix(&root.walk)
        .map(|relative| root.show.join(relative))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The reason a walk failed: the operating system's words when it has them.
fn fault_message(error: &ignore::Error) -> String {
    match error.io_error() {
        Some(error) => io_message(error),
        None => error.to_string(),
    }
}

/// The operating system's reason in GNU's words.
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "std::io::ErrorKind is non_exhaustive; a kind added later reads through Display"
)]
pub(crate) fn io_message(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::IsADirectory => "Is a directory".to_owned(),
        _ => error.to_string(),
    }
}

/// Whether `name` is a version-control directory the walk never discovers.
fn is_vcs(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| VCS.contains(&name))
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
