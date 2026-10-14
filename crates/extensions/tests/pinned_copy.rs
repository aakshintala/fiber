//! A pinned copy's transaction: what a failure leaves behind, the
//! completion marker, legacy rebuilds and home-anchored confinement
//! (`docs/extensions.md`, "Pinning"). Through the public [`Store`] API,
//! with dependencies from local tagged repositories; no network, no loading.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs;
use std::sync::Arc;

use common::{
    FIBER, Repos, Setup, approval, approve, copy, counter, counting_root, declare, dep_version,
    depends_home, digest_of, installing, item, manifest, named, outside, outside_names, project,
    ready, store, write,
};
use extensions::{Error, Fetch, Index, Origin, Store, declared_items, hash};
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::json;

#[test]
fn the_roots_install_step_sees_committed_dependencies() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(
        helper,
        "",
        "v1.0.0",
        &installing(helper, &[], "echo built > built.txt"),
        &[],
    );
    let root = "fiber.test/root";
    let slug = config::dir_name(helper);
    declare(
        &setup,
        &installing(
            root,
            &[(helper, "1.0")],
            &format!("test -f .fiber-depends/extensions/{slug}/built.txt && pwd > where.txt"),
        ),
    );
    let digest = approve(&setup, &repos);
    // The root's step found the dependency's install output: the commit
    // ran before it, at the copy's own path.
    assert_eq!(
        fs::read_to_string(copy(&setup, &digest).join("where.txt"))
            .unwrap()
            .trim(),
        fs::canonicalize(copy(&setup, &digest))
            .unwrap()
            .to_string_lossy()
    );
}

#[test]
fn a_dependency_step_failure_leaves_no_copy_markers_or_approval() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(
        helper,
        "",
        "v1.0.0",
        &installing(helper, &[], "exit 3"),
        &[],
    );
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.0")]));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(err.to_string().contains(helper), "{err}");
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    assert!(
        !depends_home(&setup, &digest).exists(),
        "the copy went with its dependency home"
    );
    assert!(!approval(&setup, &digest).exists());
}

#[test]
fn a_root_step_failure_leaves_no_copy_markers_or_approval() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(&setup, &installing(root, &[(helper, "1.0")], "exit 3"));
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    assert!(!approval(&setup, &digest).exists());
}

#[test]
fn a_failed_rebuild_keeps_the_earlier_approval_but_no_complete_copy() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    let flag = setup.root().join("fail");
    let step = format!(
        "test ! -e '{}' || exit 3",
        flag.display().to_string().replace('\'', "'\\''")
    );
    declare(&setup, &installing(root, &[(helper, "1.0")], &step));
    let digest = approve(&setup, &repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    // A legacy copy without its completion marker rebuilds: break the
    // rebuild, and the approval stays byte-identical with no complete
    // copy for a later caller to rebuild.
    fs::remove_file(depends_home(&setup, &digest).join(".complete")).unwrap();
    fs::write(&flag, "fail").unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    // With the failure gone, the next approval builds the copy again.
    fs::remove_file(&flag).unwrap();
    assert_eq!(approve(&setup, &repos), digest);
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
}

#[test]
fn a_record_failure_keeps_the_finished_copy_and_a_retry_records_without_reinstalling() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(
        &setup,
        &installing(root, &[(helper, "1.0")], "echo x >> counter"),
    );
    // A file where the approvals directory belongs makes recording fail
    // after the copy, with its ready marker, is published.
    let blocked = approval(&setup, &digest_of(&setup))
        .parent()
        .unwrap()
        .to_path_buf();
    fs::create_dir_all(blocked.parent().unwrap()).unwrap();
    fs::write(&blocked, "blocked").unwrap();
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(copy(&setup, &digest).is_dir());
    assert!(ready(&setup, &digest).is_file());
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
    // The retry finds the finished copy and only records: the step ran once.
    fs::remove_file(&blocked).unwrap();
    let clock = FakeClock::new();
    store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap();
    assert!(approval(&setup, &digest).is_file());
    assert_eq!(
        fs::read_to_string(copy(&setup, &digest).join("counter")).unwrap(),
        "x\n"
    );
}

