//! Compare and restore baseline entries (`docs/sandbox.md`, "Protected
//! paths"). One recursive reconciler restores file, directory, symlink and
//! missing entries; the top-level wrapper creates parents and records the
//! report. File comparison never follows symlinks.

use std::collections::BTreeMap;
use std::path::Path;

use super::baseline::{Entry, EntryKind, StatePaths, read_baseline};
use super::{Report, RestoreFailure};

/// Compares every protected path against the baseline, restores any
/// difference and reports it. A missing baseline records nothing, so a
/// recovery with no baseline restores nothing. A baseline for another
/// project identity is an error and remains untouched. A failed entry is
/// reported with its cause and never as restored.
pub(crate) fn compare_restore_report(dir: &StatePaths) -> Result<Report, std::io::Error> {
    let baseline = match read_baseline(&dir.baseline) {
        Ok(baseline) => baseline,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Report {
                restored: Vec::new(),
                failed: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    if !super::baseline::belongs_to_project(&baseline.entries, &dir.identity_marker) {
        // A colliding project key can name this directory too. Do not treat
        // the other identity's baseline as empty: recovery would then delete
        // the only copy of its pending restore.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the protected-path baseline belongs to another project identity",
        ));
    }
    Ok(compare_restore_entries(&baseline.entries))
}

/// Compares a supplied baseline, allowing an approved write to finish safely
/// when its new entries are held in memory after a failed persistence.
pub(crate) fn compare_restore_entries(entries: &[Entry]) -> Report {
    let mut report = Report {
        restored: Vec::new(),
        failed: Vec::new(),
    };
    for entry in entries {
        if let Err(source) = reconcile(entry, &mut report) {
            report.failed.push(RestoreFailure {
                path: entry.display.clone(),
                message: source,
            });
        }
    }
    report
}

/// Compares one top-level entry and restores it when it differs. The
/// wrapper creates the parent a file needs and records the report; the
/// recursive helper below owns every kind. Returns the cause when the
/// compare or the restore fails.
pub(crate) fn reconcile(entry: &Entry, report: &mut Report) -> Result<(), String> {
    let path = Path::new(&entry.path);
    if matches!(entry.kind, EntryKind::File { .. })
        && let Some(parent) = path.parent()
    {
        std::fs::create_dir_all(parent).map_err(|source| source.to_string())?;
    }
    if reconcile_kind(path, &entry.kind)? {
        report.restored.push(entry.display.clone());
    }
    Ok(())
}

/// Compares one recorded kind against the live path and restores it when
/// it differs. Returns whether the path changed. A symlink to a file with
/// matching bytes is a changed symlink, never an unchanged file: the file
/// case checks the entry type without following links.
fn reconcile_kind(path: &Path, kind: &EntryKind) -> Result<bool, String> {
    match kind {
        EntryKind::File { mode, bytes } => {
            // Without following symlinks: a symlink, a directory or a
            // missing path is never an unchanged file. A read failure on
            // a regular file is a failed compare, never a mismatch: the
            // caller reports it with the path instead of restoring over
            // an unreadable file.
            // A non-file is never unchanged: it falls through to the
            // restore below.
            if is_regular_file(path) {
                let current = std::fs::read(path).map_err(|source| {
                    format!("{} could not be compared: {source}", path.display())
                })?;
                if current == *bytes && mode_matches(path, *mode) {
                    return Ok(false);
                }
            }
            remove_any(path)?;
            std::fs::write(path, bytes).map_err(|source| source.to_string())?;
            apply_mode(path, *mode)?;
            Ok(true)
        }
        EntryKind::Dir { mode, children } => {
            let is_dir = std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_dir())
                .unwrap_or(false);
            let mut changed = false;
            if !is_dir {
                remove_any(path)?;
                std::fs::create_dir_all(path).map_err(|source| source.to_string())?;
                changed = true;
            }
            apply_mode(path, *mode)?;
            if !mode_matches(path, *mode) {
                changed = true;
            }
            reconcile_children(path, children, &mut changed)?;
            Ok(changed)
        }
        EntryKind::Symlink { target } => {
            let same = std::fs::read_link(path)
                .map(|current| current.to_string_lossy() == *target)
                .unwrap_or(false);
            if same {
                return Ok(false);
            }
            remove_any(path)?;
            std::os::unix::fs::symlink(target, path).map_err(|source| source.to_string())?;
            Ok(true)
        }
        EntryKind::Missing => {
            if std::fs::symlink_metadata(path).is_err() {
                return Ok(false);
            }
            remove_any(path)?;
            Ok(true)
        }
    }
}

/// Whether `path` is a regular file, without following symlinks.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_file())
        .unwrap_or(false)
}

/// Whether the path's permission bits equal the recorded ones.
fn mode_matches(path: &Path, mode: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .map(|meta| (meta.permissions().mode() & 0o7777) == (mode & 0o7777))
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        true
    }
}

/// Reapplies the recorded permission bits.
fn apply_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|source| source.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Reconciles a recorded directory's children: restores each recorded
/// child, and removes every current entry the baseline never recorded, so
/// a rename, a new file or a new directory entry cannot escape detection.
fn reconcile_children(
    dir: &Path,
    children: &BTreeMap<String, EntryKind>,
    changed: &mut bool,
) -> Result<(), String> {
    for (name, kind) in children {
        let path = dir.join(name);
        if reconcile_kind(&path, kind)? {
            *changed = true;
        }
    }
    let mut current = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|source| source.to_string())? {
        let entry = entry.map_err(|source| source.to_string())?;
        current.push(entry.file_name().to_string_lossy().into_owned());
    }
    for name in current {
        if !children.contains_key(&name) {
            remove_any(&dir.join(&name))?;
            *changed = true;
        }
    }
    Ok(())
}

/// Removes whatever `path` holds: a file or symlink, or a directory tree.
/// Absent is already removed.
fn remove_any(path: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if meta.file_type().is_dir() {
        std::fs::remove_dir_all(path).map_err(|source| source.to_string())
    } else {
        std::fs::remove_file(path).map_err(|source| source.to_string())
    }
}
