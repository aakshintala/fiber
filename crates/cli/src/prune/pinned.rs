//! Which pinned copies prune lists and removes (`docs/invocation.md`,
//! "Deleting and pruning"): the ownership scan, the worktree match, and
//! removal with its revalidation. `cli` keeps the decisions; `extensions`
//! owns the copies, and the `worktree` crate lists the worktrees.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::clock::Clock;

use super::worktrees::scope_keys;

/// A pinned row prune prints.
pub(super) enum PinnedRow {
    /// A copy no worktree matches: removed after confirmation.
    Removable {
        /// The content hash.
        hash: String,
        /// Logical bytes.
        bytes: u64,
    },
    /// A copy kept for a reason: held, or prune cannot judge it.
    Kept {
        /// The content hash.
        hash: String,
        /// Why it is kept.
        why: String,
    },
    /// The whole pinned step is skipped, with why.
    Skipped {
        /// Why nothing is judged.
        why: String,
    },
}

/// A copy prune will remove, with what its revalidation needs.
pub(crate) struct PlannedCopy {
    /// The content hash.
    pub(crate) hash: String,
    /// The listed bytes.
    bytes: u64,
    /// The owner keys at selection, sorted.
    owners: Vec<String>,
    /// Whether the per-machine record named it at selection.
    machine_wide: bool,
    /// Every resolving identity at selection, with its trees then.
    identities: Vec<SelectedIdentity>,
}

/// An identity that resolved at selection.
struct SelectedIdentity {
    /// The owner it resolved for.
    owner: String,
    /// The identity directory.
    path: PathBuf,
    /// Every tree judged, with its hashes then.
    trees: Vec<SelectedTree>,
}

/// A tree judged at selection.
struct SelectedTree {
    /// The tree's path.
    path: PathBuf,
    /// The content hashes its files matched then.
    hashes: BTreeSet<String>,
}

/// What prune lists and removes on the pinned side.
pub(super) struct Plan {
    /// The pinned rows, by hash.
    pub(super) rows: Vec<PinnedRow>,
    /// The removals, in row order.
    pub(crate) removals: Vec<PlannedCopy>,
}

/// What removing the planned copies freed and what it could not.
pub(super) struct RemovedPinned {
    /// The listed bytes of every removed copy.
    pub(super) freed: u64,
    /// Each copy that stayed: `pinned <hash>`, and why.
    pub(super) failures: Vec<(String, ErrorCode, String)>,
    /// How many copies prune tried to remove.
    pub(super) total: usize,
}

/// The environment variables that redirect `git ls-files` elsewhere, so
/// prune judges nothing while any is set.
const REDIRECTING_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

/// Lists the pinned copies in scope with no worktree match: inside a git
/// repository the repository's project, outside one every project, and
/// hub-wide every project. A copy another project also approved is kept
/// and not listed, as is a copy no project record names at all. A
/// per-machine record alone keeps a copy; it never lists one its project
/// records name.
pub(super) fn select(
    home: &Path,
    started: &[log::Started],
    scope: super::Scope<'_>,
    clock: &dyn Clock,
) -> Plan {
    select_with(
        home,
        started,
        scope,
        clock,
        &|name| std::env::var_os(name),
        &|dir| worktree::list(dir),
    )
}

/// Lists with the environment lookup and the worktree lister injected, so
/// tests need neither the process environment nor `git`.
fn select_with(
    home: &Path,
    started: &[log::Started],
    scope: super::Scope<'_>,
    clock: &dyn Clock,
    env: &dyn Fn(&str) -> Option<OsString>,
    list: &dyn Fn(&Path) -> Result<Vec<worktree::Listed>, worktree::Error>,
) -> Plan {
    for name in REDIRECTING_ENV {
        if env(name).is_some() {
            return Plan {
                rows: vec![PinnedRow::Skipped {
                    why: format!("{name} is set, so git may read another repository"),
                }],
                removals: Vec::new(),
            };
        }
    }
    let kept = match extensions::Store::kept_copies(home) {
        Ok(kept) => kept,
        Err(error) => {
            return Plan {
                rows: vec![PinnedRow::Skipped {
                    why: error.to_string(),
                }],
                removals: Vec::new(),
            };
        }
    };
    let keys = scope_keys(home, scope);
    select_candidates(home, started, scope, clock, list, &keys, &kept)
}