#[test]
fn a_completion_failure_leaves_no_markers_no_copy_and_no_approval() {
    for (step, what) in [
        // The step removes the dependency home: completion has nowhere to go.
        ("rm -rf .fiber-depends", "missing"),
        // The step leaves a file where the home goes: not a directory.
        ("rm -rf .fiber-depends && touch .fiber-depends", "file"),
        // The step removes only the extensions root: the strict re-check
        // after prepare still fails, so no completion is published.
        ("rm -rf .fiber-depends/extensions", "extensions"),
    ] {
        let setup = Setup::new();
        let mut repos = Repos::new(&setup);
        let helper = "example.com/acme/helper";
        repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
        let root = "fiber.test/root";
        declare(&setup, &installing(root, &[(helper, "1.0")], step));
        let clock = FakeClock::new();
        let (found, digest) = item(&setup, &clock);
        let err = store(&setup, &repos, &clock)
            .approve(&found, &digest)
            .unwrap_err();
        assert!(err.to_string().contains(".fiber-depends"), "{what}: {err}");
        assert!(!copy(&setup, &digest).exists(), "{what}");
        assert!(!ready(&setup, &digest).exists(), "{what}");
        assert!(!approval(&setup, &digest).exists(), "{what}");
    }
}

#[test]
fn an_outer_ready_failure_leaves_no_copy_and_no_approval() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    // The step parks a directory where the outer marker goes, so writing
    // it fails after everything else is built.
    declare(
        &setup,
        &installing(
            root,
            &[(helper, "1.0")],
            "mkdir ../\"$(basename \"$PWD\")\".ready",
        ),
    );
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let marker = ready(&setup, &digest);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(
        err.to_string().contains(&marker.display().to_string()),
        "{err}"
    );
    assert!(matches!(err, Error::Io { path, .. } if path == marker));
    assert!(!copy(&setup, &digest).exists());
    assert!(!marker.is_file());
    assert!(!approval(&setup, &digest).exists());
}

#[test]
fn an_extension_without_dependencies_needs_no_fetch_and_makes_no_dependency_home() {
    let setup = Setup::new();
    let root = "fiber.test/root";
    declare(&setup, &manifest(root));
    let clock = FakeClock::new();
    // A missing git program: nothing may fetch.
    let fetch = Fetch::new(
        Origin::new("fiber-no-such-git-program", |r| r.to_owned()),
        FIBER,
        Arc::clone(&clock) as Arc<_>,
    );
    let store = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>).fetching(fetch);
    let (found, digest) = item(&setup, &clock);
    store.approve(&found, &digest).unwrap();
    assert!(copy(&setup, &digest).is_dir());
    assert!(!depends_home(&setup, &digest).exists());
}

#[test]
fn dependencies_without_a_fetch_fail_and_install_nothing() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(&setup, &named(root, &[(helper, "1.0")]));
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    let err = plain.approve(&found, &digest).unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains("dependencies"), "{err}");
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    assert!(!approval(&setup, &digest).exists());
}

#[test]
fn reserved_dependency_paths_are_refused_and_a_similar_prefix_is_allowed() {
    for (rel, allowed) in [
        (".fiber-depends", false),
        (".fiber-depends/inner.txt", false),
        (".fiber-depends2.txt", true),
    ] {
        let setup = Setup::new();
        let root = "fiber.test/root";
        declare(&setup, &manifest(root));
        write(&setup.workspace().join("pkg").join(rel), "reserved\n");
        let clock = FakeClock::new();
        let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
        let (found, digest) = item(&setup, &clock);
        if allowed {
            plain.approve(&found, &digest).unwrap();
            assert!(copy(&setup, &digest).join(rel).is_file());
        } else {
            let err = plain.approve(&found, &digest).unwrap_err();
            assert!(matches!(err, Error::Pin { .. }), "{rel}: {err}");
            assert!(err.to_string().contains(".fiber-depends"), "{rel}: {err}");
            assert!(!copy(&setup, &digest).exists(), "{rel}");
            assert!(!approval(&setup, &digest).exists(), "{rel}");
        }
    }
}

