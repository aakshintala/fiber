//! A package requires a dependency's Lua modules with a dep-qualified
//! name, for personal installs and repository copies
//! (`docs/extensions.md`, "What a package holds" and "Notes for authors").
//! Through the public session API, with the manifest's `depends` driving
//! the load: no test touches the crate-internal dependency map directly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};

use common::{Repos, Setup, approve, dep_dir, manifest, project, write};
use config::{Config, ProjectKey, Sources};
use contract::events::CallStatus;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use extensions::{Fetch, RepositorySet, Request, SessionExtensions, StateSeed, plan};
use fakes::clock::FakeClock;
use fakes::git::git;
use fakes::{Deadline, NoLock};
use serde_json::Map;

const DEP: &str = "example.com/acme/dep";
const ROOT: &str = "example.com/acme/root";

/// The dependency's `trim` module: what every root below requires.
const TRIM: &str = "return { trim = function(s) return s end }";

/// `manifest(name)` declaring the `acme` data provider every tagged package
/// carries, so the session loads the package instead of skipping it for an
/// undeclared provider.
fn manifest_with_provider(name: &str) -> serde_json::Value {
    let mut manifest = manifest(name);
    manifest["providers"] = serde_json::json!({"acme": ["http://127.0.0.1:1/v1"]});
    manifest
}

/// A root whose `after_tool` hook requires `name` and returns the answer
/// after the call's own content. A refused require fails the hook: with a
/// non-blocking hook the refusal lands in the answer's notices, with a
/// blocking one the call is withheld.
fn root_init_require(name: &str, on_failure: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"{on_failure}\",\n\
           run = function(call)\n\
             local dep = require(\"{name}\")\n\
             return {{ content = call.content .. \"|\" .. dep.trim(\"x\") }} end }})\n"
    )
}

/// Installs `ROOT` depending on `DEP` the way `fiber extension install`
/// does, from local repositories.
fn install_pair(setup: &Setup, root_init: &str) {
    let mut repos = Repos::new(setup);
    repos.tag(
        DEP,
        "",
        "v1.0.0",
        &manifest_with_provider(DEP),
        &[("trim.lua", TRIM), ("init.lua", "-- dependency entry\n")],
    );
    let mut root_manifest = manifest_with_provider(ROOT);
    root_manifest["depends"] = serde_json::json!({DEP: "1.0"});
    repos.tag(
        ROOT,
        "",
        "v1.0.0",
        &root_manifest,
        &[("init.lua", root_init)],
    );
    plan(
        &setup.home(),
        &Request::Install(ROOT.into()),
        "0.1.0",
        &repos.origin(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Loads `setup`'s home the way a session start does: on its own thread
/// under `MUST_SUCCEED_WITHIN`, so a hang fails the test instead of hanging it.
/// Loads `home` with `workspace` the way a session start does: on its own
/// thread under `MUST_SUCCEED_WITHIN`, so a hang fails the test instead of hanging it.
fn load_at(
    home: &std::path::Path,
    workspace: &std::path::Path,
    clock: Arc<FakeClock>,
) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let home = home.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .map(Arc::new)
        .expect("waited for the extensions to load")
}

fn load(setup: &Setup, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    load_at(&setup.home(), &setup.workspace(), clock)
}

/// Asks the session's `after_tool` hooks about a completed call whose
/// content is `content`, on its own thread.
fn ask_with(session: &Arc<SessionExtensions>, content: &str) -> mpsc::Receiver<AfterToolAnswer> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    let content = content.to_owned();
    // The call borrows the arguments map, so both are built on the worker.
    std::thread::spawn(move || {
        let arguments = Map::new();
        let call = AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: &content,
            details: None,
            process: None,
            spilled: None,
            not_regular: false,
            delivery: None,
        };
        match tx.send(session.after_tool(&call)) {
            Ok(()) | Err(std::sync::mpsc::SendError(_)) => {}
        }
    });
    rx
}

fn ask(session: &Arc<SessionExtensions>) -> mpsc::Receiver<AfterToolAnswer> {
    ask_with(session, "x")
}

/// The hook chain's answer, failing the test on a hang instead of hanging it.
fn answer(rx: mpsc::Receiver<AfterToolAnswer>) -> AfterToolAnswer {
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the answer")
}

/// The refusal notice of a run whose non-blocking hook require was
/// refused: the loader's error text, exactly as reported.
fn refused(session: &Arc<SessionExtensions>) -> String {
    let answer = answer(ask(session));
    assert_eq!(answer.notices.len(), 1, "{answer:?}");
    let notice = answer.notices.into_iter().next().unwrap();
    assert_eq!(notice.code, contract::ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some(ROOT));
    notice.message
}

