//! Tests for the pinned side of prune: ownership, the worktree match,
//! rows, revalidation and removal, with real `git` and approval fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::fs;
use std::io::BufRead;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::events::OfferedKind;
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::json;

use super::super::{PruneArgs, prune_run};
use super::*;

const HOOK: &str = r#"{"hooks":{"fmt":{"point":"after_tool","command":"scripts/fmt.sh"}}}"#;

/// Fiber home and a plain workspace outside any repository, removed on
/// drop. Git repositories are added per test under the root.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        Self {
            root: fakes::TempDir::new(prefix),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// A workspace in a plain directory, outside any repository.
    fn workspace(&self) -> PathBuf {
        let dir = self.root.path().join("workspace");
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap()
    }

    /// A git repository at `name` with hook `fmt` running `script` on
    /// `main`, committed. Returned canonicalized.
    fn repo(&self, name: &str, script: &str) -> PathBuf {
        let dir = self.root.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet"]);
        write_hook(&dir, script);
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "--quiet", "-m", "first"]);
        fs::canonicalize(&dir).unwrap()
    }

    /// A linked worktree of `repo` on a new branch with its own hook
    /// script, committed. Returned canonicalized.
    fn worktree(&self, repo: &Path, branch: &str, name: &str, script: &str) -> PathBuf {
        let path = self.root.path().join(name);
        git(
            repo,
            &["worktree", "add", "-b", branch, path.to_str().unwrap()],
        );
        write_hook(&path, script);
        git(&path, &["add", "."]);
        git(&path, &["commit", "--quiet", "-m", branch]);
        fs::canonicalize(&path).unwrap()
    }
}

/// Hook `fmt` running `script` in `dir`.
fn write_hook(dir: &Path, script: &str) {
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(dir.join("scripts/fmt.sh"), script).unwrap();
    fs::set_permissions(
        dir.join("scripts/fmt.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::create_dir_all(dir.join(".fiber/config")).unwrap();
    fs::write(dir.join(".fiber/config/hooks.json"), HOOK).unwrap();
}

/// The project key of the repository `dir` is in, as prune computes it.
fn key_of(dir: &Path) -> String {
    log::project_key(&worktree::resolve_project(dir).path)
}

/// The hook `fmt` the tree at `dir` declares, with its content hash.
fn hook_item(home: &Path, dir: &Path, clock: &FakeClock) -> (extensions::RepoItem, String) {
    let items = extensions::declared_items(dir, clock).unwrap();
    let item = items
        .into_iter()
        .find(|item| item.kind == OfferedKind::Hook && item.name == "fmt")
        .unwrap();
    let hash = extensions::hash(&mut extensions::Index::load(home), &item).unwrap();
    (item, hash)
}

/// Approves the hook `fmt` the tree at `dir` declares for `key`, and
/// returns its content hash.
fn approve(home: &Path, key: &str, dir: &Path, clock: &FakeClock) -> String {
    let (item, hash) = hook_item(home, dir, clock);
    extensions::Store::new(
        home,
        &config::ProjectKey::new(key).unwrap(),
        FakeClock::new(),
    )
    .approve(&item, &hash)
    .unwrap();
    hash
}

/// A session `id` under `key` whose first line records `workspace`, and
/// the common directory `repository` when given.
fn session(home: &Path, key: &str, id: &str, workspace: &str, repository: Option<&str>) {
    let dir = home.join("projects").join(key).join("sessions").join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut payload = json!({"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(repository) = repository {
        payload["worktree"] =
            json!({"path": workspace, "branch": "main", "repository": repository});
    }
    let line = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0, "payload": payload});
    fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
}

fn approval(home: &Path, key: &str, hash: &str) -> PathBuf {
    home.join("projects").join(key).join("approvals").join(hash)
}

fn copy_dir(home: &Path, hash: &str) -> PathBuf {
    home.join("pinned").join(hash)
}

fn marker(home: &Path, hash: &str) -> PathBuf {
    home.join("pinned").join(format!("{hash}.ready"))
}

/// A file mode restored on drop, so a failure still leaves the tree
/// writable.
struct RestoreMode {
    path: PathBuf,
}

impl RestoreMode {
    fn set(path: &Path, mode: u32) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self {
            path: path.to_owned(),
        }
    }
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755)).unwrap_or(());
    }
}

/// Runs `prune_run` with no session work: its stdout, stderr and result.
fn run_prune(
    home: &Path,
    workspace: &Path,
    dry_run: bool,
    yes: bool,
    force: bool,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, String, Result<(), crate::CliError>) {
    let args = PruneArgs {
        older_than: None,
        cascade: false,
        dry_run,
        yes,
        force,
    };
    let clock = FakeClock::new();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ask = crate::sessions::Ask {
        yes,
        terminal,
        input,
        err: &mut err,
    };
    let got = prune_run(
        home,
        workspace,
        &args,
        clock.as_ref(),
        ask,
        &mut out,
        &mut || -> Result<doors::hub::Hub, doors::Error> { panic!("nothing connects") },
    );
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        got,
    )
}

/// How the `b` worktree goes away.
enum RemoveB {
    /// `git worktree remove`: the entry goes with it.
    Git,
    /// Deleted from the filesystem: the entry stays, missing.
    RmRf,
    /// It stays.
    Keep,
}