#[test]
fn ready_markers_are_empty_for_an_extension_a_hook_and_a_server() {
    let setup = Setup::new();
    let ws = setup.workspace();
    if !ws.join(".git").exists() {
        git(&ws, &["init", "--quiet"]);
    }
    write(
        &ws.join("pkg/extension.json"),
        &manifest("fiber.test/root").to_string(),
    );
    write(&ws.join("pkg/init.lua"), "-- entry\n");
    write(&ws.join("scripts/fmt.sh"), "cargo fmt\n");
    write(
        &ws.join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "pkg"}]}).to_string(),
    );
    write(
        &ws.join(".fiber/config/hooks.json"),
        &json!({"hooks": {"fmt": {"point": "x", "command": "scripts/fmt.sh"}}}).to_string(),
    );
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let items = declared_items(&setup.workspace(), &*clock).unwrap();
    assert_eq!(items.len(), 2);
    for found in &items {
        let digest = hash(&mut Index::load(&setup.home()), found).unwrap();
        plain.approve(found, &digest).unwrap();
        assert_eq!(fs::read(ready(&setup, &digest)).unwrap(), b"");
    }
}

#[test]
fn a_legacy_copy_without_completion_rebuilds_through_approve() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    assert_eq!(counter(&setup, &digest), "x\n");
    // A copy from before completion markers: the marker is gone, with
    // junk the rebuild must clear.
    fs::remove_file(depends_home(&setup, &digest).join(".complete")).unwrap();
    fs::write(copy(&setup, &digest).join("junk"), "junk").unwrap();
    assert_eq!(approve(&setup, &repos), digest);
    // The rebuild clears the copy, so its counter restarts with one run;
    // the junk is gone and completion is written again.
    assert_eq!(counter(&setup, &digest), "x\n");
    assert!(!copy(&setup, &digest).join("junk").exists());
    assert!(
        depends_home(&setup, &digest).join(".complete").is_file(),
        "the rebuild writes completion"
    );
}

#[test]
fn a_legacy_copy_with_a_directory_completion_marker_is_refused() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    // A non-file where the completion marker goes is refused before it
    // is ever opened, not read as present.
    let marker = depends_home(&setup, &digest).join(".complete");
    fs::remove_file(&marker).unwrap();
    fs::create_dir(&marker).unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".complete"), "{err}");
    assert!(err.to_string().contains("not a regular file"), "{err}");
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert_eq!(counter(&setup, &digest), "x\n");
}

#[test]
fn a_legacy_copy_with_a_linked_completion_marker_is_refused() {
    use std::os::unix::fs::symlink;
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    let (outside_dir, target) = outside(&setup.root());
    // A link where the completion marker goes is refused before it is
    // ever opened, not read through: the target stays untouched.
    let marker = depends_home(&setup, &digest).join(".complete");
    fs::remove_file(&marker).unwrap();
    symlink(&target, &marker).unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".complete"), "{err}");
    assert!(err.to_string().contains("is a symbolic link"), "{err}");
    assert!(!err.to_string().contains("untouched"), "{err}");
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert_eq!(outside_names(&outside_dir), ["target.txt"]);
    assert_eq!(counter(&setup, &digest), "x\n");
}

#[test]
fn a_legacy_copy_without_a_dependency_home_rebuilds() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    // A copy from before dependency homes: no home at all still rebuilds
    // instead of failing.
    fs::remove_dir_all(depends_home(&setup, &digest)).unwrap();
    assert_eq!(approve(&setup, &repos), digest);
    assert_eq!(counter(&setup, &digest), "x\n");
    assert!(
        depends_home(&setup, &digest).join(".complete").is_file(),
        "the rebuild writes completion"
    );
}

#[test]
fn a_legacy_copy_without_completion_rebuilds_through_ensure_copy_keeping_its_approval() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    fs::remove_file(depends_home(&setup, &digest).join(".complete")).unwrap();
    fs::write(copy(&setup, &digest).join("junk"), "junk").unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    // Rebuilding for a load records no approval: the bytes stay identical.
    let held = store(&setup, &repos, &clock)
        .ensure_copy(&found, &digest)
        .unwrap();
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
    assert!(!copy(&setup, &digest).join("junk").exists());
    assert_eq!(counter(&setup, &digest), "x\n");
    drop(held);
}