#[test]
fn a_personal_install_loads_a_declared_dependency_module() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "blocking"),
    );
    let session = load(&setup, FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn an_undeclared_dependency_is_refused() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require("example.com/acme/other:trim", "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("`dependency 'example.com/acme/other' not installed`"),
        "{message}"
    );
}

#[test]
fn a_declared_but_missing_dependency_names_the_dependency() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "non-blocking"),
    );
    // The dependency was installed, then removed: the map still points at
    // its directory, so the require names it instead of reading the file
    // loader's error.
    std::fs::remove_dir_all(setup.home().join("extensions").join(config::dir_name(DEP))).unwrap();
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    let want = format!("`dependency '{DEP}' not installed`");
    assert!(message.contains(&want), "{message} vs {want}");
}

#[test]
fn a_missing_module_names_the_module() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:nosuch"), "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(message.contains("`nosuch.lua`"), "{message}");
}

#[test]
fn an_empty_module_is_refused_as_an_unknown_dependency() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:"), "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains(&format!("`dependency '{DEP}' not installed`")),
        "{message}"
    );
}

#[test]
fn a_trailing_colon_after_a_module_names_the_module() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:a:"), "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    // The remainder past the matched key is `a:`, not empty, so the
    // loader reports the missing module file rather than the
    // dependency.
    let message = refused(&session);
    assert!(message.contains("`a:.lua`"), "{message}");
}

#[test]
fn an_empty_name_is_refused_as_an_unknown_dependency() {
    let setup = Setup::new();
    install_pair(&setup, &root_init_require("", "non-blocking"));
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("`dependency '' not installed`"),
        "{message}"
    );
}

#[test]
fn a_leading_colon_is_refused_as_an_unknown_dependency() {
    let setup = Setup::new();
    install_pair(&setup, &root_init_require(":trim", "non-blocking"));
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("`dependency '' not installed`"),
        "{message}"
    );
}

#[test]
fn a_second_colon_stays_in_the_module() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:a:b"), "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    // The split is at the first colon only: `a:b` maps to the file
    // `a:b.lua`, which is missing.
    let message = refused(&session);
    assert!(message.contains("`a:b.lua`"), "{message}");
}

#[test]
fn a_key_prefix_without_a_colon_boundary_is_not_a_match() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}X:trim"), "non-blocking"),
    );
    let session = load(&setup, FakeClock::new());
    // `DEP` is a string prefix of the head but the boundary byte is `X`,
    // not a colon, so no key matches: the head names the missing
    // dependency instead of the dependency's missing module.
    let message = refused(&session);
    assert!(
        message.contains(&format!("`dependency '{DEP}X' not installed`")),
        "{message}"
    );
}

#[test]
fn a_colon_holding_depends_key_loads_through_the_longest_match() {
    let setup = Setup::new();
    install_pair(&setup, &root_init_require("weird:key:trim", "blocking"));
    // The installed manifest gains a colon-holding key after the install:
    // the manifest is what the session reads, not the install record.
    let manifest = setup
        .home()
        .join("extensions")
        .join(config::dir_name(ROOT))
        .join("extension.json");
    let mut root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    root["depends"]
        .as_object_mut()
        .unwrap()
        .insert("weird:key".to_owned(), serde_json::json!("1.0"));
    std::fs::write(&manifest, root.to_string()).unwrap();
    // The dependency directory is created by hand: the loader serves the
    // filesystem, so no install record is needed for the mapped name.
    let dep = setup
        .home()
        .join("extensions")
        .join(config::dir_name("weird:key"));
    std::fs::create_dir_all(&dep).unwrap();
    std::fs::write(dep.join("trim.lua"), TRIM).unwrap();
    let session = load(&setup, FakeClock::new());
    // The longest prefix `weird:key` wins over the shorter head `weird`,
    // which no key holds, so the module serves.
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn overlapping_depends_keys_pin_the_longest_win() {
    let setup = Setup::new();
    install_pair(&setup, &root_init_require("dep:a:trim", "blocking"));
    let manifest = setup
        .home()
        .join("extensions")
        .join(config::dir_name(ROOT))
        .join("extension.json");
    let mut root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let depends = root["depends"].as_object_mut().unwrap();
    depends.insert("dep".to_owned(), serde_json::json!("1.0"));
    depends.insert("dep:a".to_owned(), serde_json::json!("1.0"));
    std::fs::write(&manifest, root.to_string()).unwrap();
    // The shorter key holds a decoy answering "wrong": the longest match
    // must serve the `dep:a` directory instead.
    let short = setup
        .home()
        .join("extensions")
        .join(config::dir_name("dep"));
    std::fs::create_dir_all(&short).unwrap();
    std::fs::write(
        short.join("trim.lua"),
        "return { trim = function(s) return \"wrong\" end }",
    )
    .unwrap();
    let long = setup
        .home()
        .join("extensions")
        .join(config::dir_name("dep:a"));
    std::fs::create_dir_all(&long).unwrap();
    std::fs::write(long.join("trim.lua"), TRIM).unwrap();
    let session = load(&setup, FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn a_symlink_pointing_out_of_the_dependency_is_refused() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:link"), "non-blocking"),
    );
    let outside = setup.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("secret.lua"),
        "return { trim = function(s) return s end }",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.join("secret.lua"),
        setup
            .home()
            .join("extensions")
            .join(config::dir_name(DEP))
            .join("link.lua"),
    )
    .unwrap();
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("outside the extension's directory"),
        "{message}"
    );
}

