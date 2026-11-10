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
        json!({"id": "c", "command": "recent", "args": {"archived": true}}),
        json!({"id": "c", "command": "recent", "args": {"project": "p", "archived": false}}),
        json!({"id": "c", "command": "sessions", "args": {"project": "p"}}),
        json!({"id": "c", "command": "sessions", "args": {"project": "p", "active": true}}),
        json!({"id": "c", "command": "sessions", "args": {"archived": true}}),
        json!({"id": "c", "command": "sessions",
            "args": {"project": "p", "active": false, "archived": false}}),
        json!({"id": "c", "command": "archive", "args": {"session": "s_1"}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w", "model": "m",
            "overrides": ["a=b"], "worktree": true,
            "content": [{"type": "text", "text": "t"}]}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w"}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w", "continues": "s_2"}}),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w", "supervised": true}}),
        json!({"id": "c", "command": "delete", "args": {"session": "s_1"}}),
        json!({"id": "c", "command": "delete",
            "args": {"session": "s_1", "cascade": true, "expect": ["s_2"]}}),
        json!({"id": "c", "command": "delete",
            "args": {"session": "s_1", "dry_run": true}}),
        json!({"id": "c", "command": "prune", "args": {}}),
        json!({"id": "c", "command": "prune",
            "args": {"older_than": "30d", "cascade": true, "force": false, "dry_run": true,
                "expect": {"sessions": [{"id": "s_9", "cascade": ["s_9"]}],
                    "worktrees": [], "pinned": [], "diagnostics": []}}}),
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
        json!({"id": "c", "command": "read_workspace_file",
            "args": {"workspace": "/w", "path": "src/main.rs"}}),
        json!({"id": "c", "command": "write_workspace_file",
            "args": {"workspace": "/w", "path": "src/main.rs", "text": "t", "version": "v"}}),
        json!({"id": "c", "command": "status", "args": {}}),
        json!({"id": "c", "command": "refresh", "args": {}}),
        json!({"id": "c", "command": "stop", "args": {}}),
        json!({"id": "c", "command": "stop", "args": {"sessions": true}}),
        json!({"id": "c", "command": "stop_keepers", "args": {"keepers": ["s_1", "s_2"]}}),
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
fn archive_reads_a_session_and_refuses_anything_else() {
    for (args, expected) in [
        (r#"{"session":"s_1"}"#, Ok(())),
        (r#"{}"#, Err(())),
        (r#"{"session":null}"#, Err(())),
        (r#"{"session":1}"#, Err(())),
        (r#"{"session":"s_1","future":1}"#, Err(())),
        (r#"{"future":1}"#, Err(())),
    ] {
        let sample = serde_json::json!({
            "id": "c",
            "command": "archive",
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        match expected {
            Ok(()) => {
                let read = serde_json::from_value::<HubCommandLine>(sample.clone())
                    .unwrap_or_else(|e| panic!("{sample}: {e}"));
                assert_eq!(
                    read.command,
                    HubCommand::Archive(Archive {
                        session: SessionId("s_1".into()),
                    }),
                    "{sample}"
                );
                assert_eq!(serde_json::to_value(&read).unwrap(), sample, "{sample}");
            }
            Err(()) => assert!(
                serde_json::from_value::<HubCommandLine>(sample.clone()).is_err(),
                "{sample}"
            ),
        }
    }
    let full = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::Archive(Archive {
            session: SessionId("s_1".into()),
        }),
    };
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        json!({"id": "c", "command": "archive", "args": {"session": "s_1"}})
    );
}

#[test]
fn recent_and_sessions_read_archived_absent_false_and_true() {
    for (command, args, expected) in [
        ("recent", r#"{}"#, None),
        ("recent", r#"{"archived":true}"#, Some(true)),
        ("recent", r#"{"archived":false}"#, Some(false)),
        ("sessions", r#"{}"#, None),
        ("sessions", r#"{"archived":true}"#, Some(true)),
        ("sessions", r#"{"archived":false}"#, Some(false)),
    ] {
        let sample = serde_json::json!({
            "id": "c",
            "command": command,
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        let read = serde_json::from_value::<HubCommandLine>(sample.clone())
            .unwrap_or_else(|e| panic!("{sample}: {e}"));
        let archived = if let HubCommand::Recent(recent) = &read.command {
            recent.archived
        } else if let HubCommand::Sessions(args) = &read.command {
            args.archived
        } else {
            panic!("{sample} reads as {:?}", read.command)
        };
        assert_eq!(archived, expected, "{sample}");
        assert_eq!(serde_json::to_value(&read).unwrap(), sample, "{sample}");
    }
}

#[test]
fn recent_and_sessions_read_a_null_archived_as_absent() {
    // The hub boundary refuses an explicit null from its raw args map;
    // the typed struct reads it as absent, like every other optional
    // boolean, so a client struct never writes one.
    for command in ["recent", "sessions"] {
        let sample = serde_json::json!({
            "id": "c",
            "command": command,
            "args": {"archived": null},
        });
        let read = serde_json::from_value::<HubCommandLine>(sample.clone())
            .unwrap_or_else(|e| panic!("{sample}: {e}"));
        let archived = if let HubCommand::Recent(recent) = &read.command {
            recent.archived
        } else if let HubCommand::Sessions(args) = &read.command {
            args.archived
        } else {
            panic!("{sample} reads as {:?}", read.command)
        };
        assert_eq!(archived, None, "{sample}");
    }
}

#[test]
fn recent_and_sessions_refuse_a_misshaped_archived() {
    for (command, args) in [
        ("recent", r#"{"archived":"yes"}"#),
        ("recent", r#"{"archived":1}"#),
        ("recent", r#"{"archived":true,"future":1}"#),
        ("sessions", r#"{"archived":"yes"}"#),
        ("sessions", r#"{"archived":1}"#),
        ("sessions", r#"{"archived":true,"future":1}"#),
    ] {
        let sample = serde_json::json!({
            "id": "c",
            "command": command,
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        assert!(
            serde_json::from_value::<HubCommandLine>(sample.clone()).is_err(),
            "{sample}"
        );
    }
}

#[test]
fn sessions_without_archived_writes_no_archived() {
    let line = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::Sessions(SessionsArgs {
            project: None,
            active: None,
            archived: None,
        }),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "sessions", "args": {}}),
    );
    let line = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::Recent(Recent {
            before: None,
            project: None,
            archived: None,
        }),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "recent", "args": {}}),
    );
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
            dry_run: None,
        }),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "delete", "args": {"session": "s_1"}}),
    );
}

#[test]
fn stop_reads_sessions_and_refuses_anything_else() {
    for (args, expected) in [
        (r#"{}"#, Ok(None)),
        (r#"{"sessions":true}"#, Ok(Some(true))),
        (r#"{"sessions":false}"#, Ok(Some(false))),
        (r#"{"sessions":null}"#, Err(())),
        (r#"{"sessions":"yes"}"#, Err(())),
        (r#"{"sessions":1}"#, Err(())),
        (r#"{"sessions":true,"future":1}"#, Err(())),
        (r#"{"future":1}"#, Err(())),
    ] {
        let sample = serde_json::json!({
            "id": "c",
            "command": "stop",
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        match expected {
            Ok(sessions) => assert_eq!(
                serde_json::from_value::<HubCommandLine>(sample.clone())
                    .unwrap_or_else(|e| panic!("{sample}: {e}"))
                    .command,
                HubCommand::Stop(StopArgs { sessions }),
                "{sample}"
            ),
            Err(()) => assert!(
                serde_json::from_value::<HubCommandLine>(sample.clone()).is_err(),
                "{sample}"
            ),
        }
    }
    let absent: HubCommandLine = serde_json::from_value(json!({
        "id": "c",
        "command": "stop",
        "args": {},
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&absent).unwrap(),
        json!({"id": "c", "command": "stop", "args": {}})
    );
}

#[test]
fn stop_keepers_reads_id_arrays_and_refuses_anything_else() {
    let kept = ["s_1", "s_2"].map(|id| SessionId(id.into()));
    for (args, expected) in [
        (
            r#"{"keepers":["s_1","s_2"]}"#,
            Ok(HubCommand::StopKeepers(StopKeepers {
                keepers: kept.to_vec(),
            })),
        ),
        (r#"{}"#, Err(())),
        (r#"{"keepers":null}"#, Err(())),
        (r#"{"keepers":"s_1"}"#, Err(())),
        (r#"{"keepers":[1]}"#, Err(())),
        (r#"{"keepers":[]}"#, Err(())),
        (r#"{"keepers":["s_1"],"future":1}"#, Err(())),
        (r#"{"future":1}"#, Err(())),
    ] {
        let sample = serde_json::json!({
            "id": "c",
            "command": "stop_keepers",
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        match expected {
            Ok(command) => assert_eq!(
                serde_json::from_value::<HubCommandLine>(sample.clone())
                    .unwrap_or_else(|e| panic!("{sample}: {e}"))
                    .command,
                command,
                "{sample}"
            ),
            Err(()) => assert!(
                serde_json::from_value::<HubCommandLine>(sample.clone()).is_err(),
                "{sample}"
            ),
        }
    }
    let full = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::StopKeepers(StopKeepers {
            keepers: kept.to_vec(),
        }),
    };
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        json!({"id": "c", "command": "stop_keepers", "args": {"keepers": ["s_1", "s_2"]}})
    );
}

#[test]
fn stop_names_the_sessions_shape_in_its_null_refusal() {
    let null = serde_json::from_value::<HubCommandLine>(json!({
        "id": "c",
        "command": "stop",
        "args": {"sessions": null},
    }))
    .unwrap_err()
    .to_string();
    assert!(
        null.contains("`sessions` is null, which stop refuses"),
        "null refusal names the shape: {null}"
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

#[test]
fn start_reads_supervised_and_refuses_a_misshaped_one() {
    for (args, expected) in [
        (r#"{"workspace":"/w"}"#, Ok(None)),
        (r#"{"workspace":"/w","supervised":true}"#, Ok(Some(true))),
        (r#"{"workspace":"/w","supervised":false}"#, Ok(Some(false))),
        (r#"{"workspace":"/w","supervised":"yes"}"#, Err(())),
        (r#"{"workspace":"/w","supervised":1}"#, Err(())),
        (
            r#"{"workspace":"/w","supervised":true,"future":1}"#,
            Err(()),
        ),
    ] {
        let sample = json!({
            "id": "c",
            "command": "start",
            "args": serde_json::from_str::<Value>(args).unwrap(),
        });
        let read = serde_json::from_value::<HubCommandLine>(sample.clone());
        match expected {
            Ok(supervised) => {
                let HubCommand::Start(start) =
                    read.unwrap_or_else(|e| panic!("{sample}: {e}")).command
                else {
                    panic!("{sample} reads as start");
                };
                assert_eq!(start.supervised, supervised, "{sample}");
            }
            Err(()) => assert!(read.is_err(), "{sample}"),
        }
    }
}

#[test]
fn start_without_supervised_writes_no_supervised() {
    let line = HubCommandLine {
        id: CommandId("c".into()),
        command: HubCommand::Start(Start {
            workspace: "/w".into(),
            model: None,
            overrides: None,
            worktree: None,
            continues: None,
            supervised: None,
            content: None,
        }),
    };
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        json!({"id": "c", "command": "start", "args": {"workspace": "/w"}}),
    );
}
