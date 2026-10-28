//! Tests for the session selection: age, locks, the graph, cycles and
//! scope, under a fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use contract::clock::{Clock, wall_ms};
use fakes::Deadline;
use serde_json::json;

use super::*;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

fn wall() -> SystemTime {
    fakes::clock::FakeClock::new().wall()
}

fn older_than() -> Duration {
    Duration::from_secs(30 * 24 * 60 * 60)
}

/// Fiber home and a workspace outside any repository, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("cli-prune-sessions"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("workspace")).unwrap()
    }

    fn sessions(&self) -> PathBuf {
        log::sessions_dir(&self.home(), &worktree::project(&self.workspace()))
    }

    /// Session `id` continuing `from`, with `workspace` and a last `ts`:
    /// its log, one artifact and no lock.
    fn session(&self, id: &str, workspace: &str, from: Option<&str>, ts: u64) {
        session_in(&self.sessions().join(id), workspace, from, ts);
    }

    fn select(&self, cascade: bool) -> Selected {
        select(
            &self.home(),
            &log::started_sessions(&self.home()),
            &self.workspace(),
            Some(older_than()),
            cascade,
            wall(),
        )
    }

    fn deletes(&self, cascade: bool) -> Vec<String> {
        self.select(cascade)
            .deletes
            .iter()
            .map(|d| d.id.0.clone())
            .collect()
    }
}

fn session_in(dir: &Path, workspace: &str, from: Option<&str>, ts: u64) {
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": workspace});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "seq": 0, "payload": payload});
    let last = json!({"kind": "x", "ts": ts});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"bytes").unwrap();
}

fn old_ts() -> u64 {
    0
}

/// A moved parent's log: the `session_started`, then the `moved` last
/// line naming the child, as a prune meets it.
fn moved_root(dir: &Path, workspace: &str, child: &str, ts: u64) {
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({"kind": "session_started", "seq": 0,
        "payload": {"workspace": workspace}});
    let last = json!({"kind": "moved", "ts": ts,
        "payload": {"new_session_id": child, "seq": 1, "workspace": "/w2"}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"bytes").unwrap();
}

/// A workspace outside the setup's, so its sessions inventory under
/// another project.
fn other_workspace(setup: &Setup) -> (PathBuf, PathBuf) {
    let workspace = setup.root.path().join("elsewhere");
    fs::create_dir_all(&workspace).unwrap();
    let sessions = log::sessions_dir(&setup.home(), &worktree::project(&workspace));
    (workspace, sessions)
}

fn young_ts() -> u64 {
    wall_ms(wall())
}

fn row_ids(rows: &[SessionRow]) -> Vec<&str> {
    rows.iter().map(super::row_id).collect()
}

#[test]
fn an_age_exactly_equal_to_older_than_is_kept() {
    let setup = Setup::new();
    let ts = wall_ms(wall()) - 30 * DAY_MS;
    setup.session("s_00000000000000a1", "/w", None, ts);
    assert!(setup.deletes(false).is_empty());
    assert!(setup.deletes(true).is_empty());
}

#[test]
fn selection_reports_floored_whole_days_at_the_boundary() {
    let setup = Setup::new();
    let now = wall_ms(wall());
    setup.session("s_00000000000000a1", "/w", None, now - 30 * DAY_MS - 1);
    setup.session(
        "s_00000000000000b2",
        "/w",
        None,
        now - 31 * DAY_MS - 12 * 60 * 60 * 1000,
    );
    let selected = setup.select(false);
    assert_eq!(
        setup.deletes(false),
        ["s_00000000000000a1", "s_00000000000000b2"]
    );
    let mut ages = Vec::new();
    for row in &selected.rows {
        if let SessionRow::Deletable { age_days, .. } = row {
            ages.push(*age_days);
        } else {
            panic!("a boundary session is deletable: {}", super::row_id(row));
        }
    }
    assert_eq!(ages, [30, 31]);
}

#[test]
fn a_cascade_dependent_with_an_unreadable_last_line_is_still_deleted() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    let child = setup.sessions().join("s_00000000000000b2");
    fs::create_dir_all(child.join("artifacts")).unwrap();
    let first = json!({
        "kind": "session_started",
        "seq": 0,
        "payload": {
            "workspace": "/w",
            "forked_from": {"session_id": "s_00000000000000a1", "seq": 1},
        },
    });
    fs::write(child.join("events.jsonl"), format!("{first}\nnot json\n")).unwrap();
    fs::write(child.join("artifacts/a.txt"), b"bytes").unwrap();
    let selected = setup.select(true);
    // One cascade delete covers both: the root's delete removes the
    // unreadable dependent too.
    assert_eq!(selected.deletes.len(), 1);
    assert_eq!(selected.deletes[0].id.0, "s_00000000000000a1");
    assert!(selected.deletes[0].cascade);
    assert_eq!(
        row_ids(&selected.rows),
        ["s_00000000000000a1", "s_00000000000000b2"]
    );
    let dependent = &selected.rows[1];
    assert!(matches!(dependent, SessionRow::Continues { .. }));
    if let SessionRow::Continues {
        age_days,
        bytes,
        dir,
        parent,
        delete,
        ..
    } = dependent
    {
        assert_eq!(*age_days, None);
        assert_eq!(*parent, "s_00000000000000a1");
        assert_eq!(*delete, 0);
        assert_eq!(*dir, child);
        assert!(*bytes > 0);
    }
}

