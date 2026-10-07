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
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::json;

use super::super::{PruneArgs, prune_run};
use super::*;

const PROJECT: &str = "p";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

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

fn select(setup: &Setup, force: bool) -> Planned {
    super::select(&setup.home(), &setup.workspace(), force, wall())
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
fn a_clean_worktree_lists_one_removable_row() {
    let setup = Setup::new("cli-prune-wt-clean");
    let dir = setup.worktree("s_00000000000000e1");
    let planned = select(&setup, false);
    assert_eq!(planned.rows.len(), 1);
    assert_eq!(planned.removals.len(), 1);
    let (name, forced) = removable(&planned.rows[0]);
    assert_eq!((name, forced), ("s_00000000000000e1", None));
    assert_eq!(
        line(&planned.rows[0]),
        format!(
            "worktree  s_00000000000000e1  fiber/s_00000000000000e1  clean  0d  {}",
            super::super::format_size(worktree_bytes(&dir)),
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
    let planned = select(&setup, false);
    assert!(planned.removals.is_empty());
    let lines: Vec<String> = planned.rows.iter().map(line).collect();
    let size = |dir: &Path| super::super::format_size(worktree_bytes(dir));
    assert!(lines.contains(
        &format!(
            "worktree  s_00000000000000e2  fiber/s_00000000000000e2  uncommitted  0d  {}  skipped: removing it would lose uncommitted or ignored files",
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
            "worktree  s_00000000000000e4  fiber/s_00000000000000e4  uncommitted  0d  {}  skipped: removing it would lose uncommitted or ignored files and commits found nowhere else",
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
    let planned = select(&setup, true);
    assert_eq!(planned.removals.len(), 1);
    let (name, forced) = removable(&planned.rows[0]);
    assert_eq!(name, "s_00000000000000e2");
    assert_eq!(forced, Some("uncommitted or ignored files"));
    assert!(
        line(&planned.rows[0]).ends_with("  forced: loses uncommitted or ignored files"),
        "{}",
        line(&planned.rows[0])
    );
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
    let mut planned = select(&setup, true);
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
    let planned = select(&setup, true);
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
    let planned = select(&setup, true);
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
    let planned = select(&setup, true);
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
    let mut planned = select(&setup, false);
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
    let mut planned = select(&setup, false);
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
    let planned = select(&setup, false);
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(planned.locks.len(), 1, "the plan owns the user's lock");
    assert!(
        matches!(log::try_hold(&user), Ok(log::Hold::Busy)),
        "the lock is held while the plan lives"
    );
    drop(planned);
    // Another test's `git` child can fork while the lock's descriptor is
    // open and keep the flock past this thread's close until it execs, so
    // the first re-acquires after the drop can see `Busy`. A lock the plan
    // kept would never come back: the bound only passes what a scheduling
    // artifact delays.
    let mut held = false;
    for _ in 0..200_000 {
        if matches!(log::try_hold(&user), Ok(log::Hold::Held(_))) {
            held = true;
            break;
        }
    }
    assert!(held, "no lock is kept once the plan drops");
}

#[test]
fn a_partial_removal_is_reported_and_still_counts_as_freed() {
    let setup = Setup::new("cli-prune-wt-partial");
    let dir = setup.worktree("s_00000000000000e1");
    let listed = worktree_bytes(&dir);
    assert!(listed > 0);
    let mut planned = select(&setup, false);
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
    let planned = super::select(&setup.home(), &setup.workspace(), false, wall());
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
    let planned = select(&setup, false);
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(removable(&planned.rows[0]).0, "s_00000000000000e1");
}

#[test]
fn an_exited_user_is_held_once_and_the_worktree_still_goes() {
    let setup = Setup::new("cli-prune-wt-exited-user");
    let dir = setup.worktree("s_00000000000000e1");
    setup.user("s_00000000000000a1", &dir);
    let mut planned = select(&setup, false);
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
    let planned = select(&setup, false);
    assert_eq!(planned.rows.len(), 1);
    assert_eq!(removable(&planned.rows[0]).0, "s_00000000000000e1");
}

#[test]
fn a_symlink_inside_counts_no_bytes() {
    let setup = Setup::new("cli-prune-wt-link-bytes");
    let dir = setup.worktree("s_00000000000000e1");
    let before = worktree_bytes(&dir);
    assert!(before > 0);
    fs::write(setup.repo().join("big.txt"), "y".repeat(100_000)).unwrap();
    std::os::unix::fs::symlink(setup.repo().join("big.txt"), dir.join("big.txt")).unwrap();
    assert_eq!(worktree_bytes(&dir), before);
}

#[test]
fn force_through_prune_run_prints_forced_removes_and_frees() {
    let setup = Setup::new("cli-prune-wt-run");
    let dir = setup.worktree("s_00000000000000e2");
    fs::write(dir.join("notes.txt"), "scratch").unwrap();
    let listed = worktree_bytes(&dir);
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
        wall(),
        ask,
        &mut out,
        &mut || -> io::Result<doors::hub::Hub> { panic!("nothing connects") },
    );
    got.unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("forced: loses uncommitted or ignored files"),
        "{out}"
    );
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
