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

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Repos, Setup, manifest, write};
use config::ProjectKey;
use contract::events::OfferedKind;
use extensions::{Error, Fetch, Index, Origin, Store, declared_items, hash};
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::{Value, json};

const FIBER: &str = "0.1.0";

fn project() -> ProjectKey {
    ProjectKey::new("pin").unwrap()
}

fn named(name: &str, depends: &[(&str, &str)]) -> Value {
    let mut m = manifest(name);
    m.as_object_mut().unwrap().insert(
        "depends".into(),
        json!(depends.iter().copied().collect::<BTreeMap<_, _>>()),
    );
    m
}

/// The workspace declares the package at `pkg` with this manifest, listed
/// by git as a repository package is.
fn declare(setup: &Setup, root: &Value) {
    let ws = setup.workspace();
    if !ws.join(".git").exists() {
        git(&ws, &["init", "--quiet"]);
    }
    write(&ws.join("pkg/extension.json"), &root.to_string());
    write(&ws.join("pkg/init.lua"), "-- entry\n");
    write(
        &ws.join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "pkg"}]}).to_string(),
    );
}

fn store(setup: &Setup, repos: &Repos, clock: &Arc<FakeClock>) -> Store {
    Store::new(&setup.home(), &project(), Arc::clone(clock) as Arc<_>).fetching(Fetch::new(
        repos.origin(),
        FIBER,
        Arc::clone(clock) as Arc<_>,
    ))
}

/// The declared extension and its content hash.
fn item(setup: &Setup, clock: &Arc<FakeClock>) -> (extensions::RepoItem, String) {
    let items = declared_items(&setup.workspace(), &**clock).unwrap();
    let found = items
        .into_iter()
        .find(|i| i.kind == OfferedKind::Extension)
        .unwrap();
    let digest = hash(&mut Index::load(&setup.home()), &found).unwrap();
    (found, digest)
}

/// Approves the declared extension with its dependencies fetched from
/// `repos`, and returns its content hash.
fn approve(setup: &Setup, repos: &Repos) -> String {
    let clock = FakeClock::new();
    let store = store(setup, repos, &clock);
    let (found, digest) = item(setup, &clock);
    store.approve(&found, &digest).unwrap();
    digest
}

fn depends_home(setup: &Setup, digest: &str) -> PathBuf {
    setup
        .home()
        .join("pinned")
        .join(digest)
        .join(".fiber-depends")
}

fn dep_dir(setup: &Setup, digest: &str, dep: &str) -> PathBuf {
    depends_home(setup, digest)
        .join("extensions")
        .join(config::dir_name(dep))
}

/// The installed version of `dep` inside the pinned copy: the tag it was
/// fetched at, as its install record says (a package's manifest keeps its
/// own version across tags).
fn dep_version(setup: &Setup, digest: &str, dep: &str) -> String {
    dep_record(setup, digest, dep)
        .get("version")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}