/// `main`, `a` and `b` approved for one key, with `b` going away as told.
struct Branches {
    repo: PathBuf,
    wt_a: PathBuf,
    wt_b: PathBuf,
    key: String,
    clock: Arc<FakeClock>,
    main: String,
    a: String,
    b: String,
}

fn three_branches(prefix: &str, remove: RemoveB) -> (Setup, Branches) {
    let setup = Setup::new(prefix);
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let wt_a = setup.worktree(&repo, "a", "wt-a", "a\n");
    let wt_b = setup.worktree(&repo, "b", "wt-b", "b\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    let main = approve(&home, &key, &repo, clock.as_ref());
    let a = approve(&home, &key, &wt_a, clock.as_ref());
    let b = approve(&home, &key, &wt_b, clock.as_ref());
    match remove {
        RemoveB::Git => {
            git(&repo, &["worktree", "remove", wt_b.to_str().unwrap()]);
        }
        RemoveB::RmRf => {
            fs::remove_dir_all(&wt_b).unwrap();
        }
        RemoveB::Keep => {}
    }
    (
        setup,
        Branches {
            repo,
            wt_a,
            wt_b,
            key,
            clock,
            main,
            a,
            b,
        },
    )
}

/// The removable rows of a plan, by hash.
fn removable(plan: &Plan) -> Vec<(&str, u64)> {
    plan.rows
        .iter()
        .filter_map(|row| match row {
            PinnedRow::Removable { hash, bytes } => Some((hash.as_str(), *bytes)),
            PinnedRow::Kept { .. } | PinnedRow::Skipped { .. } => None,
        })
        .collect()
}

/// Scans `home` and selects the pinned plan for `workspace`: every call
/// stays one line at its site.
fn plan(home: &Path, workspace: &Path, clock: &dyn contract::clock::Clock) -> Plan {
    select(home, &log::started_sessions(home), workspace, clock)
}

#[test]
fn an_unmatched_copy_is_listed_and_removed() {
    let (setup, branches) = three_branches("pinned-unmatched", RemoveB::Git);
    let home = setup.home();
    let bytes = log::session_bytes(&copy_dir(&home, &branches.b));
    assert!(bytes > 0);
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan), vec![(branches.b.as_str(), bytes)]);
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!(
            "pinned  {}  {}",
            branches.b,
            super::super::format_size(bytes)
        )
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, bytes);
    assert!(removed.failures.is_empty());
    assert!(copy_dir(&home, &branches.b).symlink_metadata().is_err());
    assert!(marker(&home, &branches.b).symlink_metadata().is_err());
    assert!(copy_dir(&home, &branches.a).is_dir());
    assert!(copy_dir(&home, &branches.main).is_dir());
    // Every approval record stays, so the same version checked out again
    // is not offered again.
    for hash in [&branches.main, &branches.a, &branches.b] {
        assert!(approval(&home, &branches.key, hash).is_file());
    }
}

#[test]
fn prune_lists_frees_and_dry_runs_an_unmatched_copy() {
    let (setup, branches) = three_branches("pinned-prune-run", RemoveB::Git);
    let home = setup.home();
    let bytes = log::session_bytes(&copy_dir(&home, &branches.b));
    let size = super::super::format_size(bytes);
    let mut input: &[u8] = b"";
    let (out, _, got) = run_prune(&home, &branches.repo, true, false, false, false, &mut input);
    got.unwrap();
    assert!(
        out.contains(&format!("pinned  {}  {size}", branches.b)),
        "{out}"
    );
    assert!(out.contains(&format!("would free {size}")), "{out}");
    assert!(
        copy_dir(&home, &branches.b).is_dir(),
        "dry-run removes nothing"
    );
    let (out, _, got) = run_prune(&home, &branches.repo, false, true, false, false, &mut input);
    got.unwrap();
    assert!(
        out.contains(&format!("pinned  {}  {size}", branches.b)),
        "{out}"
    );
    assert!(out.contains(&format!("freed {size}")), "{out}");
    assert!(copy_dir(&home, &branches.b).symlink_metadata().is_err());
}

#[test]
fn a_copy_a_worktree_still_matches_is_kept() {
    let (setup, branches) = three_branches("pinned-matched", RemoveB::Keep);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert!(plan.rows.is_empty());
    for hash in [&branches.main, &branches.a, &branches.b] {
        assert!(copy_dir(&home, hash).is_dir());
    }
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert_eq!(removed.freed, 0);
}

#[test]
fn a_worktree_missing_from_the_filesystem_frees_its_copy() {
    let (setup, branches) = three_branches("pinned-rmrf", RemoveB::RmRf);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    assert!(matches!(plan.rows[0], PinnedRow::Removable { .. }));
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(copy_dir(&home, &branches.b).symlink_metadata().is_err());
    assert!(copy_dir(&home, &branches.a).is_dir());
}

