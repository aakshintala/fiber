//! Pinning still takes the global lock for finished copies: an
//! integration test cannot see the lock-attempt signal, so this
//! synchronized case lives here, beside the store.

use std::fs::File;
use std::sync::mpsc;
use std::time::Duration;

use config::ProjectKey;
use fakes::Deadline;

use super::declared_tests::Repo;
use super::store::set_lock_attempt;
use super::{Index, Store, hash};

/// One wait for an operation's answer: a hang fails the test instead of
/// hanging it.
const WAIT: Duration = Duration::from_secs(10);

/// The separate short wait proving no answer arrives while the global lock
/// is held.
const HELD: Duration = Duration::from_secs(2);

fn project() -> ProjectKey {
    ProjectKey::new("pin-sync").unwrap()
}

fn store(repo: &Repo) -> Store {
    Store::new(&repo.home(), &project(), fakes::clock::FakeClock::new())
}

#[test]
fn ensure_copy_waits_on_the_global_lock_even_for_a_finished_copy() {
    let repo = Repo::new();
    repo.write("scripts/fmt.sh", "cargo fmt\n");
    repo.hooks(&serde_json::json!({"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}));
    let item = repo.item(contract::events::OfferedKind::Hook, "fmt");
    let digest = hash(&mut Index::load(&repo.home()), &item).unwrap();
    assert_eq!(store(&repo).decision(item.kind, &digest), None);
    // The first approval blocks on the filesystem, so it runs on a worker
    // under one deadline: a stall fails the test instead of hanging it.
    let (done, approved) = mpsc::channel();
    let approver = store(&repo);
    let waiting = item.clone();
    let waiting_digest = digest.clone();
    let worker = std::thread::spawn(move || {
        done.send(approver.approve(&waiting, &waiting_digest))
            .unwrap();
    });
    Deadline::after(WAIT)
        .recv_or_fail(&approved, "the approval")
        .unwrap();
    worker.join().unwrap();
    // Hold `pinned/.lock` itself, then start the copy on a worker: its
    // result must not answer while the lock is held.
    let held = File::options()
        .create(true)
        .append(true)
        .open(repo.home().join("pinned/.lock"))
        .unwrap();
    held.lock().unwrap();
    let (attempted, attempt) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    set_lock_attempt(Some(attempted));
    let worker = store(&repo);
    let waiting = digest.clone();
    std::thread::spawn(move || done.send(worker.ensure_copy(&item, &waiting)));
    // The worker sends its entered-operation signal past its own lock
    // check; only then does the held lock prove the wait.
    Deadline::after(WAIT).recv_or_fail(&attempt, "the lock attempt");
    assert!(
        matches!(
            Deadline::after(HELD).recv(&finished),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "no copy while the global lock is held"
    );
    drop(held);
    let held = Deadline::after(WAIT)
        .recv_or_fail(&finished, "the copy")
        .unwrap();
    set_lock_attempt(None);
    // The hold keeps the marker's exclusive lock refused until it drops.
    let marker = repo.home().join("pinned").join(format!("{digest}.ready"));
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(matches!(
        probe.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(held);
    let probe = File::options().read(true).open(&marker).unwrap();
    assert!(probe.try_lock().is_ok());
}
