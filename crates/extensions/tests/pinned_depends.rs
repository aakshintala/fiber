//! Approving a repository extension installs its manifest `depends` inside
//! the pinned copy (`docs/extensions.md`, "Pinning"): the lowest tags that
//! meet every minimum, resolved with no personal install in scope, committed
//! before the root's install step runs. No network: every dependency comes
//! from a local tagged repository.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Mutex, atomic::Ordering, mpsc};
use std::time::Duration;

use common::{
    FIBER, GIT_PROGRESS, Repos, STALL_SLICES, Setup, StoreOperations, approve, decide_on_worker,
    declare, dep_dir, dep_names, dep_record, dep_version, depends_home, item, manifest, named,
    on_worker_within, project, store, unapproved_on_worker, write,
};
use contract::events::OfferDecision;
use contract::repository::{Decided, RepositoryCode};
use extensions::{Error, Fetch, Origin, SessionOffer, Store};
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::{Value, json};

#[test]
fn a_dependency_installs_at_its_lowest_meeting_version_not_the_newest() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    for tag in ["v1.1.0", "v1.2.0", "v1.4.0"] {
        repos.tag(helper, "", tag, &manifest(helper), &[]);
    }
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.2")]));
    let digest = approve(&setup, &repos);
    // Exactly 1.2: neither the newest nor below the minimum.
    assert_eq!(dep_version(&setup, &digest, helper), "v1.2.0");
    let record = dep_record(&setup, &digest, helper);
    assert_eq!(record.get("requested"), Some(&Value::Bool(false)));
    assert!(record.get("floor").is_none(), "{record}");
    assert_eq!(record.get("version"), Some(&Value::String("v1.2.0".into())));
}

#[test]
fn two_direct_dependencies_and_a_transitive_third_all_install() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let (base, mid, other) = (
        "example.com/acme/base",
        "example.com/acme/mid",
        "example.com/acme/other",
    );
    repos.tag(base, "", "v1.0.0", &manifest(base), &[]);
    repos.tag(mid, "", "v1.0.0", &named(mid, &[(base, "1.0")]), &[]);
    repos.tag(other, "", "v1.0.0", &manifest(other), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(mid, "1.0"), (other, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(
        dep_names(&setup, &digest),
        vec![
            config::dir_name(base),
            config::dir_name(mid),
            config::dir_name(other),
        ]
    );
    for dep in [base, mid, other] {
        assert_eq!(
            dep_record(&setup, &digest, dep).get("requested"),
            Some(&Value::Bool(false))
        );
    }
}

#[test]
fn the_root_has_no_record_and_its_install_step_runs_once_at_the_pinned_path() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(
        helper,
        "",
        "v1.0.0",
        &manifest(helper),
        &[("built.txt", "built\n")],
    );
    let root = "fiber.test/root";
    let mut root_manifest = named(root, &[(helper, "1.0")]);
    root_manifest.as_object_mut().unwrap().insert(
        "install".into(),
        json!(["sh", "-c", "pwd > where.txt; echo ran >> runs.txt"]),
    );
    declare(&setup, &root_manifest);
    let digest = approve(&setup, &repos);
    let copy = setup.home().join("pinned").join(&digest);
    // The step ran at the copy's own path, exactly once.
    assert_eq!(
        fs::read_to_string(copy.join("where.txt")).unwrap().trim(),
        fs::canonicalize(&copy).unwrap().to_string_lossy()
    );
    assert_eq!(fs::read_to_string(copy.join("runs.txt")).unwrap(), "ran\n");
    // The root itself is the copy, not an install: no record beside it.
    assert!(!copy.join(".fiber.json").exists());
    assert!(
        !depends_home(&setup, &digest)
            .join("extensions")
            .join(config::dir_name(root))
            .exists()
    );
    // What the dependency's own install step built is there too: the commit
    // ran before the root's step.
    assert!(dep_dir(&setup, &digest, helper).join("built.txt").is_file());
}