#[test]
fn a_locked_worktree_missing_from_the_filesystem_is_uncertain() {
    let (setup, branches) = three_branches("pinned-locked", RemoveB::Keep);
    let home = setup.home();
    git(
        &branches.repo,
        &["worktree", "lock", branches.wt_b.to_str().unwrap()],
    );
    fs::remove_dir_all(&branches.wt_b).unwrap();
    let listed = worktree::list(&branches.repo).unwrap();
    let missing = listed
        .iter()
        .find(|entry| entry.locked)
        .expect("the locked worktree is listed")
        .path
        .clone();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!(
            "pinned  {}  skipped: the locked worktree {} is missing",
            branches.b,
            missing.display()
        )
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn an_unreadable_tree_config_keeps_every_candidate() {
    let (setup, branches) = three_branches("pinned-unreadable", RemoveB::Git);
    let home = setup.home();
    fs::write(branches.wt_a.join(".fiber/config.json"), "{invalid").unwrap();
    let error = extensions::declared_items(&branches.wt_a, branches.clock.as_ref())
        .unwrap_err()
        .to_string();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(plan.rows.len(), 3);
    for hash in [&branches.main, &branches.a, &branches.b] {
        let expected = format!(
            "pinned  {hash}  skipped: the worktree {} cannot be read: {error}",
            branches.wt_a.display()
        );
        assert!(
            plan.rows.iter().any(|row| pinned_line(row) == expected),
            "{expected}"
        );
    }
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn a_bare_entry_is_skipped_and_does_not_block_pruning() {
    let setup = Setup::new("pinned-bare");
    let home = setup.home();
    let seed = setup.repo("seed", "seed\n");
    let bare = setup.root.path().join("bare.git");
    git(
        &seed,
        &["clone", "--quiet", "--bare", ".", bare.to_str().unwrap()],
    );
    let bare = fs::canonicalize(&bare).unwrap();
    let wt = setup.root.path().join("wt");
    git(&bare, &["worktree", "add", "-b", "w", wt.to_str().unwrap()]);
    let wt = fs::canonicalize(&wt).unwrap();
    write_hook(&wt, "w\n");
    let key = key_of(&wt);
    let clock = FakeClock::new();
    let hash = approve(&home, &key, &wt, clock.as_ref());
    // The worktree matches: nothing is listed, and the bare entry does
    // not block the match.
    let plan1 = plan(&home, &wt, clock.as_ref());
    assert!(plan1.rows.is_empty());
    git(
        &bare,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    // Only the bare entry is left: the copy is removable.
    let plan2 = plan(&home, &bare, clock.as_ref());
    assert_eq!(removable(&plan2).len(), 1);
    assert_eq!(removable(&plan2)[0].0, hash.as_str());
}

#[test]
fn a_failed_worktree_list_keeps_every_candidate() {
    let (setup, branches) = three_branches("pinned-list-fails", RemoveB::Git);
    let home = setup.home();
    let list = |_: &Path| -> Result<Vec<worktree::Listed>, worktree::Error> {
        Err(worktree::Error::GitMissing)
    };
    let plan = select_with(
        &home,
        &log::started_sessions(&home),
        &branches.repo,
        branches.clock.as_ref(),
        &|_| None,
        &list,
    );
    assert_eq!(plan.rows.len(), 3);
    for row in &plan.rows {
        assert_eq!(
            pinned_line(row),
            format!(
                "pinned  {}  skipped: git cannot list the worktrees: git is not installed.",
                row_hash(row)
            )
        );
    }
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &branches.b).is_dir());
}

/// A path resolving through two symlink names that point at each other:
/// any metadata read through it fails with an error other than missing.
fn error_loop(root: &Path) -> (PathBuf, String) {
    let first = root.join("loop-a");
    let second = root.join("loop-b");
    symlink("loop-b", &first).unwrap();
    symlink("loop-a", &second).unwrap();
    let through = first.join("inner");
    let err = fs::symlink_metadata(&through).unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
    (through, err.to_string())
}

#[test]
fn an_unreadable_listed_worktree_keeps_every_candidate() {
    let (setup, branches) = three_branches("pinned-eloop-entry", RemoveB::Git);
    let home = setup.home();
    // A listed worktree whose metadata fails with an error other than
    // missing: every candidate is uncertain, never removable.
    let (listed, err) = error_loop(setup.root.path());
    let expected = format!(
        "pinned  {}  skipped: the worktree {} cannot be read: {err}",
        branches.b,
        listed.display()
    );
    let list = |_: &Path| -> Result<Vec<worktree::Listed>, worktree::Error> {
        Ok(vec![worktree::Listed {
            path: listed.clone(),
            bare: false,
            locked: false,
        }])
    };
    let plan = select_with(
        &home,
        &log::started_sessions(&home),
        &branches.repo,
        branches.clock.as_ref(),
        &|_| None,
        &list,
    );
    assert_eq!(plan.rows.len(), 3);
    assert!(
        plan.rows.iter().any(|row| pinned_line(row) == expected),
        "{expected}"
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn an_unreadable_recorded_identity_keeps_its_copy() {
    let setup = Setup::new("pinned-eloop-identity");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    let hash = approve(&home, &key, &repo, clock.as_ref());
    // No tree matches any more, but the recorded identity cannot even be
    // read: the copy is uncertain, never removable.
    write_hook(&repo, "changed\n");
    let (identity, err) = error_loop(setup.root.path());
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        identity.to_str().unwrap(),
        None,
    );
    let plan = plan(&home, &repo, clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!(
            "pinned  {hash}  skipped: the worktree {} cannot be read: {err}",
            identity.display()
        )
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &hash).is_dir());
}

#[test]
fn a_held_copy_is_listed_as_skipped_and_frees_nothing() {
    let setup = Setup::new("pinned-held");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let wt_a = setup.worktree(&repo, "a", "wt-a", "a\n");
    let wt_b = setup.worktree(&repo, "b", "wt-b", "b\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    approve(&home, &key, &repo, clock.as_ref());
    approve(&home, &key, &wt_a, clock.as_ref());
    let (item_b, hash_b) = hook_item(&home, &wt_b, clock.as_ref());
    approve(&home, &key, &wt_b, clock.as_ref());
    let store = extensions::Store::new(
        &home,
        &config::ProjectKey::new(&key).unwrap(),
        FakeClock::new(),
    );
    let held = store.ensure_copy(&item_b, &hash_b).unwrap();
    git(&repo, &["worktree", "remove", wt_b.to_str().unwrap()]);
    let plan = plan(&home, &repo, clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!("pinned  {hash_b}  skipped: a session holds it")
    );
    let mut input: &[u8] = b"";
    let (out, _, got) = run_prune(&home, &repo, false, true, false, false, &mut input);
    got.unwrap();
    assert!(out.contains("skipped: a session holds it"), "{out}");
    assert!(out.contains("freed 0 B"), "{out}");
    assert!(copy_dir(&home, &hash_b).is_dir());
    drop(held);
}

#[test]
fn a_hold_taken_after_listing_fails_the_removal() {
    let (setup, branches) = three_branches("pinned-hold-race", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // A shared hold on the marker, as a running session keeps: taken
    // after the listing, refused at removal.
    let mut held: Option<fs::File> = None;
    let mut before_remove = |hash: &str| {
        if hash == branches.b {
            let marker_file = fs::File::options()
                .read(true)
                .open(marker(&home, &branches.b))
                .unwrap();
            marker_file.lock_shared().unwrap();
            held = Some(marker_file);
        }
    };
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {}", branches.b),
            ErrorCode::IoFailed,
            "a session holds it".to_owned()
        )]
    );
    assert!(copy_dir(&home, &branches.b).is_dir());
    assert!(held.is_some());
}

