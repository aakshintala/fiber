//! Which sessions prune deletes (Ruling 6): the scope filter, the graph,
//! cycles and selection, all over `log::started_sessions`.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use contract::SessionId;
use contract::clock::wall_ms;

/// A session prune will delete, with the hub call that deletes it.
pub(crate) struct Delete {
    /// The session to send.
    pub(crate) id: SessionId,
    /// Whether to send `cascade: true`.
    pub(crate) cascade: bool,
}

/// A session row prune prints.
pub(crate) enum SessionRow {
    /// An old exited session to delete.
    Deletable {
        /// The session.
        id: String,
        /// Whole days old, floored.
        age_days: u64,
        /// Logical bytes.
        bytes: u64,
        /// The session's directory.
        dir: PathBuf,
        /// The index into the deletes that removes it.
        delete: usize,
    },
    /// An old session kept because a session it is not deleting points at it.
    Blocked {
        /// The session.
        id: String,
        /// Whole days old, floored.
        age_days: u64,
        /// `D(s)` minus `C`, sorted.
        blockers: Vec<String>,
    },
    /// A session skipped with the cycle it sits in or under.
    Cycle {
        /// The session.
        id: String,
        /// Whole days old, floored.
        age_days: u64,
        /// The cyclic sessions in its closure, sorted.
        members: Vec<String>,
    },
    /// A session whose last line does not read.
    Unreadable {
        /// The session.
        id: String,
    },
    /// A dependent cascade deletes with its root.
    Continues {
        /// The session.
        id: String,
        /// Whole days old, floored; `None` when its last line does not
        /// read, which never blocks the root's cascade delete.
        age_days: Option<u64>,
        /// Logical bytes.
        bytes: u64,
        /// The session's directory.
        dir: PathBuf,
        /// The session it directly continues.
        parent: String,
        /// The index into the deletes that removes it.
        delete: usize,
    },
}

/// What prune deletes and lists on the session side.
pub(crate) struct Selected {
    /// The session rows, sessions by id.
    pub(crate) rows: Vec<SessionRow>,
    /// The hub calls, in send order.
    pub(crate) deletes: Vec<Delete>,
}

/// An old exited in-scope session with its age and size.
struct Candidate {
    id: String,
    dir: PathBuf,
    age_days: u64,
    bytes: u64,
}

/// Selects the sessions to delete: the scope filter, then Ruling 6 over
/// `log::started_sessions`. Without `older_than` no session is deleted
/// and no session is listed.
pub(crate) fn select(
    home: &Path,
    workspace: &Path,
    older_than: Option<Duration>,
    cascade: bool,
    now: SystemTime,
) -> Selected {
    let Some(older_than) = older_than else {
        return Selected {
            rows: Vec::new(),
            deletes: Vec::new(),
        };
    };
    let all = log::started_sessions(home);
    let mut by_id: HashMap<String, (PathBuf, Option<String>)> = HashMap::new();
    let mut children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for started in &all {
        by_id.entry(started.id.0.clone()).or_insert((
            started.dir.clone(),
            started.forked_from.as_ref().map(|id| id.0.clone()),
        ));
        if let Some(from) = started.forked_from.as_ref() {
            children
                .entry(from.0.clone())
                .or_default()
                .insert(started.id.0.clone());
        }
    }
    let dependents_of = |id: &str| dependents(&children, id);
    let cyclic: BTreeSet<String> = by_id
        .keys()
        .filter(|id| reaches(&children, id, id))
        .cloned()
        .collect();
    let resolved = doors::resolve_project(workspace);
    let in_repo = resolved.in_repository;
    let project = resolved.path;
    let sessions_dir = log::sessions_dir(home, &project);
    let mut memo: HashMap<String, bool> = HashMap::new();
    let now_ms = wall_ms(now);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    let mut in_scope = |_id: &str, dir: &Path, workspace: &Option<String>| -> bool {
        if in_repo {
            if dir.parent() != Some(sessions_dir.as_path()) {
                return false;
            }
            let Some(work) = workspace else {
                return true;
            };
            if !Path::new(work).exists() {
                return true;
            }
            *memo
                .entry(work.clone())
                .or_insert_with(|| doors::project(Path::new(work)) == project)
        } else {
            true
        }
    };
    for started in &all {
        if !in_scope(&started.id.0, &started.dir, &started.workspace) {
            continue;
        }
        if started.workspace.is_none() {
            continue;
        }
        let Ok(hold) = log::try_hold(&started.dir) else {
            continue;
        };
        let exited = matches!(hold, log::Hold::Held(_));
        drop(hold);
        if !exited {
            continue;
        }
        let Some(ts) = log::last_ts(&started.dir) else {
            unreadable.push(started.id.0.clone());
            continue;
        };
        let diff = now_ms.saturating_sub(ts);
        if Duration::from_millis(diff) <= older_than {
            continue;
        }
        candidates.push(Candidate {
            id: started.id.0.clone(),
            dir: started.dir.clone(),
            age_days: diff / (24 * 60 * 60 * 1000),
            bytes: log::session_bytes(&started.dir),
        });
    }
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    unreadable.sort();
    let in_c: BTreeSet<String> = candidates.iter().map(|c| c.id.clone()).collect();
    let depend_sets: HashMap<String, BTreeSet<String>> = candidates
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                dependents_of(&c.id).into_iter().collect::<BTreeSet<_>>(),
            )
        })
        .collect();
    let closure_has_cycle = |id: &str| -> bool {
        if cyclic.contains(id) {
            return true;
        }
        depend_sets
            .get(id)
            .is_some_and(|set| set.iter().any(|d| cyclic.contains(d)))
    };
    let cycle_members = |id: &str| -> Vec<String> {
        let mut members: BTreeSet<String> = BTreeSet::new();
        if cyclic.contains(id) {
            members.insert(id.to_owned());
        }
        if let Some(set) = depend_sets.get(id) {
            members.extend(set.iter().filter(|d| cyclic.contains(*d)).cloned());
        }
        members.into_iter().collect()
    };
    if cascade {
        select_cascade(
            &candidates,
            &depend_sets,
            &by_id,
            &unreadable,
            closure_has_cycle,
            cycle_members,
            now_ms,
        )
    } else {
        select_plain(
            &candidates,
            &in_c,
            &depend_sets,
            &unreadable,
            closure_has_cycle,
            cycle_members,
        )
    }
}