#[test]
fn an_installed_personal_version_and_floor_neither_constrain_nor_change() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v2.0.0", &manifest(helper), &[]);
    // A personal install the person asked for, moved to its floor. Each
    // runs on a worker under one deadline: planning borrows its clock,
    // so the clock, home and origin are built inside the worker.
    personal_commit(&setup, &repos, extensions::Request::Install(helper.into()));
    personal_commit(
        &setup,
        &repos,
        extensions::Request::UpdateAll(helper.into()),
    );
    for tag in ["v1.1.0", "v1.2.0", "v1.4.0"] {
        repos.tag(helper, "", tag, &manifest(helper), &[]);
    }
    let before = snapshot(&setup.home().join("extensions"));
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.2")]));
    let digest = approve(&setup, &repos);
    // The pinned copy takes 1.2 despite the personal 2.0.0 and its floor.
    assert_eq!(dep_version(&setup, &digest, helper), "v1.2.0");
    assert_eq!(snapshot(&setup.home().join("extensions")), before);
}

/// Every file under `dir`, as path and bytes, in order.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(top) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(&top).unwrap().map(Result::unwrap).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((
                    path.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

/// A personal plan and commit on a worker under one deadline: the plan
/// borrows its clock, so the clock, home and origin are built inside the
/// worker and only the commit result crosses back.
fn personal_commit(setup: &Setup, repos: &Repos, request: extensions::Request) {
    let home = setup.home();
    let origin = repos.origin();
    let (done, result) = std::sync::mpsc::channel();
    let deadline = fakes::Deadline::after(std::time::Duration::from_secs(10));
    let worker = std::thread::spawn(move || {
        let clock = FakeClock::new();
        let planned = extensions::plan(&home, &request, FIBER, &origin, &*clock).unwrap();
        done.send(planned.commit()).unwrap();
    });
    deadline
        .recv_or_fail(&result, "the personal install")
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn two_roots_choosing_apart_get_distinct_copies_and_records() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    for tag in ["v1.2.0", "v1.4.0"] {
        repos.tag(helper, "", tag, &manifest(helper), &[]);
    }
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.2")]));
    let first = approve(&setup, &repos);
    assert_eq!(dep_version(&setup, &first, helper), "v1.2.0");
    declare(&setup, &named(root, &[(helper, "1.4")]));
    let second = approve(&setup, &repos);
    assert_ne!(first, second);
    assert_eq!(dep_version(&setup, &second, helper), "v1.4.0");
    assert_eq!(dep_version(&setup, &first, helper), "v1.2.0");
}

#[test]
fn a_diamond_settles_on_the_combined_minimum() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    for tag in ["v1.2.0", "v1.4.0"] {
        repos.tag(helper, "", tag, &manifest(helper), &[]);
    }
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(a, "", "v1.0.0", &named(a, &[(helper, "1.2")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(helper, "1.4")]), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(a, "1.0"), (b, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(dep_version(&setup, &digest, helper), "v1.4.0");
}

#[test]
fn two_majors_stop_naming_both_requirers() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    for tag in ["v1.2.0", "v2.0.0"] {
        repos.tag(helper, "", tag, &manifest(helper), &[]);
    }
    let (a, b) = ("example.com/acme/a", "example.com/acme/b");
    repos.tag(a, "", "v1.0.0", &named(a, &[(helper, "1.2")]), &[]);
    repos.tag(b, "", "v1.0.0", &named(b, &[(helper, "2.0")]), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(a, "1.0"), (b, "1.0")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve_on_worker(&found, &digest)
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains(a) && text.contains(b), "{text}");
    assert!(matches!(err, Error::MajorConflict { .. }), "{text}");
    assert!(!setup.home().join("pinned").join(&digest).exists());
}

#[test]
fn a_missing_repository_is_not_found_and_installs_nothing() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[("example.com/acme/missing", "1.0")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve_on_worker(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::NoRepository { .. }), "{err}");
    assert!(!setup.home().join("pinned").join(&digest).exists());
}

#[test]
fn no_tag_meeting_the_minimum_is_no_version_and_installs_nothing() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.1.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.2")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve_on_worker(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::NoVersion { .. }), "{err}");
    assert!(!setup.home().join("pinned").join(&digest).exists());
}

