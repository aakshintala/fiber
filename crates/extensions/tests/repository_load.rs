//! A session start never waits on another process's approval: the
//! bounded lock try behind it (issue #620, part 2). Through the public
//! [`Store`] API, and through the [`RepositorySet`] a session loads:
//! which declared extensions load from their pinned copies, and the
//! notices choosing them raises.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::type_complexity,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{
    FIBER, Repos, Setup, StoreOperations, copy, install, install_lua, installing, manifest, named,
    project, provider, ready, write,
};
use config::{Config, ProjectKey, Secret, Sources, dir_name, store_credential};
use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{LoadedExtension, Notice, OfferedKind};
use extensions::{
    ApprovedExtension, CopyRemoval, Fetch, Index, Providers, RepoItem, RepositorySet,
    SessionExtensions, StateSeed, Store, approved_extension, approved_extensions, declared_items,
    hash, list,
};
use fakes::Deadline;
use fakes::NoLock;
use fakes::clock::FakeClock;
use fakes::git::git;
use serde_json::{Value, json};

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

/// A workspace declaring these repository packages, each `(dir, manifest,
/// files, required)`, with a git repository so the declarations read.
fn declare_pkgs(setup: &Setup, pkgs: &[(&str, Value, &[(&str, &str)], bool)]) {
    let workspace = setup.workspace();
    if !workspace.join(".git").exists() {
        git(&workspace, &["init", "--quiet"]);
    }
    let mut paths = Vec::new();
    for (dir, manifest, files, required) in pkgs {
        let mut entry = json!({"path": dir});
        if *required {
            entry["required"] = json!(true);
        }
        paths.push(entry);
        write(
            &workspace.join(dir).join("extension.json"),
            &manifest.to_string(),
        );
        for (file, text) in *files {
            write(&workspace.join(dir).join(file), text);
        }
    }
    write(
        &workspace.join(".fiber/config.json"),
        &json!({"repository_extensions": paths}).to_string(),
    );
}

/// `fiber.test/<short>`'s package manifest at `version`.
fn package(short: &str, version: &str) -> Value {
    let mut m = manifest(&format!("fiber.test/{short}"));
    m["version"] = json!(version);
    m
}

/// The declared item of `kind` named `name` with its content hash: for
/// approving, nevering and skipping without loading anything.
fn digest_of(
    setup: &Setup,
    clock: &Arc<FakeClock>,
    kind: OfferedKind,
    name: &str,
) -> (RepoItem, String) {
    let items = declared_items(&setup.workspace(), &**clock).unwrap();
    let item = items
        .into_iter()
        .find(|found| found.kind == kind && found.name == name)
        .unwrap();
    let digest = hash(&mut Index::load(&setup.home()), &item).unwrap();
    (item, digest)
}

/// Approves one declared item of `kind` on a worker under one deadline,
/// and returns its content hash.
fn approve_kind(
    setup: &Setup,
    repos: &Repos,
    clock: &Arc<FakeClock>,
    kind: OfferedKind,
    name: &str,
) -> String {
    let (item, digest) = digest_of(setup, clock, kind, name);
    common::store(setup, repos, clock)
        .approve_on_worker(&item, &digest)
        .unwrap();
    digest
}

/// The set the session loads: every declared extension this project
/// approved, at its current content.
fn repository_set(
    setup: &Setup,
    repos: &Repos,
    clock: &Arc<FakeClock>,
    skipped: &BTreeSet<(String, String)>,
) -> RepositorySet {
    RepositorySet::load(
        &setup.home(),
        &project(),
        &setup.workspace(),
        skipped,
        Fetch::new(repos.origin(), FIBER, clock.clone()),
    )
}

/// The effective configuration for `setup`, with these `-c` overrides.
fn session_config(setup: &Setup, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: project(),
        overrides: overrides.iter().map(|arg| (*arg).to_owned()).collect(),
    })
    .unwrap()
}

/// Loads `set` the way a session start does: on a worker under one
/// deadline, so a hang fails the test instead of hanging it.
fn load_session(setup: &Setup, set: RepositorySet, overrides: &[&str]) -> SessionExtensions {
    let config = session_config(setup, overrides);
    let home = setup.home();
    let (done, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        done.send(SessionExtensions::load_repository(
            &home,
            &config,
            FakeClock::new(),
            locks,
            None,
            StateSeed::new(),
            set,
        ))
        .unwrap();
    });
    Deadline::after(WAIT).recv_or_fail(&rx, "the session load")
}

/// Reads the session's providers from `set`, the way a session start
/// does: no Lua runs, so this runs inline.
fn load_providers(
    setup: &Setup,
    set: &RepositorySet,
    overrides: &[&str],
) -> (Providers, Vec<Notice>) {
    let config = session_config(setup, overrides);
    Providers::load_repository(&setup.home(), &config, set).unwrap()
}

/// The canonical workspace: what `pinned.json` records paths under.
fn canonical_workspace(setup: &Setup) -> PathBuf {
    setup.workspace().canonicalize().unwrap()
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

#[test]
fn set_loads_an_approved_package_from_its_pinned_copy() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(&setup, &[("lint", package("lint", "v2.0.0"), &[], false)]);
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        session.loaded(),
        vec![LoadedExtension {
            name: "fiber.test/lint".to_owned(),
            version: "v2.0.0".to_owned(),
            replaces: Vec::new(),
        }]
    );
    assert_eq!(
        session.dirs(),
        vec![("fiber.test/lint".to_owned(), copy(&setup, &digest))]
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn set_leaves_out_what_this_content_does_not_approve() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[
            ("a", package("a", "v1.0.0"), &[], false),
            ("b", package("b", "v1.0.0"), &[], false),
            ("c", package("c", "v1.0.0"), &[], false),
        ],
    );
    write(&setup.workspace().join("scripts/fmt.sh"), "cargo fmt\n");
    write(
        &setup.workspace().join(".fiber/config/hooks.json"),
        &json!({"hooks": {"fmt": {"point": "after_tool", "command": "scripts/fmt.sh"}}})
            .to_string(),
    );
    let clock = FakeClock::new();
    // Unapproved, never'd and skipped content never starts, and neither
    // does an approved hook: the extension loaders load extensions.
    let (b_item, b_digest) = digest_of(&setup, &clock, OfferedKind::Extension, "fiber.test/b");
    common::store(&setup, &repos, &clock)
        .never(&b_item, &b_digest)
        .unwrap();
    let c_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/c",
    );
    approve_kind(&setup, &repos, &clock, OfferedKind::Hook, "fmt");
    let mut skipped = BTreeSet::new();
    skipped.insert(("fiber.test/c".to_owned(), c_digest));
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &skipped),
        &[],
    );
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn approvals_do_not_cross_projects() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(&setup, &[("lint", package("lint", "v2.0.0"), &[], false)]);
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    let other = ProjectKey::new("other").unwrap();
    let set = RepositorySet::load(
        &setup.home(),
        &other,
        &setup.workspace(),
        &BTreeSet::new(),
        Fetch::new(repos.origin(), FIBER, clock.clone()),
    );
    let session = load_session(&setup, set, &[]);
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn a_workspace_declaring_nothing_touches_nothing() {
    let setup = Setup::new();
    git(&setup.workspace(), &["init", "--quiet"]);
    write(&setup.workspace().join(".fiber/config.json"), "{}");
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty(), "{:?}", session.notices());
    assert!(!setup.home().join("pinned.json").exists());
    assert!(!setup.home().join("pinned").exists());
}

