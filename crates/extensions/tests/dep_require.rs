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

use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Repos, Setup, manifest};
use config::{Config, ProjectKey, Sources};
use contract::events::CallStatus;
use contract::hook::{AfterToolAnswer, AfterToolCall, AfterToolOutcome, Hooks};
use extensions::{Request, SessionExtensions, plan};
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::Map;

/// How long a test waits for the session load or one answer before failing.
const WAIT: Duration = Duration::from_secs(5);

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
/// under `WAIT`, so a hang fails the test instead of hanging it.
fn load(setup: &Setup, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let home = setup.home();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .map(Arc::new)
        .expect("waited for the extensions to load")
}

/// Asks the session's `after_tool` hooks about a completed call whose
/// content is `x`, on its own thread.
fn ask(session: &Arc<SessionExtensions>) -> mpsc::Receiver<AfterToolAnswer> {
    let (tx, rx) = mpsc::channel();
    let session = Arc::clone(session);
    // The call borrows the arguments map, so both are built on the worker.
    std::thread::spawn(move || {
        let arguments = Map::new();
        let call = AfterToolCall {
            tool: "read",
            arguments: &arguments,
            status: CallStatus::Completed,
            content: "x",
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

/// The hook chain's answer, failing the test on a hang instead of hanging it.
fn answer(rx: mpsc::Receiver<AfterToolAnswer>) -> AfterToolAnswer {
    Deadline::after(WAIT)
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
fn a_colon_holding_depends_key_is_unaddressable() {
    let setup = Setup::new();
    install_pair(&setup, &root_init_require("weird:key:trim", "non-blocking"));
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
    let session = load(&setup, FakeClock::new());
    // The split is at the first colon: the head `weird` never equals the
    // colon-holding key, so the lookup fails as an unknown dependency.
    let message = refused(&session);
    assert!(
        message.contains("`dependency 'weird' not installed`"),
        "{message}"
    );
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
