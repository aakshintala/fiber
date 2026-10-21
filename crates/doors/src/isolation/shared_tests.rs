//! Tests for the per-worktree lock file: shared holds, exclusive claims
//! and removals, through real `flock` on separate descriptors.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use contract::ErrorCode;
use fakes::Deadline;

use super::{ATTEMPTS, Claim, Removal, claim, exclusive, exclusive_with, share, share_with};

/// One budget for the whole wait: setup, the code under test and the
/// assertions run on one thread under one deadline (`docs/testing.md`,
/// "Waits and timeouts").
const BUDGET: Duration = Duration::from_secs(40);

/// The lock file beside `worktree`.
fn lock_of(worktree: &std::path::Path) -> std::path::PathBuf {
    let name = worktree.file_name().unwrap().to_string_lossy().into_owned();
    worktree.with_file_name(format!("{name}.lock"))
}

/// An open descriptor on the existing file at `path`.
fn open_existing(path: &std::path::Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// `try_lock` (exclusive) on a fresh descriptor must fail while shares are
/// held, and succeed once every hold is dropped.
fn assert_exclusive_blocked_then_free(path: &std::path::Path) {
    assert!(
        matches!(
            open_existing(path).try_lock(),
            Err(TryLockError::WouldBlock)
        ),
        "shares block an exclusive take"
    );
}

#[test]
fn share_creates_the_lock_file_and_holds_it_shared() {
    fakes::within(
        "share_creates_the_lock_file_and_holds_it_shared",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-share");
            let worktree = home.path().join("wt");
            let shared = share(&worktree).unwrap();
            let lock = lock_of(&worktree);
            assert!(lock.is_file(), "the lock file is beside the worktree path");
            // A second share coexists: both hold `LOCK_SH`.
            let second = share(&worktree).unwrap();
            assert_exclusive_blocked_then_free(&lock);
            drop(shared);
            drop(second);
            open_existing(&lock).try_lock().unwrap();
        },
    );
}

#[test]
fn share_names_a_lock_file_it_cannot_open() {
    fakes::within(
        "share_names_a_lock_file_it_cannot_open",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-share-io");
            let file = home.path().join("file");
            std::fs::write(&file, "x").unwrap();
            let worktree = file.join("wt");
            let failure = share(&worktree).unwrap_err();
            assert_eq!(failure.code, ErrorCode::IoFailed);
            let lock = lock_of(&worktree);
            assert_eq!(
                failure.message,
                format!("{}: {}.", lock.display(), std::io::ErrorKind::NotADirectory),
                "the sentence names the lock file and the kind only"
            );
        },
    );
}

#[test]
fn claim_creates_then_downgrades_to_a_share() {
    fakes::within(
        "claim_creates_then_downgrades_to_a_share",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-create");
            let worktree = home.path().join("wt");
            let creating = match claim(&worktree, &|| {}).unwrap() {
                Claim::Create(creating) => creating,
                Claim::Present(_) => panic!("an absent path claims a creation"),
            };
            let lock = lock_of(&worktree);
            // While the creation holds `LOCK_EX`, no share takes hold.
            assert!(
                matches!(
                    open_existing(&lock).try_lock_shared(),
                    Err(TryLockError::WouldBlock)
                ),
                "the creation holds exclusive"
            );
            let shared = creating.downgrade().unwrap();
            // After the downgrade a second share succeeds, and an exclusive
            // take waits.
            let _second = share(&worktree).unwrap();
            assert_exclusive_blocked_then_free(&lock);
            drop(shared);
        },
    );
}

#[test]
fn claim_on_a_present_path_shares_and_makes_missing_parents() {
    fakes::within(
        "claim_on_a_present_path_shares_and_makes_missing_parents",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-present");
            let worktree = home.path().join("wt");
            std::fs::create_dir(&worktree).unwrap();
            let _present = match claim(&worktree, &|| {}).unwrap() {
                Claim::Present(shared) => shared,
                Claim::Create(_) => panic!("a present path shares, never creates"),
            };
            assert_exclusive_blocked_then_free(&lock_of(&worktree));
            // An absent path whose lock-file parent directory is missing
            // creates the parent.
            let nested = home.path().join("nodir").join("wt");
            match claim(&nested, &|| {}).unwrap() {
                Claim::Create(_) => {}
                Claim::Present(_) => panic!("an absent path claims a creation"),
            }
            assert!(lock_of(&nested).is_file(), "the parent was made");
        },
    );
}