/// Lists the candidates among `kept`: a copy with at least one
/// project-record owner and every owner in scope. A copy with no
/// project-record owner is kept, and a per-machine record changes nothing.
fn select_candidates(
    home: &Path,
    started: &[log::Started],
    scope: super::Scope<'_>,
    clock: &dyn Clock,
    list: &dyn Fn(&Path) -> Result<Vec<worktree::Listed>, worktree::Error>,
    keys: &[String],
    kept: &[extensions::KeptCopy],
) -> Plan {
    let candidates: Vec<&extensions::KeptCopy> = kept
        .iter()
        .filter(|kept| {
            !kept.owners.is_empty() && kept.owners.iter().all(|owner| keys.contains(owner))
        })
        .collect();
    // Every recorded identity by its session's project key.
    let mut identities: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for started in started {
        let Some(key) = session_key(started) else {
            continue;
        };
        let Some(identity) = identity_of(started) else {
            continue;
        };
        identities.entry(key).or_default().push(identity);
    }
    let mut index = extensions::Index::load(home);
    let mut rows = Vec::new();
    let mut removals = Vec::new();
    for kept in candidates {
        // The owner's every recorded identity. Inside a repository the
        // workspace's own repository is one more identity of its project.
        let mut owned: Vec<(String, PathBuf)> = Vec::new();
        for owner in &kept.owners {
            if let Some(more) = identities.get(owner) {
                owned.extend(
                    more.iter()
                        .map(|identity| (owner.clone(), identity.clone())),
                );
            }
        }
        if let super::Scope::Project(workspace) = scope {
            let resolved = doors::resolve_project(workspace);
            if resolved.in_repository {
                owned.push((log::project_key(&resolved.path), workspace.to_path_buf()));
            }
        }
        match judge(&kept.hash, clock, list, &mut index, &owned) {
            Judged::Matched => {}
            Judged::Uncertain(why) => rows.push(PinnedRow::Kept {
                hash: kept.hash.clone(),
                why,
            }),
            Judged::Removable(identities) => {
                match extensions::Store::remove_copy(home, &kept.hash, true, &mut || Ok(())) {
                    Ok(extensions::CopyRemoval::Held) => rows.push(PinnedRow::Kept {
                        hash: kept.hash.clone(),
                        why: "a session holds it".to_owned(),
                    }),
                    Ok(extensions::CopyRemoval::Removed | extensions::CopyRemoval::Absent) => {
                        let bytes = log::session_bytes(&kept.dir);
                        rows.push(PinnedRow::Removable {
                            hash: kept.hash.clone(),
                            bytes,
                        });
                        removals.push(PlannedCopy {
                            hash: kept.hash.clone(),
                            bytes,
                            owners: kept.owners.clone(),
                            machine_wide: kept.machine_wide,
                            identities,
                        });
                    }
                    // The probe's check never refuses.
                    Ok(extensions::CopyRemoval::Refused(_)) => {}
                    Err(error) => rows.push(PinnedRow::Kept {
                        hash: kept.hash.clone(),
                        why: error.to_string(),
                    }),
                }
            }
        }
    }
    rows.sort_by(|a, b| row_hash(a).cmp(row_hash(b)));
    index.save().unwrap_or(());
    Plan { rows, removals }
}

/// How `judge` left a candidate.
enum Judged {
    /// Some owner's tree matched it: kept, not listed.
    Matched,
    /// Kept with its reason.
    Uncertain(String),
    /// No tree matched it: every resolving identity, with its trees.
    Removable(Vec<SelectedIdentity>),
}