#[test]
fn a_cascade_dependent_reports_its_floored_age() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    let half_day_ms = 12 * 60 * 60 * 1000;
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        wall_ms(wall()) - 10 * DAY_MS - half_day_ms,
    );
    let selected = setup.select(true);
    assert_eq!(selected.deletes.len(), 1);
    assert!(matches!(&selected.rows[1], SessionRow::Continues { .. }));
    if let SessionRow::Continues { age_days, .. } = &selected.rows[1] {
        assert_eq!(*age_days, Some(10));
    }
}

#[test]
fn a_held_lock_excludes_a_session() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    let dir = setup.sessions().join("s_00000000000000a1");
    fs::write(dir.join("session.lock"), b"").unwrap();
    let file = fs::File::open(dir.join("session.lock")).unwrap();
    file.try_lock().unwrap();
    assert!(setup.deletes(false).is_empty());
    drop(file);
    assert_eq!(setup.deletes(false), ["s_00000000000000a1"]);
}

#[test]
fn an_unreadable_last_line_is_skipped_with_its_reason() {
    let setup = Setup::new();
    let dir = setup.sessions().join("s_00000000000000a1");
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"kind": "session_started", "payload": {"workspace": "/w"}});
    fs::write(dir.join("events.jsonl"), format!("{first}\nnot json\n")).unwrap();
    let selected = setup.select(false);
    assert!(selected.deletes.is_empty());
    assert_eq!(row_ids(&selected.rows), ["s_00000000000000a1"]);
    assert!(matches!(&selected.rows[0], SessionRow::Unreadable { .. }));
}

#[test]
fn a_young_fork_blocks_an_old_root_and_the_row_names_it() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        young_ts(),
    );
    let selected = setup.select(false);
    assert!(selected.deletes.is_empty());
    // The young fork is not listed without cascade.
    assert_eq!(row_ids(&selected.rows), ["s_00000000000000a1"]);
    assert!(matches!(&selected.rows[0], SessionRow::Blocked { .. }));
    if let SessionRow::Blocked { blockers, .. } = &selected.rows[0] {
        assert_eq!(blockers, &["s_00000000000000b2".to_owned()]);
    }
}

#[test]
fn a_young_moved_child_blocks_an_old_root_and_the_row_names_it() {
    let setup = Setup::new();
    moved_root(
        &setup.sessions().join("s_00000000000000a1"),
        "/w",
        "s_00000000000000b2",
        old_ts(),
    );
    // The moved child lives in another project: the inventory still
    // names it as the old root's blocker.
    let (other, sessions) = other_workspace(&setup);
    session_in(
        &sessions.join("s_00000000000000b2"),
        other.to_str().unwrap(),
        Some("s_00000000000000a1"),
        young_ts(),
    );
    let selected = setup.select(false);
    assert!(selected.deletes.is_empty());
    // The young moved child is not listed without cascade.
    assert_eq!(row_ids(&selected.rows), ["s_00000000000000a1"]);
    assert!(matches!(&selected.rows[0], SessionRow::Blocked { .. }));
    if let SessionRow::Blocked { blockers, .. } = &selected.rows[0] {
        assert_eq!(blockers, &["s_00000000000000b2".to_owned()]);
    }
}