#[test]
fn a_loser_that_finds_the_worktree_present_never_adds() {
    fakes::within(
        "a_loser_that_finds_the_worktree_present_never_adds",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-loser");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            let holder = open_existing(&lock);
            holder.lock().unwrap();
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let waiting = worktree.clone();
            let loser = std::thread::spawn(move || {
                let before_wait = || waiting_tx.send(()).unwrap();
                claim(&waiting, &before_wait)
            });
            // The claim is blocked behind the holder's exclusive hold.
            wait.recv_or_fail(&waiting_rx, "the loser waits");
            std::fs::create_dir(&worktree).unwrap();
            drop(holder);
            match loser.join().unwrap() {
                Ok(Claim::Present(_)) => {}
                Ok(Claim::Create(_)) => panic!("the path is present: no creation"),
                Err(failure) => panic!("the loser shares: {failure:?}"),
            }
            // The seam ran exactly once: one wait, then the path check.
            assert!(waiting_rx.try_recv().is_err(), "the wait ran once");
        },
    );
}

#[test]
fn a_loser_that_finds_no_worktree_creates_on_its_second_attempt() {
    fakes::within(
        "a_loser_that_finds_no_worktree_creates_on_its_second_attempt",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-retry");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            let holder = open_existing(&lock);
            holder.lock().unwrap();
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let retrying = worktree.clone();
            let loser = std::thread::spawn(move || {
                let before_wait = || waiting_tx.send(()).unwrap();
                claim(&retrying, &before_wait)
            });
            wait.recv_or_fail(&waiting_rx, "the loser waits");
            // The path stays absent: the second attempt claims it.
            drop(holder);
            match loser.join().unwrap() {
                Ok(Claim::Create(_)) => {}
                Ok(Claim::Present(_)) => panic!("the path is absent: no share"),
                Err(failure) => panic!("the loser creates: {failure:?}"),
            }
            assert!(waiting_rx.try_recv().is_err(), "the wait ran once");
        },
    );
}

#[test]
fn claim_under_a_running_session_fails_after_its_attempts() {
    fakes::within(
        "claim_under_a_running_session_fails_after_its_attempts",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-in-use");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            // A running session's share: the worktree was deleted by
            // hand under it, so the path stays absent.
            let holder = open_existing(&lock);
            holder.lock_shared().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let counting = Arc::clone(&calls);
            let before_wait = move || {
                counting.fetch_add(1, Ordering::SeqCst);
            };
            // No blocking hold is ever taken, so this returns without
            // sleeping.
            let failure = claim(&worktree, &before_wait).unwrap_err();
            assert_eq!(failure.code, ErrorCode::IoFailed);
            assert_eq!(
                failure.message,
                format!(
                    "The worktree {} is in use by another process, so Fiber did not make it again.",
                    worktree.display()
                )
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                4,
                "one wait per attempt: four attempts, not three or five"
            );
            assert_eq!(ATTEMPTS, 4);
            drop(holder);
        },
    );
}

#[test]
fn exclusive_creates_takes_and_yields() {
    fakes::within(
        "exclusive_creates_takes_and_yields",
        Deadline::after(BUDGET),
        || {
            let _wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-exclusive");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            // No lock file: it is created and taken.
            match exclusive(&worktree).unwrap() {
                Removal::Exclusive(_file) => assert!(lock.is_file()),
                Removal::InUse => panic!("nothing holds the lock"),
            }
            // An unheld file is taken, blocking shares meanwhile.
            match exclusive(&worktree).unwrap() {
                Removal::Exclusive(_file) => assert_exclusive_blocked_then_free(&lock),
                Removal::InUse => panic!("nothing holds the lock"),
            }
            // A held share yields.
            let holder = open_existing(&lock);
            holder.lock_shared().unwrap();
            assert!(
                matches!(exclusive(&worktree).unwrap(), Removal::InUse),
                "a held share keeps the worktree"
            );
            drop(holder);
            // A directory at the lock path is an I/O error naming it.
            let other = home.path().join("other");
            let other_lock = lock_of(&other);
            std::fs::create_dir(&other_lock).unwrap();
            match exclusive(&other).unwrap_err() {
                worktree::Error::Io { path, .. } => assert_eq!(path, other_lock),
                worktree::Error::GitMissing
                | worktree::Error::NotARepository { .. }
                | worktree::Error::Exists { .. }
                | worktree::Error::Git { .. } => panic!("expected an I/O error"),
            }
        },
    );
}