#[test]
fn a_dependency_needing_a_newer_fiber_or_another_api_is_refused() {
    for (field, value, check) in [
        ("fiber", json!("99.0.0"), "or later"),
        ("api", json!(999), "API"),
    ] {
        let setup = Setup::new();
        let mut repos = Repos::new(&setup);
        let helper = "example.com/acme/helper";
        let mut bad = manifest(helper);
        bad[field] = value;
        repos.tag(helper, "", "v1.0.0", &bad, &[]);
        let root = "fiber.test/root";
        declare(&setup, &named(root, &[(helper, "1.0")]));
        let clock = FakeClock::new();
        let (found, digest) = item(&setup, &clock);
        let err = store(&setup, &repos, &clock)
            .approve_on_worker(&found, &digest)
            .unwrap_err();
        assert!(err.to_string().contains(check), "{field}: {err}");
        assert!(!setup.home().join("pinned").join(&digest).exists());
    }
}

#[test]
fn two_dependencies_with_one_directory_are_refused_naming_both() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    // One repository, two directories whose names slug to the same
    // directory: `p-q` and `p/q`.
    let (one, two) = ("example.com/acme/repo/p-q", "example.com/acme/repo/p/q");
    repos.tag("example.com/acme/repo", "p-q", "", &manifest(one), &[]);
    repos.tag(
        "example.com/acme/repo",
        "p/q",
        "v1.0.0",
        &manifest(two),
        &[],
    );
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(one, "1.0"), (two, "1.0")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve_on_worker(&found, &digest)
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains(one) && text.contains(two), "{text}");
    assert!(matches!(err, Error::SlugTaken { .. }), "{text}");
    assert!(!setup.home().join("pinned").join(&digest).exists());
}

#[test]
fn a_replaced_manifests_obsolete_dependency_is_pruned_before_deferred_errors() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (z, zz, d) = (n("z"), n("zz"), n("d"));
    for tag in ["v1.0.0", "v2.0.0"] {
        repos.tag(&d, "", tag, &manifest(&d), &[]);
    }
    // `z` 1.0 needs `d` 1.x; `z` 1.5 needs `d` 2.x.
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&d, "1.0")]), &[]);
    repos.tag(&z, "", "v1.5.0", &named(&z, &[(&d, "2.0")]), &[]);
    // `zz` forces the replacement up to `z` 1.5.
    repos.tag(
        &zz,
        "",
        "v1.0.0",
        &named(&zz, &[(&z, "1.5"), (&d, "2.0")]),
        &[],
    );
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(&z, "1.0"), (&zz, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(dep_version(&setup, &digest, &z), "v1.5.0");
    assert_eq!(dep_version(&setup, &digest, &d), "v2.0.0");
    // The `d` 1.0 fetched for the replaced manifest is pruned: only the
    // settled three remain.
    assert_eq!(
        dep_names(&setup, &digest),
        vec![
            config::dir_name(&d),
            config::dir_name(&z),
            config::dir_name(&zz),
        ]
    );
}

/// How long a held operation may stay silent before the stall bound fails
/// it: far below the full operation bound, so firing proves the stall
/// bound (not a total wait) ended the test.
const STALL: Duration = Duration::from_millis(500);
#[test]
fn a_stalled_approval_fails_within_the_stall_bound() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.0")]));
    // Hold the approval at its first git call: the url closure parks on a
    // gate the test owns, so no git work starts and only the stall bound
    // can end the wait. Later calls pass through: an approval resolves one
    // URL per fetch. The repository path repeats `Repos::tag`: it has no
    // accessor, and a drift fails loudly (the fetch errors instead of
    // stalling, and the expected panic never arrives).
    let (release, gate) = mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let root_path = setup.root();
    let origin = Origin::new("git", move |repo: &str| {
        if let Some(gate) = gate.lock().unwrap().take() {
            gate.recv().unwrap();
        }
        format!(
            "file://{}",
            root_path
                .join("repos")
                .join(repo.replace('/', "-"))
                .display()
        )
    });
    let clock = FakeClock::new();
    let store = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>)
        .fetching(Fetch::new(origin, FIBER, Arc::clone(&clock) as Arc<_>));
    let (found, digest) = item(&setup, &clock);
    // The stall bound fires on the worker thread: run it there and receive
    // the panic under the outer deadline.
    let (failed, panicked) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            on_worker_within(STALL, "the approval", move || {
                store.approve(&found, &digest)
            })
        }));
        failed.send(outcome).unwrap();
    });
    let outcome =
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&panicked, "the stalled approval");
    let message = outcome.expect_err("a stalled approval fails");
    let text = message
        .downcast_ref::<String>()
        .expect("the stall panic carries its message");
    assert!(text.contains("the approval"), "unexpected panic: {text}");
    release.send(()).unwrap();
}