#[test]
fn with_cascade_one_delete_for_the_root_covers_the_young_moved_child() {
    let setup = Setup::new();
    moved_root(
        &setup.sessions().join("s_00000000000000a1"),
        "/w",
        "s_00000000000000b2",
        old_ts(),
    );
    let (other, sessions) = other_workspace(&setup);
    session_in(
        &sessions.join("s_00000000000000b2"),
        other.to_str().unwrap(),
        Some("s_00000000000000a1"),
        young_ts(),
    );
    let selected = setup.select(true);
    assert_eq!(selected.deletes.len(), 1);
    assert_eq!(selected.deletes[0].id.0, "s_00000000000000a1");
    assert!(selected.deletes[0].cascade);
    assert_eq!(
        row_ids(&selected.rows),
        ["s_00000000000000a1", "s_00000000000000b2"]
    );
    assert!(matches!(
        &selected.rows[1],
        SessionRow::Continues { parent, .. } if parent == "s_00000000000000a1"
    ));
}

#[test]
fn a_chain_of_old_through_young_blocks_both_old_sessions() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        old_ts(),
    );
    setup.session(
        "s_00000000000000c3",
        "/w",
        Some("s_00000000000000b2"),
        young_ts(),
    );
    let selected = setup.select(false);
    assert!(selected.deletes.is_empty());
    assert_eq!(selected.rows.len(), 2);
    for row in &selected.rows {
        assert!(matches!(row, SessionRow::Blocked { .. }));
        if let SessionRow::Blocked { blockers, .. } = row {
            assert_eq!(blockers, &["s_00000000000000c3".to_owned()]);
        }
    }
}

#[test]
fn old_root_plus_old_fork_without_cascade_deletes_the_fork_first() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        old_ts(),
    );
    let selected = setup.select(false);
    assert_eq!(
        selected
            .deletes
            .iter()
            .map(|d| (d.id.0.as_str(), d.cascade))
            .collect::<Vec<_>>(),
        [("s_00000000000000b2", false), ("s_00000000000000a1", false)]
    );
}

#[test]
fn with_cascade_one_delete_for_the_root_covers_the_young_fork() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        young_ts(),
    );
    let selected = setup.select(true);
    assert_eq!(selected.deletes.len(), 1);
    assert_eq!(selected.deletes[0].id.0, "s_00000000000000a1");
    assert!(selected.deletes[0].cascade);
    assert_eq!(
        row_ids(&selected.rows),
        ["s_00000000000000a1", "s_00000000000000b2"]
    );
    assert!(matches!(
        &selected.rows[1],
        SessionRow::Continues { parent, .. } if parent == "s_00000000000000a1"
    ));
}

#[test]
fn in_project_scope_a_session_from_another_project_is_excluded_and_a_missing_workspace_is_included()
{
    let root = fakes::TempDir::new("cli-prune-scope");
    let repo = root.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    let workspace = fs::canonicalize(&repo).unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let sessions = log::sessions_dir(&home, &worktree::project(&workspace));
    let other = root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let missing = root.path().join("gone");
    session_in(
        &sessions.join("s_00000000000000a1"),
        other.to_str().unwrap(),
        None,
        old_ts(),
    );
    session_in(
        &sessions.join("s_00000000000000b2"),
        missing.to_str().unwrap(),
        None,
        old_ts(),
    );
    let selected = select(
        &home,
        &log::started_sessions(&home),
        &workspace,
        Some(older_than()),
        false,
        wall(),
    );
    assert_eq!(
        selected
            .deletes
            .iter()
            .map(|d| d.id.0.clone())
            .collect::<Vec<_>>(),
        ["s_00000000000000b2"]
    );
}