/// What one identity resolved to.
struct IdentityOutcome {
    /// Whether the identity resolved to its owner's key.
    resolved: bool,
    /// Every tree judged, with its hashes.
    trees: Vec<SelectedTree>,
    /// A tree that could not be read, refusing every candidate.
    hard: Option<String>,
    /// A locked tree that is missing, refusing only unmatched copies.
    soft: Option<String>,
}

/// Judges one identity: resolves it to its owner's key, then lists and
/// hashes its trees. A directory that is gone, resolves elsewhere, or
/// was already judged through another identity contributes no match.
fn judge_identity(
    owner: &str,
    identity: &Path,
    clock: &dyn Clock,
    list: &dyn Fn(&Path) -> Result<Vec<worktree::Listed>, worktree::Error>,
    index: &mut extensions::Index,
    seen: &mut HashSet<PathBuf>,
) -> IdentityOutcome {
    let mut outcome = IdentityOutcome {
        resolved: false,
        trees: Vec::new(),
        hard: None,
        soft: None,
    };
    // A directory that is gone contributes no match.
    match fs::symlink_metadata(identity) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return outcome,
        Err(e) => {
            outcome.hard = Some(format!(
                "the worktree {} cannot be read: {e}",
                identity.display()
            ));
            return outcome;
        }
        Ok(_) => {}
    }
    let resolved = doors::resolve_project(identity);
    if log::project_key(&resolved.path) != owner {
        return outcome;
    }
    if !seen.insert(resolved.path.clone()) {
        // Already judged through another identity: resolved, but its
        // trees are recorded once.
        outcome.resolved = true;
        return outcome;
    }
    outcome.resolved = true;
    if !resolved.in_repository {
        // Outside a repository a project's tree is its directory.
        match hash_tree(identity, clock, index) {
            Ok(hashes) => outcome.trees.push(SelectedTree {
                path: identity.to_path_buf(),
                hashes,
            }),
            Err(error) => {
                outcome.hard = Some(format!(
                    "the worktree {} cannot be read: {error}",
                    identity.display()
                ));
            }
        }
        return outcome;
    }
    match list(identity) {
        Err(error) => {
            outcome.hard = Some(format!("git cannot list the worktrees: {error}"));
        }
        Ok(entries) => {
            for entry in entries {
                // A bare entry has no files.
                if entry.bare {
                    continue;
                }
                match fs::symlink_metadata(&entry.path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        if entry.locked {
                            outcome.soft = outcome.soft.or(Some(format!(
                                "the locked worktree {} is missing",
                                entry.path.display()
                            )));
                        } else {
                            outcome.trees.push(SelectedTree {
                                path: entry.path.clone(),
                                hashes: BTreeSet::new(),
                            });
                        }
                    }
                    Err(e) => {
                        outcome.hard = outcome.hard.or(Some(format!(
                            "the worktree {} cannot be read: {e}",
                            entry.path.display()
                        )));
                    }
                    Ok(_) => match hash_tree(&entry.path, clock, index) {
                        Ok(hashes) => outcome.trees.push(SelectedTree {
                            path: entry.path.clone(),
                            hashes,
                        }),
                        Err(error) => {
                            outcome.hard = outcome.hard.or(Some(format!(
                                "the worktree {} cannot be read: {error}",
                                entry.path.display()
                            )));
                        }
                    },
                }
            }
        }
    }
    outcome
}

