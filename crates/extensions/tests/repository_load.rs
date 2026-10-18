//! A session start never waits on another process's approval: the
//! bounded lock try behind it (issue #620, part 2). Through the public
//! [`Store`] API, with a declared, approved hook item.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs::{self, File};
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, project, write};
use contract::clock::Clock;
use contract::events::OfferedKind;
use extensions::{Index, RepoItem, Store, declared_items, hash};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::json;

/// One wait for the copy's answer: a hang fails the test instead of
/// hanging it.
const WAIT: Duration = Duration::from_secs(10);

/// The bounded try's whole wait: fifty tries, ten milliseconds apart, on
/// the store's clock. A changed bound or sleep moves `now()` off exactly
/// half a second and fails the busy test below.
const BUSY_WITHIN: Duration = Duration::from_millis(500);

/// A declared hook item, approved into `setup`'s home, with its content
/// hash. A hook pins no dependencies and runs no install step, so the
/// approval blocks on nothing and runs inline.
fn approved_hook(setup: &Setup, clock: &std::sync::Arc<FakeClock>) -> (Store, RepoItem, String) {
    let workspace = setup.workspace();
    git(&workspace, &["init", "--quiet"]);
    write(&workspace.join("scripts/fmt.sh"), "cargo fmt\n");
    write(
        &workspace.join(".fiber/config/hooks.json"),
        &json!({"hooks": {"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}})
            .to_string(),
    );
    let store = Store::new(&setup.home(), &project(), clock.clone());
    let items = declared_items(&workspace, &**clock).unwrap();
    let item = items
        .into_iter()
        .find(|found| found.kind == OfferedKind::Hook && found.name == "fmt")
        .unwrap();
    let digest = hash(&mut Index::load(&setup.home()), &item).unwrap();
    store.approve(&item, &digest).unwrap();
    (store, item, digest)
}

/// Every byte the approval left in Fiber home: each file under the copy,
/// the marker and the approval file, so the tests prove what a refused or
/// returned copy changed.
fn snapshot(setup: &Setup, digest: &str) -> Vec<(String, Vec<u8>)> {
    let home = setup.home();
    let mut paths = vec![
        home.join("pinned").join(format!("{digest}.ready")),
        home.join("projects")
            .join("pin")
            .join("approvals")
            .join(digest),
    ];
    let mut dirs = vec![home.join("pinned").join(digest)];
    while let Some(dir) = dirs.pop() {
        let mut entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                dirs.push(path);
            } else {
                paths.push(path);
            }
        }
    }
    let mut found: Vec<(String, Vec<u8>)> = paths
        .iter()
        .map(|path| {
            (
                path.strip_prefix(&home)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn try_ensure_copy_fails_busy_after_a_bounded_wait_on_a_held_lock() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (store, item, digest) = approved_hook(&setup, &clock);
    let before = snapshot(&setup, &digest);
    // Hold `pinned/.lock` itself, then start the copy on a worker under
    // one deadline: the bounded try answers `Busy` instead of waiting on
    // another process's approval, which the deadline turns into a failure
    // when the try blocks.
    let held = File::options()
        .create(true)
        .append(true)
        .open(setup.home().join("pinned/.lock"))
        .unwrap();
    held.lock().unwrap();
    let start = clock.now();
    let (done, finished) = mpsc::channel();
    let worker = store.clone();
    let waiting = item.clone();
    let waiting_digest = digest.clone();
    std::thread::spawn(move || {
        done.send(worker.try_ensure_copy(&waiting, &waiting_digest))
            .unwrap()
    });
    let result = Deadline::after(WAIT).recv_or_fail(&finished, "the copy");
    assert!(
        matches!(result, Err(extensions::Error::Busy)),
        "a held lock answers Busy, got {result:?}"
    );
    // Fifty sleeps of ten milliseconds on the store's clock: a changed
    // bound or sleep moves `now()` off exactly half a second.
    assert_eq!(clock.now().duration_since(start), BUSY_WITHIN);
    // A refused copy changes no byte of the copy, its marker or its record.
    assert_eq!(snapshot(&setup, &digest), before);
    drop(held);
}

#[test]
fn try_ensure_copy_holds_a_free_copy_until_the_hold_drops() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let (store, item, digest) = approved_hook(&setup, &clock);
    let hold = store.try_ensure_copy(&item, &digest).unwrap();
    // The hold keeps the marker's exclusive lock refused until it drops.
    let marker = setup.home().join("pinned").join(format!("{digest}.ready"));
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(matches!(
        probe.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(hold);
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_ok());
}