/// Declares `ROOT` depending on `DEP` at `minimum` in the workspace with
/// this entry script, the way a repository package declares it.
fn declare_copy(setup: &Setup, minimum: &str, root_init: &str) {
    let ws = setup.workspace();
    if !ws.join(".git").exists() {
        git(&ws, &["init", "--quiet"]);
    }
    let mut root_manifest = manifest_with_provider(ROOT);
    root_manifest["depends"] = serde_json::json!({DEP: minimum});
    write(&ws.join("pkg/extension.json"), &root_manifest.to_string());
    write(&ws.join("pkg/init.lua"), root_init);
    write(
        &ws.join(".fiber/config.json"),
        &serde_json::json!({"repository_extensions": [{"path": "pkg"}]}).to_string(),
    );
}

/// Tags `DEP` at `tag` holding `trim`, and approves the declared copy.
/// Returns the repositories and the approved digest.
fn approve_copy<'a>(setup: &'a Setup, tag: &str, trim: &str) -> (Repos<'a>, String) {
    let mut repos = Repos::new(setup);
    repos.tag(
        DEP,
        "",
        tag,
        &manifest_with_provider(DEP),
        &[("trim.lua", trim), ("init.lua", "-- dependency entry\n")],
    );
    let digest = approve(setup, &repos);
    (repos, digest)
}

/// Loads the approved set the way a session start does.
fn load_set(setup: &Setup, repos: &Repos) -> Arc<SessionExtensions> {
    let clock = FakeClock::new();
    let set = RepositorySet::load(
        &setup.home(),
        &project(),
        &setup.workspace(),
        &BTreeSet::new(),
        Fetch::new(repos.origin(), "0.1.0", clock.clone()),
        None,
    );
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: project(),
        overrides: Vec::new(),
    })
    .unwrap();
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load_repository(
            &home,
            &config,
            clock,
            locks,
            None,
            StateSeed::new(),
            set,
            None,
        ));
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .map(Arc::new)
        .expect("waited for the extensions to load")
}

/// Approves the declared copy with `DEP` tagged at `tag` holding `trim`,
/// and loads the approval the way a session start does.
fn approve_and_load(setup: &Setup, tag: &str, trim: &str) -> Arc<SessionExtensions> {
    let (repos, _) = approve_copy(setup, tag, trim);
    load_set(setup, &repos)
}