/// The stall bound of the re-arm tests. It only ever fires on a failure,
/// so a wide one costs a passing run nothing: it tolerates a test thread,
/// a worker or a `git clone` stalled for several seconds on a loaded host,
/// while a genuinely stuck worker still fails within it.
const REARM_STALL: Duration = Duration::from_secs(5);
/// Slices each held fetch stays silent: 4 slices are 0.8 s, a sixth of the
/// bound, so one hold plus seconds of descheduling still stays inside it.
const HOLD_SLICES: u64 = 4;
/// Rounds the synthetic worker parks: `PACES` holds of `HOLD_SLICES` span
/// more slices than the bound allows silent ones, so only re-arming lets
/// the run finish.
const PACES: u64 = 8;
/// How long one observation poll waits: short, so polls never near a stall.
const POLL: Duration = Duration::from_millis(10);
/// Observation polls before the test gives up, so a worker that never
/// progresses fails instead of hanging: 3000 polls are at least 30 s, far
/// beyond any slow `git clone`, far below the nextest kill.
const POLLS: u32 = 3000;

/// Holds the work for `HOLD_SLICES` whole stall-bound slices: waits, by
/// short receives that must time out, until `on_worker_within` has passed
/// that many slices without an answer. The test bumps nothing, so the hold
/// is silence the bound counts.
fn hold_slices<T>(returned: &mpsc::Receiver<T>) {
    let start = STALL_SLICES.load(Ordering::Relaxed);
    for _ in 0..POLLS {
        match fakes::Deadline::after(POLL).recv(returned) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the approval failed while held")
            }
            Ok(_) => panic!("the held work answered before it was released"),
        }
        if STALL_SLICES.load(Ordering::Relaxed) >= start + HOLD_SLICES {
            return;
        }
    }
    panic!("the stall bound never passed {HOLD_SLICES} slices");
}

/// The run waited through more slices than the bound allows silent ones
/// and still answered: progress alone kept it alive.
fn assert_spanned_bound() {
    let slices = u128::from(STALL_SLICES.load(Ordering::Relaxed));
    let allowed = REARM_STALL.as_millis() / common::ROUND.as_millis();
    assert!(
        slices > allowed,
        "the run spanned only {slices} slices, not past the bound's {allowed}"
    );
}

/// Receives while proving the test's own progress: a bump before every
/// short poll, so a worker descheduled after a release is never mistaken
/// for a stall. Only `hold_slices` stays silent.
fn recv_bumping<T>(rx: &mpsc::Receiver<T>, waited_for: &str) -> T {
    for _ in 0..POLLS {
        GIT_PROGRESS.fetch_add(1, Ordering::Relaxed);
        match fakes::Deadline::after(POLL).recv(rx) {
            Ok(value) => return value,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the sender for {waited_for} hung up")
            }
        }
    }
    panic!("never received {waited_for}");
}

#[test]
fn git_progress_re_arms_the_stall_bound() {
    let (release, gate) = mpsc::channel::<()>();
    let (parked_tx, parked) = mpsc::channel::<()>();
    let (finished, returned) = mpsc::channel();
    std::thread::spawn(move || {
        // Simulated slow git work: every round proves progress, then parks
        // on a gate the test owns. No single silence reaches the stall
        // bound, but the rounds span past it in total: without re-arming,
        // the bound would fire.
        let value = on_worker_within(REARM_STALL, "the approval", move || {
            for _ in 0..PACES {
                GIT_PROGRESS.fetch_add(1, Ordering::Relaxed);
                parked_tx.send(()).unwrap();
                gate.recv().unwrap();
            }
            42
        });
        finished.send(value).unwrap();
    });
    for _ in 0..PACES {
        recv_bumping(&parked, "the paced work to park");
        hold_slices(&returned);
        release.send(()).unwrap();
    }
    let value = recv_bumping(&returned, "the paced answer");
    assert_eq!(value, 42);
    assert_spanned_bound();
}