/// Judges one kept copy against its owners' trees: every distinct recorded
/// repository (or workspace) its sessions named, de-duplicated by the
/// repository root. An identity that no longer resolves contributes no
/// match. A directory counts only when it resolves to its owner's key.
fn judge(
    hash: &str,
    clock: &dyn Clock,
    list: &dyn Fn(&Path) -> Result<Vec<worktree::Listed>, worktree::Error>,
    index: &mut extensions::Index,
    owned: &[(String, PathBuf)],
) -> Judged {
    // A tree that cannot be read makes every candidate of its owner
    // uncertain, even one another tree matches. A locked tree that is
    // missing only matters when nothing else matches: it may hold the
    // copy on an unmounted drive.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut identities = Vec::new();
    let mut resolved_any = false;
    let mut uncertain_hard: Option<String> = None;
    let mut uncertain_soft: Option<String> = None;
    for (owner, identity) in owned {
        let outcome = judge_identity(owner, identity, clock, list, index, &mut seen);
        resolved_any = resolved_any || outcome.resolved;
        uncertain_hard = uncertain_hard.or(outcome.hard);
        uncertain_soft = uncertain_soft.or(outcome.soft);
        if outcome.resolved {
            identities.push(SelectedIdentity {
                owner: owner.clone(),
                path: identity.clone(),
                trees: outcome.trees,
            });
        }
    }
    if let Some(why) = uncertain_hard {
        return Judged::Uncertain(why);
    }
    let matched = identities
        .iter()
        .any(|identity| identity.trees.iter().any(|tree| tree.hashes.contains(hash)));
    if matched {
        return Judged::Matched;
    }
    if let Some(why) = uncertain_soft {
        return Judged::Uncertain(why);
    }
    if !resolved_any {
        return Judged::Uncertain("its project's directory cannot be found".to_owned());
    }
    Judged::Removable(identities)
}

/// A session's project key: the name of its directory's grandparent. No
/// path is built from it.
fn session_key(started: &log::Started) -> Option<String> {
    started
        .dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|key| key.to_string_lossy().into_owned())
}

/// A session's identity: its first log line's recorded repository, else
/// that line's workspace. A line that does not read as `session_started`
/// gives no identity.
fn identity_of(started: &log::Started) -> Option<PathBuf> {
    let mut lines = log::lines(&started.dir).ok()?;
    let first = lines.next()?;
    let line = first.ok()?;
    match contract::events::Event::from_envelope(&line) {
        Ok(Some(contract::events::Event::SessionStarted(session))) => Some(
            session
                .worktree
                .and_then(|worktree| worktree.repository)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(session.workspace)),
        ),
        _ => None,
    }
}

/// A row's hash, for the listing order. Whole-step rows sort first.
fn row_hash(row: &PinnedRow) -> &str {
    match row {
        PinnedRow::Removable { hash, .. } | PinnedRow::Kept { hash, .. } => hash,
        PinnedRow::Skipped { .. } => "",
    }
}

/// One pinned row, fields separated by two spaces.
pub(super) fn pinned_line(row: &PinnedRow) -> String {
    match row {
        PinnedRow::Removable { hash, bytes } => {
            format!("pinned  {hash}  {}", super::format_size(*bytes))
        }
        PinnedRow::Kept { hash, why } => {
            format!("pinned  {hash}  skipped: {why}")
        }
        PinnedRow::Skipped { why } => {
            format!("pinned  skipped: {why}")
        }
    }
}

/// The listed bytes of every pinned copy prune will remove.
pub(super) fn listed_bytes(plan: &Plan) -> u64 {
    plan.removals.iter().map(|removal| removal.bytes).sum()
}

/// Removes every planned copy, in row order: `before_remove` runs just
/// before a copy's removal, so tests can take a hold or change a tree
/// between listing and removal. Production passes a no-op. Each removal
/// revalidates under `pinned/.lock`: a changed owner set, a tree that now
/// matches, or an unreadable tree refuses it.
pub(super) fn remove_planned(
    home: &Path,
    plan: &Plan,
    clock: &dyn Clock,
    before_remove: &mut dyn FnMut(&str),
) -> RemovedPinned {
    let mut freed = 0_u64;
    let mut failures = Vec::new();
    for removal in &plan.removals {
        before_remove(&removal.hash);
        let mut check = || revalidate(home, clock, removal);
        match extensions::Store::remove_copy(home, &removal.hash, false, &mut check) {
            Ok(extensions::CopyRemoval::Removed) => freed += removal.bytes,
            Ok(extensions::CopyRemoval::Absent) => {}
            Ok(extensions::CopyRemoval::Held) => failures.push((
                format!("pinned {}", removal.hash),
                ErrorCode::IoFailed,
                "a session holds it".to_owned(),
            )),
            Ok(extensions::CopyRemoval::Refused(reason)) => failures.push((
                format!("pinned {}", removal.hash),
                ErrorCode::IoFailed,
                reason,
            )),
            Err(error) => failures.push((
                format!("pinned {}", removal.hash),
                ErrorCode::IoFailed,
                error.to_string(),
            )),
        }
    }
    RemovedPinned {
        freed,
        failures,
        total: plan.removals.len(),
    }
}

