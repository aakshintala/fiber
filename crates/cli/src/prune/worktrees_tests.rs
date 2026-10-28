//! Tests for the worktree side of prune: listing, rows, the running guard,
//! revalidation and removal, with real `git` and session fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::json;

use super::super::{PruneArgs, prune_run};
use super::*;
use fakes::Deadline;
use fakes::git::git;

const PROJECT: &str = "p";

/// Fiber home, a workspace outside any repository, and a git repository
/// backing the worktrees, all removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        let setup = Self {
            root: fakes::TempDir::new(prefix),
        };
        fs::create_dir_all(setup.home().join("projects")).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        fs::create_dir_all(setup.repo()).unwrap();
        git(setup.repo().as_path(), &["init", "--quiet"]);
        fs::write(setup.repo().join("file.txt"), "x").unwrap();
        git(setup.repo().as_path(), &["add", "."]);
        git(
            setup.repo().as_path(),
            &["commit", "--quiet", "-m", "first"],
        );
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("workspace")).unwrap()
    }

    fn repo(&self) -> PathBuf {
        self.root.path().join("repo")
    }

    fn worktrees(&self) -> PathBuf {
        self.home().join("projects").join(PROJECT).join("worktrees")
    }

    /// A kept worktree `id` on branch `fiber/<id>`, returned canonicalized.
    fn worktree(&self, id: &str) -> PathBuf {
        fs::create_dir_all(self.worktrees()).unwrap();
        let path = self.worktrees().join(id);
        git(
            self.repo().as_path(),
            &[
                "worktree",
                "add",
                "-b",
                &format!("fiber/{id}"),
                path.to_str().unwrap(),
            ],
        );
        fs::canonicalize(&path).unwrap()
    }

    /// A session `id` whose workspace is `workspace`, returned with its
    /// directory.
    fn user(&self, id: &str, workspace: &Path) -> PathBuf {
        let dir = self
            .home()
            .join("projects")
            .join(PROJECT)
            .join("sessions")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        let first = json!({
            "kind": "session_started", "seq": 0,
            "payload": {"workspace": workspace.to_str().unwrap()},
        });
        let last = json!({"kind": "x", "ts": 0});
        fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
        dir
    }

    fn branch_exists(&self, id: &str) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(self.repo())
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/fiber/{id}"),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

fn wall() -> std::time::SystemTime {
    fakes::clock::FakeClock::new().wall()
}

fn clock() -> std::sync::Arc<fakes::clock::FakeClock> {
    fakes::clock::FakeClock::new()
}

fn select(setup: &Setup, force: bool, dry_run: bool) -> Planned {
    super::select(
        &setup.home(),
        &log::started_sessions(&setup.home()),
        &setup.workspace(),
        force,
        dry_run,
        wall(),
    )
}

fn removable(row: &WorktreeRow) -> (&str, Option<&str>) {
    match row {
        WorktreeRow::Removable { name, forced, .. } => (name, forced.as_deref()),
        WorktreeRow::Skipped { .. }
        | WorktreeRow::Running { .. }
        | WorktreeRow::Uncertain { .. } => {
            panic!("expected a removable row, found {row:?}")
        }
    }
}

fn line(row: &WorktreeRow) -> String {
    worktree_line(row)
}

#[test]
fn prune_skips_shared_worktrees_even_when_forced_and_leaves_locks() {
    fakes::within(
        "prune shared worktrees",
        Deadline::after(Duration::from_secs(30)),
        || {
            for force in [false, true] {
                let setup = Setup::new("prune-shared");
                let path = setup.worktree("s_0123456789abcdef");
                let hold = doors::share(&path).unwrap();
                let mut plan = select(&setup, force, false);
                assert_eq!(plan.rows.len(), 1);
                assert!(line(&plan.rows[0]).contains("in use"));
                assert!(plan.removals.is_empty());
                let removed = remove_planned(&setup.home(), &mut plan, force, &mut |_| {});
                assert_eq!(removed.total, 0);
                assert!(path.exists());
                assert!(path.with_extension("lock").exists());
                drop(hold);
                let mut plan = select(&setup, force, false);
                assert_eq!(plan.removals.len(), 1);
                let removed = remove_planned(&setup.home(), &mut plan, force, &mut |_| {});
                assert!(removed.failures.is_empty());
                assert!(!path.exists());
                assert!(
                    path.with_extension("lock").exists(),
                    "prune never unlinks a lock file"
                );
            }
        },
    );
}