/// The install record of `dep` inside the pinned copy.
fn dep_record(setup: &Setup, digest: &str, dep: &str) -> Value {
    let text = fs::read_to_string(dep_dir(setup, digest, dep).join(".fiber.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The names installed inside the pinned copy, in order; dotfiles are
/// the plan's staging directories, never installs.
fn dep_names(setup: &Setup, digest: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(depends_home(setup, digest).join("extensions"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.retain(|n| !n.starts_with('.'));
    names.sort();
    names
}

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

/// The manifest `name` with `depends` and this install step.
fn installing(name: &str, depends: &[(&str, &str)], step: &str) -> Value {
    let mut m = named(name, depends);
    m.as_object_mut()
        .unwrap()
        .insert("install".into(), json!(["sh", "-c", step]));
    m
}

/// The approval file for `digest` under the test project.
fn approval(setup: &Setup, digest: &str) -> PathBuf {
    setup
        .home()
        .join("projects")
        .join("pin")
        .join("approvals")
        .join(digest)
}

fn ready(setup: &Setup, digest: &str) -> PathBuf {
    setup.home().join("pinned").join(format!("{digest}.ready"))
}

fn copy(setup: &Setup, digest: &str) -> PathBuf {
    setup.home().join("pinned").join(digest)
}

/// A store with dependencies fetched from `repos`, or with no fetching.
fn fetching_store(setup: &Setup, repos: &Repos, clock: &Arc<FakeClock>) -> Store {
    store(setup, repos, clock)
}

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
    let err = fetching_store(&setup, &repos, &clock)
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
    fetching_store(&setup, &repos, &clock)
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
    fetching_store(&setup, &repos, &clock)
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
    fetching_store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(copy(&setup, &digest).is_dir());
    assert!(ready(&setup, &digest).is_file());
    assert!(depends_home(&setup, &digest).join(".complete").is_file());
    // The retry finds the finished copy and only records: the step ran once.
    fs::remove_file(&blocked).unwrap();
    let clock = FakeClock::new();
    fetching_store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap();
    assert!(approval(&setup, &digest).is_file());
    assert_eq!(
        fs::read_to_string(copy(&setup, &digest).join("counter")).unwrap(),
        "x\n"
    );
}

/// The content hash of the declared extension, without approving.
fn digest_of(setup: &Setup) -> String {
    let clock = FakeClock::new();
    item(setup, &clock).1
}

#[test]
fn a_completion_failure_leaves_no_markers_no_copy_and_no_approval() {
    for (step, what) in [
        // The step removes the dependency home: completion has nowhere to go.
        ("rm -rf .fiber-depends", "missing"),
        // The step leaves a file where the home goes: not a directory.
        ("rm -rf .fiber-depends && touch .fiber-depends", "file"),
    ] {
        let setup = Setup::new();
        let mut repos = Repos::new(&setup);
        let helper = "example.com/acme/helper";
        repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
        let root = "fiber.test/root";
        declare(&setup, &installing(root, &[(helper, "1.0")], step));
        let clock = FakeClock::new();
        let (found, digest) = item(&setup, &clock);
        let err = fetching_store(&setup, &repos, &clock)
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
    let err = fetching_store(&setup, &repos, &clock)
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

/// Approves a counting root with one dependency, and returns its hash.
fn counting_root(setup: &Setup, repos: &mut Repos) -> String {
    let helper = "example.com/acme/helper";
    repos.tag(helper, "", "v1.0.0", &manifest(helper), &[]);
    let root = "fiber.test/root";
    declare(
        setup,
        &installing(root, &[(helper, "1.0")], "echo x >> counter"),
    );
    approve(setup, repos)
}

fn counter(setup: &Setup, digest: &str) -> String {
    fs::read_to_string(copy(setup, digest).join("counter")).unwrap_or_default()
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
    let held = fetching_store(&setup, &repos, &clock)
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
    let err = fetching_store(&setup, &repos, &clock)
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

/// A file outside Fiber home with sentinel bytes, and its directory.
fn outside(root: &Path) -> (PathBuf, PathBuf) {
    let dir = root.join("outside");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target.txt");
    fs::write(&target, "untouched\n").unwrap();
    (dir, target)
}

fn outside_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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
    let err = fetching_store(&setup, &repos, &clock)
        .approve(&found, &digest)
        .unwrap_err();
    assert!(matches!(err, Error::Pin { .. }), "{err}");
    assert!(err.to_string().contains(".fiber-depends"), "{err}");
    assert!(!err.to_string().contains("untouched"), "{err}");
    // The link itself goes with the unfinished copy; the target is
    // byte-identical and holds no copied files.
    assert!(!copy(&setup, &digest).exists());
    assert!(!ready(&setup, &digest).exists());
    assert!(!approval(&setup, &digest).exists());
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched\n");
    assert_eq!(outside_names(&outside_dir), ["target.txt"]);
}