/// The transitive dependents of `id`, never `id` itself: breadth-first,
/// each once.
fn dependents(children: &BTreeMap<String, BTreeSet<String>>, id: &str) -> Vec<String> {
    let mut seen = BTreeSet::from([id.to_owned()]);
    let mut queue = VecDeque::from([id.to_owned()]);
    let mut found = Vec::new();
    while let Some(parent) = queue.pop_front() {
        if let Some(kids) = children.get(&parent) {
            for child in kids {
                if seen.insert(child.clone()) {
                    found.push(child.clone());
                    queue.push_back(child.clone());
                }
            }
        }
    }
    found
}

/// Whether the walk from `from` reaches `target` again in one or more steps.
fn reaches(children: &BTreeMap<String, BTreeSet<String>>, from: &str, target: &str) -> bool {
    let mut seen = BTreeSet::from([from.to_owned()]);
    let mut queue = VecDeque::from([from.to_owned()]);
    while let Some(parent) = queue.pop_front() {
        if let Some(kids) = children.get(&parent) {
            for child in kids {
                if child == target {
                    return true;
                }
                if seen.insert(child.clone()) {
                    queue.push_back(child.clone());
                }
            }
        }
    }
    false
}

/// Selection without `--cascade`: `s` is deleted when `D(s)` is a subset of
/// `C`, sent without cascade ordered by `|D|` then id.
fn select_plain(
    candidates: &[Candidate],
    in_c: &BTreeSet<String>,
    depend_sets: &HashMap<String, BTreeSet<String>>,
    unreadable: &[String],
    closure_has_cycle: impl Fn(&str) -> bool,
    cycle_members: impl Fn(&str) -> Vec<String>,
) -> Selected {
    let mut rows = Vec::new();
    let mut ordered: Vec<&Candidate> = Vec::new();
    for candidate in candidates {
        if closure_has_cycle(&candidate.id) {
            rows.push(SessionRow::Cycle {
                id: candidate.id.clone(),
                age_days: candidate.age_days,
                members: cycle_members(&candidate.id),
            });
            continue;
        }
        let empty = BTreeSet::new();
        let set = depend_sets.get(&candidate.id).unwrap_or(&empty);
        if set.iter().all(|d| in_c.contains(d)) {
            ordered.push(candidate);
        } else {
            let mut blockers: Vec<String> =
                set.iter().filter(|d| !in_c.contains(*d)).cloned().collect();
            blockers.sort();
            rows.push(SessionRow::Blocked {
                id: candidate.id.clone(),
                age_days: candidate.age_days,
                blockers,
            });
        }
    }
    ordered.sort_by(|a, b| {
        let ka = depend_sets.get(&a.id).map(BTreeSet::len).unwrap_or(0);
        let kb = depend_sets.get(&b.id).map(BTreeSet::len).unwrap_or(0);
        ka.cmp(&kb).then(a.id.cmp(&b.id))
    });
    let mut deletes = Vec::new();
    for candidate in ordered {
        let delete = deletes.len();
        deletes.push(Delete {
            id: SessionId(candidate.id.clone()),
            cascade: false,
        });
        rows.push(SessionRow::Deletable {
            id: candidate.id.clone(),
            age_days: candidate.age_days,
            bytes: candidate.bytes,
            dir: candidate.dir.clone(),
            delete,
        });
    }
    for id in unreadable {
        rows.push(SessionRow::Unreadable { id: id.clone() });
    }
    rows.sort_by(row_id_cmp);
    Selected { rows, deletes }
}

