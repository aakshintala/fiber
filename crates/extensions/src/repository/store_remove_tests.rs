//! `Store::remove_copy` and `Store::kept_copies`: removing a pinned copy
//! atomically under `pinned/.lock`, and listing which copies have approval
//! records.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{self, File, TryLockError};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use config::ProjectKey;
use contract::events::OfferedKind;
use fakes::Deadline;
use serde_json::json;

use super::super::declared_tests::Repo;
use super::super::{Index, RepoItem, hash};
use super::{CopyRemoval, Store, set_lock_attempt};
use crate::Error;

const KEY: &str = "-tmp-project-.git";

/// One wait for an operation's answer: a hang fails the test instead of
/// hanging it.
const WAIT: Duration = Duration::from_secs(10);

/// The separate short wait proving no answer arrives while the pinned lock
/// is held. The lock is one blocking `File::lock` call with no probe
/// interval to count, so the bound only has to cover the no-lock path,
/// which is metadata checks and a marker lock, with no install: a bound
/// with a named purpose, not a sleep.
const HELD: Duration = Duration::from_millis(500);

fn store(repo: &Repo, key: &str) -> Store {
    Store::new(
        &repo.home(),
        &ProjectKey::new(key).unwrap(),
        fakes::clock::FakeClock::new(),
    )
}

fn hashed(item: &RepoItem) -> String {
    hash(&mut Index::scratch(), item).unwrap()
}

fn hook(repo: &Repo) -> RepoItem {
    repo.write("scripts/fmt.sh", "cargo fmt\n");
    repo.executable("scripts/fmt.sh");
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 30000}}),
    );
    repo.item(OfferedKind::Hook, "fmt")
}

fn server(repo: &Repo) -> RepoItem {
    repo.write("srv/run.js", "run\n");
    repo.config(&json!({"mcp": {"servers": {"db": {"command": "node", "args": ["srv/run.js"]}}}}));
    repo.item(OfferedKind::McpServer, "db")
}

/// The `.ready` marker beside a copy in `pinned/`.
fn ready(home: &Path, hash: &str) -> PathBuf {
    home.join("pinned").join(format!("{hash}.ready"))
}

/// Approves `item` and returns its hash, with `pinned.json` written so the
/// removal can prove it keeps the index.
fn approved(repo: &Repo, key: &str, item: &RepoItem) -> String {
    let home = repo.home();
    let mut index = Index::load(&home);
    let digest = hash(&mut index, item).unwrap();
    index.save().unwrap();
    store(repo, key).approve(item, &digest).unwrap();
    digest
}

/// A file mode restored on drop, so a failure still leaves the tree
/// writable.
struct RestoreMode {
    path: PathBuf,
    mode: u32,
}

impl RestoreMode {
    fn set(path: &Path, mode: u32) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self {
            path: path.to_owned(),
            mode: 0o755,
        }
    }
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode)).unwrap_or(());
    }
}

/// The removal a call found, failing the test on an error.
fn found(result: Result<CopyRemoval, Error>) -> CopyRemoval {
    match result {
        Ok(removal) => removal,
        Err(error) => panic!("expected a removal, got error {error}"),
    }
}

/// The lock-attempt signal, cleared on drop so a failure cannot leak it
/// into another test in this process.
struct LockAttempt {
    _private: (),
}

impl LockAttempt {
    fn take() -> (Self, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        set_lock_attempt(Some(tx));
        (Self { _private: () }, rx)
    }
}

impl Drop for LockAttempt {
    fn drop(&mut self) {
        set_lock_attempt(None);
    }
}

#[test]
fn a_finished_copy_is_removed_with_its_marker_and_keeps_the_rest() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    let mut ok = || Ok::<(), String>(());
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut ok)),
        CopyRemoval::Removed
    );
    assert!(
        home.join("pinned")
            .join(&digest)
            .symlink_metadata()
            .is_err()
    );
    assert!(ready(&home, &digest).symlink_metadata().is_err());
    assert!(home.join("pinned/.lock").is_file());
    assert!(home.join("pinned.json").is_file());
    assert!(
        home.join("projects")
            .join(KEY)
            .join("approvals")
            .join(&digest)
            .is_file()
    );
}

#[test]
fn dry_run_reports_removed_and_changes_nothing_without_calling_check() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    let mut called = 0;
    let mut check = || {
        called += 1;
        Ok::<(), String>(())
    };
    assert_eq!(
        found(Store::remove_copy(&home, &digest, true, &mut check)),
        CopyRemoval::Removed
    );
    assert_eq!(called, 0);
    assert!(home.join("pinned").join(&digest).is_dir());
    assert!(ready(&home, &digest).is_file());
}