#[test]
fn prune_cannot_enter_the_add_to_child_share_gap_or_a_new_share_after_listing() {
    fakes::within(
        "prune handoff interleavings",
        Deadline::after(Duration::from_secs(30)),
        || {
            for force in [false, true] {
                let setup = Setup::new("prune-handoff");
                fs::create_dir_all(setup.worktrees()).unwrap();
                let path = setup.worktrees().join("s_0123456789abcdef");
                let parent = doors::share(&path).unwrap();
                let path = setup.worktree("s_0123456789abcdef");
                // Forced pause: add is complete, but no child share or session
                // row exists yet (docs/invocation.md, "Isolation").
                let plan = select(&setup, force, false);
                assert!(plan.removals.is_empty());
                assert!(line(&plan.rows[0]).contains("in use"));
                let child = doors::share(&path).unwrap();
                drop(parent);
                assert!(path.exists());
                drop(child);
                let mut plan = select(&setup, force, false);
                assert_eq!(plan.removals.len(), 1);
                let mut late_child = None;
                let removed = remove_planned(&setup.home(), &mut plan, force, &mut |dir| {
                    late_child = Some(doors::share(dir).unwrap());
                });
                assert_eq!(removed.failures.len(), 1);
                assert_eq!(removed.failures[0].2, "in use");
                assert!(path.exists());
                assert!(setup.branch_exists("s_0123456789abcdef"));
                assert!(path.with_extension("lock").exists());
            }
        },
    );
}

#[test]
fn a_clean_worktree_lists_one_removable_row() {
    let setup = Setup::new("cli-prune-wt-clean");
    let dir = setup.worktree("s_00000000000000e1");
    let planned = select(&setup, false, false);
    assert_eq!(planned.rows.len(), 1);
    assert_eq!(planned.removals.len(), 1);
    let (name, forced) = removable(&planned.rows[0]);
    assert_eq!((name, forced), ("s_00000000000000e1", None));
    assert_eq!(
        line(&planned.rows[0]),
        format!(
            "worktree  s_00000000000000e1  fiber/s_00000000000000e1  clean  0d  {}",
            super::super::format_size(log::session_bytes(&dir)),
        )
    );
}

#[test]
fn every_skip_reason_has_its_exact_text() {
    let setup = Setup::new("cli-prune-wt-skips");
    let dirty = setup.worktree("s_00000000000000e2");
    fs::write(dirty.join("notes.txt"), "scratch").unwrap();
    let unique = setup.worktree("s_00000000000000e3");
    fs::write(unique.join("more.txt"), "y").unwrap();
    git(&unique, &["add", "."]);
    git(&unique, &["commit", "--quiet", "-m", "second"]);
    let both = setup.worktree("s_00000000000000e4");
    fs::write(both.join("more.txt"), "e4").unwrap();
    git(&both, &["add", "."]);
    git(&both, &["commit", "--quiet", "-m", "fourth"]);
    fs::write(both.join("notes.txt"), "scratch").unwrap();
    let plain = setup.worktrees().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let detached = setup.worktrees().join("detached");
    git(
        setup.repo().as_path(),
        &["worktree", "add", "--detach", detached.to_str().unwrap()],
    );
    let broken = setup.worktrees().join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join(".git"), "gitdir: /nonexistent-admin-dir/x\n").unwrap();
    let planned = select(&setup, false, false);
    assert!(planned.removals.is_empty());
    let lines: Vec<String> = planned.rows.iter().map(line).collect();
    let size = |dir: &Path| super::super::format_size(log::session_bytes(dir));
    assert!(lines.contains(
        &format!(
            "worktree  s_00000000000000e2  fiber/s_00000000000000e2  uncommitted  0d  {}  skipped: removing it would lose uncommitted files",
            size(&dirty),
        )
    ), "{lines:?}");
    assert!(lines.contains(
        &format!(
            "worktree  s_00000000000000e3  fiber/s_00000000000000e3  clean  0d  {}  skipped: removing it would lose commits found nowhere else",
            size(&unique),
        )
    ), "{lines:?}");
    assert!(lines.contains(
        &format!(
            "worktree  s_00000000000000e4  fiber/s_00000000000000e4  uncommitted  0d  {}  skipped: removing it would lose uncommitted files and commits found nowhere else",
            size(&both),
        )
    ), "{lines:?}");
    assert!(
        lines.contains(&"worktree  plain  skipped: not a git worktree".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"worktree  detached  skipped: its HEAD is detached".to_owned()),
        "{lines:?}"
    );
    let broken_line = lines
        .iter()
        .find(|l| l.starts_with("worktree  broken  skipped: git cannot read it: "))
        .unwrap_or_else(|| panic!("no broken row in {lines:?}"));
    assert!(broken_line.len() > "worktree  broken  skipped: git cannot read it: ".len());
}