#[test]
fn pinned_json_records_each_declared_path_after_load_and_listing() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[(
            "lint",
            package("lint", "v2.0.0"),
            &[
                ("init.lua", "-- nothing\n"),
                ("providers/acme.json", "{}\n"),
            ],
            false,
        )],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    drop(set);
    let workspace = canonical_workspace(&setup);
    let pinned: Value =
        serde_json::from_slice(&fs::read(setup.home().join("pinned.json")).unwrap()).unwrap();
    for rel in [
        "lint/extension.json",
        "lint/init.lua",
        "lint/providers/acme.json",
    ] {
        let key = workspace.join(rel).to_string_lossy().into_owned();
        assert!(pinned.get(&key).is_some(), "no {key} in {pinned}");
    }
    // The listing hashes the same way, and saves the index it used.
    let listed =
        approved_extensions(&setup.home(), &project(), &setup.workspace(), &*clock).unwrap();
    assert_eq!(
        listed,
        vec![ApprovedExtension {
            name: "fiber.test/lint".to_owned(),
            version: "v2.0.0".to_owned(),
            path: "lint".to_owned(),
            hash: digest,
        }]
    );
    let again: Value =
        serde_json::from_slice(&fs::read(setup.home().join("pinned.json")).unwrap()).unwrap();
    assert_eq!(pinned, again);
}

#[test]
fn a_missing_workspace_path_gives_an_empty_set_and_no_notice() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    let set = RepositorySet::load(
        &setup.home(),
        &project(),
        &setup.workspace().join("missing"),
        &BTreeSet::new(),
        Fetch::new(repos.origin(), FIBER, clock.clone()),
    );
    let session = load_session(&setup, set, &[]);
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn a_declaration_that_fails_to_read_gives_an_empty_set_and_no_notice() {
    let setup = Setup::new();
    git(&setup.workspace(), &["init", "--quiet"]);
    write(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"repository_extensions": [{"path": "nope"}]}).to_string(),
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert!(session.loaded().is_empty());
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn a_missing_copy_is_rebuilt_and_loaded() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(&setup, &[("lint", package("lint", "v2.0.0"), &[], false)]);
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    fs::remove_dir_all(copy(&setup, &digest)).unwrap();
    fs::remove_file(ready(&setup, &digest)).unwrap();
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        session.loaded(),
        vec![LoadedExtension {
            name: "fiber.test/lint".to_owned(),
            version: "v2.0.0".to_owned(),
            replaces: Vec::new(),
        }]
    );
    assert!(copy(&setup, &digest).is_dir());
}

#[test]
fn a_failed_rebuild_is_left_out_with_a_notice() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    let lint = installing("fiber.test/lint", &[], "test ! -e ../rebuild-block");
    declare_pkgs(&setup, &[("lint", lint, &[], false)]);
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    // The approval's own install step passed; the rebuild fails on this.
    write(&setup.home().join("pinned").join("rebuild-block"), "");
    fs::remove_dir_all(copy(&setup, &digest)).unwrap();
    fs::remove_file(ready(&setup, &digest)).unwrap();
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert!(session.loaded().is_empty());
    // The install failure's text ends with the empty stderr it carries.
    let why = "`sh -c test ! -e ../rebuild-block` failed: ";
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: format!(
                "The repository's approved copy of `fiber.test/lint` could not be made, \
                 so it was not loaded: `fiber.test/lint`: its install step failed: {why}"
            ),
            extension: Some("fiber.test/lint".to_owned()),
        }]
    );
    assert!(!copy(&setup, &digest).exists());
}

#[test]
fn a_held_lock_gives_busy_and_keeps_the_personal_install() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "lint",
        &json!({}),
        "fiber.command('a', {timeout=1000, run=function() return 'a' end})\n",
    );
    declare_pkgs(
        &setup,
        &[(
            "lint",
            package("lint", "v2.0.0"),
            &[(
                "init.lua",
                "fiber.command('b', {timeout=1000, run=function() return 'b' end})\n",
            )],
            false,
        )],
    );
    let repos = Repos::new(&setup);
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    // Hold `pinned/.lock` itself: the bounded copy try answers `Busy`
    // instead of waiting on another process's approval.
    let held = File::options()
        .create(true)
        .append(true)
        .open(setup.home().join("pinned/.lock"))
        .unwrap();
    held.lock().unwrap();
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    drop(held);
    // The personal install loads; the repository copy does not.
    assert_eq!(
        session.loaded(),
        vec![LoadedExtension {
            name: "fiber.test/lint".to_owned(),
            version: "v1.0.0".to_owned(),
            replaces: Vec::new(),
        }]
    );
    let mut names: Vec<String> = session
        .commands()
        .iter()
        .map(|command| command.name.clone())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: "The repository's approved copy of `fiber.test/lint` was being \
                      changed by another Fiber, so it was not loaded."
                .to_owned(),
            extension: Some("fiber.test/lint".to_owned()),
        }]
    );
}

#[test]
fn remove_copy_sees_held_while_the_session_lives() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(&setup, &[("lint", package("lint", "v2.0.0"), &[], false)]);
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/lint",
    );
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    // The set's hold refuses the removal before any session exists.
    let mut ok = || Ok::<(), String>(());
    assert_eq!(
        Store::remove_copy(&setup.home(), &digest, false, &mut ok).unwrap(),
        CopyRemoval::Held
    );
    // The holds move into the session, which keeps refusing.
    let session = load_session(&setup, set, &[]);
    assert_eq!(session.loaded().len(), 1);
    assert_eq!(
        Store::remove_copy(&setup.home(), &digest, false, &mut ok).unwrap(),
        CopyRemoval::Held
    );
    drop(session);
    assert_eq!(
        Store::remove_copy(&setup.home(), &digest, false, &mut ok).unwrap(),
        CopyRemoval::Removed
    );
}

/// A tagged helper with a provider declaration, a command and a module
/// only its own directory holds: dependencies install with their records'
/// versions and their own require roots.
fn tag_helper(repos: &mut Repos, command: &str) {
    let helper = "zzz.test/owner/helper";
    let mut hm = manifest(helper);
    hm["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    repos.tag(
        helper,
        "",
        "v1.2.0",
        &hm,
        &[
            (
                "init.lua",
                &format!(
                    "require(\"secret\")\nfiber.command('{command}', \
                     {{timeout=1000, run=function() return '{command}' end}})\n"
                ),
            ),
            ("secret.lua", "return 1\n"),
        ],
    );
}

fn loaded_names(session: &SessionExtensions) -> Vec<(String, String)> {
    session
        .loaded()
        .iter()
        .map(|loaded| (loaded.name.clone(), loaded.version.clone()))
        .collect()
}

