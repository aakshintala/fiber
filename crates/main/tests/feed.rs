//! Binary-level tests of `recent.jsonl` and the hub's feed
//! (`docs/invocation.md`, "The hub"; `docs/state.md`, "Recently exited
//! sessions"): every session appends its own row as it exits, and the hub
//! serves `feed` and `recent` across projects.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

/// The rows of `home`'s `recent.jsonl`, parsed.
fn rows(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join("recent.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The project key of `workspace`: the name of its `projects/<key>`
/// directory.
fn project_key(home: &Path, workspace: &Path) -> String {
    log::sessions_dir(home, &doors::project(workspace))
        .parent()
        .and_then(Path::file_name)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The `session_id` on the first stdout line of a run.
fn session_of(stdout: &[u8]) -> String {
    let first: Value =
        serde_json::from_str(String::from_utf8_lossy(stdout).lines().next().unwrap()).unwrap();
    first["session_id"].as_str().unwrap().to_owned()
}

#[test]
fn a_fiber_ask_run_appends_exactly_one_exited_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit("fiber ask", command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = session_of(&output.stdout);
    let rows = rows(&setup.home());
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["session_id"], id.as_str());
    assert_eq!(row["how"], "exited");
    assert_eq!(
        row["project"],
        project_key(&setup.home(), &setup.workspace()).as_str()
    );
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    assert_eq!(
        fs::canonicalize(row["workspace"].as_str().unwrap()).unwrap(),
        workspace
    );
    assert_eq!(row["status"]["state"], "idle");
    assert_eq!(row["name"], row["status"]["name"]);
    assert!(row["ts"].as_u64().unwrap() > 0);
}

#[test]
fn a_session_that_exits_on_a_pending_approval_leaves_a_waiting_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[json!({
        "type": "response.output_item.done",
        "item": {"type": "function_call", "id": "fc_call_1", "call_id": "call_1", "name": "shell",
            "arguments": json!({"command": "echo hi"}).to_string()}
    })])])
    .unwrap();
    setup.provider(&server);
    // A standing ask: the session asks a person, and the idle delay of 0
    // ends it on the pending request (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}),
    );
    let id = doors::mint("s_");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let output = run_to_exit(
        "the session",
        setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            &workspace,
            "--prompt",
            "run it",
        ]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = rows(&setup.home());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["session_id"], id.as_str());
    assert_eq!(rows[0]["how"], "exited");
    assert_eq!(rows[0]["status"]["state"], "waiting", "{:?}", rows[0]);
    assert_eq!(rows[0]["status"]["waiting"]["kind"], "approval");
}

#[test]
fn a_session_that_never_got_a_prompt_appends_no_row() {
    let setup = Setup::new();
    let server = ProviderServer::start(Vec::<fakes::Response>::new()).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}),
    );
    let id = doors::mint("s_");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let output = run_to_exit(
        "the session",
        setup.fiber(&["session", "--id", &id, "--workspace", &workspace]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rows(&setup.home()).is_empty());
    assert!(!setup.home().join("recent.jsonl").exists());
}