#[test]
fn revalidation_refuses_changed_owners() {
    let (setup, branches) = three_branches("pinned-reval-owners", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // A second repository with the same hook content, approved by a
    // second project after the listing.
    let other = setup.repo("other", "b\n");
    let (_, other_hash) = hook_item(&home, &other, branches.clock.as_ref());
    assert_eq!(other_hash, branches.b);
    let other_key = key_of(&other);
    assert_ne!(other_key, branches.key);
    let mut before_remove = |_: &str| {
        approve(&home, &other_key, &other, branches.clock.as_ref());
    };
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {}", branches.b),
            ErrorCode::IoFailed,
            "its approvals changed since it was listed".to_owned()
        )]
    );
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn revalidation_refuses_a_new_machine_wide_record() {
    let (setup, branches) = three_branches("pinned-reval-machine", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // A per-machine record for the planned hash appears after the
    // listing: the copy is kept, even though its project owners did not
    // change.
    let mut before_remove = |_: &str| {
        fs::create_dir_all(home.join("approvals")).unwrap();
        fs::write(home.join("approvals").join(&branches.b), "{}").unwrap();
    };
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {}", branches.b),
            ErrorCode::IoFailed,
            "its approvals changed since it was listed".to_owned()
        )]
    );
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn revalidation_refuses_a_newly_matching_tree() {
    let (setup, branches) = three_branches("pinned-reval-match", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // After the listing a worktree is checked out on the planned version
    // again: the branch stayed when its worktree was removed.
    let mut before_remove = |_: &str| {
        let path = setup.root.path().join("wt-b2");
        git(
            &branches.repo,
            &["worktree", "add", path.to_str().unwrap(), "b"],
        );
    };
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {}", branches.b),
            ErrorCode::IoFailed,
            "a worktree matches it now".to_owned()
        )]
    );
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn revalidation_refuses_an_unreadable_tree() {
    let (setup, branches) = three_branches("pinned-reval-unreadable", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    let mut before_remove = |_: &str| {
        fs::write(branches.wt_a.join(".fiber/config.json"), "{invalid").unwrap();
    };
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut before_remove);
    let error = extensions::declared_items(&branches.wt_a, branches.clock.as_ref())
        .unwrap_err()
        .to_string();
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {}", branches.b),
            ErrorCode::IoFailed,
            format!(
                "the worktree {} cannot be read: {error}",
                branches.wt_a.display()
            )
        )]
    );
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn a_copy_another_project_also_approved_is_out_of_scope_inside_a_repository() {
    let setup = Setup::new("pinned-scoped-in");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let wt_a = setup.worktree(&repo, "a", "wt-a", "a\n");
    let wt_b = setup.worktree(&repo, "b", "wt-b", "b\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    approve(&home, &key, &repo, clock.as_ref());
    approve(&home, &key, &wt_a, clock.as_ref());
    let (item_b, hash_b) = hook_item(&home, &wt_b, clock.as_ref());
    approve(&home, &key, &wt_b, clock.as_ref());
    // Another project approves the same copy: inside this repository it
    // is out of scope, so the copy is kept and not listed.
    extensions::Store::new(
        &home,
        &config::ProjectKey::new("other").unwrap(),
        FakeClock::new(),
    )
    .never(&item_b, &hash_b)
    .unwrap();
    git(&repo, &["worktree", "remove", wt_b.to_str().unwrap()]);
    let plan = plan(&home, &repo, clock.as_ref());
    assert!(plan.rows.is_empty());
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &hash_b).is_dir());
}