fn command_names(session: &SessionExtensions) -> Vec<String> {
    let mut names: Vec<String> = session
        .commands()
        .iter()
        .map(|command| command.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn a_package_and_its_dependencies_load_from_the_copy() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[(
            "a",
            a,
            &[(
                "init.lua",
                "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    // The package loads from its copy, before its dependency, which
    // loads with its record's version and registers its own command.
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/a".to_owned(), "v1.0.0".to_owned()),
            ("zzz.test/owner/helper".to_owned(), "v1.2.0".to_owned()),
        ]
    );
    let copy = copy(&setup, &digest);
    assert_eq!(
        session.dirs(),
        vec![
            ("fiber.test/a".to_owned(), copy.clone()),
            (
                "zzz.test/owner/helper".to_owned(),
                copy.join(".fiber-depends")
                    .join("extensions")
                    .join("zzz.test-owner-helper"),
            ),
        ]
    );
    assert_eq!(command_names(&session), vec!["h", "pa"]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn two_versions_of_one_dependency_load_the_first_declared_package_s() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h12");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[(
            "a",
            a,
            &[(
                "init.lua",
                "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    // The second package's copy pins the newer helper: settling keeps
    // the first declared package's version, with one notice.
    let helper = "zzz.test/owner/helper";
    let mut hm = manifest(helper);
    hm["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    repos.tag(
        helper,
        "",
        "v1.4.0",
        &hm,
        &[(
            "init.lua",
            "fiber.command('h14', {timeout=1000, run=function() return 'h14' end})\n",
        )],
    );
    // The second package needs a newer helper, so its copy pins the
    // newer tag: settling keeps the first declared package's version.
    let b = named("fiber.test/b", &[(helper, "1.4")]);
    declare_pkgs(
        &setup,
        &[
            (
                "a",
                named("fiber.test/a", &[(helper, "1.0")]),
                &[(
                    "init.lua",
                    "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
                )],
                false,
            ),
            (
                "b",
                b,
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
        ],
    );
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/a".to_owned(), "v1.0.0".to_owned()),
            ("fiber.test/b".to_owned(), "v1.0.0".to_owned()),
            ("zzz.test/owner/helper".to_owned(), "v1.2.0".to_owned()),
        ]
    );
    assert_eq!(command_names(&session), vec!["h12", "pa", "pb"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionShadowed,
            message: "The repository's `zzz.test/owner/helper` v1.2.0 loads in this project; \
                      `zzz.test/owner/helper` v1.4.0 does not: two versions never load \
                      under one name."
                .to_owned(),
            extension: Some("zzz.test/owner/helper".to_owned()),
        }]
    );
}

#[test]
fn a_declared_package_beats_a_dependency_of_the_same_name() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h12");
    // Declared after the package that brings the dependency: the
    // declared entry still wins, in every declaration order.
    let helper = "zzz.test/owner/helper";
    let mut h = manifest(helper);
    h["version"] = json!("v9.9.9");
    let b = named("fiber.test/b", &[(helper, "1.0")]);
    declare_pkgs(
        &setup,
        &[
            (
                "b",
                b,
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
            (
                "h",
                h,
                &[(
                    "init.lua",
                    "fiber.command('ph', {timeout=1000, run=function() return 'ph' end})\n",
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    approve_kind(&setup, &repos, &clock, OfferedKind::Extension, helper);
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/b".to_owned(), "v1.0.0".to_owned()),
            ("zzz.test/owner/helper".to_owned(), "v9.9.9".to_owned()),
        ]
    );
    assert_eq!(command_names(&session), vec!["pb", "ph"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionShadowed,
            message: "The repository's `zzz.test/owner/helper` v9.9.9 loads in this project; \
                      `zzz.test/owner/helper` v1.2.0 does not: two versions never load \
                      under one name."
                .to_owned(),
            extension: Some("zzz.test/owner/helper".to_owned()),
        }]
    );
}

#[test]
fn a_package_cannot_require_into_fiber_depends() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    // `secret.lua` lives only under `.fiber-depends/`: the package's
    // require names no such file from its own root, so its load fails
    // while the dependency's own require of it succeeds.
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[("a", a, &[("init.lua", "require(\"secret\")\n")], false)],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![("zzz.test/owner/helper".to_owned(), "v1.2.0".to_owned())]
    );
    assert_eq!(command_names(&session), vec!["h"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: "`fiber.test/a`: init.lua:1: `secret.lua`: No such file or directory \
                      (os error 2)"
                .to_owned(),
            extension: Some("fiber.test/a".to_owned()),
        }]
    );
}

#[test]
fn a_file_where_the_dependency_root_goes_loads_the_package_without_them() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[(
            "a",
            a,
            &[(
                "init.lua",
                "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    // A file where `.fiber-depends/extensions/` goes: the package still
    // loads, its dependencies do not, with one notice.
    let root = config::extensions_dir(&copy(&setup, &digest).join(".fiber-depends"));
    fs::remove_dir_all(&root).unwrap();
    write(&root, "not a directory");
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![("fiber.test/a".to_owned(), "v1.0.0".to_owned())]
    );
    assert_eq!(command_names(&session), vec!["pa"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: format!(
                "The repository's approved copy of `fiber.test/a` has a link or a file \
                 at {}, so its dependencies were not loaded.",
                root.display()
            ),
            extension: Some("fiber.test/a".to_owned()),
        }]
    );
}

#[test]
fn links_and_files_where_dependencies_live_leave_them_out() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    let b = named("fiber.test/b", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[
            (
                "a",
                a,
                &[(
                    "init.lua",
                    "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
                )],
                false,
            ),
            (
                "b",
                b,
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    let a_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let b_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    // A link to a directory is not a real directory, and neither is a
    // plain file: each copy loads without its dependencies, with one
    // notice naming the path.
    let slug = "zzz.test-owner-helper";
    let a_dep = config::extensions_dir(&copy(&setup, &a_digest).join(".fiber-depends")).join(slug);
    let target = a_dep.with_file_name(format!("{slug}.real"));
    fs::rename(&a_dep, &target).unwrap();
    std::os::unix::fs::symlink(&target, &a_dep).unwrap();
    let b_dep = config::extensions_dir(&copy(&setup, &b_digest).join(".fiber-depends")).join(slug);
    fs::remove_dir_all(&b_dep).unwrap();
    write(&b_dep, "not a directory");
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/a".to_owned(), "v1.0.0".to_owned()),
            ("fiber.test/b".to_owned(), "v1.0.0".to_owned()),
        ]
    );
    assert_eq!(command_names(&session), vec!["pa", "pb"]);
    assert_eq!(
        session.notices(),
        vec![
            Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "The repository's approved copy of `fiber.test/a` has a link or a file \
                     at {}, so its dependencies were not loaded.",
                    a_dep.display()
                ),
                extension: Some("fiber.test/a".to_owned()),
            },
            Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "The repository's approved copy of `fiber.test/b` has a link or a file \
                     at {}, so its dependencies were not loaded.",
                    b_dep.display()
                ),
                extension: Some("fiber.test/b".to_owned()),
            },
        ]
    );
}

