use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::*;

const DOC: &str = include_str!("../../../../docs/invocation.md");

/// The names in the first column of the `| Hub command | `args` |` table.
fn doc_hub_commands() -> BTreeSet<&'static str> {
    let table = DOC.split("| Hub command | `args` |").nth(1).unwrap();
    table
        .lines()
        .skip(2)
        .take_while(|line| line.starts_with('|'))
        .map(|line| line.split('`').nth(1).unwrap())
        .collect()
}

/// One line per hub command, with every key it takes. Optional keys appear
/// both present and absent across the samples; `configure` has one sample
/// with arguments and one without.
fn samples() -> Vec<Value> {
    vec![
        json!({"id": "c", "command": "authenticate", "args": {"token": "t"}}),
        json!({"id": "c", "command": "pair", "args": {"code": "p"}}),
        json!({"id": "c", "command": "feed", "args": {}}),
        json!({"id": "c", "command": "dismiss", "args": {"session": "s_1"}}),
        json!({"id": "c", "command": "recent", "args": {"before": "s_1"}}),
        json!({"id": "c", "command": "recent", "args": {"project": "p"}}),
        json!({"id": "c", "command": "sessions", "args": {"project": "p"}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w", "model": "m",
            "overrides": ["a=b"], "worktree": true,
            "content": [{"type": "text", "text": "t"}]}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w"}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w", "continues": "s_2"}}),
        json!({"id": "c", "command": "delete", "args": {"session": "s_1"}}),
        json!({"id": "c", "command": "delete",
            "args": {"session": "s_1", "cascade": true, "expect": ["s_2"]}}),
        json!({"id": "c", "command": "prompt_history", "args": {"project": "p"}}),
        json!({"id": "c", "command": "prompt_history",
            "args": {"project": "p", "before": 7}}),
        json!({"id": "c", "command": "read_file",
            "args": {"session": "s_1", "path": "artifacts/i_3f2a.png"}}),
        json!({"id": "c", "command": "directories", "args": {"path": "/w"}}),
        json!({"id": "c", "command": "search_files",
            "args": {"workspace": "/w", "text": "t", "fresh": true}}),
        json!({"id": "c", "command": "search_files",
            "args": {"workspace": "/w", "text": "t"}}),
        json!({"id": "c", "command": "device_login", "args": {"name": "d"}}),
        json!({"id": "c", "command": "device_login_result", "args": {}}),
        json!({"id": "c", "command": "device_login_cancel", "args": {}}),
        json!({"id": "c", "command": "configure",
            "args": {"operation": "settings", "workspace": "/w"}}),
        json!({"id": "c", "command": "configure", "args": {"operation": "settings"}}),
        json!({"id": "c", "command": "read_config",
            "args": {"workspace": "/w", "path": "fiber.md"}}),
        json!({"id": "c", "command": "write_config",
            "args": {"workspace": "/w", "path": "fiber.md", "text": "t", "version": "v"}}),
        json!({"id": "c", "command": "status", "args": {}}),
        json!({"id": "c", "command": "refresh", "args": {}}),
        json!({"id": "c", "command": "stop", "args": {}}),
        json!({"id": "c", "command": "placeholder_env",
            "args": {"names": ["DATABRICKS_HOST"]}}),
        json!({"id": "c", "command": "pairing_code", "args": {"device": "d"}}),
        json!({"id": "c", "command": "devices", "args": {}}),
        json!({"id": "c", "command": "revoke", "args": {"device": "d"}}),
    ]
}

#[test]
fn every_hub_sample_reads_and_writes_back_unchanged() {
    for sample in samples() {
        let line: HubCommandLine =
            serde_json::from_value(sample.clone()).unwrap_or_else(|e| panic!("{sample}: {e}"));
        assert_eq!(serde_json::to_value(&line).unwrap(), sample);
    }
}

#[test]
fn the_hub_samples_cover_every_hub_command_in_the_doc() {
    let named: BTreeSet<String> = samples()
        .iter()
        .map(|s| s["command"].as_str().unwrap().to_owned())
        .collect();
    let doc: BTreeSet<String> = doc_hub_commands().into_iter().map(str::to_owned).collect();
    assert_eq!(named, doc);
}

#[test]
fn commands_without_args_write_an_empty_args() {
    let line = HubCommandLine {
        id: CommandId("c_status".into()),
        command: HubCommand::Status(NoArgs {}),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c_status", "command": "status", "args": {}}),
    );
}

#[test]
fn a_secret_is_never_in_debug() {
    let authenticate = HubCommand::Authenticate(Authenticate {
        token: Secret::new("t-planted".into()),
    });
    assert!(
        !format!("{authenticate:?}").contains("t-planted"),
        "the token is never in Debug"
    );
    let pair = HubCommand::Pair(Pair {
        code: Secret::new("p-planted".into()),
    });
    assert!(
        !format!("{pair:?}").contains("p-planted"),
        "the code is never in Debug"
    );
    let line = HubCommandLine {
        id: CommandId("c".into()),
        command: authenticate,
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "authenticate", "args": {"token": "t-planted"}}),
    );
}

#[test]
fn delete_without_cascade_writes_no_cascade() {
    let line = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::Delete(Delete {
            session: SessionId("s_1".into()),
            cascade: None,
            expect: None,
        }),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "delete", "args": {"session": "s_1"}}),
    );
}

#[test]
fn configure_debug_never_shows_arguments() {
    let configure = Configure {
        operation: "store_key".into(),
        arguments: serde_json::Map::from_iter([("key".into(), Value::from("sk-test"))]),
    };
    let debug = format!("{configure:?}");
    assert!(!debug.contains("sk-test"), "the key is never in Debug");
    assert!(debug.contains("store_key"), "the operation stays");
}