/// Revalidates one planned copy under `pinned/.lock`: its owners and its
/// per-machine record are read again, and its identities' trees listed
/// and hashed again with a fresh index. A tree this run removed keeps its
/// selection-time hashes.
fn revalidate(home: &Path, clock: &dyn Clock, planned: &PlannedCopy) -> Result<(), String> {
    let fresh = extensions::Store::kept_copies(home).map_err(|error| error.to_string())?;
    let current = fresh.iter().find(|kept| kept.hash == planned.hash);
    let owners = current.map(|kept| kept.owners.clone()).unwrap_or_default();
    let machine_wide = current.map(|kept| kept.machine_wide).unwrap_or(false);
    if owners != planned.owners || machine_wide != planned.machine_wide {
        return Err("its approvals changed since it was listed".to_owned());
    }
    let mut index = extensions::Index::load(home);
    for identity in &planned.identities {
        for (_, hashes) in refresh(identity, clock, &mut index)? {
            if hashes.contains(&planned.hash) {
                return Err("a worktree matches it now".to_owned());
            }
        }
    }
    Ok(())
}

/// The identity's current trees with fresh hashes: re-listed and
/// re-hashed, so a worktree added after the listing still refuses the
/// removal. A selection tree this run removed keeps its selection-time
/// hashes; an identity that no longer resolves keeps them all.
fn refresh(
    identity: &SelectedIdentity,
    clock: &dyn Clock,
    index: &mut extensions::Index,
) -> Result<Vec<(PathBuf, BTreeSet<String>)>, String> {
    let fallback = || {
        identity
            .trees
            .iter()
            .map(|tree| (tree.path.clone(), tree.hashes.clone()))
            .collect()
    };
    let mut seen = HashSet::new();
    let outcome = judge_identity(
        &identity.owner,
        &identity.path,
        clock,
        &|dir| worktree::list(dir),
        index,
        &mut seen,
    );
    if let Some(reason) = outcome.hard {
        return Err(reason);
    }
    if !outcome.resolved {
        return Ok(fallback());
    }
    let mut current: Vec<(PathBuf, BTreeSet<String>)> = outcome
        .trees
        .into_iter()
        .map(|tree| (tree.path, tree.hashes))
        .collect();
    merge_removed(&mut current, &identity.trees);
    Ok(current)
}

/// Adds every selection-time tree the re-listing no longer names, with its
/// selection-time hashes: a tree this run removed still counts. Selection
/// lists a copy only when no tree matched it, so an added tree never holds
/// the planned hash and cannot change the verdict; no test through the
/// public API can stage a case where dropping or duplicating one changes
/// what prune removes.
#[cfg_attr(false, mutants::skip)]
fn merge_removed(current: &mut Vec<(PathBuf, BTreeSet<String>)>, previous: &[SelectedTree]) {
    for tree in previous {
        if !current.iter().any(|(path, _)| path == &tree.path) {
            current.push((tree.path.clone(), tree.hashes.clone()));
        }
    }
}

/// The content hashes the declared code in the tree at `path` matches.
fn hash_tree(
    path: &Path,
    clock: &dyn Clock,
    index: &mut extensions::Index,
) -> Result<BTreeSet<String>, String> {
    extensions::declared_items(path, clock)
        .map_err(|error| error.to_string())?
        .iter()
        .map(|item| extensions::hash(index, item).map_err(|error| error.to_string()))
        .collect()
}

#[cfg(test)]
#[path = "pinned_tests.rs"]
mod tests;