#[test]
fn a_missing_dependency_root_skips_quietly() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[(
            "a",
            a,
            &[(
                "init.lua",
                "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    // A missing `.fiber-depends/extensions/` holds no dependencies: the
    // completion marker still stands, so nothing rebuilds and nothing is
    // reported.
    fs::remove_dir_all(config::extensions_dir(
        &copy(&setup, &digest).join(".fiber-depends"),
    ))
    .unwrap();
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![("fiber.test/a".to_owned(), "v1.0.0".to_owned())]
    );
    assert_eq!(command_names(&session), vec!["pa"]);
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn a_damaged_dependency_leaves_the_package_loaded_with_a_notice() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[(
            "a",
            a,
            &[(
                "init.lua",
                "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    // Damage the installed dependency's record: its manifest still reads,
    // so the notice names it.
    let root = config::extensions_dir(&copy(&setup, &digest).join(".fiber-depends"));
    for entry in fs::read_dir(&root).unwrap() {
        let record = entry.unwrap().path().join(".fiber.json");
        if record.is_file() {
            fs::remove_file(&record).unwrap();
        }
    }
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&session),
        vec![("fiber.test/a".to_owned(), "v1.0.0".to_owned())]
    );
    assert_eq!(command_names(&session), vec!["pa"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionFailed,
            message: "`zzz.test/owner/helper` is damaged; run `fiber extension reinstall \
                      zzz.test/owner/helper`."
                .to_owned(),
            extension: Some("zzz.test/owner/helper".to_owned()),
        }]
    );
}

#[test]
fn an_approved_copy_replaces_the_personal_install_of_the_same_name() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "x",
        &json!({}),
        "fiber.command('k', {timeout=1000, run=function() return 'k' end})\n",
    );
    declare_pkgs(
        &setup,
        &[(
            "x",
            package("x", "v2.0.0"),
            &[(
                "init.lua",
                "fiber.command('n', {timeout=1000, run=function() return 'n' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/x",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    // The repository copy loads with its manifest's version, in place of
    // the personal install, with one notice naming both versions.
    assert_eq!(
        session.loaded(),
        vec![LoadedExtension {
            name: "fiber.test/x".to_owned(),
            version: "v2.0.0".to_owned(),
            replaces: Vec::new(),
        }]
    );
    assert_eq!(
        session.dirs(),
        vec![("fiber.test/x".to_owned(), copy(&setup, &digest))]
    );
    assert_eq!(command_names(&session), vec!["n"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionShadowed,
            message: "The repository's approved copy of `fiber.test/x` v2.0.0 loads in this \
                      project in place of your install, v1.0.0."
                .to_owned(),
            extension: Some("fiber.test/x".to_owned()),
        }]
    );
}

#[test]
fn merged_entries_load_in_slug_order() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "m",
        &json!({}),
        "fiber.command('pm', {timeout=1000, run=function() return 'pm' end})\n",
    );
    declare_pkgs(
        &setup,
        &[(
            "b",
            package("b", "v1.0.0"),
            &[(
                "init.lua",
                "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    // One merged list sorted by slug, not grouped by source: the
    // repository entry's slug sorts before the personal install's.
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/b".to_owned(), "v1.0.0".to_owned()),
            ("fiber.test/m".to_owned(), "v1.0.0".to_owned()),
        ]
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn repository_entries_sort_by_slug_with_a_name_fallback() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    // `zeta` sorts by its slug, while `.dot` has no slug and sorts by
    // its name, first. Declared the other way round, so declaration
    // order alone would crown the wrong copy: with empty keys the last
    // entry wins, and `dot`'s provider data would stand.
    let mut zeta = package("zeta", "v1.0.0");
    zeta["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    let mut dot = manifest(".dot");
    dot["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    declare_pkgs(
        &setup,
        &[
            (
                "zeta",
                zeta,
                &[(
                    "providers/acme.json",
                    &provider("acme", &["zeta-model"]).to_string(),
                )],
                false,
            ),
            (
                "dot",
                dot,
                &[(
                    "providers/acme.json",
                    &provider("acme", &["dot-model"]).to_string(),
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/zeta",
    );
    approve_kind(&setup, &repos, &clock, OfferedKind::Extension, ".dot");
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    // The last entry in slug order wins: `.dot`'s name sorts before
    // `zeta`'s slug, so `zeta`'s copy supplies `acme`.
    let data = providers.data("acme");
    let ids: Vec<&str> = data.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["zeta-model"]);
    assert_eq!(providers.extension_of("acme"), Some("fiber.test/zeta"));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn a_disabled_personal_install_stays_out_but_the_shadow_is_reported() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "x",
        &json!({}),
        "fiber.command('k', {timeout=1000, run=function() return 'k' end})\n",
    );
    declare_pkgs(
        &setup,
        &[(
            "x",
            package("x", "v2.0.0"),
            &[(
                "init.lua",
                "fiber.command('n', {timeout=1000, run=function() return 'n' end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/x",
    );
    // `enabled = false` turns off the personal install only: the
    // approved copy still loads, and the shadow notice still names both.
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &["extensions.\"fiber.test/x\".enabled=false"],
    );
    assert_eq!(
        loaded_names(&session),
        vec![("fiber.test/x".to_owned(), "v2.0.0".to_owned())]
    );
    assert_eq!(command_names(&session), vec!["n"]);
    assert_eq!(
        session.notices(),
        vec![Notice {
            code: ErrorCode::ExtensionShadowed,
            message: "The repository's approved copy of `fiber.test/x` v2.0.0 loads in this \
                      project in place of your install, v1.0.0."
                .to_owned(),
            extension: Some("fiber.test/x".to_owned()),
        }]
    );
}

/// Reloads `session` with `set` the way a session reload does: on a
/// worker under one deadline.
fn reload_session(
    setup: &Setup,
    session: SessionExtensions,
    set: RepositorySet,
    overrides: &[&str],
) -> SessionExtensions {
    let config = session_config(setup, overrides);
    let home = setup.home();
    let (done, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        done.send(session.reload_repository(&home, &config, FakeClock::new(), locks, set))
            .unwrap();
    });
    Deadline::after(WAIT).recv_or_fail(&rx, "the session reload")
}

#[test]
fn reload_repository_rebuilds_from_a_new_set_on_the_live_registry() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    // The init reads back the state the previous load's init wrote: the
    // command it registers names what the live registry still holds.
    declare_pkgs(
        &setup,
        &[(
            "a",
            package("a", "v1.0.0"),
            &[(
                "init.lua",
                "local seen = state.get(\"seen\")\n\
                 pcall(function() state.set(\"seen\", \"v1\") end)\n\
                 fiber.command(\"sees_\" .. (seen or \"nothing\"), \
                 {timeout=1000, run=function() return \"v1\" end})\n",
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(command_names(&session), vec!["sees_nothing"]);
    // A second package joins the declarations: the reload rebuilds from
    // the new set, on the same folded state.
    declare_pkgs(
        &setup,
        &[
            (
                "a",
                package("a", "v1.0.0"),
                &[(
                    "init.lua",
                    "local seen = state.get(\"seen\")\n\
             pcall(function() state.set(\"seen\", \"v1\") end)\n\
             fiber.command(\"sees_\" .. (seen or \"nothing\"), \
             {timeout=1000, run=function() return \"v1\" end})\n",
                )],
                false,
            ),
            (
                "b",
                package("b", "v1.0.0"),
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
        ],
    );
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    let reloaded = reload_session(
        &setup,
        session,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(
        loaded_names(&reloaded),
        vec![
            ("fiber.test/a".to_owned(), "v1.0.0".to_owned()),
            ("fiber.test/b".to_owned(), "v1.0.0".to_owned()),
        ]
    );
    // `sees_v1`: the reloaded init read the first load's write back from
    // the live registry; `pb`: it rebuilt from the new set.
    assert_eq!(command_names(&reloaded), vec!["pb", "sees_v1"]);
    assert!(reloaded.notices().is_empty(), "{:?}", reloaded.notices());
}

#[test]
fn missing_required_reports_required_extensions_that_did_not_load() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[
            (
                "a",
                package("a", "v1.0.0"),
                &[("init.lua", "error(\"boom\")\n")],
                true,
            ),
            (
                "b",
                package("b", "v1.0.0"),
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    // The required extension's init fails, so it never loads; the other
    // package loads.
    assert_eq!(
        loaded_names(&session),
        vec![("fiber.test/b".to_owned(), "v1.0.0".to_owned())]
    );
    assert_eq!(session.missing_required(), vec!["fiber.test/a".to_owned()]);
}

#[test]
fn listing_names_no_pinned_copy() {
    let setup = Setup::new();
    install_lua(&setup, "y", &json!({}), "");
    declare_pkgs(&setup, &[("z", package("z", "v1.0.0"), &[], false)]);
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/z",
    );
    let session = load_session(
        &setup,
        repository_set(&setup, &repos, &clock, &BTreeSet::new()),
        &[],
    );
    assert_eq!(session.loaded().len(), 2);
    // The listing names the personal install, never the repository copy
    // or its dependencies.
    let listing = list(&setup.home(), &*clock).unwrap();
    let names: Vec<&str> = listing
        .installed
        .iter()
        .map(|installed| installed.name.as_str())
        .collect();
    assert_eq!(names, vec!["fiber.test/y"]);
    assert!(listing.damaged.is_empty());
}

#[test]
fn provider_data_flows_from_a_repository_copy() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    let mut m = package("acme", "v1.0.0");
    m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    declare_pkgs(
        &setup,
        &[(
            "acme",
            m,
            &[(
                "providers/acme.json",
                &provider("acme", &["repo-model"]).to_string(),
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    let data = providers.data("acme");
    let ids: Vec<&str> = data.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["repo-model"]);
    assert_eq!(providers.extension_of("acme"), Some("fiber.test/acme"));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn provider_data_from_a_shadowed_install_keeps_the_repository_copy() {
    let setup = Setup::new();
    let installed = install_lua(
        &setup,
        "acme",
        &json!({"providers": {"acme": ["http://127.0.0.1:1/v1"]}}),
        "",
    );
    write(
        &installed.join("providers/acme.json"),
        &provider("acme", &["personal-model"]).to_string(),
    );
    let mut m = package("acme", "v1.0.0");
    m["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    declare_pkgs(
        &setup,
        &[(
            "repo",
            m,
            &[(
                "providers/acme.json",
                &provider("acme", &["repo-model"]).to_string(),
            )],
            false,
        )],
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    // The personal install's data is replaced, with the shadow notice.
    let data = providers.data("acme");
    let ids: Vec<&str> = data.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["repo-model"]);
    assert_eq!(providers.extension_of("acme"), Some("fiber.test/acme"));
    assert_eq!(
        notices,
        vec![Notice {
            code: ErrorCode::ExtensionShadowed,
            message: "The repository's approved copy of `fiber.test/acme` v1.0.0 loads in \
                      this project in place of your install, v1.0.0."
                .to_owned(),
            extension: Some("fiber.test/acme".to_owned()),
        }]
    );
}

#[test]
fn approved_extension_finds_the_current_approval() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[
            ("a", package("a", "v1.0.0"), &[], false),
            ("b", package("b", "v2.0.0"), &[], false),
        ],
    );
    let clock = FakeClock::new();
    let a_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let found = approved_extension(
        &setup.home(),
        &project(),
        &setup.workspace(),
        "fiber.test/a",
        &*clock,
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.0.path, "a");
    assert_eq!(found.1, a_digest);
    // Unapproved content, and unknown names, are `None`.
    assert!(
        approved_extension(
            &setup.home(),
            &project(),
            &setup.workspace(),
            "fiber.test/b",
            &*clock
        )
        .unwrap()
        .is_none()
    );
    assert!(
        approved_extension(
            &setup.home(),
            &project(),
            &setup.workspace(),
            "fiber.test/nope",
            &*clock
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn listing_shows_approved_extensions_in_declaration_order() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[
            ("a", package("a", "v1.0.0"), &[], false),
            ("b", package("b", "v2.0.0"), &[], false),
        ],
    );
    let clock = FakeClock::new();
    let a_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let b_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    assert_eq!(
        approved_extensions(&setup.home(), &project(), &setup.workspace(), &*clock).unwrap(),
        vec![
            ApprovedExtension {
                name: "fiber.test/a".to_owned(),
                version: "v1.0.0".to_owned(),
                path: "a".to_owned(),
                hash: a_digest,
            },
            ApprovedExtension {
                name: "fiber.test/b".to_owned(),
                version: "v2.0.0".to_owned(),
                path: "b".to_owned(),
                hash: b_digest,
            },
        ]
    );
}

#[test]
fn listing_ignores_nevered_and_unapproved_content() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(
        &setup,
        &[
            ("a", package("a", "v1.0.0"), &[], false),
            ("b", package("b", "v1.0.0"), &[], false),
            ("c", package("c", "v1.0.0"), &[], false),
        ],
    );
    let clock = FakeClock::new();
    let a_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let (b_item, b_digest) = digest_of(&setup, &clock, OfferedKind::Extension, "fiber.test/b");
    common::store(&setup, &repos, &clock)
        .never(&b_item, &b_digest)
        .unwrap();
    assert_eq!(
        approved_extensions(&setup.home(), &project(), &setup.workspace(), &*clock).unwrap(),
        vec![ApprovedExtension {
            name: "fiber.test/a".to_owned(),
            version: "v1.0.0".to_owned(),
            path: "a".to_owned(),
            hash: a_digest,
        }]
    );
}

#[test]
fn a_lingering_approval_for_a_removed_declaration_does_not_list() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    declare_pkgs(&setup, &[("a", package("a", "v1.0.0"), &[], false)]);
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    // The declaration is gone; the approval record stays, but lists
    // nothing and changes nothing in Fiber home: the earlier approval's
    // copy stays as the approval left it, and no index is written.
    declare_pkgs(&setup, &[]);
    fn tree(home: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut dirs = vec![home.join("pinned")];
        while let Some(dir) = dirs.pop() {
            let mut entries: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    dirs.push(path.clone());
                }
                out.push(path);
            }
        }
        out
    }
    let before = tree(&setup.home());
    assert_eq!(
        approved_extensions(&setup.home(), &project(), &setup.workspace(), &*clock).unwrap(),
        Vec::new()
    );
    assert_eq!(tree(&setup.home()), before);
    assert!(!setup.home().join("pinned.json").exists());
}

/// Every file under Fiber home, sorted: a read-only load writes nothing,
/// so the tree before and after it is identical.
fn home_files(setup: &Setup) -> Vec<String> {
    let home = setup.home();
    let mut found = Vec::new();
    let mut dirs = vec![home.clone()];
    while let Some(dir) = dirs.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                found.push(
                    path.strip_prefix(&home)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    found.sort();
    found
}

/// A declared provider package: `fiber.test/<short>` at `version`
/// registering `provider` with these model ids.
fn provider_pkgs(setup: &Setup, pkgs: &[(&str, &str, &str, &[&str], bool)]) {
    let mut owned: Vec<(String, Value, Vec<(String, String)>, bool)> = Vec::new();
    for (short, version, provider_name, ids, required) in pkgs {
        let mut m = package(short, version);
        m["providers"] = json!({*provider_name: ["http://127.0.0.1:1/v1"]});
        owned.push((
            (*short).to_owned(),
            m,
            vec![(
                format!("providers/{provider_name}.json"),
                provider(provider_name, ids).to_string(),
            )],
            *required,
        ));
    }
    let specs: Vec<(&str, Value, Vec<(&str, &str)>, bool)> = owned
        .iter()
        .map(|(short, m, files, required)| {
            (
                short.as_str(),
                m.clone(),
                files
                    .iter()
                    .map(|(path, data)| (path.as_str(), data.as_str()))
                    .collect(),
                *required,
            )
        })
        .collect();
    let refs: Vec<(&str, Value, &[(&str, &str)], bool)> = specs
        .iter()
        .map(|(short, m, files, required)| (*short, m.clone(), files.as_slice(), *required))
        .collect();
    declare_pkgs(setup, &refs);
}

#[test]
fn load_pinned_skips_a_missing_copy_without_building_or_writing() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    provider_pkgs(&setup, &[("acme", "v1.0.0", "acme", &["repo-model"], true)]);
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    // A prune collects a finished copy no worktree matches any more:
    // the copy and its marker go, the approval record stays.
    fs::remove_dir_all(copy(&setup, &digest)).unwrap();
    fs::remove_file(ready(&setup, &digest)).unwrap();
    let before = home_files(&setup);
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    assert!(providers.names().next().is_none());
    assert!(notices.is_empty(), "{notices:?}");
    // Nothing was rebuilt and nothing was written.
    assert!(
        !setup.home().join("pinned").join(&digest).exists(),
        "a missing copy is skipped, never rebuilt"
    );
    assert_eq!(home_files(&setup), before);
    // The required extension never loaded, so the session still reports
    // it missing.
    let session = load_session(&setup, set, &[]);
    assert_eq!(
        session.missing_required(),
        vec!["fiber.test/acme".to_owned()]
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn load_pinned_gives_the_empty_set_when_the_marker_is_held() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    provider_pkgs(
        &setup,
        &[("acme", "v1.0.0", "acme", &["repo-model"], false)],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    // An approval rebuilding the copy holds the marker exclusively: the
    // read-only load waits on its own clock instead of on that process.
    let held = File::options()
        .read(true)
        .write(true)
        .open(ready(&setup, &digest))
        .unwrap();
    held.lock().unwrap();
    let start = clock.now();
    let home = setup.home();
    let workspace = setup.workspace();
    let key = project();
    let worker_clock = clock.clone();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(RepositorySet::load_pinned(
            &home,
            &key,
            &workspace,
            worker_clock,
        ))
        .unwrap();
    });
    let set = Deadline::after(WAIT).recv_or_fail(&finished, "the read-only load");
    let (providers, notices) = load_providers(&setup, &set, &[]);
    assert!(providers.names().next().is_none());
    assert!(notices.is_empty(), "{notices:?}");
    // Fifty tries, ten milliseconds apart, on the store's clock.
    assert_eq!(clock.now().duration_since(start), BUSY_WITHIN);
    drop(held);
}

#[test]
fn load_pinned_skips_a_copy_without_its_dependency_marker_but_expands_others() {
    let setup = Setup::new();
    let mut repos = Repos::new(&setup);
    tag_helper(&mut repos, "h");
    let a = named("fiber.test/a", &[("zzz.test/owner/helper", "1.0")]);
    let b = named("fiber.test/b", &[("zzz.test/owner/helper", "1.0")]);
    declare_pkgs(
        &setup,
        &[
            (
                "a",
                a,
                &[(
                    "init.lua",
                    "fiber.command('pa', {timeout=1000, run=function() return 'pa' end})\n",
                )],
                false,
            ),
            (
                "b",
                b,
                &[(
                    "init.lua",
                    "fiber.command('pb', {timeout=1000, run=function() return 'pb' end})\n",
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/a",
    );
    let b_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/b",
    );
    // A dependency-bearing copy without its completion marker is legacy:
    // the session path would rebuild it, so the read-only load skips it.
    fs::remove_file(copy(&setup, &b_digest).join(".fiber-depends/.complete")).unwrap();
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let session = load_session(&setup, set, &[]);
    // The skipped copy stays silent, while the held copy's dependencies
    // still expand.
    assert_eq!(
        loaded_names(&session),
        vec![
            ("fiber.test/a".to_owned(), "v1.0.0".to_owned()),
            ("zzz.test/owner/helper".to_owned(), "v1.2.0".to_owned()),
        ]
    );
    assert!(session.notices().is_empty(), "{:?}", session.notices());
}

#[test]
fn load_pinned_reads_neither_unapproved_nor_never_content_and_writes_nothing() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    provider_pkgs(
        &setup,
        &[
            ("acme", "v1.0.0", "acme", &["repo-model"], false),
            ("bravo", "v1.0.0", "bravo", &["other-model"], false),
        ],
    );
    let clock = FakeClock::new();
    // Approved for another project only: the copy exists, but this
    // project approved nothing, so it must not load here.
    let (acme, acme_digest) = digest_of(&setup, &clock, OfferedKind::Extension, "fiber.test/acme");
    Store::new(
        &setup.home(),
        &ProjectKey::new("other").unwrap(),
        clock.clone(),
    )
    .approve(&acme, &acme_digest)
    .unwrap();
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/bravo",
    );
    // Never after approving keeps the finished copy: the decision still
    // bars it, so the copy sitting there must not load.
    let (item, digest) = digest_of(&setup, &clock, OfferedKind::Extension, "fiber.test/bravo");
    Store::new(&setup.home(), &project(), clock.clone())
        .never(&item, &digest)
        .unwrap();
    let copy = copy(&setup, &digest);
    assert!(copy.is_dir());
    let before = home_files(&setup);
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    assert!(providers.names().next().is_none());
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(home_files(&setup), before);
    assert!(copy.is_dir(), "a barred copy is left alone, never removed");
}

#[test]
fn try_hold_finished_refuses_a_non_hash_and_an_empty_declaration_writes_nothing() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let before = home_files(&setup);
    // A name that is not a content hash is refused before anything is
    // read or locked.
    assert!(matches!(
        Store::new(&setup.home(), &project(), clock.clone()).try_hold_finished("not-a-hash"),
        Err(extensions::Error::Pin { .. })
    ));
    assert_eq!(home_files(&setup), before);
    // A workspace declaring no extension touches nothing in Fiber home.
    declare_pkgs(&setup, &[]);
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    assert!(providers.names().next().is_none());
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(home_files(&setup), before);
    assert!(!setup.home().join("pinned").exists());
    assert!(!setup.home().join("pinned.json").exists());
}

#[test]
fn try_hold_finished_refuses_a_file_or_a_link_where_the_copy_goes() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    provider_pkgs(
        &setup,
        &[("acme", "v1.0.0", "acme", &["repo-model"], false)],
    );
    let clock = FakeClock::new();
    let digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    let store = Store::new(&setup.home(), &project(), clock.clone());
    // A stray file where the copy goes is refused, never followed: with
    // the directory check removed the hold would take the marker instead.
    fs::remove_dir_all(copy(&setup, &digest)).unwrap();
    write(&copy(&setup, &digest), "not a directory");
    match store.try_hold_finished(&digest) {
        Err(extensions::Error::Pin { why, .. }) => assert!(
            why.contains("is a symbolic link or a file"),
            "a file copy is refused, got {why:?}"
        ),
        other => panic!("a file copy is refused, got {other:?}"),
    }
    #[cfg(unix)]
    {
        fs::remove_file(copy(&setup, &digest)).unwrap();
        std::os::unix::fs::symlink("elsewhere", copy(&setup, &digest)).unwrap();
        match store.try_hold_finished(&digest) {
            Err(extensions::Error::Pin { why, .. }) => assert!(
                why.contains("is a symbolic link or a file"),
                "a linked copy is refused, got {why:?}"
            ),
            other => panic!("a linked copy is refused, got {other:?}"),
        }
    }
}

#[test]
fn try_hold_finished_reports_a_missing_copy_as_having_no_pinned_copy() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let store = Store::new(&setup.home(), &project(), clock);
    // A valid hash with no copy on disk names the missing copy: with the
    // `NotFound` check inverted or removed it reports a generic IO error.
    let missing = "a".repeat(64);
    match store.try_hold_finished(&missing) {
        Err(extensions::Error::Pin { why, .. }) => assert!(
            why.contains("has no pinned copy"),
            "a missing copy names itself, got {why:?}"
        ),
        other => panic!("a missing copy names itself, got {other:?}"),
    }
}

#[test]
fn try_hold_finished_reports_an_unreadable_copy_path_as_an_io_error() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    // A NUL byte in the home path fails the copy's metadata read with
    // `InvalidInput`, never `NotFound`: with every metadata error reading
    // as a missing copy it would report "has no pinned copy".
    let mut home = setup.home();
    home.push("ho\0me");
    let store = Store::new(&home, &project(), clock);
    match store.try_hold_finished(&"b".repeat(64)) {
        Err(extensions::Error::Io { .. }) => {}
        other => panic!("an unreadable copy path is an IO error, got {other:?}"),
    }
}

#[test]
fn load_pinned_skips_an_unfinished_copy_and_a_file_where_the_copy_goes() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    provider_pkgs(
        &setup,
        &[
            ("acme", "v1.0.0", "acme", &["repo-model"], false),
            ("bravo", "v1.0.0", "bravo", &["other-model"], false),
        ],
    );
    let clock = FakeClock::new();
    let a_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/acme",
    );
    let b_digest = approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/bravo",
    );
    // An unfinished copy keeps its directory but no marker; a stray file
    // where the copy goes keeps the marker but no directory.
    fs::remove_file(ready(&setup, &a_digest)).unwrap();
    fs::remove_dir_all(copy(&setup, &b_digest)).unwrap();
    write(&copy(&setup, &b_digest), "not a directory");
    let before = home_files(&setup);
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    assert!(providers.names().next().is_none());
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(home_files(&setup), before);
}

#[test]
fn load_pinned_keeps_the_first_declared_copy_of_one_name_silently() {
    let setup = Setup::new();
    let repos = Repos::new(&setup);
    // Two directories declare one extension name with different versions
    // and models, so the loaded models tell which declaration won.
    let mut first = manifest("fiber.test/dup");
    first["providers"] = json!({"dup": ["http://127.0.0.1:1/v1"]});
    let mut second = first.clone();
    second["version"] = json!("v2.0.0");
    declare_pkgs(
        &setup,
        &[
            (
                "d1",
                first,
                &[(
                    "providers/dup.json",
                    &provider("dup", &["v1-model"]).to_string(),
                )],
                false,
            ),
            (
                "d2",
                second,
                &[(
                    "providers/dup.json",
                    &provider("dup", &["v2-model"]).to_string(),
                )],
                false,
            ),
        ],
    );
    let clock = FakeClock::new();
    let store = common::store(&setup, &repos, &clock);
    for item in declared_items(&setup.workspace(), &*clock)
        .unwrap()
        .into_iter()
        .filter(|found| found.kind == OfferedKind::Extension)
    {
        let digest = hash(&mut Index::load(&setup.home()), &item).unwrap();
        store.approve_on_worker(&item, &digest).unwrap();
    }
    let set =
        RepositorySet::load_pinned(&setup.home(), &project(), &setup.workspace(), clock.clone());
    let (providers, notices) = load_providers(&setup, &set, &[]);
    // Settling keeps the first declared copy, with no notice: the
    // read-only load never reports the set.
    let data = providers.data("dup");
    let ids: Vec<&str> = data.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids, vec!["v1-model"]);
    assert!(notices.is_empty(), "{notices:?}");
}

/// Installs `fiber.test/mixed` with `init`: static `acme` data with
/// `static_id`, a `providers/acme.lua` discovering `found_id`, and
/// manifest-declared providers. `lazy` adds a manifest-declared lazy
/// command. Returns the installed directory.
fn install_mixed(
    setup: &Setup,
    init: &str,
    lazy: bool,
    static_id: &str,
    found_id: &str,
) -> PathBuf {
    let name = "fiber.test/mixed";
    let mut listed = manifest(name);
    listed["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    if lazy {
        listed["commands"] = json!({"lazy-cmd": {"description": "a lazy command"}});
    }
    let source = setup.source("mixed", &listed, &[provider("acme", &[static_id])]);
    write(&source.join("init.lua"), init);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let dir = setup.home().join("extensions").join(dir_name(name));
    write(
        &dir.join("providers/acme.lua"),
        &format!(
            "fiber.provider(\"acme\", {{\n\
             credential = {{ timeout = 1000, run = function() \
             return {{ token = \"t\", expires_at = 1893456000 }} end }},\n\
             models = {{ timeout = 1000, run = function() \
             return {{ {{ id = \"{found_id}\", protocol = \"openai-responses\", \
             base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 }} }} end }},\n\
             }})\n"
        ),
    );
    dir
}

/// The mixed package's init: an eager command and a hook. Running it
/// offers both, so their absence proves it never ran.
fn mixed_init() -> String {
    "fiber.command('eager-cmd', {timeout=1000, run=function() return 'eager' end})\n\
     fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"non-blocking\", \
     run = function(call) return { content = \"h\" } end })\n"
        .to_owned()
}

/// Stores the `acme` login the mixed package's discovery reads.
fn seed_acme(setup: &Setup) {
    store_credential(&setup.home(), "acme", "default", &Secret::new("k".into())).unwrap();
}

fn disabled_mixed() -> [&'static str; 1] {
    ["extensions.\"fiber.test/mixed\".enabled=false"]
}

#[test]
fn a_disabled_mixed_package_keeps_provider_models_without_init_commands_or_hooks() {
    let setup = Setup::new();
    install_mixed(&setup, &mixed_init(), false, "static-model", "found-model");
    seed_acme(&setup);
    let overrides = disabled_mixed();
    let set = RepositorySet::default();
    // The static data resolves with no session at all.
    let (providers, notices) = load_providers(&setup, &set, &overrides);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/static-model").is_ok());
    assert!(providers.resolve("acme/found-model").is_err());
    let session = load_session(&setup, set, &overrides);
    // No non-provider activation: no commands, hooks, or loaded entry.
    assert_eq!(command_names(&session), Vec::<String>::new());
    assert!(!session.has_hooks());
    assert!(session.registrations().is_empty());
    assert!(
        session
            .loaded_names()
            .iter()
            .all(|name| name != "fiber.test/mixed")
    );
    assert!(
        session
            .lua_providers()
            .iter()
            .any(|(_, provider)| provider.name() == "acme"),
        "expected the acme provider-only handle: {:?}",
        session
            .lua_providers()
            .iter()
            .map(|(_, provider)| provider.name())
            .collect::<Vec<_>>()
    );
    // Uncached discovery still runs through the provider-only handles.
    let config = session_config(&setup, &overrides);
    let mut providers = load_providers(&setup, &RepositorySet::default(), &overrides).0;
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/found-model").is_ok());
    // An explicit refresh starts the credentialed provider.
    let handles: Vec<Arc<extensions::LuaProvider>> = session
        .lua_providers()
        .iter()
        .map(|(_, p)| Arc::clone(p))
        .collect();
    let started = extensions::refresh_lists(
        &handles,
        &providers,
        &config,
        None,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(
        started.iter().any(|(name, _)| name == "acme"),
        "expected the acme refresh to start: {:?}",
        started.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    for (_, handle) in started {
        fakes::within(
            "handle.join",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || handle.join().unwrap(),
        )
        .unwrap();
    }
    // An age-based refresh skips the fresh cache and starts the stale one.
    let cache = setup.home().join("cache/models/acme.json");
    let started = extensions::refresh_lists(
        &handles,
        &providers,
        &config,
        Some(Duration::from_secs(24 * 60 * 60)),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(started.is_empty());
    // The session runs on a fake clock at a fixed wall epoch, so the
    // stale mark is relative to that wall, not the process clock.
    std::fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_modified(FakeClock::new().wall() - Duration::from_secs(2 * 24 * 60 * 60))
        .unwrap();
    let started = extensions::refresh_lists(
        &handles,
        &providers,
        &config,
        Some(Duration::from_secs(24 * 60 * 60)),
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(
        started.iter().any(|(name, _)| name == "acme"),
        "expected the acme refresh to start: {:?}",
        started.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    for (_, handle) in started {
        fakes::within(
            "handle.join",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || handle.join().unwrap(),
        )
        .unwrap();
    }
    let cached: Vec<config::ModelData> = config::read_model_cache(&setup.home(), "acme")
        .unwrap()
        .unwrap();
    assert_eq!(
        cached.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["found-model"]
    );
}

#[test]
fn a_credentialless_disabled_mixed_package_starts_no_provider_callback() {
    let setup = Setup::new();
    install_mixed(&setup, &mixed_init(), false, "static-model", "found-model");
    let overrides = disabled_mixed();
    let set = RepositorySet::default();
    // The static data still resolves: data loads, discovery does not run.
    let (providers, notices) = load_providers(&setup, &set, &overrides);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/static-model").is_ok());
    let session = load_session(&setup, set, &overrides);
    assert_eq!(command_names(&session), Vec::<String>::new());
    assert!(!session.has_hooks());
    let config = session_config(&setup, &overrides);
    let mut providers = load_providers(&setup, &RepositorySet::default(), &overrides).0;
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/found-model").is_err());
    let handles: Vec<Arc<extensions::LuaProvider>> = session
        .lua_providers()
        .iter()
        .map(|(_, p)| Arc::clone(p))
        .collect();
    let started = extensions::refresh_lists(
        &handles,
        &providers,
        &config,
        None,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(started.is_empty());
}

#[test]
fn a_disabled_lazy_package_offers_no_lazy_commands() {
    let setup = Setup::new();
    install_mixed(&setup, &mixed_init(), true, "static-model", "found-model");
    seed_acme(&setup);
    let overrides = disabled_mixed();
    let set = RepositorySet::default();
    // The static data resolves with no session at all.
    let (providers, notices) = load_providers(&setup, &set, &overrides);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/static-model").is_ok());
    let session = load_session(&setup, set, &overrides);
    // The manifest-declared lazy command stays unoffered.
    assert_eq!(command_names(&session), Vec::<String>::new());
    assert!(!session.has_hooks());
    // Uncached discovery still runs through the provider-only handles.
    let config = session_config(&setup, &overrides);
    let mut providers = load_providers(&setup, &RepositorySet::default(), &overrides).0;
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/found-model").is_ok());
}

#[test]
fn an_enabled_mixed_package_runs_init_and_offers_commands_and_hooks() {
    let setup = Setup::new();
    install_mixed(&setup, &mixed_init(), false, "static-model", "found-model");
    seed_acme(&setup);
    let set = RepositorySet::default();
    let session = load_session(&setup, set, &[]);
    assert!(
        session
            .loaded_names()
            .iter()
            .any(|name| name == "fiber.test/mixed")
    );
    assert_eq!(command_names(&session), ["eager-cmd"]);
    assert!(session.has_hooks());
    let config = session_config(&setup, &[]);
    let mut providers = load_providers(&setup, &RepositorySet::default(), &[]).0;
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/found-model").is_ok());
}

#[test]
fn a_disabled_mixed_package_with_a_trapping_init_loads_discovers_and_refreshes() {
    let setup = Setup::new();
    install_mixed(
        &setup,
        "error(\"mixed init must not run\")\n",
        false,
        "static-model",
        "found-model",
    );
    seed_acme(&setup);
    let overrides = disabled_mixed();
    let set = RepositorySet::default();
    let session = load_session(&setup, set, &overrides);
    assert!(
        session
            .notices()
            .iter()
            .all(|notice| !notice.message.contains("must not run")),
        "{:?}",
        session.notices()
    );
    let config = session_config(&setup, &overrides);
    let mut providers = load_providers(&setup, &RepositorySet::default(), &overrides).0;
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/found-model").is_ok());
    let handles: Vec<Arc<extensions::LuaProvider>> = session
        .lua_providers()
        .iter()
        .map(|(_, p)| Arc::clone(p))
        .collect();
    let started = extensions::refresh_lists(
        &handles,
        &providers,
        &config,
        None,
        &fakes::children::grouped_output,
        &|_| None,
    );
    assert!(
        started.iter().any(|(name, _)| name == "acme"),
        "expected the acme refresh to start: {:?}",
        started.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    for (_, handle) in started {
        fakes::within(
            "handle.join",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || handle.join().unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn a_disabled_approved_mixed_copy_serves_pinned_models_without_non_provider_activation() {
    let setup = Setup::new();
    // The personal install is shadowed: only the pinned copy may serve.
    install_mixed(
        &setup,
        &mixed_init(),
        true,
        "personal-static",
        "personal-model",
    );
    seed_acme(&setup);
    let mut listed = package("mixed", "v2.0.0");
    listed["providers"] = json!({"acme": ["http://127.0.0.1:1/v1"]});
    listed["commands"] = json!({"lazy-cmd": {"description": "a lazy command"}});
    let init = mixed_init();
    let data = provider("acme", &["pinned-static"]).to_string();
    declare_pkgs(
        &setup,
        &[(
            "mixed",
            listed,
            &[
                ("init.lua", init.as_str()),
                ("providers/acme.json", data.as_str()),
                (
                    "providers/acme.lua",
                    "fiber.provider(\"acme\", {\n\
                     credential = { timeout = 1000, run = function() \
                     return { token = \"t\", expires_at = 1893456000 } end },\n\
                     models = { timeout = 1000, run = function() \
                     return { { id = \"pinned-model\", protocol = \"openai-responses\", \
                     base_url = \"http://127.0.0.1:1/v1\", context_window = 1000 } } end },\n\
                     })\n",
                ),
            ],
            false,
        )],
    );
    let clock = FakeClock::new();
    let repos = Repos::new(&setup);
    approve_kind(
        &setup,
        &repos,
        &clock,
        OfferedKind::Extension,
        "fiber.test/mixed",
    );
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    let overrides = disabled_mixed();
    let shadowed = Notice {
        code: ErrorCode::ExtensionShadowed,
        message: "The repository's approved copy of `fiber.test/mixed` v2.0.0 loads in this \
                  project in place of your install, v1.0.0."
            .to_owned(),
        extension: Some("fiber.test/mixed".to_owned()),
    };
    let session = load_session(&setup, set, &overrides);
    assert_eq!(session.notices(), vec![shadowed.clone()]);
    assert_eq!(command_names(&session), Vec::<String>::new());
    assert!(!session.has_hooks());
    // The pinned copy's data loads, and its scripts stay eligible: the
    // personal copy's models never appear.
    let set = repository_set(&setup, &repos, &clock, &BTreeSet::new());
    let (providers, notices) = load_providers(&setup, &set, &overrides);
    assert_eq!(notices, vec![shadowed]);
    assert!(providers.resolve("acme/pinned-static").is_ok());
    assert!(providers.resolve("acme/personal-static").is_err());
    assert!(providers.could_register("acme"));
    let config = session_config(&setup, &overrides);
    let mut providers = load_providers(&setup, &set, &overrides).0;
    let session = load_session(&setup, set, &overrides);
    let notices =
        providers.add_session(&session, &config, &fakes::children::grouped_output, &|_| {
            None
        });
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/pinned-model").is_ok());
    assert!(providers.resolve("acme/personal-model").is_err());
}

#[test]
fn an_ordinary_disabled_personal_package_stays_out() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "plain",
        &json!({}),
        "fiber.command('plain-cmd', {timeout=1000, run=function() return 'plain' end})\n",
    );
    let overrides = ["extensions.\"fiber.test/plain\".enabled=false"];
    let set = RepositorySet::default();
    let session = load_session(&setup, set, &overrides);
    assert!(
        session
            .loaded_names()
            .iter()
            .all(|name| name != "fiber.test/plain")
    );
    assert_eq!(command_names(&session), Vec::<String>::new());
    let (providers, notices) = load_providers(&setup, &RepositorySet::default(), &overrides);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().count(), 0);
}

#[test]
fn a_disabled_mixed_process_package_keeps_static_data_but_starts_no_child() {
    let setup = Setup::new();
    let name = "fiber.test/proc";
    let mut listed = manifest(name);
    listed["process"] = json!({"program": "run", "exit_timeout_ms": 0});
    let source = setup.source("proc", &listed, &[provider("acme", &["static-model"])]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let overrides = ["extensions.\"fiber.test/proc\".enabled=false"];
    let set = RepositorySet::default();
    // The static data loads through `Providers`, with no session at all.
    let (providers, notices) = load_providers(&setup, &set, &overrides);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(providers.resolve("acme/static-model").is_ok());
    let session = load_session(&setup, set, &overrides);
    assert!(
        session
            .loaded_names()
            .iter()
            .all(|name| name != "fiber.test/proc")
    );
    assert_eq!(command_names(&session), Vec::<String>::new());
    assert!(session.lua_providers().is_empty());
}