#[test]
fn exclusive_never_takes_a_lock_file_it_did_not_open() {
    fakes::within(
        "exclusive_never_takes_a_lock_file_it_did_not_open",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-exclusive-stale");
            let worktree = home.path().join("wt");
            std::fs::create_dir(&worktree).unwrap();
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            let holder = open_existing(&lock);
            holder.lock().unwrap();
            let (opened_tx, opened_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let first = Arc::new(AtomicBool::new(true));
            let removing = worktree.clone();
            let remover = std::thread::spawn(move || {
                let hook = || {
                    opened_tx.send(()).unwrap();
                    if first.swap(false, Ordering::SeqCst) {
                        wait.recv_or_fail(&release_rx, "the test releases the removal");
                    }
                };
                exclusive_with(&removing, &hook)
            });
            // The removal opened the old inode and waits behind it.
            wait.recv_or_fail(&opened_rx, "the removal opens the old file");
            // A newer holder replaces the file while the removal waits.
            std::fs::remove_file(&lock).unwrap();
            std::fs::write(&lock, "").unwrap();
            let newer = open_existing(&lock);
            newer.lock_shared().unwrap();
            drop(holder);
            release_tx.send(()).unwrap();
            // The stale inode is free, but it is not the lock file: the
            // removal reopens, finds the newer share, and yields.
            match remover.join().unwrap() {
                Ok(Removal::InUse) => {}
                Ok(Removal::Exclusive(_)) => {
                    panic!("the stale file is not the lock file")
                }
                Err(error) => panic!("the removal yields: {error:?}"),
            }
            drop(newer);
            assert!(worktree.is_dir(), "the worktree is untouched");
        },
    );
}

#[test]
fn a_share_after_an_unlink_locks_the_new_file() {
    fakes::within(
        "a_share_after_an_unlink_locks_the_new_file",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-share-stale");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            let holder = open_existing(&lock);
            holder.lock().unwrap();
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let sharing = worktree.clone();
            let sharer = std::thread::spawn(move || {
                let before_wait = || waiting_tx.send(()).unwrap();
                share_with(&sharing, &before_wait)
            });
            wait.recv_or_fail(&waiting_rx, "the share waits");
            std::fs::remove_file(&lock).unwrap();
            drop(holder);
            let shared = sharer.join().unwrap().unwrap();
            // The hold is on the file at the lock path, not the unlinked one.
            let held = shared._file.metadata().unwrap();
            let current = std::fs::metadata(&lock).unwrap();
            assert_eq!((held.dev(), held.ino()), (current.dev(), current.ino()));
            assert_exclusive_blocked_then_free(&lock);
        },
    );
}

#[test]
fn a_claim_after_an_unlink_locks_the_new_file() {
    fakes::within(
        "a_claim_after_an_unlink_locks_the_new_file",
        Deadline::after(BUDGET),
        || {
            let wait = fakes::Deadline::after(BUDGET / 2);
            let home = fakes::TempDir::new("doors-claim-stale");
            let worktree = home.path().join("wt");
            let lock = lock_of(&worktree);
            std::fs::write(&lock, "").unwrap();
            let holder = open_existing(&lock);
            holder.lock().unwrap();
            let (waiting_tx, waiting_rx) = mpsc::channel::<()>();
            let claiming = worktree.clone();
            let claimer = std::thread::spawn(move || {
                let before_wait = || waiting_tx.send(()).unwrap();
                claim(&claiming, &before_wait)
            });
            wait.recv_or_fail(&waiting_rx, "the claim waits");
            std::fs::remove_file(&lock).unwrap();
            drop(holder);
            // The path is absent, so the claim creates: on the new file.
            let creating = match claimer.join().unwrap() {
                Ok(Claim::Create(creating)) => creating,
                Ok(Claim::Present(_)) => panic!("the path is absent: no share"),
                Err(failure) => panic!("the claim creates: {failure:?}"),
            };
            let shared = creating.downgrade().unwrap();
            let held = shared._file.metadata().unwrap();
            let current = std::fs::metadata(&lock).unwrap();
            assert_eq!((held.dev(), held.ino()), (current.dev(), current.ino()));
            assert_exclusive_blocked_then_free(&lock);
        },
    );
}