#[test]
fn shared_copies_are_judged_by_every_owner_outside_a_repository() {
    let setup = Setup::new("pinned-scoped-out");
    let home = setup.home();
    let repo_a = setup.repo("ra", "a\n");
    let repo_b = setup.repo("rb", "b\n");
    let key_a = key_of(&repo_a);
    let key_b = key_of(&repo_b);
    assert_ne!(key_a, key_b);
    let clock = FakeClock::new();
    let (_, hash) = hook_item(&home, &repo_b, clock.as_ref());
    approve(&home, &key_a, &repo_b, clock.as_ref());
    approve(&home, &key_b, &repo_b, clock.as_ref());
    session(
        &home,
        &key_a,
        "s_aaaaaaaaaaaaaaaa",
        repo_a.to_str().unwrap(),
        None,
    );
    session(
        &home,
        &key_b,
        "s_aaaaaaaaaaaaaaab",
        repo_b.to_str().unwrap(),
        None,
    );
    let workspace = setup.workspace();
    // The other project's tree still matches: kept, not listed.
    let plan1 = plan(&home, &workspace, clock.as_ref());
    assert!(plan1.rows.is_empty());
    // After it changes too, neither tree matches: removed.
    write_hook(&repo_b, "changed\n");
    let plan2 = plan(&home, &workspace, clock.as_ref());
    assert_eq!(removable(&plan2).len(), 1);
    assert_eq!(removable(&plan2)[0].0, hash.as_str());
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan2, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(copy_dir(&home, &hash).symlink_metadata().is_err());
}

/// Two repositories sharing one project key, with the second's hook
/// approved: the first's hook differs, the second's matches.
fn slug_setup(prefix: &str) -> (Setup, PathBuf, PathBuf, String, Arc<FakeClock>, String) {
    let setup = Setup::new(prefix);
    let home = setup.home();
    let first = setup.repo("x/a-b", "a\n");
    let second = setup.repo("x/a/b", "b\n");
    let key = key_of(&first);
    assert_eq!(key, key_of(&second));
    let clock = FakeClock::new();
    let kept = approve(&home, &key, &second, clock.as_ref());
    (setup, first, second, key, clock, kept)
}

#[test]
fn two_identities_under_one_slug_keep_a_copy_matched_by_either() {
    let (setup, first, second, key, clock, kept) = slug_setup("pinned-slug-keep");
    let home = setup.home();
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        first.to_str().unwrap(),
        None,
    );
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaab",
        second.to_str().unwrap(),
        None,
    );
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &kept).is_dir());
}

#[test]
fn two_identities_under_one_slug_remove_a_copy_matched_by_neither() {
    let (setup, first, second, key, clock, kept) = slug_setup("pinned-slug-remove");
    let home = setup.home();
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        first.to_str().unwrap(),
        None,
    );
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaab",
        second.to_str().unwrap(),
        None,
    );
    write_hook(&second, "changed\n");
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    assert_eq!(removable(&plan)[0].0, kept.as_str());
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(copy_dir(&home, &kept).symlink_metadata().is_err());
}

#[test]
fn two_identities_under_one_slug_survive_a_deleted_identity() {
    let (setup, first, second, key, clock, kept) = slug_setup("pinned-slug-deleted");
    let home = setup.home();
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        first.to_str().unwrap(),
        None,
    );
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaab",
        second.to_str().unwrap(),
        None,
    );
    fs::remove_dir_all(&first).unwrap();
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &kept).is_dir());
}

/// Two repositories sharing one project key, with sessions recording
/// their common directories and workspaces pointing elsewhere.
fn recorded_setup(prefix: &str) -> (Setup, PathBuf, PathBuf, String, Arc<FakeClock>, String) {
    let setup = Setup::new(prefix);
    let home = setup.home();
    let first = setup.repo("r/a-b", "a\n");
    let second = setup.repo("r/a/b", "b\n");
    let key = key_of(&first);
    assert_eq!(key, key_of(&second));
    let clock = FakeClock::new();
    let kept = approve(&home, &key, &second, clock.as_ref());
    let elsewhere = setup.workspace();
    let common = |repo: &Path| worktree::resolve_project(repo).path;
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        elsewhere.to_str().unwrap(),
        Some(common(&first).to_str().unwrap()),
    );
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaab",
        elsewhere.to_str().unwrap(),
        Some(common(&second).to_str().unwrap()),
    );
    (setup, first, second, key, clock, kept)
}

#[test]
fn recorded_repository_decides_a_shared_slug() {
    let (setup, first, _, _, clock, kept) = recorded_setup("pinned-rec-keep");
    let home = setup.home();
    // One recorded repository is gone, but the other still has a tree
    // matching the copy: kept, not listed.
    fs::remove_dir_all(&first).unwrap();
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &kept).is_dir());
}

#[test]
fn recorded_slug_removes_a_copy_matched_by_neither() {
    let (setup, first, second, _, clock, kept) = recorded_setup("pinned-rec-remove");
    let home = setup.home();
    write_hook(&first, "changed\n");
    write_hook(&second, "changed\n");
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    assert_eq!(removable(&plan)[0].0, kept.as_str());
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(copy_dir(&home, &kept).symlink_metadata().is_err());
}

#[test]
fn recorded_slug_falls_back_to_the_workspace() {
    let setup = Setup::new("pinned-rec-fallback");
    let home = setup.home();
    let second = setup.repo("r/a/b", "b\n");
    let key = key_of(&second);
    let clock = FakeClock::new();
    let kept = approve(&home, &key, &second, clock.as_ref());
    // No worktree repository recorded: the workspace decides, and it
    // still matches, so the copy is kept.
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        second.to_str().unwrap(),
        None,
    );
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &kept).is_dir());
}

