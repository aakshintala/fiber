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

use std::fs;
use std::path::Path;

use common::{
    FIBER, Repos, Setup, approve, declare, dep_dir, dep_names, dep_record, dep_version,
    depends_home, item, manifest, named, store,
};
use extensions::Error;
use fakes::clock::FakeClock;
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
    // A personal install the person asked for, moved to its floor.
    extensions::plan(
        &setup.home(),
        &extensions::Request::Install(helper.into()),
        FIBER,
        &repos.origin(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::UpdateAll(helper.into()),
        FIBER,
        &repos.origin(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
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
        .approve(&found, &digest)
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
        .approve(&found, &digest)
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
        .approve(&found, &digest)
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
            .approve(&found, &digest)
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
        .approve(&found, &digest)
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