#[test]
fn force_lists_losing_worktrees_as_forced() {
    let setup = Setup::new("cli-prune-wt-force-rows");
    let dirty = setup.worktree("s_00000000000000e2");
    fs::write(dirty.join("notes.txt"), "scratch").unwrap();
    let planned = select(&setup, true, false);
    assert_eq!(planned.removals.len(), 1);
    let (name, forced) = removable(&planned.rows[0]);
    assert_eq!(name, "s_00000000000000e2");
    assert_eq!(forced, Some("uncommitted files"));
    assert!(
        line(&planned.rows[0]).ends_with("  forced: loses uncommitted files"),
        "{}",
        line(&planned.rows[0])
    );
}

#[test]
fn revalidation_covers_each_cleanliness_and_force_case_with_exact_totals() {
    for (case, initially_dirty, force, make_dirty, should_remove) in [
        ("clean-unforced", false, false, false, true),
        ("clean-forced", false, true, false, true),
        ("dirty-unforced", false, false, true, false),
        ("dirty-forced", true, true, false, true),
    ] {
        let setup = Setup::new(&format!("cli-prune-wt-guard-{case}"));
        let dir = setup.worktree("s_00000000000000e1");
        if initially_dirty {
            fs::write(dir.join("notes.txt"), "scratch").unwrap();
        }
        let bytes = log::session_bytes(&dir);
        assert!(bytes > 0);
        let mut planned = select(&setup, force, false);
        assert_eq!(listed_bytes(&planned), bytes, "{case}: listed bytes");
        let mut before_remove = |path: &Path| {
            if make_dirty {
                fs::write(path.join("notes.txt"), "scratch").unwrap();
            }
        };
        let removed = remove_planned(&setup.home(), &mut planned, force, &mut before_remove);
        assert_eq!(removed.total, 1, "{case}: total");
        assert_eq!(
            removed.freed,
            if should_remove { bytes } else { 0 },
            "{case}: freed"
        );
        if should_remove {
            assert!(
                removed.failures.is_empty(),
                "{case}: {:?}",
                removed.failures
            );
            assert!(!dir.exists(), "{case}: worktree remains");
            assert!(
                !setup.branch_exists("s_00000000000000e1"),
                "{case}: branch remains"
            );
        } else {
            assert_eq!(removed.failures.len(), 1, "{case}: failures");
            assert_eq!(
                removed.failures[0].2,
                "it changed since it was listed: removing it would now lose uncommitted files",
                "{case}: guard reason"
            );
            assert!(dir.is_dir(), "{case}: worktree was removed");
            assert!(
                setup.branch_exists("s_00000000000000e1"),
                "{case}: branch was removed"
            );
        }
    }
}

#[test]
fn force_removes_the_dirty_and_unique_cases() {
    let setup = Setup::new("cli-prune-wt-force");
    let dirty = setup.worktree("s_00000000000000e2");
    fs::write(dirty.join("notes.txt"), "scratch").unwrap();
    let unique = setup.worktree("s_00000000000000e3");
    fs::write(unique.join("more.txt"), "y").unwrap();
    git(&unique, &["add", "."]);
    git(&unique, &["commit", "--quiet", "-m", "second"]);
    let mut planned = select(&setup, true, false);
    assert_eq!(planned.removals.len(), 2);
    let removed = remove_planned(&setup.home(), &mut planned, true, &mut |_| {});
    assert!(removed.failures.is_empty(), "{:?}", removed.failures);
    assert_eq!(removed.total, 2);
    assert!(!dirty.exists() && !unique.exists());
    assert!(!setup.branch_exists("s_00000000000000e2"));
    assert!(!setup.branch_exists("s_00000000000000e3"));
}

