//! Which worktrees prune lists and removes (`docs/invocation.md`, "Deleting
//! and pruning"): the scope scan, the running-session guard, the rows, and
//! removal with its revalidation. `cli` keeps the decisions; the `worktree`
//! crate runs `git`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use contract::ErrorCode;

/// A worktree row prune prints.
#[derive(Debug)]
pub(crate) enum WorktreeRow {
    /// A worktree to remove: clean, or losing something with `--force`.
    Removable {
        /// The directory name under `worktrees/`.
        name: String,
        /// The short branch it checks out.
        branch: String,
        /// Whether `git status` reports anything, ignored files included.
        uncommitted: bool,
        /// Whole days old, floored, from the directory's mtime.
        age_days: u64,
        /// Logical bytes.
        bytes: u64,
        /// What removing it loses; set only with `--force`.
        forced: Option<String>,
    },
    /// A worktree kept because removing it would lose something.
    Skipped {
        /// The directory name under `worktrees/`.
        name: String,
        /// The short branch it checks out.
        branch: String,
        /// Whether `git status` reports anything, ignored files included.
        uncommitted: bool,
        /// Whole days old, floored, from the directory's mtime.
        age_days: u64,
        /// Logical bytes.
        bytes: u64,
        /// What removing it would lose.
        what: String,
    },
    /// A worktree a running session works in, never removed.
    Running {
        /// The directory name under `worktrees/`.
        name: String,
    },
    /// A worktree prune cannot judge, kept with its reason.
    Uncertain {
        /// The directory name under `worktrees/`.
        name: String,
        /// Why it is kept.
        reason: String,
    },
}

/// A worktree prune will remove, with its project for the revalidation.
pub(crate) struct PlannedRemoval {
    /// The worktree's directory.
    dir: PathBuf,
    /// The listed bytes.
    bytes: u64,
    /// The directory name under `worktrees/`.
    name: String,
    /// The project key holding the worktree.
    project: String,
}

/// What prune lists and removes on the worktree side.
pub(crate) struct Planned {
    /// The worktree rows, by name.
    pub(crate) rows: Vec<WorktreeRow>,
    /// The removals, in row order.
    pub(crate) removals: Vec<PlannedRemoval>,
    /// The session locks held until the run ends.
    locks: Vec<log::SessionLock>,
    /// The held sessions' ids, so no lock is taken twice.
    held: HashSet<String>,
}

/// A listed worktree with the parts its removal needs.
struct Listed {
    /// The row prune prints.
    row: WorktreeRow,
    /// The removal, when the row is removable.
    removal: Option<RemovalParts>,
}

/// What removing a listed worktree needs.
struct RemovalParts {
    /// The worktree's directory.
    dir: PathBuf,
    /// The listed bytes.
    bytes: u64,
    /// The project key holding the worktree.
    project: String,
}

/// What removing the planned worktrees freed and what it could not.
pub(crate) struct RemovedWorktrees {
    /// The listed bytes of every wholly removed worktree, plus the listed
    /// bytes of every partial removal.
    pub(crate) freed: u64,
    /// Each worktree that stayed, or lost only its worktree: its row name
    /// and why.
    pub(crate) failures: Vec<(String, ErrorCode, String)>,
    /// How many worktrees prune tried to remove.
    pub(crate) total: usize,
}