#[test]
fn a_repository_copy_serves_its_own_dependencies() {
    let setup = Setup::new();
    declare_copy(
        &setup,
        "1.0",
        &root_init_require(&format!("{DEP}:trim"), "blocking"),
    );
    let session = approve_and_load(&setup, "v1.0.0", TRIM);
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn a_repository_copy_prefers_its_own_dependency_over_a_personal_install() {
    let setup = Setup::new();
    // A personal install of the dependency answering "personal".
    let mut repos = Repos::new(&setup);
    repos.tag(
        DEP,
        "",
        "v1.0.0",
        &manifest_with_provider(DEP),
        &[(
            "trim.lua",
            "return { trim = function(s) return \"personal\" end }",
        )],
    );
    plan(
        &setup.home(),
        &Request::Install(DEP.into()),
        "0.1.0",
        &repos.origin(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    // The copy pins a newer dependency answering "copy": the copy-local
    // one wins, so the personal install never leaks across the boundary.
    declare_copy(
        &setup,
        "2.0",
        &root_init_require(&format!("{DEP}:trim"), "blocking"),
    );
    let session = approve_and_load(&setup, "v2.0.0", TRIM);
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn a_missing_copy_local_dependency_names_the_dependency() {
    let setup = Setup::new();
    // A personal install that a fallback would read: the copy-local miss
    // must name the dependency instead.
    let mut repos = Repos::new(&setup);
    repos.tag(
        DEP,
        "",
        "v1.0.0",
        &manifest_with_provider(DEP),
        &[(
            "trim.lua",
            "return { trim = function(s) return \"personal\" end }",
        )],
    );
    plan(
        &setup.home(),
        &Request::Install(DEP.into()),
        "0.1.0",
        &repos.origin(),
        &*FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    declare_copy(
        &setup,
        "1.0",
        &root_init_require(&format!("{DEP}:trim"), "non-blocking"),
    );
    // The copy's dependency is gone, while the personal one stays: the
    // map still points copy-local, so the require names it with no home
    // read. The tagged repositories stay alive for the set load, which
    // reuses the existing copy without fetching.
    let digest = approve(&setup, &repos);
    std::fs::remove_dir_all(dep_dir(&setup, &digest, DEP)).unwrap();
    let session = load_set(&setup, &repos);
    let message = refused(&session);
    assert!(
        message.contains(&format!("`dependency '{DEP}' not installed`")),
        "{message}"
    );
}

#[test]
fn a_symlinked_fiber_home_serves_the_dependency() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "blocking"),
    );
    // Fiber home reached through a symlink: the map keeps the alias path
    // for its missing-directory message, while serving canonicalizes per
    // read.
    let linked = setup.root().join("linked-home");
    std::os::unix::fs::symlink(setup.home(), &linked).unwrap();
    let session = load_at(&linked, &setup.workspace(), FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}

#[test]
fn a_symlinked_dependency_root_is_refused() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "non-blocking"),
    );
    // The installed directory moves under a staging directory inside the
    // managed extensions root, with a symlink in its place: the mapped
    // path's final component is a symlink, so the require is refused
    // even though the canonical target stays inside the tree. The
    // staging directory starts with a dot, so discovery skips it as an
    // install in progress.
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    let target = setup
        .home()
        .join("extensions")
        .join(".inside-tree")
        .join(config::dir_name(DEP));
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::rename(&dir, &target).unwrap();
    std::os::unix::fs::symlink(&target, &dir).unwrap();
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("outside the extension's directory"),
        "{message}"
    );
}

#[test]
fn a_symlinked_dependency_root_to_a_sibling_is_refused() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "non-blocking"),
    );
    // A sibling dependency directory inside the managed extensions root:
    // the link target stays inside the tree, but the mapped path's final
    // component is a symlink, so the require is refused.
    let sibling = setup
        .home()
        .join("extensions")
        .join(config::dir_name("example.com/acme/sibling"));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("trim.lua"), TRIM).unwrap();
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&sibling, &dir).unwrap();
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("outside the extension's directory"),
        "{message}"
    );
}

#[test]
fn a_dependency_root_outside_fiber_home_is_refused() {
    let setup = Setup::new();
    install_pair(
        &setup,
        &root_init_require(&format!("{DEP}:trim"), "non-blocking"),
    );
    // The link target sits outside the managed extensions root: the map
    // entry authorizes the name, but the canonical target leaves the
    // tree, so the require is refused with the outside-directory error.
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    let target = setup
        .root()
        .join("outside-home")
        .join(config::dir_name(DEP));
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::rename(&dir, &target).unwrap();
    std::os::unix::fs::symlink(&target, &dir).unwrap();
    let session = load(&setup, FakeClock::new());
    let message = refused(&session);
    assert!(
        message.contains("outside the extension's directory"),
        "{message}"
    );
}