#[test]
fn force_never_removes_a_running_or_an_uncertain_worktree() {
    let setup = Setup::new("cli-prune-wt-force-kept");
    let dir = setup.worktree("s_00000000000000e1");
    let user = setup.user("s_00000000000000a1", &dir);
    let held = match log::try_hold(&user) {
        Ok(log::Hold::Held(held)) => held,
        Ok(log::Hold::Busy) | Err(_) => panic!("the user's lock is held"),
    };
    let plain = setup.worktrees().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let planned = select(&setup, true, false);
    assert!(planned.removals.is_empty());
    let lines: Vec<String> = planned.rows.iter().map(line).collect();
    assert!(
        lines.contains(
            &"worktree  s_00000000000000e1  skipped: a running session works in it".to_owned()
        ),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"worktree  plain  skipped: not a git worktree".to_owned()),
        "{lines:?}"
    );
    assert!(dir.is_dir());
    drop(held);
}

#[test]
fn a_user_in_a_subdirectory_counts() {
    let setup = Setup::new("cli-prune-wt-subdir");
    let dir = setup.worktree("s_00000000000000e1");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let user = setup.user("s_00000000000000a1", &sub);
    let held = match log::try_hold(&user) {
        Ok(log::Hold::Held(held)) => held,
        Ok(log::Hold::Busy) | Err(_) => panic!("the user's lock is held"),
    };
    let planned = select(&setup, true, false);
    assert!(planned.removals.is_empty());
    assert!(
        planned
            .rows
            .iter()
            .map(line)
            .any(|l| l == "worktree  s_00000000000000e1  skipped: a running session works in it"),
        "{:?}",
        planned.rows.iter().map(line).collect::<Vec<_>>()
    );
    drop(held);
}