#[test]
fn a_never_record_with_a_leftover_copy_is_removed() {
    let setup = Setup::new("pinned-never");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    let (item, hash) = hook_item(&home, &repo, clock.as_ref());
    let store = extensions::Store::new(
        &home,
        &config::ProjectKey::new(&key).unwrap(),
        FakeClock::new(),
    );
    store.approve(&item, &hash).unwrap();
    // A never replaces the approval but leaves the copy behind.
    store.never(&item, &hash).unwrap();
    write_hook(&repo, "changed\n");
    let plan = plan(&home, &repo, clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(copy_dir(&home, &hash).symlink_metadata().is_err());
    assert!(approval(&home, &key, &hash).is_file());
}

/// Declares an MCP server `db` in `dir`.
fn declare_server(dir: &Path) {
    fs::create_dir_all(dir.join("srv")).unwrap();
    fs::write(dir.join("srv/run.js"), "run\n").unwrap();
    fs::write(
        dir.join(".fiber/config.json"),
        r#"{"mcp":{"servers":{"db":{"command":"node","args":["srv/run.js"]}}}}"#,
    )
    .unwrap();
}

/// The MCP server `dir` declares, with its content hash.
fn server_item(home: &Path, dir: &Path, clock: &FakeClock) -> (extensions::RepoItem, String) {
    let items = extensions::declared_items(dir, clock).unwrap();
    let item = items
        .into_iter()
        .find(|item| item.kind == OfferedKind::McpServer)
        .unwrap();
    let hash = extensions::hash(&mut extensions::Index::load(home), &item).unwrap();
    (item, hash)
}
/// Approves the MCP server `dir` declares for `key`, which records per
/// machine and in the project's record, and returns its hash.
fn approve_server_key(home: &Path, key: &str, dir: &Path, clock: &FakeClock) -> String {
    let (item, hash) = server_item(home, dir, clock);
    extensions::Store::new(
        home,
        &config::ProjectKey::new(key).unwrap(),
        FakeClock::new(),
    )
    .approve(&item, &hash)
    .unwrap();
    hash
}

/// Approves an MCP server, which records per machine, and returns its
/// hash.
fn approve_server(home: &Path, dir: &Path, clock: &FakeClock) -> String {
    declare_server(dir);
    approve_server_key(home, "project", dir, clock)
}

#[test]
fn a_server_copy_is_not_listed_inside_a_repository() {
    let setup = Setup::new("pinned-server-in");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let clock = FakeClock::new();
    let hash = approve_server(&home, &repo, clock.as_ref());
    let plan = plan(&home, &repo, clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &hash).is_dir());
}

#[test]
fn a_server_copy_is_not_listed_outside_a_repository() {
    let setup = Setup::new("pinned-server-out");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let clock = FakeClock::new();
    let hash = approve_server(&home, &repo, clock.as_ref());
    let key = key_of(&repo);
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        repo.to_str().unwrap(),
        None,
    );
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    // Judged as an extension copy is: the project record names it, but
    // its project has no recorded identity, so it is kept, not listed.
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!("pinned  {hash}  skipped: its project's directory cannot be found")
    );
    assert!(copy_dir(&home, &hash).is_dir());
}

/// Points the server `db` in `dir` at another file, so no approved copy
/// matches the tree any more.
fn redeclare_server(dir: &Path) {
    fs::write(dir.join("srv/changed.js"), "changed\n").unwrap();
    fs::write(
        dir.join(".fiber/config.json"),
        r#"{"mcp":{"servers":{"db":{"command":"node","args":["srv/changed.js"]}}}}"#,
    )
    .unwrap();
}

#[test]
fn a_server_copy_with_an_in_scope_project_record_is_a_candidate() {
    let setup = Setup::new("pinned-server-candidate");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    declare_server(&repo);
    let hash = approve_server_key(&home, &key, &repo, clock.as_ref());
    // No worktree matches the approved copy any more: the per-machine
    // record does not keep it from being listed.
    redeclare_server(&repo);
    let plan = plan(&home, &repo, clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    assert_eq!(removable(&plan)[0].0, hash.as_str());
    assert_eq!(plan.rows.len(), 1);
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 1);
    assert!(removed.failures.is_empty());
    assert!(copy_dir(&home, &hash).symlink_metadata().is_err());
}

#[test]
fn a_server_copy_with_no_project_record_is_not_listed() {
    let setup = Setup::new("pinned-server-orphan");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    declare_server(&repo);
    let hash = approve_server_key(&home, &key, &repo, clock.as_ref());
    // A copy approved before project records existed: the per-machine
    // record stands, but no project record names it, so it is kept.
    fs::remove_file(home.join("projects").join(&key).join("servers").join(&hash)).unwrap();
    redeclare_server(&repo);
    let plan = plan(&home, &repo, clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &hash).is_dir());
}