#[test]
fn a_held_legacy_copy_is_left_alone_then_rebuilds_once_released() {
    use std::fs::File;
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    fs::remove_file(depends_home(&setup, &digest).join(".complete")).unwrap();
    fs::write(copy(&setup, &digest).join("junk"), "junk").unwrap();
    // A session running the copy holds its marker shared.
    let marker = ready(&setup, &digest);
    let session = File::options()
        .read(true)
        .write(true)
        .open(&marker)
        .unwrap();
    session.lock_shared().unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Busy), "{err}");
    // Nothing changed: no byte, no inode, no approval, no completion.
    assert_eq!(
        fs::read_to_string(copy(&setup, &digest).join("junk")).unwrap(),
        "junk"
    );
    assert_eq!(counter(&setup, &digest), "x\n");
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert!(!depends_home(&setup, &digest).join(".complete").exists());
    drop(session);
    assert_eq!(approve(&setup, &repos), digest);
    assert_eq!(counter(&setup, &digest), "x\n");
    assert!(!copy(&setup, &digest).join("junk").exists());
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
}

#[test]
fn a_legacy_copy_without_dependencies_is_kept_without_fetching() {
    let setup = Setup::new();
    let root = "fiber.test/root";
    declare(&setup, &installing(root, &[], "echo x >> counter"));
    let clock = FakeClock::new();
    // No fetching configured at all: a copy without dependencies never
    // needs it, legacy or not.
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    plain.approve(&found, &digest).unwrap();
    fs::write(copy(&setup, &digest).join("junk"), "junk").unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    plain.approve(&found, &digest).unwrap();
    assert_eq!(
        fs::read_to_string(copy(&setup, &digest).join("junk")).unwrap(),
        "junk"
    );
    assert_eq!(counter(&setup, &digest), "x\n");
}

#[test]
fn a_link_where_the_pinned_root_goes_fails_and_writes_nothing_outside() {
    let setup = Setup::new();
    let (outside_dir, target) = outside(&setup.root());
    let pinned = setup.home().join("pinned");
    std::os::unix::fs::symlink(&outside_dir, &pinned).unwrap();
    let root = "fiber.test/root";
    declare(&setup, &manifest(root));
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    let err = plain.approve(&found, &digest).unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains("pinned"), "{err}");
    assert!(err.to_string().contains("is a symbolic link"), "{err}");
    assert_eq!(outside_names(&outside_dir), ["target.txt"]);
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert!(!copy(&setup, &digest).exists());
}

#[test]
fn a_file_where_the_pinned_root_goes_fails_naming_it() {
    let setup = Setup::new();
    let pinned = setup.home().join("pinned");
    fs::create_dir_all(setup.home()).unwrap();
    fs::write(&pinned, "stray\n").unwrap();
    let root = "fiber.test/root";
    declare(&setup, &manifest(root));
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    let err = plain.approve(&found, &digest).unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains("pinned"), "{err}");
    assert!(err.to_string().contains("is not a directory"), "{err}");
    assert_eq!(fs::read_to_string(&pinned).unwrap(), "stray\n");
    assert!(!copy(&setup, &digest).exists());
}

#[test]
fn a_link_where_the_ready_marker_goes_is_refused_before_opening() {
    let setup = Setup::new();
    let (_outside_dir, target) = outside(&setup.root());
    let root = "fiber.test/root";
    declare(&setup, &manifest(root));
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    // A stale marker that is a link out of home: refused, never opened.
    fs::create_dir_all(setup.home().join("pinned")).unwrap();
    std::os::unix::fs::symlink(&target, ready(&setup, &digest)).unwrap();
    let err = plain.approve(&found, &digest).unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".ready"), "{err}");
    assert!(err.to_string().contains("is a symbolic link"), "{err}");
    assert!(!err.to_string().contains("untouched"), "{err}");
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert!(!copy(&setup, &digest).exists());
}

#[test]
fn a_missing_pinned_root_and_a_home_behind_a_link_both_build() {
    let setup = Setup::new();
    assert!(!setup.home().join("pinned").exists());
    let root = "fiber.test/root";
    declare(&setup, &manifest(root));
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, digest) = item(&setup, &clock);
    plain.approve(&found, &digest).unwrap();
    assert!(copy(&setup, &digest).is_dir());
    // The same home reached through a link builds too: the link itself is
    // trusted, everything under it is checked.
    let alias = setup.root().join("alias-home");
    std::os::unix::fs::symlink(setup.home(), &alias).unwrap();
    declare(&setup, &installing(root, &[], "echo y >> counter"));
    let clock = FakeClock::new();
    let aliased = Store::new(&alias, &project(), Arc::clone(&clock) as Arc<_>);
    let (found, second) = item(&setup, &clock);
    aliased.approve(&found, &second).unwrap();
    assert!(copy(&setup, &second).is_dir());
}