#[test]
fn a_retargeted_dependency_root_is_refused_when_re_resolved() {
    let setup = Setup::new();
    // The hook requires the module named by the call, so each ask loads a
    // module the earlier ask never cached: the Lua require cache reuses
    // values by full qualified name.
    install_pair(
        &setup,
        "fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\",\n\
           run = function(call)\n\
             local dep = require(\"example.com/acme/dep:\" .. call.content)\n\
             return { content = dep.trim(\"x\") } end })\n",
    );
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    // The dependency starts as a real directory inside the managed root,
    // so the first require serves.
    std::fs::write(
        dir.join("one.lua"),
        "return { trim = function(s) return \"a\" end }",
    )
    .unwrap();
    let session = load(&setup, FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask_with(&session, "one")).outcome
    else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("a"));
    // The root is replaced mid-run by a link to a new target inside the
    // tree, under a dot staging directory discovery skips: the next
    // require re-resolves per read and refuses the link even though its
    // target stays inside the tree. The ask names a module the first ask
    // never loaded, so the refusal proves the re-resolved check fires,
    // not the require cache.
    let second = setup
        .home()
        .join("extensions")
        .join(".retarget")
        .join("second");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        second.join("two.lua"),
        "return { trim = function(s) return \"b\" end }",
    )
    .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&second, &dir).unwrap();
    let answer = answer(ask_with(&session, "two"));
    assert_eq!(answer.notices.len(), 1, "{answer:?}");
    let notice = answer.notices.into_iter().next().unwrap();
    assert_eq!(notice.code, contract::ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some(ROOT));
    assert!(
        notice.message.contains("outside the extension's directory"),
        "{}",
        notice.message
    );
}

#[test]
fn a_retargeted_dependency_root_outside_the_tree_is_refused() {
    let setup = Setup::new();
    // The hook requires the module named by the call, so the second ask
    // loads a module the first ask never cached.
    install_pair(
        &setup,
        "fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\",\n\
           run = function(call)\n\
             local dep = require(\"example.com/acme/dep:\" .. call.content)\n\
             return { content = dep.trim(\"x\") } end })\n",
    );
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    // The dependency starts as a real directory serving its first module;
    // the second target sits inside Fiber home but outside the managed
    // root, pinning the check to the extensions root rather than the home.
    std::fs::write(
        dir.join("one.lua"),
        "return { trim = function(s) return \"a\" end }",
    )
    .unwrap();
    let outside = setup.home().join("data-outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("two.lua"),
        "return { trim = function(s) return \"b\" end }",
    )
    .unwrap();
    let session = load(&setup, FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask_with(&session, "one")).outcome
    else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("a"));
    // The root is replaced mid-run by a link outside the tree: the next
    // require re-resolves per read and refuses the new target, even
    // though the module exists there. Both asks name modules the other
    // never loaded, so the refusal proves the location check fires, not
    // the require cache or the missing-module one.
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&outside, &dir).unwrap();
    let answer = answer(ask_with(&session, "two"));
    assert_eq!(answer.notices.len(), 1, "{answer:?}");
    let notice = answer.notices.into_iter().next().unwrap();
    assert_eq!(notice.code, contract::ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some(ROOT));
    assert!(
        notice.message.contains("outside the extension's directory"),
        "{}",
        notice.message
    );
}

#[test]
fn a_link_replaced_by_a_real_inside_tree_dir_serves() {
    let setup = Setup::new();
    // The hook requires the module named by the call, so each ask loads a
    // module the earlier ask never cached: the Lua require cache reuses
    // values by full qualified name.
    install_pair(
        &setup,
        "fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\",\n\
           run = function(call)\n\
             local dep = require(\"example.com/acme/dep:\" .. call.content)\n\
             return { content = dep.trim(\"x\") } end })\n",
    );
    let dir = setup.home().join("extensions").join(config::dir_name(DEP));
    // The root starts as a link into a dot staging directory discovery
    // skips: the first require refuses the link even though its target
    // stays inside the tree.
    let staged = setup
        .home()
        .join("extensions")
        .join(".retarget-real")
        .join("first");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(
        staged.join("one.lua"),
        "return { trim = function(s) return \"a\" end }",
    )
    .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(&staged, &dir).unwrap();
    let session = load(&setup, FakeClock::new());
    let refused_answer = answer(ask_with(&session, "one"));
    assert_eq!(refused_answer.notices.len(), 1, "{refused_answer:?}");
    let notice = refused_answer.notices.into_iter().next().unwrap();
    assert_eq!(notice.code, contract::ErrorCode::HookFailed);
    assert_eq!(notice.extension.as_deref(), Some(ROOT));
    assert!(
        notice.message.contains("outside the extension's directory"),
        "{}",
        notice.message
    );
    // The link is replaced mid-run by a real directory inside the tree
    // holding a module the first ask never loaded: the next require
    // re-resolves per read and serves it.
    std::fs::remove_file(&dir).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("two.lua"),
        "return { trim = function(s) return \"b\" end }",
    )
    .unwrap();
    let AfterToolOutcome::Changed { content, .. } = answer(ask_with(&session, "two")).outcome
    else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("b"));
}