#[test]
fn an_unwritable_session_lock_counts_as_running() {
    let setup = Setup::new("cli-prune-wt-unwritable");
    let dir = setup.worktree("s_00000000000000e1");
    let user = setup.user("s_00000000000000a1", &dir);
    // The log still reads with the directory read-only, but taking the
    // lock fails: an error counts as running, `--force` included.
    fs::set_permissions(&user, fs::Permissions::from_mode(0o555)).unwrap();
    let planned = select(&setup, true, false);
    fs::set_permissions(&user, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(planned.removals.is_empty());
    assert!(
        planned
            .rows
            .iter()
            .map(line)
            .any(|l| l == "worktree  s_00000000000000e1  skipped: a running session works in it"),
        "{:?}",
        planned.rows.iter().map(line).collect::<Vec<_>>()
    );
}

#[test]
fn a_user_that_appears_after_the_scan_is_caught() {
    let setup = Setup::new("cli-prune-wt-new-user");
    let dir = setup.worktree("s_00000000000000e1");
    let mut planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    // The hook starts a session in the worktree and holds its lock, so the
    // revalidation finds it running.
    let stash: Rc<RefCell<Option<log::SessionLock>>> = Rc::new(RefCell::new(None));
    let moved = Rc::clone(&stash);
    let mut before_remove = |_: &Path| {
        let home = setup.home();
        let user = home
            .join("projects")
            .join(PROJECT)
            .join("sessions")
            .join("s_00000000000000a1");
        fs::create_dir_all(&user).unwrap();
        let first = json!({
            "kind": "session_started", "seq": 0,
            "payload": {"workspace": dir.to_str().unwrap()},
        });
        fs::write(user.join("events.jsonl"), format!("{first}\n")).unwrap();
        let held = match log::try_hold(&user) {
            Ok(log::Hold::Held(held)) => held,
            Ok(log::Hold::Busy) | Err(_) => panic!("the new user's lock is held"),
        };
        *moved.borrow_mut() = Some(held);
    };
    let removed = remove_planned(&setup.home(), &mut planned, false, &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(removed.failures.len(), 1);
    assert_eq!(removed.failures[0].0, "worktree s_00000000000000e1");
    assert!(
        removed.failures[0]
            .2
            .contains("a running session works in it"),
        "{:?}",
        removed.failures
    );
    assert!(dir.is_dir(), "the worktree stays");
    drop(stash);
}

#[test]
fn a_commit_after_listing_is_not_removed_without_force() {
    let setup = Setup::new("cli-prune-wt-changed");
    let dir = setup.worktree("s_00000000000000e1");
    let mut planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    let mut before_remove = |path: &Path| {
        fs::write(path.join("more.txt"), "y").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "--quiet", "-m", "second"]);
    };
    let removed = remove_planned(&setup.home(), &mut planned, false, &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(removed.failures.len(), 1);
    assert_eq!(
        removed.failures[0].2,
        "it changed since it was listed: removing it would now lose commits found nowhere else",
        "{:?}",
        removed.failures
    );
    assert!(dir.is_dir(), "the worktree stays");
    assert!(
        setup.branch_exists("s_00000000000000e1"),
        "the branch stays"
    );
}

#[test]
fn dropping_the_plan_releases_every_lock() {
    let setup = Setup::new("cli-prune-wt-locks");
    let dir = setup.worktree("s_00000000000000e1");
    let user = setup.user("s_00000000000000a1", &dir);
    let planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(planned.locks.len(), 1, "the plan owns the user's lock");
    assert!(
        matches!(log::try_hold(&user), Ok(log::Hold::Busy)),
        "the lock is held while the plan lives"
    );
    drop(planned);
    // Nextest runs each test in its own process, so no other test's
    // `git` child can hold the lock past this drop: it is free at once.
    assert!(
        matches!(log::try_hold(&user), Ok(log::Hold::Held(_))),
        "no lock is kept once the plan drops"
    );
}

#[test]
fn dry_run_drops_each_lock_before_output() {
    let setup = Setup::new("cli-prune-wt-dry-run-locks");
    let dir = setup.worktree("s_00000000000000e1");
    let user = setup.user("s_00000000000000a1", &dir);
    let planned = select(&setup, false, true);
    assert_eq!(planned.removals.len(), 1);
    assert!(
        planned.locks.is_empty(),
        "dry-run holds no lock while the plan lives"
    );
    // Output happens while the plan lives: the lock is already free
    // here, not only after prune returns.
    assert!(
        matches!(log::try_hold(&user), Ok(log::Hold::Held(_))),
        "the lock is free while the dry-run plan lives"
    );
}

#[test]
fn dry_run_still_skips_a_running_worktree() {
    let setup = Setup::new("cli-prune-wt-dry-run-running");
    let dir = setup.worktree("s_00000000000000e1");
    let user = setup.user("s_00000000000000a1", &dir);
    let held = match log::try_hold(&user) {
        Ok(log::Hold::Held(held)) => held,
        Ok(log::Hold::Busy) | Err(_) => panic!("the user's lock is held"),
    };
    let planned = select(&setup, false, true);
    assert!(planned.removals.is_empty());
    assert!(
        planned
            .rows
            .iter()
            .map(line)
            .any(|l| l == "worktree  s_00000000000000e1  skipped: a running session works in it"),
        "{:?}",
        planned.rows.iter().map(line).collect::<Vec<_>>()
    );
    drop(held);
}

#[test]
fn a_partial_removal_is_reported_and_still_counts_as_freed() {
    let setup = Setup::new("cli-prune-wt-partial");
    let dir = setup.worktree("s_00000000000000e1");
    let listed = log::session_bytes(&dir);
    assert!(listed > 0);
    let mut planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    // The hook takes write permission off the branch's directory, so
    // `branch -D` has nothing it can unlink: the worktree goes and only
    // its branch is kept.
    let mut before_remove = |_: &Path| {
        let dir = setup
            .repo()
            .join(".git")
            .join("refs")
            .join("heads")
            .join("fiber");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    };
    let removed = remove_planned(&setup.home(), &mut planned, false, &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, listed);
    assert_eq!(removed.failures.len(), 1);
    assert_eq!(removed.failures[0].0, "worktree s_00000000000000e1");
    assert!(
        removed.failures[0].2.contains("branch -D"),
        "{:?}",
        removed.failures
    );
    assert!(!dir.exists(), "the worktree is gone");
    assert!(
        setup.branch_exists("s_00000000000000e1"),
        "the branch is kept"
    );
}

#[test]
fn age_comes_from_the_directory_mtime() {
    let setup = Setup::new("cli-prune-wt-age");
    let dir = setup.worktree("s_00000000000000e1");
    let at = wall() - Duration::from_secs(12 * 24 * 60 * 60);
    fs::File::open(&dir).unwrap().set_modified(at).unwrap();
    // A directory newer than now is 0 days old, never negative.
    let future = setup.worktree("s_00000000000000e2");
    let ahead = wall() + Duration::from_secs(2 * 24 * 60 * 60);
    fs::File::open(&future)
        .unwrap()
        .set_modified(ahead)
        .unwrap();
    let planned = select(&setup, false, false);
    assert_eq!(planned.rows.len(), 2);
    assert!(
        line(&planned.rows[0]).contains("  12d  "),
        "{}",
        line(&planned.rows[0])
    );
    assert!(
        line(&planned.rows[1]).contains("  0d  "),
        "{}",
        line(&planned.rows[1])
    );
}

#[test]
fn a_session_working_elsewhere_does_not_block() {
    let setup = Setup::new("cli-prune-wt-elsewhere");
    setup.worktree("s_00000000000000e1");
    setup.user("s_00000000000000a1", Path::new("/elsewhere"));
    let planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(removable(&planned.rows[0]).0, "s_00000000000000e1");
}

#[test]
fn an_exited_user_is_held_once_and_the_worktree_still_goes() {
    let setup = Setup::new("cli-prune-wt-exited-user");
    let dir = setup.worktree("s_00000000000000e1");
    setup.user("s_00000000000000a1", &dir);
    let mut planned = select(&setup, false, false);
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(planned.locks.len(), 1);
    // The revalidation must not take the user's lock twice: a second
    // descriptor would see `Busy` and mistake it for running.
    let removed = remove_planned(&setup.home(), &mut planned, false, &mut |_| {});
    assert!(removed.failures.is_empty(), "{:?}", removed.failures);
    assert_eq!(removed.total, 1);
    assert!(!dir.exists(), "the worktree is gone");
    assert!(
        !setup.branch_exists("s_00000000000000e1"),
        "the branch is gone"
    );
}

#[test]
fn a_symlinked_entry_is_skipped() {
    let setup = Setup::new("cli-prune-wt-link");
    let dir = setup.worktree("s_00000000000000e1");
    fs::create_dir_all(setup.worktrees()).unwrap();
    std::os::unix::fs::symlink(&dir, setup.worktrees().join("alias")).unwrap();
    let planned = select(&setup, false, false);
    assert_eq!(planned.rows.len(), 1);
    assert_eq!(removable(&planned.rows[0]).0, "s_00000000000000e1");
}

#[test]
fn a_symlink_inside_counts_no_bytes() {
    let setup = Setup::new("cli-prune-wt-link-bytes");
    let dir = setup.worktree("s_00000000000000e1");
    let before = log::session_bytes(&dir);
    assert!(before > 0);
    fs::write(setup.repo().join("big.txt"), "y".repeat(100_000)).unwrap();
    std::os::unix::fs::symlink(setup.repo().join("big.txt"), dir.join("big.txt")).unwrap();
    assert_eq!(log::session_bytes(&dir), before);
}

#[test]
fn force_through_prune_run_prints_forced_removes_and_frees() {
    let setup = Setup::new("cli-prune-wt-run");
    let dir = setup.worktree("s_00000000000000e2");
    fs::write(dir.join("notes.txt"), "scratch").unwrap();
    let listed = log::session_bytes(&dir);
    let args = PruneArgs {
        older_than: None,
        cascade: false,
        dry_run: false,
        yes: true,
        force: true,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input: &[u8] = b"";
    let ask = crate::sessions::Ask {
        yes: true,
        terminal: false,
        input: &mut input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        &args,
        clock().as_ref(),
        ask,
        &mut out,
        &mut || -> Result<doors::hub::Hub, doors::Error> { panic!("nothing connects") },
    );
    got.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("forced: loses uncommitted files"), "{out}");
    assert!(
        out.contains(&format!("freed {}", super::super::format_size(listed))),
        "{out}"
    );
    assert!(!dir.exists(), "the worktree is gone");
    assert!(
        !setup.branch_exists("s_00000000000000e2"),
        "the branch is gone"
    );
}

/// Commits `target/` and `.env` to `.gitignore` on `main`, so worktrees
/// made after hold ignored files without any unique commit.
fn ignore_target_and_env(setup: &Setup) {
    fs::write(setup.repo().join(".gitignore"), "target/\n.env\n").unwrap();
    git(&setup.repo(), &["add", ".gitignore"]);
    git(&setup.repo(), &["commit", "--quiet", "-m", "ignore"]);
}

#[test]
fn an_ignored_only_worktree_is_removable_and_names_its_ignored_files() {
    let setup = Setup::new("cli-prune-wt-ignored-only");
    ignore_target_and_env(&setup);
    let dir = setup.worktree("s_00000000000000e3");
    fs::create_dir_all(dir.join("target")).unwrap();
    fs::write(dir.join("target/out.bin"), "1234567890").unwrap();
    fs::write(dir.join(".env"), "123456").unwrap();
    let planned = select(&setup, false, false);
    assert_eq!(planned.rows.len(), 1);
    assert_eq!(planned.removals.len(), 1);
    let (name, forced) = removable(&planned.rows[0]);
    assert_eq!((name, forced), ("s_00000000000000e3", None));
    assert_eq!(
        line(&planned.rows[0]),
        format!(
            "worktree  s_00000000000000e3  fiber/s_00000000000000e3  clean  0d  {}  ignored 16 B: target/, .env",
            super::super::format_size(log::session_bytes(&dir)),
        )
    );
    let mut planned = select(&setup, false, false);
    let removed = remove_planned(&setup.home(), &mut planned, false, &mut |_| {});
    assert!(removed.failures.is_empty(), "{:?}", removed.failures);
    assert_eq!(removed.total, 1);
    assert!(!dir.exists(), "the worktree is gone");
    assert!(
        !setup.branch_exists("s_00000000000000e3"),
        "the branch is gone"
    );
}

#[test]
fn an_untracked_file_alongside_ignored_files_is_skipped_without_an_ignored_segment() {
    let setup = Setup::new("cli-prune-wt-ignored-dirty");
    ignore_target_and_env(&setup);
    let dir = setup.worktree("s_00000000000000e2");
    fs::write(dir.join("notes.txt"), "scratch").unwrap();
    fs::write(dir.join(".env"), "123456").unwrap();
    let planned = select(&setup, false, false);
    assert!(planned.removals.is_empty());
    let lines: Vec<String> = planned.rows.iter().map(line).collect();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].ends_with("skipped: removing it would lose uncommitted files"),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains("ignored"), "{}", lines[0]);
    assert!(dir.is_dir(), "the worktree stays");
}