#[test]
fn an_install_step_that_links_the_dependency_home_away_leaves_the_target_alone() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let (outside_dir, target) = outside(&setup.root());
    let root = "fiber.test/root";
    declare(
        &setup,
        &installing(
            root,
            &[(helper, "1.0")],
            &format!(
                "rm -rf .fiber-depends && ln -s '{}' .fiber-depends",
                target.display().to_string().replace('\'', "'\\''")
            ),
        ),
    );
    let clock = FakeClock::new();
    let (found, digest) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".fiber-depends"), "{err}");
    assert!(err.to_string().contains("is a symbolic link"), "{err}");
    assert!(!err.to_string().contains("untouched"), "{err}");
    // The link itself goes with the unfinished copy; the target is
    // byte-identical and holds no copied files.
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    assert!(!approval(&setup, &digest).exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert_eq!(outside_names(&outside_dir), ["target.txt"]);
}

#[test]
fn a_finished_copy_with_dependencies_is_reused_without_fetching() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    assert_eq!(counter(&setup, &digest), "x\n");
    // The finished copy needs no fetch: a store without one reuses it,
    // recording the approval without running any install step again.
    let clock = FakeClock::new();
    let plain = Store::new(&setup.home(), &project(), Arc::clone(&clock) as Arc<_>);
    let (found, _) = item(&setup, &clock);
    plain.approve(&found, &digest).unwrap();
    assert_eq!(counter(&setup, &digest), "x\n");
    assert!(approval(&setup, &digest).is_file());
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
    assert_eq!(
        dep_version(&setup, &digest, "example.com/acme/helper"),
        "v1.0.0"
    );
}

#[test]
fn a_legacy_copy_whose_dependency_home_is_a_link_is_refused() {
    use std::os::unix::fs::symlink;
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    let (outside_dir, target) = outside(&setup.root());
    // A legacy dependency home replaced by a link out of home: refused
    // naming the link, with the target untouched.
    let home = depends_home(&setup, &digest);
    fs::remove_file(home.join(".complete")).unwrap();
    fs::remove_dir_all(&home).unwrap();
    symlink(&target, &home).unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".fiber-depends"), "{err}");
    assert!(err.to_string().contains("is a symbolic link"), "{err}");
    assert!(!err.to_string().contains("untouched"), "{err}");
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert_eq!(outside_names(&outside_dir), ["target.txt"]);
    assert!(!home.join(".complete").exists());
}

#[test]
fn an_unreadable_completion_marker_fails_naming_it() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    let digest = counting_root(&setup, &mut repos);
    let before = fs::read(approval(&setup, &digest)).unwrap();
    // A legacy copy whose dependency home lost search permission: reading
    // the completion marker fails naming the marker, with the copy, its
    // marker and the approval intact. (Same user, never root: permission
    // bits only matter outside it.)
    let home = depends_home(&setup, &digest);
    fs::remove_file(home.join(".complete")).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o000)).unwrap();
    let clock = FakeClock::new();
    let (found, _) = item(&setup, &clock);
    let err = store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(err, Error::Io { .. }), "{err}");
    let marker = home.join(".complete");
    assert!(
        matches!(err, Error::Io { ref path, .. } if path == &marker),
        "{err}"
    );
    assert!(copy(&setup, &digest).is_dir());
    assert!(ready(&setup, &digest).is_file());
    assert_eq!(fs::read(approval(&setup, &digest)).unwrap(), before);
}

#[test]
fn fetch_debug_names_the_version_and_skips_the_origin() {
    // The `Debug` names the fetch's version but never the origin's
    // internals: a body replaced with an empty `Ok` (the surviving
    // mutant) prints none of them. The public API suffices: no unit
    // seam is needed.
    let fetch = Fetch::new(
        Origin::new("fiber-test-git", |name| name.to_owned()),
        "0.1.0",
        FakeClock::new(),
    );
    let text = format!("{fetch:?}");
    assert!(text.contains("Fetch"), "{text}");
    assert!(text.contains("0.1.0"), "{text}");
    assert!(!text.contains("fiber-test-git"), "{text}");
}