#[test]
fn git_progress_from_a_real_approval_re_arms_the_stall_bound() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (z, zz, d) = (n("z"), n("zz"), n("d"));
    for tag in ["v1.0.0", "v2.0.0"] {
        repos.tag(&d, "", tag, &manifest(&d), &[]);
    }
    // `z` 1.0 needs `d` 1.x; `z` 1.5 needs `d` 2.x.
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&d, "1.0")]), &[]);
    repos.tag(&z, "", "v1.5.0", &named(&z, &[(&d, "2.0")]), &[]);
    // `zz` forces the replacement up to `z` 1.5.
    repos.tag(
        &zz,
        "",
        "v1.0.0",
        &named(&zz, &[(&z, "1.5"), (&d, "2.0")]),
        &[],
    );
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(&z, "1.0"), (&zz, "1.0")]));
    // Every fetch proves progress, then parks on a gate the test owns, so
    // the approval spans past the bound with no silence ever reaching it.
    // The repository path repeats `Repos::tag` as in the stall test above.
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let (parked_tx, parked) = mpsc::channel::<()>();
    let parked_tx = Mutex::new(parked_tx);
    let root_path = setup.root();
    let origin = Origin::new("git", move |repo: &str| {
        GIT_PROGRESS.fetch_add(1, Ordering::Relaxed);
        parked_tx.lock().unwrap().send(()).unwrap();
        gate.lock().unwrap().recv().unwrap();
        format!(
            "file://{}",
            root_path
                .join("repos")
                .join(repo.replace('/', "-"))
                .display()
        )
    });
    let clock = FakeClock::new();
    let store = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>)
        .fetching(Fetch::new(origin, FIBER, Arc::clone(&clock) as Arc<_>));
    let (found, digest) = item(&setup, &clock);
    let (finished, returned) = mpsc::channel();
    std::thread::spawn(move || {
        finished
            .send(on_worker_within(REARM_STALL, "the approval", move || {
                store.approve(&found, &digest)
            }))
            .unwrap();
    });
    // Hold every fetch for a slice, then release it, until the approval
    // answers. An answer while a fetch is held, or a hangup, fails inside
    // `hold_slices`.
    let mut value = None;
    for _ in 0..POLLS {
        // The store's git work between fetches is live: the test proves
        // progress on every poll, outside the holds.
        GIT_PROGRESS.fetch_add(1, Ordering::Relaxed);
        // A hangup here means the store dropped its origin when the
        // approval ended: no more fetches, so the answer is next.
        if let Ok(()) = fakes::Deadline::after(POLL).recv(&parked) {
            hold_slices(&returned);
            release.send(()).unwrap();
            continue;
        }
        match fakes::Deadline::after(POLL).recv(&returned) {
            Ok(returned) => {
                value = Some(returned);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the worker for the approval hung up before sending")
            }
        }
    }
    value
        .expect("the held approval answers")
        .expect("the held approval succeeds");
    assert_spanned_bound();
}