/// Lists the kept worktrees in scope: inside a git repository the
/// repository's project, outside one every project. Unless `dry_run`,
/// each one's users' locks are held until the run ends: `--dry-run` drops
/// each lock at once, so the scan and output hold none. A symlinked entry
/// is not listed. Dropping the plan releases every kept lock.
pub(crate) fn select(
    home: &Path,
    workspace: &Path,
    force: bool,
    dry_run: bool,
    now: SystemTime,
) -> Planned {
    let keys = scope_keys(home, workspace);
    let users = log::started_sessions(home);
    let mut planned = Planned {
        rows: Vec::new(),
        removals: Vec::new(),
        locks: Vec::new(),
        held: HashSet::new(),
    };
    // Every listed worktree, sorted by name.
    let mut listed: Vec<Listed> = Vec::new();
    for key in &keys {
        let worktrees = home.join("projects").join(key).join("worktrees");
        let Ok(entries) = std::fs::read_dir(&worktrees) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type` does not follow a link: a symlinked entry is
            // not listed.
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let dir = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(canonical) = std::fs::canonicalize(&dir) else {
                listed.push(Listed {
                    row: WorktreeRow::Uncertain {
                        name,
                        reason: "git cannot read it: the directory cannot be resolved".to_owned(),
                    },
                    removal: None,
                });
                continue;
            };
            if hold_users(&users, &canonical, &mut planned, dry_run) {
                listed.push(Listed {
                    row: WorktreeRow::Running { name },
                    removal: None,
                });
                continue;
            }
            let inspected = match judged(&dir) {
                Ok(inspected) => inspected,
                Err(reason) => {
                    listed.push(Listed {
                        row: WorktreeRow::Uncertain { name, reason },
                        removal: None,
                    });
                    continue;
                }
            };
            let bytes = log::session_bytes(&dir);
            let age = age_days(&dir, now);
            let row = |forced: Option<String>| WorktreeRow::Removable {
                name: name.clone(),
                branch: inspected.branch.clone(),
                uncommitted: inspected.uncommitted,
                age_days: age,
                bytes,
                forced,
            };
            let parts = || RemovalParts {
                dir: dir.clone(),
                bytes,
                project: key.clone(),
            };
            match (
                losses(inspected.uncommitted, inspected.unique_commits),
                force,
            ) {
                (None, _) => listed.push(Listed {
                    row: row(None),
                    removal: Some(parts()),
                }),
                (Some(what), true) => listed.push(Listed {
                    row: row(Some(what)),
                    removal: Some(parts()),
                }),
                (Some(what), false) => listed.push(Listed {
                    row: WorktreeRow::Skipped {
                        name: name.clone(),
                        branch: inspected.branch.clone(),
                        uncommitted: inspected.uncommitted,
                        age_days: age,
                        bytes,
                        what,
                    },
                    removal: None,
                }),
            }
        }
    }
    listed.sort_by(|a, b| row_name(&a.row).cmp(row_name(&b.row)));
    for item in listed {
        let name = row_name(&item.row).to_owned();
        planned.rows.push(item.row);
        if let Some(parts) = item.removal {
            planned.removals.push(PlannedRemoval {
                dir: parts.dir,
                bytes: parts.bytes,
                name,
                project: parts.project,
            });
        }
    }
    planned
}

/// Removes every planned worktree, in row order: `before_remove` runs just
/// before a worktree's revalidation, so tests can change the world between
/// listing and removal. Production passes a no-op. Each removal revalidates
/// the worktree's users and inspects it again; the second inspection
/// decides, by the same rule and `force`.
pub(crate) fn remove_planned(
    home: &Path,
    planned: &mut Planned,
    force: bool,
    before_remove: &mut dyn FnMut(&Path),
) -> RemovedWorktrees {
    let mut freed = 0_u64;
    let mut failures = Vec::new();
    // Taken out for the loop so each removal revalidates against the
    // plan's locks without borrowing them; put back at the end.
    let removals = std::mem::take(&mut planned.removals);
    let total = removals.len();
    for removal in &removals {
        before_remove(&removal.dir);
        let stopped = revalidate(home, planned, removal);
        if let Err(message) = stopped {
            failures.push((
                format!("worktree {}", removal.name),
                ErrorCode::IoFailed,
                message,
            ));
            continue;
        }
        let second = match judged(&removal.dir) {
            Ok(inspected) => inspected,
            Err(reason) => {
                failures.push((
                    format!("worktree {}", removal.name),
                    ErrorCode::IoFailed,
                    reason,
                ));
                continue;
            }
        };
        let unclean = losses(second.uncommitted, second.unique_commits);
        if let Some(what) = &unclean
            && !force
        {
            failures.push((
                format!("worktree {}", removal.name),
                ErrorCode::IoFailed,
                format!("it changed since it was listed: removing it would now lose {what}"),
            ));
            continue;
        }
        // `remove` gets `force: true` only when `--force` was given and
        // the second inspection is not clean.
        let forced = force && unclean.is_some();
        match worktree::remove(&removal.dir, &second, forced) {
            Ok(worktree::Removed::Whole) => freed += removal.bytes,
            Ok(worktree::Removed::BranchKept(error)) => {
                // The worktree is gone and only its branch stays: a
                // failure, and its bytes still count as freed.
                freed += removal.bytes;
                failures.push((
                    format!("worktree {}", removal.name),
                    ErrorCode::IoFailed,
                    error.to_string(),
                ));
            }
            Err(error) => {
                failures.push((
                    format!("worktree {}", removal.name),
                    ErrorCode::IoFailed,
                    error.to_string(),
                ));
            }
        }
    }
    planned.removals = removals;
    RemovedWorktrees {
        freed,
        failures,
        total,
    }
}

/// One worktree row, fields separated by two spaces.
pub(crate) fn worktree_line(row: &WorktreeRow) -> String {
    let state = |uncommitted: bool| {
        if uncommitted { "uncommitted" } else { "clean" }
    };
    match row {
        WorktreeRow::Removable {
            name,
            branch,
            uncommitted,
            age_days,
            bytes,
            forced,
            ..
        } => {
            let tail = forced
                .as_ref()
                .map(|what| format!("  forced: loses {what}"))
                .unwrap_or_default();
            format!(
                "worktree  {name}  {branch}  {}  {}  {}{tail}",
                state(*uncommitted),
                super::format_age(*age_days),
                super::format_size(*bytes),
            )
        }
        WorktreeRow::Skipped {
            name,
            branch,
            uncommitted,
            age_days,
            bytes,
            what,
        } => format!(
            "worktree  {name}  {branch}  {}  {}  {}  skipped: removing it would lose {what}",
            state(*uncommitted),
            super::format_age(*age_days),
            super::format_size(*bytes),
        ),
        WorktreeRow::Running { name } => {
            format!("worktree  {name}  skipped: a running session works in it")
        }
        WorktreeRow::Uncertain { name, reason } => {
            format!("worktree  {name}  skipped: {reason}")
        }
    }
}

/// The listed bytes of every worktree prune will remove.
pub(crate) fn listed_bytes(planned: &Planned) -> u64 {
    planned.removals.iter().map(|removal| removal.bytes).sum()
}

/// The project keys in scope: the repository's project inside one, every
/// project under `projects/` outside one.
fn scope_keys(home: &Path, workspace: &Path) -> Vec<String> {
    if super::in_repository(workspace) {
        return vec![log::project_key(&doors::project(workspace))];
    }
    let mut keys = Vec::new();
    let Ok(projects) = std::fs::read_dir(home.join("projects")) else {
        return keys;
    };
    for project in projects.flatten() {
        if project.file_type().is_ok_and(|kind| kind.is_dir()) {
            keys.push(project.file_name().to_string_lossy().into_owned());
        }
    }
    keys.sort();
    keys
}

/// Whether `workspace` names a session working in the canonical worktree:
/// the workspace is the worktree or lies under it.
fn is_user(workspace: &Option<String>, canonical: &Path) -> bool {
    workspace
        .as_ref()
        .is_some_and(|work| Path::new(work).starts_with(canonical))
}

/// Holds every unheld user's lock, keeping each until the run ends: `true`
/// when a user is running, with `--force` too. A user already held is
/// never locked twice. With `dry_run` each taken lock is dropped at once,
/// so the plan holds none.
fn hold_users(
    users: &[log::Started],
    canonical: &Path,
    planned: &mut Planned,
    dry_run: bool,
) -> bool {
    for started in users {
        if !is_user(&started.workspace, canonical) {
            continue;
        }
        if planned.held.contains(&started.id.0) {
            continue;
        }
        match log::try_hold(&started.dir) {
            Ok(log::Hold::Held(lock)) => {
                planned.held.insert(started.id.0.clone());
                if !dry_run {
                    planned.locks.push(lock);
                }
            }
            Ok(log::Hold::Busy) | Err(_) => return true,
        }
    }
    false
}

/// What `inspect` found: the worktree, or the reason it is kept.
fn judged(dir: &Path) -> Result<worktree::Inspected, String> {
    match worktree::inspect(dir) {
        Ok(worktree::Inspection::Worktree(inspected)) => Ok(inspected),
        Ok(worktree::Inspection::NotAWorktree) => Err("not a git worktree".to_owned()),
        Ok(worktree::Inspection::Detached) => Err("its HEAD is detached".to_owned()),
        Err(error) => Err(format!("git cannot read it: {error}")),
    }
}

/// What removing a worktree with this status would lose: `None` when
/// nothing is uncommitted and no commit is unique.
fn losses(uncommitted: bool, unique_commits: u64) -> Option<String> {
    if unique_commits == 0 {
        if uncommitted {
            Some("uncommitted or ignored files".to_owned())
        } else {
            None
        }
    } else if uncommitted {
        Some("uncommitted or ignored files and commits found nowhere else".to_owned())
    } else {
        Some("commits found nowhere else".to_owned())
    }
}

/// A worktree's age in whole days, floored, from its directory's mtime: 0
/// when its mtime does not read or lies in the future.
fn age_days(dir: &Path, now: SystemTime) -> u64 {
    let at = std::fs::symlink_metadata(dir)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    now.duration_since(at)
        .map(|held| held.as_secs() / (24 * 60 * 60))
        .unwrap_or(0)
}

/// Holds any new user in the removal's project: `Err` when one is running
/// now, with `--force` too.
fn revalidate(home: &Path, planned: &mut Planned, removal: &PlannedRemoval) -> Result<(), String> {
    let Ok(canonical) = std::fs::canonicalize(&removal.dir) else {
        return Err("git cannot read it: the directory cannot be resolved".to_owned());
    };
    let sessions = home
        .join("projects")
        .join(&removal.project)
        .join("sessions");
    for started in log::started_sessions(home) {
        if started.dir.parent() != Some(sessions.as_path()) {
            continue;
        }
        if !is_user(&started.workspace, &canonical) {
            continue;
        }
        if planned.held.contains(&started.id.0) {
            continue;
        }
        match log::try_hold(&started.dir) {
            Ok(log::Hold::Held(lock)) => {
                planned.locks.push(lock);
                planned.held.insert(started.id.0.clone());
            }
            Ok(log::Hold::Busy) | Err(_) => {
                return Err("a running session works in it".to_owned());
            }
        }
    }
    Ok(())
}

/// A worktree row's name.
fn row_name(row: &WorktreeRow) -> &str {
    match row {
        WorktreeRow::Removable { name, .. }
        | WorktreeRow::Skipped { name, .. }
        | WorktreeRow::Running { name }
        | WorktreeRow::Uncertain { name, .. } => name,
    }
}

#[cfg(test)]
#[path = "worktrees_tests.rs"]
mod tests;
