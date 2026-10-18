//! Binary-level tests of the hub's reviewer notes operations
//! (`docs/invocation.md`, "What the hub speaks"): a client reads both
//! layers' notes, saves them with current versions, and is refused stale
//! with an old version, all under its own `FIBER_HOME`. Every run carries
//! a wall-clock deadline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
mod support;
use serde_json::{Value, json};
use std::fs;
use std::sync::{Arc, Mutex};
use support::*;

/// Sends a `configure` command and reads its `command_accepted` answer.
fn configure(client: &Socket, id: &str, args: Value) -> Value {
    client.send(&json!({"id": id, "command": "configure", "args": args}).to_string());
    let answer = recv_answer(client, id, "the configure answer");
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    assert_eq!(answer["payload"]["command_id"], id);
    answer["payload"]["result"].clone()
}

#[test]
fn the_hub_reads_and_saves_reviewer_notes_with_version_conflicts_stale() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, hello) = connect_hub(&setup, &hub);
    assert_eq!(hello["kind"], "hub_hello");
    let workspace = setup.workspace().to_string_lossy().into_owned();

    let notes = configure(
        &client,
        "n_read_empty",
        json!({"operation":"reviewer_notes","workspace":workspace}),
    );
    assert_eq!(notes["global"]["text"], "");
    assert_eq!(notes["global"]["version"], "absent");
    assert_eq!(notes["project"]["text"], "");
    assert_eq!(notes["project"]["version"], "absent");
    let global_file = notes["global"]["file"].as_str().unwrap().to_owned();
    let project_file = notes["project"]["file"].as_str().unwrap().to_owned();
    assert!(global_file.ends_with("config.json"), "{global_file}");
    assert!(project_file.ends_with("config.json"), "{project_file}");

    let saved = configure(
        &client,
        "n_save_current",
        json!({"operation":"save_reviewer_notes","workspace":workspace,
            "global":{"version":"absent","text":"global notes"},
            "project":{"version":"absent","text":"project notes"}}),
    );
    assert_eq!(saved["saved"], "written");
    assert_eq!(
        saved["files"],
        json!([{"file":global_file,"saved":true},{"file":project_file,"saved":true}]),
        "{saved}"
    );
    let after_save = (
        fs::read(&global_file).unwrap(),
        fs::read(&project_file).unwrap(),
    );

    let notes = configure(
        &client,
        "n_read_saved",
        json!({"operation":"reviewer_notes","workspace":workspace}),
    );
    assert_eq!(notes["global"]["text"], "global notes");
    assert_eq!(notes["project"]["text"], "project notes");
    assert_ne!(notes["global"]["version"], "absent");
    assert_ne!(notes["project"]["version"], "absent");
    let global = notes["global"]["version"].as_str().unwrap().to_owned();
    let project = notes["project"]["version"].as_str().unwrap().to_owned();

    // The old global version no longer matches: both files stay byte for
    // byte as after the first save.
    let saved = configure(
        &client,
        "n_save_stale",
        json!({"operation":"save_reviewer_notes","workspace":workspace,
            "global":{"version":"absent","text":"lost global"},
            "project":{"version":project,"text":"lost project"}}),
    );
    assert_eq!(saved, json!({"saved":"stale"}), "{saved}");
    assert_eq!(fs::read(&global_file).unwrap(), after_save.0);
    assert_eq!(fs::read(&project_file).unwrap(), after_save.1);

    // The current versions still write.
    let saved = configure(
        &client,
        "n_save_again",
        json!({"operation":"save_reviewer_notes","workspace":workspace,
            "global":{"version":global,"text":"newer global"}}),
    );
    assert_eq!(
        saved,
        json!({"saved":"written","files":[{"file":global_file,"saved":true}]}),
        "{saved}"
    );
    assert_ne!(fs::read(&global_file).unwrap(), after_save.0);
    assert_eq!(fs::read(&project_file).unwrap(), after_save.1);

    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
}