#[test]
fn force_names_ignored_files_before_the_forced_losses() {
    let setup = Setup::new("cli-prune-wt-ignored-force");
    ignore_target_and_env(&setup);
    let dir = setup.worktree("s_00000000000000e2");
    fs::write(dir.join("notes.txt"), "scratch").unwrap();
    fs::write(dir.join(".env"), "123456").unwrap();
    let planned = select(&setup, true, false);
    assert_eq!(planned.removals.len(), 1);
    assert!(
        line(&planned.rows[0]).ends_with("  ignored 6 B: .env  forced: loses uncommitted files"),
        "{}",
        line(&planned.rows[0])
    );
}

#[test]
fn the_confirmation_names_ignored_files_and_removes_nothing_on_no() {
    let setup = Setup::new("cli-prune-wt-ignored-confirm");
    ignore_target_and_env(&setup);
    let dir = setup.worktree("s_00000000000000e3");
    fs::create_dir_all(dir.join("target")).unwrap();
    fs::write(dir.join("target/out.bin"), "1234567890").unwrap();
    fs::write(dir.join(".env"), "123456").unwrap();
    let args = PruneArgs {
        older_than: None,
        cascade: false,
        dry_run: false,
        yes: false,
        force: false,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut input: &[u8] = b"n\n";
    let ask = crate::sessions::Ask {
        yes: false,
        terminal: true,
        input: &mut input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        &args,
        clock().as_ref(),
        ask,
        &mut out,
        &mut || -> Result<doors::hub::Hub, doors::Error> { panic!("nothing connects") },
    );
    got.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("ignored 16 B: target/, .env"), "{out}");
    assert!(dir.is_dir(), "the worktree stays");
    assert!(
        setup.branch_exists("s_00000000000000e3"),
        "the branch stays"
    );
}