#[test]
fn revalidation_refuses_a_new_server_project_record() {
    let setup = Setup::new("pinned-server-reval");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    declare_server(&repo);
    let (item, hash) = server_item(&home, &repo, clock.as_ref());
    extensions::Store::new(
        &home,
        &config::ProjectKey::new(&key).unwrap(),
        FakeClock::new(),
    )
    .approve(&item, &hash)
    .unwrap();
    redeclare_server(&repo);
    let plan = plan(&home, &repo, clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // Another project approves the same server copy after the listing:
    // its new project record refuses the removal.
    let mut before_remove = |_: &str| {
        extensions::Store::new(
            &home,
            &config::ProjectKey::new("other").unwrap(),
            FakeClock::new(),
        )
        .approve(&item, &hash)
        .unwrap();
    };
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut before_remove);
    assert_eq!(removed.total, 1);
    assert_eq!(removed.freed, 0);
    assert_eq!(
        removed.failures,
        vec![(
            format!("pinned {hash}"),
            ErrorCode::IoFailed,
            "its approvals changed since it was listed".to_owned()
        )]
    );
    assert!(copy_dir(&home, &hash).is_dir());
}

#[test]
fn a_copy_with_no_record_is_not_listed() {
    let (setup, branches) = three_branches("pinned-orphan", RemoveB::Keep);
    let home = setup.home();
    // An orphan copy with no record anywhere: kept, not listed.
    let orphan: String = "f".repeat(64);
    fs::create_dir_all(copy_dir(&home, &orphan)).unwrap();
    fs::write(marker(&home, &orphan), "").unwrap();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &orphan).is_dir());
}

#[test]
fn unreadable_ownership_judges_no_copy() {
    let (setup, branches) = three_branches("pinned-unreadable-owns", RemoveB::Git);
    let home = setup.home();
    let approvals = home.join("projects").join(&branches.key).join("approvals");
    let _guard = RestoreMode::set(&approvals, 0o000);
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    let expected = extensions::Store::kept_copies(&home)
        .unwrap_err()
        .to_string();
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!("pinned  skipped: {expected}")
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, branches.clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn a_short_approval_name_is_ignored() {
    let (setup, branches) = three_branches("pinned-short-name", RemoveB::Git);
    let home = setup.home();
    fs::write(
        home.join("projects")
            .join(&branches.key)
            .join("approvals")
            .join("a".repeat(63)),
        "{}",
    )
    .unwrap();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    assert_eq!(removable(&plan)[0].0, branches.b.as_str());
}

#[test]
fn a_redirecting_environment_skips_the_pinned_step() {
    let (setup, branches) = three_branches("pinned-env", RemoveB::Git);
    let home = setup.home();
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        let env = |query: &str| (query == name).then(|| OsString::from("redirected"));
        let list = |_: &Path| -> Result<Vec<worktree::Listed>, worktree::Error> {
            panic!("git must not run")
        };
        let plan = select_with(
            &home,
            &log::started_sessions(&home),
            &branches.repo,
            branches.clock.as_ref(),
            &env,
            &list,
        );
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(
            pinned_line(&plan.rows[0]),
            format!("pinned  skipped: {name} is set, so git may read another repository")
        );
    }
    assert!(copy_dir(&home, &branches.b).is_dir(), "nothing is removed");
}

#[test]
fn a_project_whose_directory_is_gone_cannot_be_found() {
    let setup = Setup::new("pinned-gone");
    let home = setup.home();
    let plain = setup.root.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let plain = fs::canonicalize(&plain).unwrap();
    write_hook(&plain, "gone\n");
    let clock = FakeClock::new();
    let key = log::project_key(&plain);
    let hash = approve(&home, &key, &plain, clock.as_ref());
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        plain.to_str().unwrap(),
        None,
    );
    fs::remove_dir_all(&plain).unwrap();
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert_eq!(plan.rows.len(), 1);
    assert_eq!(
        pinned_line(&plan.rows[0]),
        format!("pinned  {hash}  skipped: its project's directory cannot be found")
    );
    let mut noop = |_: &str| {};
    let removed = remove_planned(&home, &plan, clock.as_ref(), &mut noop);
    assert_eq!(removed.total, 0);
}

#[test]
fn a_plain_directory_is_a_project_outside_a_repository() {
    let setup = Setup::new("pinned-plain");
    let home = setup.home();
    let plain = setup.root.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let plain = fs::canonicalize(&plain).unwrap();
    write_hook(&plain, "plain\n");
    let clock = FakeClock::new();
    let key = log::project_key(&plain);
    let hash = approve(&home, &key, &plain, clock.as_ref());
    // The recorded workspace is the tree: it still matches.
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        plain.to_str().unwrap(),
        None,
    );
    let plan = plan(&home, &setup.workspace(), clock.as_ref());
    assert!(plan.rows.is_empty());
    assert!(copy_dir(&home, &hash).is_dir());
}