#[test]
fn check_runs_under_the_pinned_lock_after_the_hold_test() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    let lock_path = home.join("pinned/.lock");
    // The refusal path: the copy and marker stay, and the lock is free
    // afterwards.
    let mut check = || {
        let probe = File::options().read(true).open(&lock_path).unwrap();
        assert!(matches!(probe.try_lock(), Err(TryLockError::WouldBlock)));
        Err::<(), String>("a worktree matches it now".to_owned())
    };
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut check)),
        CopyRemoval::Refused("a worktree matches it now".to_owned())
    );
    assert!(home.join("pinned").join(&digest).is_dir());
    assert!(ready(&home, &digest).is_file());
    let probe = File::options().read(true).open(&lock_path).unwrap();
    assert!(probe.try_lock().is_ok());
    // The probe holds the lock it just took: release it before the next
    // removal, which would otherwise wait on it forever.
    drop(probe);
    // The success path: the check ran, and the copy is gone.
    let mut called = 0;
    let mut check = || {
        called += 1;
        Ok::<(), String>(())
    };
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut check)),
        CopyRemoval::Removed
    );
    assert_eq!(called, 1);
    assert!(
        home.join("pinned")
            .join(&digest)
            .symlink_metadata()
            .is_err()
    );
}

#[test]
fn a_held_copy_is_held_in_both_modes_until_the_last_hold_drops() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    let held = store(&repo, KEY).ensure_copy(&item, &digest).unwrap();
    let mut called = 0;
    let mut check = || {
        called += 1;
        Ok::<(), String>(())
    };
    for dry_run in [false, true] {
        assert_eq!(
            found(Store::remove_copy(&home, &digest, dry_run, &mut check)),
            CopyRemoval::Held
        );
    }
    assert_eq!(called, 0);
    assert!(home.join("pinned").join(&digest).is_dir());
    assert!(ready(&home, &digest).is_file());
    // Two holds coexist: dropping one still holds the copy.
    let also = store(&repo, KEY).ensure_copy(&item, &digest).unwrap();
    drop(held);
    let mut ok = || Ok::<(), String>(());
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut ok)),
        CopyRemoval::Held
    );
    drop(also);
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut ok)),
        CopyRemoval::Removed
    );
    assert!(
        home.join("pinned")
            .join(&digest)
            .symlink_metadata()
            .is_err()
    );
}

#[test]
fn a_name_that_is_not_a_hash_is_refused_before_any_path_is_built() {
    for name in ["abc", &"A".repeat(64), "../x"] {
        let tmp = fakes::TempDir::new("remove-copy-bad-hash");
        let home = tmp.path().join("home");
        let mut ok = || Ok::<(), String>(());
        assert!(
            matches!(
                Store::remove_copy(&home, name, false, &mut ok),
                Err(Error::Pin { .. })
            ),
            "{name}"
        );
        assert!(
            fs::read_dir(&home).is_err(),
            "nothing is created, not even pinned/"
        );
    }
}

#[test]
fn an_absent_copy_is_absent_without_calling_check_or_creating_anything() {
    let missing: String = "0".repeat(64);
    // A fresh home: not even `pinned/` appears.
    let tmp = fakes::TempDir::new("remove-copy-absent-fresh");
    let home = tmp.path().join("home");
    let mut called = 0;
    let mut check = || {
        called += 1;
        Ok::<(), String>(())
    };
    for dry_run in [false, true] {
        assert_eq!(
            found(Store::remove_copy(&home, &missing, dry_run, &mut check)),
            CopyRemoval::Absent
        );
    }
    assert!(fs::read_dir(&home).is_err());
    // With `pinned/` present, only the lock file may appear.
    let tmp = fakes::TempDir::new("remove-copy-absent-pinned");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join("pinned")).unwrap();
    for dry_run in [false, true] {
        assert_eq!(
            found(Store::remove_copy(&home, &missing, dry_run, &mut check)),
            CopyRemoval::Absent
        );
    }
    assert_eq!(called, 0);
    let mut entries: Vec<String> = fs::read_dir(home.join("pinned"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, vec![".lock".to_owned()]);
}

#[test]
fn a_copy_with_no_marker_is_an_unfinished_attempt_and_is_removed() {
    let tmp = fakes::TempDir::new("remove-copy-unfinished");
    let home = tmp.path().join("home");
    let digest: String = "1".repeat(64);
    fs::create_dir_all(home.join("pinned").join(&digest)).unwrap();
    fs::write(home.join("pinned").join(&digest).join("part"), "half").unwrap();
    let mut called = 0;
    let mut check = || {
        called += 1;
        Ok::<(), String>(())
    };
    assert_eq!(
        found(Store::remove_copy(&home, &digest, false, &mut check)),
        CopyRemoval::Removed
    );
    assert_eq!(called, 1);
    assert!(
        home.join("pinned")
            .join(&digest)
            .symlink_metadata()
            .is_err()
    );
}