#[test]
fn a_failed_ignored_listing_is_uncertain() {
    let home = fakes::TempDir::new("cli-prune-wt-ignored-outside");
    let err = super::listed_ignored(home.path()).unwrap_err();
    assert!(err.starts_with("git cannot read it: "), "{err}");
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
fn a_git_redirect_keeps_scope_on_the_workspace_repository() {
    if std::env::var("FIBER_CLI_PRUNE_SCOPE_REDIRECT_CHILD").is_ok() {
        fakes::within(
            "the child's redirect checks",
            Deadline::after(REDIRECT_CHILD_BUDGET),
            || {
                let home =
                    PathBuf::from(std::env::var("FIBER_CLI_PRUNE_SCOPE_REDIRECT_HOME").unwrap());
                let workspace = PathBuf::from(
                    std::env::var("FIBER_CLI_PRUNE_SCOPE_REDIRECT_WORKSPACE").unwrap(),
                );
                let expected = std::env::var("FIBER_CLI_PRUNE_SCOPE_REDIRECT_EXPECTED").unwrap();
                assert_eq!(super::scope_keys(&home, &workspace), vec![expected]);
            },
        );
        return;
    }
    fakes::within(
        "a_git_redirect_keeps_scope_on_the_workspace_repository",
        Deadline::after(REDIRECT_RERUN_OUTER),
        || {
            let root = fakes::TempDir::new("cli-prune-scope-redirect");
            let repo = root.path().join("repo");
            fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "--quiet"]);
            let decoy = root.path().join("decoy");
            fs::create_dir_all(&decoy).unwrap();
            git(&decoy, &["init", "--quiet"]);
            let workspace = fs::canonicalize(&repo).unwrap();
            let decoy_canon = fs::canonicalize(&decoy).unwrap();
            let home = root.path().join("home");
            fs::create_dir_all(home.join("projects")).unwrap();
            let expected = log::project_key(&worktree::project(&workspace));
            let home_str = home.to_str().unwrap().to_owned();
            let workspace_str = workspace.to_str().unwrap().to_owned();
            let decoy_git = decoy_canon.join(".git").to_str().unwrap().to_owned();
            let decoy_str = decoy_canon.to_str().unwrap().to_owned();
            let name = module_path!().split_once("::").unwrap().1.to_owned()
                + "::a_git_redirect_keeps_scope_on_the_workspace_repository";
            let out = fakes::rerun_prepared(
                &name,
                &[
                    ("FIBER_CLI_PRUNE_SCOPE_REDIRECT_CHILD", "1"),
                    ("FIBER_CLI_PRUNE_SCOPE_REDIRECT_HOME", home_str.as_str()),
                    (
                        "FIBER_CLI_PRUNE_SCOPE_REDIRECT_WORKSPACE",
                        workspace_str.as_str(),
                    ),
                    ("FIBER_CLI_PRUNE_SCOPE_REDIRECT_EXPECTED", expected.as_str()),
                    ("GIT_DIR", decoy_git.as_str()),
                    ("GIT_WORK_TREE", decoy_str.as_str()),
                ],
                Deadline::after(REDIRECT_RERUN_WITHIN),
                |_| {},
            );
            assert!(
                out.status.success(),
                "scope must stay on the workspace repository:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        },
    );
}