/// Selection with `--cascade`: the roots get one `cascade` delete each,
/// and the rows list each root and every member of its `D`.
#[allow(
    clippy::too_many_arguments,
    reason = "one hand-off of the cascade selection: candidates, sets, lookups and rows"
)]
fn select_cascade(
    candidates: &[Candidate],
    depend_sets: &HashMap<String, BTreeSet<String>>,
    by_id: &HashMap<String, (PathBuf, Option<String>)>,
    unreadable: &[String],
    closure_has_cycle: impl Fn(&str) -> bool,
    cycle_members: impl Fn(&str) -> Vec<String>,
    now_ms: u64,
) -> Selected {
    let clean: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| !closure_has_cycle(&c.id))
        .collect();
    let mut roots: Vec<&Candidate> = clean
        .iter()
        .filter(|c| {
            !clean.iter().any(|t| {
                t.id != c.id
                    && depend_sets
                        .get(&t.id)
                        .is_some_and(|set| set.contains(&c.id))
            })
        })
        .cloned()
        .collect();
    roots.sort_by(|a, b| a.id.cmp(&b.id));
    let mut rows = Vec::new();
    let mut deletes = Vec::new();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for root in &roots {
        let delete = deletes.len();
        deletes.push(Delete {
            id: SessionId(root.id.clone()),
            cascade: true,
        });
        rows.push(SessionRow::Deletable {
            id: root.id.clone(),
            age_days: root.age_days,
            bytes: root.bytes,
            dir: root.dir.clone(),
            delete,
        });
        listed.insert(root.id.clone());
        let empty = BTreeSet::new();
        let set = depend_sets.get(&root.id).unwrap_or(&empty);
        let mut members: Vec<String> = set.iter().cloned().collect();
        members.sort();
        for member in members {
            if !listed.insert(member.clone()) {
                continue;
            }
            let Some((dir, forked)) = by_id.get(&member) else {
                continue;
            };
            let age_days =
                log::last_ts(dir).map(|ts| now_ms.saturating_sub(ts) / (24 * 60 * 60 * 1000));
            let parent = forked.clone().unwrap_or_else(|| root.id.clone());
            rows.push(SessionRow::Continues {
                id: member.clone(),
                age_days,
                bytes: log::session_bytes(dir),
                dir: dir.clone(),
                parent,
                delete,
            });
        }
    }
    for candidate in candidates {
        if closure_has_cycle(&candidate.id) {
            rows.push(SessionRow::Cycle {
                id: candidate.id.clone(),
                age_days: candidate.age_days,
                members: cycle_members(&candidate.id),
            });
        }
    }
    for id in unreadable {
        if listed.insert(id.clone()) {
            rows.push(SessionRow::Unreadable { id: id.clone() });
        }
    }
    rows.sort_by(row_id_cmp);
    Selected { rows, deletes }
}

/// Sorts session rows by id.
fn row_id_cmp(a: &SessionRow, b: &SessionRow) -> std::cmp::Ordering {
    row_id(a).cmp(row_id(b))
}

/// A session row's id.
fn row_id(row: &SessionRow) -> &str {
    match row {
        SessionRow::Deletable { id, .. }
        | SessionRow::Blocked { id, .. }
        | SessionRow::Cycle { id, .. }
        | SessionRow::Unreadable { id }
        | SessionRow::Continues { id, .. } => id,
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