#[test]
fn a_linked_or_filed_copy_is_removed_without_following_it() {
    let tmp = fakes::TempDir::new("remove-copy-stray");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join("pinned")).unwrap();
    // A link to an outside directory: the link goes, the target stays.
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious.txt"), "precious").unwrap();
    let linked: String = "2".repeat(64);
    symlink(&outside, home.join("pinned").join(&linked)).unwrap();
    fs::write(ready(&home, &linked), "").unwrap();
    // A regular file where the copy goes.
    let filed: String = "3".repeat(64);
    fs::write(home.join("pinned").join(&filed), "stray").unwrap();
    fs::write(ready(&home, &filed), "").unwrap();
    let mut ok = || Ok::<(), String>(());
    assert_eq!(
        found(Store::remove_copy(&home, &linked, false, &mut ok)),
        CopyRemoval::Removed
    );
    assert_eq!(
        found(Store::remove_copy(&home, &filed, false, &mut ok)),
        CopyRemoval::Removed
    );
    assert!(
        home.join("pinned")
            .join(&linked)
            .symlink_metadata()
            .is_err()
    );
    assert!(home.join("pinned").join(&filed).symlink_metadata().is_err());
    assert_eq!(
        fs::read_to_string(outside.join("precious.txt")).unwrap(),
        "precious"
    );
}

#[test]
fn a_linked_or_non_file_marker_is_refused_in_both_modes() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    // Replace the marker with a link to a file the test owns: the link,
    // its target and the copy all stay, and the target's bytes prove it
    // was never opened for the removal.
    let home = repo.home();
    fs::remove_file(ready(&home, &digest)).unwrap();
    let target = repo.elsewhere("owned.txt", "owned");
    symlink(&target, ready(&home, &digest)).unwrap();
    let mut ok = || Ok::<(), String>(());
    for dry_run in [false, true] {
        assert!(
            matches!(
                Store::remove_copy(&home, &digest, dry_run, &mut ok),
                Err(Error::Pin { .. })
            ),
            "dry_run={dry_run}"
        );
    }
    assert!(
        ready(&home, &digest)
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "owned");
    assert!(home.join("pinned").join(&digest).is_dir());
    // A directory where the marker goes is refused the same way.
    fs::remove_file(ready(&home, &digest)).unwrap();
    fs::create_dir(ready(&home, &digest)).unwrap();
    for dry_run in [false, true] {
        assert!(
            matches!(
                Store::remove_copy(&home, &digest, dry_run, &mut ok),
                Err(Error::Pin { .. })
            ),
            "dry_run={dry_run}"
        );
    }
    assert!(ready(&home, &digest).is_dir());
    assert!(home.join("pinned").join(&digest).is_dir());
}

#[test]
fn a_read_only_pinned_dir_is_an_error_naming_the_path() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    // Without the lock file, opening it must create it, which the
    // read-only directory refuses.
    fs::remove_file(home.join("pinned/.lock")).unwrap();
    let _guard = RestoreMode::set(&home.join("pinned"), 0o555);
    let mut ok = || Ok::<(), String>(());
    let err = Store::remove_copy(&home, &digest, false, &mut ok).unwrap_err();
    assert!(err.to_string().contains(".lock"), "{err}");
    assert!(home.join("pinned").join(&digest).is_dir());
}

#[test]
fn kept_copies_lists_each_hash_once_with_sorted_owners() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, "prune-a", &item);
    // The same content approved by a second project, as never: both count
    // as owners, approve and never alike. Recorded before the repository
    // changes below, so the hash is the approved one.
    store(&repo, "prune-b").never(&item, &digest).unwrap();
    // A second hook with an approval but no copy: never listed.
    repo.write("scripts/other.sh", "cargo test\n");
    repo.executable("scripts/other.sh");
    repo.hooks(
        &json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh", "timeout": 30000},
            "other": {"point": "after_tool", "command": "scripts/other.sh"}}),
    );
    let other = repo.item(OfferedKind::Hook, "other");
    let other_hash = hashed(&other);
    store(&repo, "prune-b").never(&other, &other_hash).unwrap();
    // A per-machine record: approving the server keeps its copy and its
    // machine-wide record.
    let server_item = server(&repo);
    let server_hash = hashed(&server_item);
    store(&repo, "prune-a")
        .approve(&server_item, &server_hash)
        .unwrap();
    // Names that are not 64 lowercase hex never count: a short one, an
    // uppercase one, a file under `projects/`, and a linked project entry.
    let approvals_a = repo
        .home()
        .join("projects")
        .join("prune-a")
        .join("approvals");
    fs::write(approvals_a.join("a".repeat(63)), "{}").unwrap();
    fs::write(approvals_a.join("B".repeat(64)), "{}").unwrap();
    fs::write(repo.home().join("projects").join("stray"), "not a project").unwrap();
    symlink(
        repo.home().join("projects").join("prune-a"),
        repo.home().join("projects").join("prune-link"),
    )
    .unwrap();
    let kept = Store::kept_copies(&repo.home()).unwrap();
    let hashes: Vec<&str> = kept.iter().map(|k| k.hash.as_str()).collect();
    assert!(hashes.contains(&digest.as_str()));
    assert!(hashes.contains(&server_hash.as_str()));
    // An approval without its copy is not listed.
    assert!(!hashes.contains(&other_hash.as_str()));
    let hook_entry = kept.iter().find(|k| k.hash == digest).unwrap();
    assert_eq!(
        hook_entry.owners,
        vec!["prune-a".to_owned(), "prune-b".to_owned()]
    );
    assert!(!hook_entry.machine_wide);
    assert_eq!(hook_entry.dir, repo.home().join("pinned").join(&digest));
    let server_entry = kept.iter().find(|k| k.hash == server_hash).unwrap();
    assert!(server_entry.owners.is_empty());
    assert!(server_entry.machine_wide);
    // Sorted by hash.
    let mut sorted = hashes.clone();
    sorted.sort();
    assert_eq!(hashes, sorted);
}