#[test]
fn outside_a_repository_every_project_is_in_scope() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    let home = setup.home();
    let projects: Vec<String> = fs::read_dir(home.join("projects"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(projects.len(), 1);
    let other_key = format!("{}-other", projects[0]);
    let other_dir = home
        .join("projects")
        .join(&other_key)
        .join("sessions")
        .join("s_00000000000000b2");
    session_in(&other_dir, "/elsewhere", None, old_ts());
    let mut deletes = setup.deletes(false);
    deletes.sort();
    assert_eq!(deletes, ["s_00000000000000a1", "s_00000000000000b2"]);
}

#[test]
fn a_cycle_of_old_sessions_is_skipped_in_both_modes_naming_both() {
    let setup = Setup::new();
    setup.session(
        "s_00000000000000a1",
        "/w",
        Some("s_00000000000000b2"),
        old_ts(),
    );
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        old_ts(),
    );
    for cascade in [false, true] {
        let selected = setup.select(cascade);
        assert!(selected.deletes.is_empty(), "cascade={cascade}");
        assert_eq!(selected.rows.len(), 2);
        for row in &selected.rows {
            assert!(matches!(row, SessionRow::Cycle { .. }));
            if let SessionRow::Cycle { members, .. } = row {
                assert_eq!(
                    members,
                    &[
                        "s_00000000000000a1".to_owned(),
                        "s_00000000000000b2".to_owned()
                    ]
                );
            }
        }
    }
}

#[test]
fn an_old_root_whose_descendant_is_in_a_cycle_is_skipped_in_both_modes() {
    // A cycle is closed under parents: each member's parent is in the
    // cycle, so no outside root can point into one. The reachable case
    // is a root inside a longer cycle: its descendants are cyclic, and
    // it is cyclic itself, so it is skipped with every cyclic member.
    let setup = Setup::new();
    setup.session(
        "s_00000000000000a1",
        "/w",
        Some("s_00000000000000c3"),
        old_ts(),
    );
    setup.session(
        "s_00000000000000b2",
        "/w",
        Some("s_00000000000000a1"),
        old_ts(),
    );
    setup.session(
        "s_00000000000000c3",
        "/w",
        Some("s_00000000000000b2"),
        old_ts(),
    );
    for cascade in [false, true] {
        let selected = setup.select(cascade);
        assert!(selected.deletes.is_empty(), "cascade={cascade}");
        let root = selected
            .rows
            .iter()
            .find(|row| super::row_id(row) == "s_00000000000000a1")
            .expect("root is listed");
        assert!(matches!(root, SessionRow::Cycle { .. }));
        if let SessionRow::Cycle { members, .. } = root {
            assert_eq!(
                members,
                &[
                    "s_00000000000000a1".to_owned(),
                    "s_00000000000000b2".to_owned(),
                    "s_00000000000000c3".to_owned()
                ]
            );
        }
    }
}

#[test]
fn an_old_root_whose_only_child_has_no_workspace_is_skipped_then_listed() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", "/w", None, old_ts());
    let dir = setup.sessions().join("s_00000000000000b2");
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({
        "kind": "session_started",
        "seq": 0,
        "payload": {"forked_from": {"session_id": "s_00000000000000a1", "seq": 1}},
    });
    let last = json!({"kind": "x", "ts": old_ts()});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    let plain = setup.select(false);
    assert!(plain.deletes.is_empty());
    assert!(matches!(&plain.rows[0], SessionRow::Blocked { .. }));
    if let SessionRow::Blocked { blockers, .. } = &plain.rows[0] {
        assert_eq!(blockers, &["s_00000000000000b2".to_owned()]);
    }
    let cascaded = setup.select(true);
    assert_eq!(cascaded.deletes.len(), 1);
    assert_eq!(
        row_ids(&cascaded.rows),
        ["s_00000000000000a1", "s_00000000000000b2"]
    );
}

/// A re-run child's own bound: it fires before the parent's wait, so the
/// two no longer contradict.
const REDIRECT_CHILD_BUDGET: Duration = Duration::from_secs(20);

/// A parent's wait for a re-run child: inside half of nextest's per-test
/// timeout (`.config/nextest.toml`, `docs/testing.md`, "Waits and timeouts").
const REDIRECT_RERUN_WITHIN: Duration = Duration::from_secs(30);

/// The outer wait around a test that re-runs a child: at least twice the
/// parent's wait (`docs/testing.md`, "Waits and timeouts").
const REDIRECT_RERUN_OUTER: Duration = Duration::from_secs(60);

