//! The opt-in live test for the databricks package
//! (`docs/testing/fakes.md`, "Live calls and evals"): one turn per protocol
//! against the real gateway. It is `#[ignore]`d and panics without
//! `FIBER_LIVE_DATABRICKS_KEY`, `FIBER_LIVE_DATABRICKS_HOST`, and the model
//! id variable; it never runs in CI.
//! It reads the package files at run time, never `include_str!`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::Path;
use std::time::Duration;

use fakes::Deadline;
use serde_json::Value;
use support::{Setup, run_to_exit};

/// How long one live run may take.
const LIVE_DEADLINE: Duration = Duration::from_secs(120);

/// Copies `providers/databricks` unmodified into `to`: no origin rewrite,
/// so discovery and turns reach the real workspace.
fn copy_unmodified(to: &Path) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/databricks");
    for file in [
        "extension.json",
        "providers/databricks.json",
        "providers/databricks.lua",
    ] {
        let text = std::fs::read_to_string(from.join(file)).unwrap();
        let dest = to.join(file);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, text).unwrap();
    }
}

/// A live home with the package installed: the `fiber` runs' root, the
/// key and workspace every child reads, and the model id `id_var` names.
struct Live {
    setup: Setup,
    env: Vec<(String, String)>,
    id: String,
}

/// The live home: panics naming the missing variable when the key, the
/// workspace, or the id `id_var` names is unset.
fn live_setup(id_var: &str) -> Live {
    let Ok(key) = std::env::var("FIBER_LIVE_DATABRICKS_KEY") else {
        panic!("the live databricks test needs FIBER_LIVE_DATABRICKS_KEY set");
    };
    let Ok(host) = std::env::var("FIBER_LIVE_DATABRICKS_HOST") else {
        panic!("the live databricks test needs FIBER_LIVE_DATABRICKS_HOST set");
    };
    let id = std::env::var(id_var)
        .unwrap_or_else(|_| panic!("{id_var} names the live model id for this protocol"));
    let setup = Setup::new();
    let to = setup.root.path().join("pkg-databricks");
    copy_unmodified(&to);
    let mut install = setup.fiber(&["extension", "install", to.to_str().unwrap()]);
    install.env("DATABRICKS_TOKEN", &key);
    install.env("DATABRICKS_HOST", &host);
    let installed = run_to_exit(
        Deadline::after(LIVE_DEADLINE),
        "fiber extension install",
        install,
    );
    assert_eq!(installed.status.code(), Some(0), "{installed:?}");
    Live {
        setup,
        env: vec![
            ("DATABRICKS_TOKEN".to_owned(), key),
            ("DATABRICKS_HOST".to_owned(), host),
        ],
        id,
    }
}

/// One live turn on the model `id_var` names: exit 0 and a non-empty reply.
#[track_caller]
fn live_turn(id_var: &str) {
    let live = live_setup(id_var);
    let Live { setup, env, id } = &live;
    let mut models = setup.fiber(&["models", "--json"]);
    for (name, value) in env {
        models.env(name, value);
    }
    let listed = run_to_exit(Deadline::after(LIVE_DEADLINE), "fiber models", models);
    assert_eq!(listed.status.code(), Some(0), "{listed:?}");
    let mut ask = setup.fiber(&[
        "ask",
        "--model",
        &format!("databricks/{id}"),
        "Reply with one short sentence.",
    ]);
    for (name, value) in env {
        ask.env(name, value);
    }
    let output = run_to_exit(Deadline::after(LIVE_DEADLINE), "fiber ask", ask);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let text = lines
        .iter()
        .find(|line| line["kind"] == "text_completed")
        .unwrap()["payload"]["text"]
        .as_str()
        .unwrap();
    assert!(!text.is_empty(), "{lines:?}");
}

#[test]
#[ignore = "live/needs FIBER_LIVE_DATABRICKS_KEY, FIBER_LIVE_DATABRICKS_HOST and FIBER_LIVE_DATABRICKS_CLAUDE; run with --run-ignored only"]
fn live_claude_turn_answers_on_the_anthropic_route() {
    live_turn("FIBER_LIVE_DATABRICKS_CLAUDE");
}

#[test]
#[ignore = "live/needs FIBER_LIVE_DATABRICKS_KEY, FIBER_LIVE_DATABRICKS_HOST and FIBER_LIVE_DATABRICKS_RESPONSES; run with --run-ignored only"]
fn live_responses_turn_answers_on_the_codex_route() {
    live_turn("FIBER_LIVE_DATABRICKS_RESPONSES");
}

#[test]
#[ignore = "live/needs FIBER_LIVE_DATABRICKS_KEY, FIBER_LIVE_DATABRICKS_HOST and FIBER_LIVE_DATABRICKS_COMPLETIONS; run with --run-ignored only"]
fn live_completions_turn_answers_on_the_mlflow_route() {
    live_turn("FIBER_LIVE_DATABRICKS_COMPLETIONS");
}
