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

/// A root whose `after_tool` hook returns the dependency's answer after the
/// call's own content.
fn root_init(dep: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"blocking\",\n\
           run = function(call)\n\
             local dep = require(\"{dep}:trim\")\n\
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

#[test]
fn a_personal_install_loads_a_declared_dependency_module() {
    let setup = Setup::new();
    install_pair(&setup, &root_init(DEP));
    let session = load(&setup, FakeClock::new());
    let AfterToolOutcome::Changed { content, .. } = answer(ask(&session)).outcome else {
        panic!("the hook runs");
    };
    assert_eq!(content.as_deref(), Some("x|x"));
}
