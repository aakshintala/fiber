//! The ignored files prune names on a removable worktree's row
//! (`docs/invocation.md`, "Deleting and pruning"): grouped by top-level
//! path, with their total size.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The ignored files in a worktree prune will remove, grouped for its row.
#[derive(Debug)]
pub(crate) struct Summary {
    /// Each top-level label with its bytes, by bytes descending then label.
    pub(crate) groups: Vec<(String, u64)>,
    /// The sum of the group bytes.
    pub(crate) bytes: u64,
    /// How many entries could not be read.
    pub(crate) unreadable: u64,
}

/// Groups `entries` by first path component and sizes them under `root`:
/// only paths git listed ever count, never a tracked sibling. A group is
/// `<first>/` when any of its entries is a directory or has more than one
/// component, else the bare name.
pub(crate) fn summarize(root: &Path, entries: &[worktree::IgnoredEntry]) -> Summary {
    struct Group {
        bytes: u64,
        suffixed: bool,
    }
    let mut groups: BTreeMap<OsString, Group> = BTreeMap::new();
    let mut unreadable = 0_u64;
    for entry in entries {
        let Some(first) = entry.path.components().next() else {
            continue;
        };
        let (size, missed) = sized(root.join(&entry.path));
        unreadable += missed;
        let group = groups.entry(first.as_os_str().to_owned()).or_insert(Group {
            bytes: 0,
            suffixed: false,
        });
        group.bytes += size;
        group.suffixed = group.suffixed || entry.is_dir || entry.path.components().count() > 1;
    }
    let mut labeled: Vec<(String, u64)> = groups
        .into_iter()
        .map(|(first, group)| {
            let mut label = label(&first);
            if group.suffixed {
                label.push('/');
            }
            (label, group.bytes)
        })
        .collect();
    labeled.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let bytes = labeled.iter().map(|(_, size)| size).sum();
    Summary {
        groups: labeled,
        bytes,
        unreadable,
    }
}

/// The row segment for `summary`: "" when nothing is ignored, otherwise
/// "  ignored \<size\>: \<label\>, …" with " (\<n\> unreadable)" when some
/// entries could not be read.
pub(crate) fn segment(summary: &Summary) -> String {
    if summary.groups.is_empty() {
        return String::new();
    }
    let labels: Vec<&str> = summary
        .groups
        .iter()
        .map(|(label, _)| label.as_str())
        .collect();
    let mut out = format!(
        "  ignored {}: {}",
        super::format_size(summary.bytes),
        labels.join(", ")
    );
    if summary.unreadable > 0 {
        out.push_str(&format!(" ({} unreadable)", summary.unreadable));
    }
    out
}

/// The label for a first path component: its lossy UTF-8, with every
/// control character replaced by its escaped form.
fn label(first: &std::ffi::OsStr) -> String {
    let mut out = String::new();
    for c in first.to_string_lossy().chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// The bytes under `path` and how many entries could not be read: every
/// entry is read with `symlink_metadata`, and a symbolic link counts its
/// own length without ever descending into it.
fn sized(path: PathBuf) -> (u64, u64) {
    let mut bytes = 0_u64;
    let mut unreadable = 0_u64;
    let mut stack = vec![path];
    while let Some(next) = stack.pop() {
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            bytes += meta.len();
        } else if meta.is_dir() {
            match std::fs::read_dir(&next) {
                Ok(entries) => {
                    for child in entries {
                        match child {
                            Ok(child) => stack.push(child.path()),
                            Err(_) => unreadable += 1,
                        }
                    }
                }
                Err(_) => unreadable += 1,
            }
        } else {
            bytes += meta.len();
        }
    }
    (bytes, unreadable)
}

#[cfg(test)]
#[path = "ignored_tests.rs"]
mod tests;