#[test]
fn an_unreachable_fetch_failure_is_not_final() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let n = |s: &str| format!("example.com/acme/{s}");
    let (z, zz, a) = (n("z"), n("zz"), n("a"));
    repos.tag(&z, "", "v1.0.0", &named(&z, &[(&a, "1")]), &[]);
    repos.tag(&z, "", "v1.5.0", &manifest(&z), &[]);
    repos.tag(&zz, "", "v1.0.0", &named(&zz, &[(&z, "1.5")]), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(&z, "1.0"), (&zz, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(dep_version(&setup, &digest, &z), "v1.5.0");
    // `a` was never fetched for the replaced manifest: only the settled
    // two remain.
    assert_eq!(
        dep_names(&setup, &digest),
        vec![config::dir_name(&z), config::dir_name(&zz)]
    );
}

#[test]
fn a_dependency_back_on_the_root_terminates_without_staging_it() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let root = "fiber.test/root";
    let helper = "example.com/acme/helper";
    // The helper needs the root back: resolution still ends, and the root
    // is never staged as its own dependency.
    repos.tag(helper, "", "v1.0.0", &named(helper, &[(root, "1.0")]), &[]);
    declare(&setup, &named(root, &[(helper, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(dep_names(&setup, &digest), vec![config::dir_name(helper)]);
}

/// An offer whose dependencies fetch from `repos`, or from nowhere when
/// `repos` is `None`: anything fetched then fails.
fn offering(setup: &Setup, repos: Option<&Repos>, clock: &Arc<FakeClock>) -> SessionOffer {
    let origin = match repos {
        Some(repos) => repos.origin(),
        None => Origin::new("fiber-no-such-git-program", |name| name.to_owned()),
    };
    SessionOffer::new(
        &setup.home(),
        &project(),
        &setup.workspace(),
        Fetch::new(origin, FIBER, Arc::clone(clock) as Arc<_>),
    )
}

#[test]
fn an_offered_approval_records_and_installs_dependencies() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.2.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.2")]));
    let clock = FakeClock::new();
    let offer = offering(&setup, Some(&repos), &clock);
    let pending = unapproved_on_worker(&offer).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        decide_on_worker(&offer, &pending[0].offered, OfferDecision::Approve).unwrap(),
        Decided::Recorded
    );
    assert_eq!(
        dep_version(&setup, &pending[0].offered.hash, helper),
        "v1.2.0"
    );
}

#[test]
fn never_skip_and_changed_content_install_nothing() {
    let setup = Setup::new();
    let ws = setup.workspace();
    if !ws.join(".git").exists() {
        git(&ws, &["init", "--quiet"]);
    }
    for package in ["never", "skip", "changed"] {
        let name = format!("fiber.test/{package}");
        write(
            &ws.join(format!("{package}/extension.json")),
            &named(&name, &[("example.com/acme/helper", "1.0")]).to_string(),
        );
        write(&ws.join(format!("{package}/init.lua")), "-- entry\n");
    }
    write(
        &ws.join(".fiber/config.json"),
        &json!({"repository_extensions": [
            {"path": "never"}, {"path": "skip"}, {"path": "changed"},
        ]})
        .to_string(),
    );
    let clock = FakeClock::new();
    // Nothing may fetch: every decision below must succeed without it.
    let offer = offering(&setup, None, &clock);
    let pending = unapproved_on_worker(&offer).unwrap();
    assert_eq!(pending.len(), 3);
    let offered = |package: &str| {
        pending
            .iter()
            .find(|u| u.offered.name == format!("fiber.test/{package}"))
            .unwrap()
            .offered
            .clone()
    };
    assert_eq!(
        offer
            .decide(&offered("never"), OfferDecision::Never)
            .unwrap(),
        Decided::Recorded
    );
    assert_eq!(
        offer.decide(&offered("skip"), OfferDecision::Skip).unwrap(),
        Decided::Obsolete
    );
    // Changed since the offer: obsolete, and nothing installs.
    write(&ws.join("changed/init.lua"), "-- changed\n");
    assert_eq!(
        decide_on_worker(
            &offering(&setup, None, &clock),
            &offered("changed"),
            OfferDecision::Approve
        )
        .unwrap(),
        Decided::Obsolete
    );
    for package in ["never", "skip", "changed"] {
        assert!(
            !setup
                .home()
                .join("pinned")
                .join(&offered(package).hash)
                .exists(),
            "{package}"
        );
    }
}

#[test]
fn a_compatible_back_edge_minimum_on_the_root_approves() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let root = "fiber.test/root";
    let helper = "example.com/acme/helper";
    // The helper needs the root itself at a minimum the root manifest's
    // version meets: the cycle approves without staging the root.
    repos.tag(helper, "", "v1.0.0", &named(helper, &[(root, "1.0")]), &[]);
    declare(&setup, &named(root, &[(helper, "1.0")]));
    let digest = approve(&setup, &repos);
    assert_eq!(dep_version(&setup, &digest, helper), "v1.0.0");
    assert_eq!(dep_names(&setup, &digest), vec![config::dir_name(helper)]);
}

#[test]
fn an_incompatible_back_edge_minimum_on_the_root_fails_and_commits_nothing() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let root = "fiber.test/root";
    let helper = "example.com/acme/helper";
    // The helper needs the root at a major version above the root
    // manifest's v1.0.0: a version conflict naming both, with nothing
    // committed anywhere.
    repos.tag(helper, "", "v1.0.0", &named(helper, &[(root, "2.0")]), &[]);
    declare(&setup, &named(root, &[(helper, "1.0")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve_on_worker(&found, &digest)
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains(root) && text.contains(helper), "{text}");
    assert!(matches!(err, Error::MajorConflict { .. }), "{text}");
    assert!(!setup.home().join("pinned").join(&digest).exists());
    assert!(!setup.home().join("projects").exists());
}