#[test]
fn a_worktree_removed_this_run_still_counts() {
    let setup = Setup::new("pinned-ruling-9");
    let home = setup.home();
    let repo = setup.repo("repo", "main\n");
    let key = key_of(&repo);
    let clock = FakeClock::new();
    // A kept worktree under Fiber home, on a branch with its own hook
    // content: the only tree matching the copy.
    let kept_dir = home.join("projects").join(&key).join("worktrees");
    fs::create_dir_all(&kept_dir).unwrap();
    let kept_wt = kept_dir.join("wt");
    git(
        &repo,
        &["worktree", "add", "-b", "w", kept_wt.to_str().unwrap()],
    );
    let kept_wt = fs::canonicalize(&kept_wt).unwrap();
    write_hook(&kept_wt, "w\n");
    git(&kept_wt, &["add", "."]);
    git(&kept_wt, &["commit", "--quiet", "-m", "w"]);
    let hash = approve(&home, &key, &kept_wt, clock.as_ref());
    // The session works in the repository: the kept worktree is only
    // ever judged through the repository's worktree list.
    session(
        &home,
        &key,
        "s_aaaaaaaaaaaaaaaa",
        repo.to_str().unwrap(),
        None,
    );
    let workspace = setup.workspace();
    let mut input: &[u8] = b"";
    // This run removes the worktree, but the copy stays: its trees were
    // hashed before any worktree was removed.
    let (out, _, got) = run_prune(&home, &workspace, false, true, true, false, &mut input);
    got.unwrap();
    assert!(out.contains("worktree  wt "), "{out}");
    assert!(!out.lines().any(|line| line.starts_with("pinned")), "{out}");
    assert!(!kept_wt.exists(), "the worktree is gone");
    assert!(copy_dir(&home, &hash).is_dir(), "the copy stays this run");
    // A second run removes it.
    let (out, _, got) = run_prune(&home, &workspace, false, true, true, false, &mut input);
    got.unwrap();
    assert!(out.contains(&format!("pinned  {hash} ")), "{out}");
    assert!(copy_dir(&home, &hash).symlink_metadata().is_err());
}

#[test]
fn dry_run_keeps_no_lock() {
    let (setup, branches) = three_branches("pinned-dry-lock", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    // The listing probed holds and released them at once.
    let lock = fs::File::options()
        .read(true)
        .open(home.join("pinned/.lock"))
        .unwrap();
    assert!(lock.try_lock().is_ok());
    let ready = fs::File::options()
        .read(true)
        .open(marker(&home, &branches.b))
        .unwrap();
    assert!(ready.try_lock().is_ok());
    drop((lock, ready));
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn a_run_with_only_pinned_copies_prints_the_exact_freed() {
    let (setup, branches) = three_branches("pinned-only", RemoveB::Git);
    let home = setup.home();
    let bytes = log::session_bytes(&copy_dir(&home, &branches.b));
    let size = super::super::format_size(bytes);
    let mut input: &[u8] = b"";
    let (out, _, got) = run_prune(&home, &branches.repo, false, true, false, false, &mut input);
    got.unwrap();
    assert_eq!(
        out,
        format!("pinned  {}  {size}\nfreed {size}\n", branches.b)
    );
}

#[test]
fn pinned_and_diagnostic_totals_add_up_on_failure() {
    let (setup, branches) = three_branches("pinned-diag-total", RemoveB::Git);
    let home = setup.home();
    // An old diagnostic log in a directory that cannot be written: its
    // removal fails while the pinned copy still deletes, so the failure
    // count adds both totals.
    let logs = home.join("logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("old.log"), b"0123456789").unwrap();
    let wall = contract::clock::Clock::wall(branches.clock.as_ref());
    fs::File::options()
        .write(true)
        .open(logs.join("old.log"))
        .unwrap()
        .set_modified(wall - Duration::from_secs(31 * 24 * 60 * 60))
        .unwrap();
    let _guard = RestoreMode::set(&logs, 0o555);
    let mut input: &[u8] = b"";
    let (out, err, got) = run_prune(&home, &branches.repo, false, true, false, false, &mut input);
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            crate::CliError::PruneFailed {
                code: contract::ErrorCode::IoFailed,
                failed: 1,
                total: 2,
            }
        ),
        "{failure:?}"
    );
    assert!(err.contains("old.log could not be deleted"), "{err}");
    assert!(copy_dir(&home, &branches.b).symlink_metadata().is_err());
    assert!(logs.join("old.log").is_file());
    assert!(out.contains(&format!("pinned  {}", branches.b)), "{out}");
}

#[test]
fn a_declined_run_deletes_nothing() {
    let (setup, branches) = three_branches("pinned-declined", RemoveB::Git);
    let home = setup.home();
    let mut input: &[u8] = b"n\n";
    let (out, err, got) = run_prune(&home, &branches.repo, false, false, false, true, &mut input);
    got.unwrap();
    assert!(out.contains(&format!("pinned  {}", branches.b)), "{out}");
    assert!(err.contains("nothing deleted"), "{err}");
    assert!(copy_dir(&home, &branches.b).is_dir());
}

#[test]
fn no_candidates_frees_nothing() {
    let setup = Setup::new("pinned-none");
    let home = setup.home();
    fs::create_dir_all(&home).unwrap();
    let mut input: &[u8] = b"";
    let (out, _, got) = run_prune(
        &home,
        &setup.workspace(),
        false,
        true,
        false,
        false,
        &mut input,
    );
    got.unwrap();
    assert_eq!(out, "freed 0 B\n");
}

#[test]
fn selection_saves_the_index() {
    let (setup, branches) = three_branches("pinned-index", RemoveB::Git);
    let home = setup.home();
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    let index = fs::read_to_string(home.join("pinned.json")).unwrap();
    assert!(index.contains("fmt.sh"), "{index}");
}

#[test]
fn a_read_only_home_still_lists() {
    let (setup, branches) = three_branches("pinned-readonly", RemoveB::Git);
    let home = setup.home();
    assert!(!home.join("pinned.json").exists());
    let _guard = RestoreMode::set(&home, 0o555);
    // The index save fails, and the run still succeeds.
    let plan = plan(&home, &branches.repo, branches.clock.as_ref());
    assert_eq!(removable(&plan).len(), 1);
    assert!(!home.join("pinned.json").exists());
}