/// Re-run bounds keep their margins: the child's budget is below the
/// parent's wait, the wait stays inside half of nextest's per-test timeout,
/// and the outer wrapper is at least twice the wait.
const _: () = assert!(
    REDIRECT_CHILD_BUDGET.as_secs() < REDIRECT_RERUN_WITHIN.as_secs()
        && REDIRECT_RERUN_WITHIN.as_secs() <= 30
        && REDIRECT_RERUN_OUTER.as_secs() >= 2 * REDIRECT_RERUN_WITHIN.as_secs()
);

#[test]
fn a_git_redirect_keeps_selection_scoped_to_the_workspace_project() {
    if std::env::var("FIBER_CLI_PRUNE_SESSIONS_REDIRECT_CHILD").is_ok() {
        fakes::within(
            "the child's redirect checks",
            Deadline::after(REDIRECT_CHILD_BUDGET),
            || {
                let home =
                    PathBuf::from(std::env::var("FIBER_CLI_PRUNE_SESSIONS_REDIRECT_HOME").unwrap());
                let workspace = PathBuf::from(
                    std::env::var("FIBER_CLI_PRUNE_SESSIONS_REDIRECT_WORKSPACE").unwrap(),
                );
                let selected = select(
                    &home,
                    &log::started_sessions(&home),
                    &workspace,
                    Some(older_than()),
                    false,
                    wall(),
                );
                let mut deletes = selected
                    .deletes
                    .iter()
                    .map(|d| d.id.0.clone())
                    .collect::<Vec<_>>();
                deletes.sort();
                assert_eq!(deletes, ["s_00000000000000a1"]);
            },
        );
        return;
    }
    fakes::within(
        "a_git_redirect_keeps_selection_scoped_to_the_workspace_project",
        Deadline::after(REDIRECT_RERUN_OUTER),
        || {
            let root = fakes::TempDir::new("cli-prune-sessions-redirect");
            let repo = root.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            fakes::git::git(&repo, &["init", "--quiet"]);
            let decoy = root.path().join("decoy");
            fs::create_dir_all(&decoy).unwrap();
            fakes::git::git(&decoy, &["init", "--quiet"]);
            let workspace = fs::canonicalize(&repo).unwrap();
            let decoy_canon = fs::canonicalize(&decoy).unwrap();
            let home = root.path().join("home");
            fs::create_dir_all(&home).unwrap();
            let sessions = log::sessions_dir(&home, &worktree::project(&workspace));
            session_in(
                &sessions.join("s_00000000000000a1"),
                workspace.to_str().unwrap(),
                None,
                old_ts(),
            );
            let decoy_sessions = log::sessions_dir(&home, &worktree::project(&decoy_canon));
            session_in(
                &decoy_sessions.join("s_00000000000000b2"),
                decoy_canon.to_str().unwrap(),
                None,
                old_ts(),
            );
            let home_str = home.to_str().unwrap().to_owned();
            let workspace_str = workspace.to_str().unwrap().to_owned();
            let decoy_git = decoy_canon.join(".git").to_str().unwrap().to_owned();
            let decoy_str = decoy_canon.to_str().unwrap().to_owned();
            let name = module_path!().split_once("::").unwrap().1.to_owned()
                + "::a_git_redirect_keeps_selection_scoped_to_the_workspace_project";
            let out = fakes::rerun_prepared(
                &name,
                &[
                    ("FIBER_CLI_PRUNE_SESSIONS_REDIRECT_CHILD", "1"),
                    ("FIBER_CLI_PRUNE_SESSIONS_REDIRECT_HOME", home_str.as_str()),
                    (
                        "FIBER_CLI_PRUNE_SESSIONS_REDIRECT_WORKSPACE",
                        workspace_str.as_str(),
                    ),
                    ("GIT_DIR", decoy_git.as_str()),
                    ("GIT_WORK_TREE", decoy_str.as_str()),
                ],
                Deadline::after(REDIRECT_RERUN_WITHIN),
                |_| {},
            );
            assert!(
                out.status.success(),
                "selection must stay scoped to the workspace project:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        },
    );
}