#[test]
fn kept_copies_is_empty_without_records_and_fails_closed_on_unreadable_ones() {
    let tmp = fakes::TempDir::new("kept-copies-empty");
    assert!(
        Store::kept_copies(&tmp.path().join("home"))
            .unwrap()
            .is_empty()
    );
    let repo = Repo::new();
    let item = hook(&repo);
    approved(&repo, KEY, &item);
    let approvals = repo.home().join("projects").join(KEY).join("approvals");
    let _guard = RestoreMode::set(&approvals, 0o000);
    let err = Store::kept_copies(&repo.home()).unwrap_err();
    assert!(err.to_string().contains("approvals"), "{err}");
}

#[test]
fn remove_copy_waits_for_the_pinned_lock_in_both_modes() {
    for dry_run in [false, true] {
        let repo = Repo::new();
        let item = hook(&repo);
        let digest = approved(&repo, KEY, &item);
        let home = repo.home();
        // Hold `pinned/.lock` itself, then start the removal on a worker:
        // its result must not answer while the lock is held.
        let held = File::options()
            .create(true)
            .append(true)
            .open(home.join("pinned/.lock"))
            .unwrap();
        held.lock().unwrap();
        let (_guard, attempt) = LockAttempt::take();
        let (done, finished) = mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut check = || Ok::<(), String>(());
                done.send(Store::remove_copy(&home, &digest, dry_run, &mut check))
                    .unwrap();
            });
            // The worker signals past its own lock check; only then does
            // the held lock prove the wait.
            Deadline::after(WAIT).recv_or_fail(&attempt, "the lock attempt");
            assert!(
                matches!(
                    Deadline::after(HELD).recv(&finished),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ),
                "no removal while the pinned lock is held"
            );
            assert!(home.join("pinned").join(&digest).is_dir());
            drop(held);
            let removal = Deadline::after(WAIT).recv_or_fail(&finished, "the removal");
            assert!(
                matches!(removal, Ok(CopyRemoval::Removed)),
                "dry_run={dry_run}"
            );
        });
        if dry_run {
            assert!(home.join("pinned").join(&digest).is_dir());
        } else {
            assert!(
                home.join("pinned")
                    .join(&digest)
                    .symlink_metadata()
                    .is_err()
            );
        }
    }
}

#[test]
fn check_runs_once_after_the_pinned_lock_is_free() {
    let repo = Repo::new();
    let item = hook(&repo);
    let digest = approved(&repo, KEY, &item);
    let home = repo.home();
    let held = File::options()
        .create(true)
        .append(true)
        .open(home.join("pinned/.lock"))
        .unwrap();
    held.lock().unwrap();
    let (_guard, attempt) = LockAttempt::take();
    let (done, finished) = mpsc::channel();
    let ran = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&ran);
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut check = || {
                counting.fetch_add(1, Ordering::SeqCst);
                Ok::<(), String>(())
            };
            done.send(Store::remove_copy(&home, &digest, false, &mut check))
                .unwrap();
        });
        Deadline::after(WAIT).recv_or_fail(&attempt, "the lock attempt");
        assert!(
            matches!(
                Deadline::after(HELD).recv(&finished),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "no removal while the pinned lock is held"
        );
        // The check has not run while the lock is held.
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        drop(held);
        let removal = Deadline::after(WAIT).recv_or_fail(&finished, "the removal");
        assert!(matches!(removal, Ok(CopyRemoval::Removed)));
    });
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(
        home.join("pinned")
            .join(&digest)
            .symlink_metadata()
            .is_err()
    );
}
