//! Command-line details a socket test cannot see.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

#[allow(clippy::duplicate_mod, reason = "unit tests share the socket fixture")]
#[path = "../tests/support/mod.rs"]
mod support;

/// Tests for the `import` driver command. Each runs in a child process
/// with a private `HOME` holding fake source homes: the process
/// environment cannot be pointed at a temporary home in-process
/// (`unsafe_code` is denied), so the parent builds the person home,
/// re-runs the test with `HOME` pointing at it, and the child drives
/// the session and exits. A child panic fails the parent through its
/// exit code.
mod import_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helpers; a failure is the test's"
    )]

    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use fakes::{Client, Deadline, TempDir};
    use serde_json::Value;

    use super::support::{Opened, kind, rejection, response, send, subscribe};

    /// The child's case marker: set, the test runs the named case in the
    /// child and returns, so the parent proves the dispatch against a
    /// private `HOME`.
    const CHILD: &str = "FIBER_DOORS_IMPORT_CHILD";

    /// How long a re-run child may run: opening a session and importing
    /// local files only. Twice it stays inside half of nextest's per-test
    /// timeout (`docs/testing.md`, "Waits and timeouts").
    const CHILD_WITHIN: Duration = Duration::from_secs(30);

    fn child_name() -> String {
        module_path!().split_once("::").unwrap().1.to_owned()
    }

    /// Writes `content` to `rel` under `root`, creating parents.
    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A fake `~/.claude`: a skill, a flat command, instruction content,
    /// two mapped servers and an unmapped secret.
    fn claude_home(root: &Path) {
        write(
            root,
            ".claude/skills/review/SKILL.md",
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        );
        write(root, ".claude/commands/greet.md", "Hello there.\n");
        write(root, ".claude/CLAUDE.md", "Be kind.\n");
        write(
            root,
            ".claude/settings.json",
            &serde_json::json!({
                "model": "opus",
                "mcpServers": {
                    "notes": {"command": "notes-server", "args": ["--fast"]},
                    "remote": {"url": "https://mcp.example.com/x"},
                },
            })
            .to_string(),
        );
        write(root, ".claude/auth.json", "{\"token\": \"x\"}\n");
    }

    /// A fake `~/.codex`: a skill, a flat prompt, instruction content, two
    /// mapped servers and a secret.
    fn codex_home(root: &Path) {
        write(
            root,
            ".codex/skills/lint/SKILL.md",
            "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
        );
        write(root, ".codex/prompts/plan.md", "Plan it.\n");
        write(root, ".codex/AGENTS.md", "Be thorough.\n");
        write(
            root,
            ".codex/config.toml",
            "[mcp_servers.alpha]\n\
             command = \"alpha-server\"\n\
             \n\
             [mcp_servers.beta]\n\
             url = \"https://beta.example.com/mcp\"\n",
        );
        write(root, ".codex/auth.json", "{\"token\": \"x\"}\n");
    }

    /// The accepted `result` of the `id` command.
    #[track_caller]
    fn accepted(client: &Client, id: &str) -> Value {
        let line = response(client, id);
        assert_eq!(kind(&line), "command_accepted", "{line}");
        line["payload"]["result"].clone()
    }

    /// The names the import copied, in answer order.
    fn imported(result: &Value) -> (Vec<&str>, Vec<&str>, Vec<&str>, Vec<&str>) {
        let imported = &result["imported"];
        (
            strings(&imported["skills"]),
            strings(&imported["commands"]),
            strings(&imported["instructions"]),
            strings(&imported["mcp_servers"]),
        )
    }

    fn strings(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect()
    }

    /// Opens a session, subscribes, and runs `drive` with the client and
    /// the Fiber home. Wire assertions happen inside `run`; disk checks
    /// after `close` keep the temporary home alive through the returned
    /// holder.
    /// Opens a session, subscribes, and runs `drive` with the client and
    /// the Fiber home. Every wire and disk assertion happens inside `run`,
    /// before `close` removes the temporary home.
    #[track_caller]
    fn with_session(drive: impl FnOnce(&Client, &Path)) {
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        let home = opened.home.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |_| {
                let client = Client::connect(&socket).unwrap();
                subscribe(&client, "sub", "summary");
                drive(&client, &home);
                Ok(())
            })
            .unwrap();
        opened.close();
    }

    /// Re-runs the current test in a child with `HOME` pointing at
    /// `person`: the child's exit code proves the case.
    #[track_caller]
    fn parent(test: &str, case: &str, person: Option<&Path>) {
        let output = fakes::rerun_prepared(
            &format!("{}::{test}", child_name()),
            &[(CHILD, case)],
            Deadline::after(CHILD_WITHIN),
            |command| match person {
                Some(dir) => {
                    command.env("HOME", dir);
                }
                None => {
                    command.env_remove("HOME");
                }
            },
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "child {case} failed: stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// The child's entry: the parent's `HOME` holds the fake person home
    /// the case reads.
    fn run_child(case: &str) {
        match case {
            "bare" => bare(),
            "one" => one(),
            "unknown" => unknown(),
            "failed" => failed(),
            "missing" => missing(),
            "nohome" => nohome(),
            other => panic!("unknown import case {other}"),
        }
    }

    /// A bare `import` runs both sources: the accepted report names every
    /// landed item, the receipts exist, and the files match the sources.
    #[track_caller]
    fn bare() {
        with_session(|client, home| {
            send(client, r#"{"id":"imp","command":"import"}"#);
            let result = accepted(client, "imp");
            assert_eq!(
                imported(&result),
                (
                    vec!["review", "lint"],
                    vec!["greet", "plan"],
                    vec!["CLAUDE.md", "AGENTS.md"],
                    vec!["notes", "remote", "alpha", "beta"],
                ),
                "{result}"
            );
            let mut left: Vec<&str> = result["left_behind"]
                .as_array()
                .unwrap()
                .iter()
                .map(|left| left["item"].as_str().unwrap())
                .collect();
            left.sort();
            assert_eq!(left, vec!["auth.json", "auth.json", "settings.json model"]);
            for left in result["left_behind"].as_array().unwrap() {
                assert!(!left["reason"].as_str().unwrap().is_empty());
            }
            assert_eq!(
                std::fs::read(home.join("skills/review/SKILL.md")).unwrap(),
                b"---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n"
            );
            assert_eq!(
                std::fs::read(home.join("CLAUDE.md")).unwrap(),
                b"Be kind.\n"
            );
            assert_eq!(
                std::fs::read(home.join("AGENTS.md")).unwrap(),
                b"Be thorough.\n"
            );
            let servers: Value =
                serde_json::from_str(&std::fs::read_to_string(home.join("config.json")).unwrap())
                    .unwrap();
            assert_eq!(
                servers.pointer("/mcp/servers/notes").unwrap(),
                &serde_json::json!({"command": "notes-server", "args": ["--fast"]})
            );
            assert_eq!(
                servers.pointer("/mcp/servers/alpha").unwrap(),
                &serde_json::json!({"command": "alpha-server"})
            );
        });
    }

    /// `{"source":"codex"}` imports only Codex: no Claude receipt, no
    /// Claude files.
    #[track_caller]
    fn one() {
        with_session(|client, home| {
            send(
                client,
                r#"{"id":"imp","command":"import","args":{"source":"codex"}}"#,
            );
            let result = accepted(client, "imp");
            assert_eq!(
                imported(&result),
                (
                    vec!["lint"],
                    vec!["plan"],
                    vec!["AGENTS.md"],
                    vec!["alpha", "beta"],
                ),
                "{result}"
            );
            assert!(home.join("import-codex.json").exists());
            assert!(!home.join("import-claude-code.json").exists());
            assert!(!home.join("skills/review").exists());
        });
    }

    /// An unknown `source` is refused `invalid_arguments` naming both
    /// sources; a non-string `source` never reaches the dispatch.
    #[track_caller]
    fn unknown() {
        with_session(|client, _| {
            send(
                client,
                r#"{"id":"bad","command":"import","args":{"source":"vim"}}"#,
            );
            assert_eq!(
                rejection(&response(client, "bad")),
                (
                    "invalid_arguments",
                    "`source` must be `claude-code` or `codex`.",
                )
            );
            send(
                client,
                r#"{"id":"unfit","command":"import","args":{"source":1}}"#,
            );
            assert_eq!(rejection(&response(client, "unfit")).0, "invalid_arguments");
            send(
                client,
                r#"{"id":"extra","command":"import","args":{"future":true}}"#,
            );
            assert_eq!(rejection(&response(client, "extra")).0, "invalid_arguments");
        });
    }

    /// A failed run writes no receipt, and a retry completes: a directory
    /// where the global file goes aborts the import before anything is
    /// written, exactly as `fiber import` fails there.
    #[track_caller]
    fn failed() {
        with_session(|client, home| {
            std::fs::create_dir_all(home.join("config.json")).unwrap();
            send(
                client,
                r#"{"id":"imp1","command":"import","args":{"source":"claude-code"}}"#,
            );
            assert_eq!(rejection(&response(client, "imp1")).0, "io_failed");
            assert!(!home.join("import-claude-code.json").exists());
            assert!(!home.join("skills/review/SKILL.md").exists());
            std::fs::remove_dir_all(home.join("config.json")).unwrap();
            send(
                client,
                r#"{"id":"imp2","command":"import","args":{"source":"claude-code"}}"#,
            );
            let result = accepted(client, "imp2");
            assert_eq!(imported(&result).0, vec!["review"]);
            assert!(home.join("import-claude-code.json").exists());
            assert!(home.join("skills/review/SKILL.md").exists());
        });
    }

    /// A missing source home imports nothing: absent paths mean nothing
    /// to import, never an error, and the receipt records the empty run.
    #[track_caller]
    fn missing() {
        with_session(|client, home| {
            send(
                client,
                r#"{"id":"imp","command":"import","args":{"source":"codex"}}"#,
            );
            let result = accepted(client, "imp");
            assert_eq!(
                imported(&result),
                (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
                "{result}"
            );
            assert!(result["left_behind"].as_array().unwrap().is_empty());
            assert!(home.join("import-codex.json").exists());
        });
    }

    /// Without `HOME` the dispatch refuses `io_failed`, as `fiber import`
    /// does, rather than reading the wrong home.
    #[track_caller]
    fn nohome() {
        assert!(std::env::var_os("HOME").is_none());
        with_session(|client, _| {
            send(client, r#"{"id":"imp","command":"import"}"#);
            assert_eq!(
                rejection(&response(client, "imp")),
                (
                    "io_failed",
                    "HOME is not set, so the source home cannot be found.",
                )
            );
        });
    }

    #[test]
    fn bare_import_runs_both_sources() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = TempDir::new("fiber-import-person");
        claude_home(person.path());
        codex_home(person.path());
        parent("bare_import_runs_both_sources", "bare", Some(person.path()));
    }

    #[test]
    fn sourced_import_runs_one_source_only() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = TempDir::new("fiber-import-person");
        claude_home(person.path());
        codex_home(person.path());
        parent(
            "sourced_import_runs_one_source_only",
            "one",
            Some(person.path()),
        );
    }

    #[test]
    fn unknown_source_is_refused_naming_both_sources() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = TempDir::new("fiber-import-person");
        parent(
            "unknown_source_is_refused_naming_both_sources",
            "unknown",
            Some(person.path()),
        );
    }

    #[test]
    fn failed_import_writes_no_receipt_and_retry_completes() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = TempDir::new("fiber-import-person");
        claude_home(person.path());
        parent(
            "failed_import_writes_no_receipt_and_retry_completes",
            "failed",
            Some(person.path()),
        );
    }

    #[test]
    fn missing_source_home_imports_nothing() {
        if let Some(case) = std::env::var_os(CHILD) {
            run_child(case.to_str().unwrap());
            return;
        }
        let person = TempDir::new("fiber-import-person");
        parent(
            "missing_source_home_imports_nothing",
            "missing",
            Some(person.path()),
        );
    }

    #[test]
    fn missing_home_is_refused_io_failed() {
        if std::env::var_os(CHILD).is_some() {
            nohome();
            return;
        }
        parent("missing_home_is_refused_io_failed", "nohome", None);
    }
}

#[test]
fn reload_delivers_an_ack_that_accepts_rejects_or_closes_when_dropped() {
    use contract::ErrorCode;
    use contract::inbox::{Delivery, Rejection};
    use fakes::{Client, Deadline};
    use std::sync::Arc;
    use support::{DEADLINE, Opened, rejection, response, send};
    for answer in ["accept", "reject", "drop"] {
        let opened = Opened::open(Vec::new());
        let socket = opened.socket.clone();
        opened
            .session
            .run(Vec::new(), Arc::new(|| false), move |inbox| {
                let client = Client::connect(&socket).unwrap();
                send(
                    &client,
                    r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
                );
                send(&client, r#"{"id":"reload","command":"reload"}"#);
                let Delivery::Reload(ack) = Deadline::after(DEADLINE)
                    .recv(&inbox)
                    .expect("reload reached the inbox")
                else {
                    panic!("reload delivery");
                };
                match answer {
                    "accept" => ack.0(Ok(None)),
                    "reject" => ack.0(Err(Rejection {
                        code: ErrorCode::ConfigInvalid,
                        message: "bad config file".into(),
                    })),
                    _ => drop(ack),
                }
                let line = response(&client, "reload");
                match answer {
                    "accept" => {
                        assert_eq!(support::kind(&line), "command_accepted");
                        assert!(line["payload"].get("result").is_none());
                    }
                    "reject" => assert_eq!(rejection(&line), ("config_invalid", "bad config file")),
                    _ => assert_eq!(rejection(&line).0, "closing"),
                }
                send(
                    &client,
                    r#"{"id":"args","command":"reload","args":{"unexpected":true}}"#,
                );
                assert_eq!(rejection(&response(&client, "args")).0, "invalid_arguments");
                Ok(())
            })
            .unwrap();
        opened.close();
    }
}

#[test]
fn mcp_servers_answers_the_installed_snapshot_rows() {
    use contract::events::{McpServerLayer, McpServerRow, McpServerState};
    use fakes::Client;
    use std::sync::Arc;
    use support::{Opened, response, send};
    let rows = vec![
        McpServerRow {
            name: "down".into(),
            layer: McpServerLayer::Global,
            state: McpServerState::Failed,
            reason: "boom".into(),
            tools: Some(0),
            approved: true,
            auth_url: None,
        },
        McpServerRow {
            name: "remote".into(),
            layer: McpServerLayer::Repository,
            state: McpServerState::NeedsLogin,
            reason: "log in".into(),
            tools: Some(3),
            approved: false,
            auth_url: Some("https://auth.example/authorize".into()),
        },
    ];
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .mcp_servers(Arc::new(move |_tools| Ok(rows.clone())));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(
                &client,
                r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            send(&client, r#"{"id":"list","command":"mcp_servers"}"#);
            let line = response(&client, "list");
            assert_eq!(support::kind(&line), "command_accepted");
            assert_eq!(
                line["payload"]["result"]["servers"],
                serde_json::json!([
                    {
                        "name": "down",
                        "layer": "global",
                        "state": "failed",
                        "reason": "boom",
                        "tools": 0,
                        "approved": true,
                    },
                    {
                        "name": "remote",
                        "layer": "repository",
                        "state": "needs-login",
                        "reason": "log in",
                        "tools": 3,
                        "approved": false,
                        "auth_url": "https://auth.example/authorize",
                    },
                ])
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn mcp_login_answers_on_its_thread_without_holding_the_reader() {
    use fakes::Client;
    use std::sync::Arc;
    use support::{Opened, response, send};
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held = std::sync::Mutex::new(held);
    let (entered_tx, entered) = std::sync::mpsc::channel::<()>();
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .mcp_servers(Arc::new(move |_tools| Ok(Vec::new())));
    opened.session.mcp_login(Arc::new(move |_server, _deliver| {
        entered_tx.send(()).unwrap();
        held.lock().unwrap().recv().unwrap();
        Ok(())
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(
                &client,
                r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            send(
                &client,
                r#"{"id":"first","command":"mcp_login","args":{"server":"remote"}}"#,
            );
            // The login parks on its own thread; the reader still
            // answers other commands while it waits.
            entered.recv().unwrap();
            send(&client, r#"{"id":"list","command":"mcp_servers"}"#);
            let list = response(&client, "list");
            assert_eq!(support::kind(&list), "command_accepted", "{list}");
            release.send(()).unwrap();
            assert_eq!(
                support::kind(&response(&client, "first")),
                "command_accepted"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn mcp_login_maps_a_busy_failure_to_a_busy_rejection() {
    use contract::ErrorCode;
    use fakes::Client;
    use std::sync::Arc;
    use support::{Opened, rejection, response, send};
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened.session.mcp_login(Arc::new(|_server, _deliver| {
        Err(contract::shapes::Failure {
            code: ErrorCode::Busy,
            message: "in flight".into(),
            retry_after_ms: None,
            provider: None,
        })
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(
                &client,
                r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            send(
                &client,
                r#"{"id":"again","command":"mcp_login","args":{"server":"remote"}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "again")),
                ("busy", "in flight")
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn mcp_login_maps_unknown_and_unapproved_servers() {
    use contract::ErrorCode;
    use fakes::Client;
    use std::sync::Arc;
    use support::{Opened, rejection, response, send};
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened.session.mcp_login(Arc::new(|server, _deliver| {
        if server == "repo" {
            return Err(contract::shapes::Failure {
                code: ErrorCode::McpServerUnapproved,
                message: "`repo` is declared by the repository but not approved; run `fiber approve` in the repository.".into(),
                retry_after_ms: None,
                provider: None,
            });
        }
        Err(contract::shapes::Failure {
            code: ErrorCode::InvalidArguments,
            message: format!("unknown server `{server}`"),
            retry_after_ms: None,
            provider: None,
        })
    }));
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |_inbox| {
            let client = Client::connect(&socket).unwrap();
            send(
                &client,
                r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            send(
                &client,
                r#"{"id":"nope","command":"mcp_login","args":{"server":"nope"}}"#,
            );
            assert_eq!(
                rejection(&response(&client, "nope")),
                ("invalid_arguments", "unknown server `nope`")
            );
            send(
                &client,
                r#"{"id":"unapproved","command":"mcp_login","args":{"server":"repo"}}"#,
            );
            let denied = response(&client, "unapproved");
            let (code, message) = rejection(&denied);
            assert_eq!(code, "mcp_server_unapproved");
            assert!(message.contains("fiber approve"), "{message}");
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn mcp_reconnect_delivers_the_server_to_the_inbox() {
    use contract::inbox::Delivery;
    use fakes::{Client, Deadline};
    use std::sync::Arc;
    use support::{DEADLINE, Opened, response, send};
    let opened = Opened::open(Vec::new());
    let socket = opened.socket.clone();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), move |inbox| {
            let client = Client::connect(&socket).unwrap();
            send(
                &client,
                r#"{"id":"sub","command":"subscribe","args":{"level":"summary"}}"#,
            );
            send(
                &client,
                r#"{"id":"again","command":"mcp_reconnect","args":{"server":"flaky"}}"#,
            );
            let Delivery::McpReconnect(args, ack) = Deadline::after(DEADLINE)
                .recv(&inbox)
                .expect("the reconnect arrives")
            else {
                panic!("mcp_reconnect delivers");
            };
            assert_eq!(args.server, "flaky");
            ack.0(Ok(None));
            assert_eq!(
                support::kind(&response(&client, "again")),
                "command_accepted"
            );
            Ok(())
        })
        .unwrap();
    opened.close();
}

#[test]
fn until_carries_the_shared_must_succeed_bound() {
    assert_eq!(
        support::UNTIL,
        fakes::MUST_SUCCEED_WITHIN,
        "until's whole-wait bound matches the shared must-succeed bound"
    );
}

/// Tests for the session-socket attribution boundary: the exact session
/// argv shape, foreign-owner skipping without argument reads, and the
/// unreadable pid in an incomplete walk.
mod attribution_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helpers; a failure is the test's"
    )]

    use std::collections::HashMap;

    use super::super::{
        Attribution, FromSessionRefusal, attribute_peer, from_session_message, session_argv_id,
        session_owners, unreadable_target_message,
    };

    fn ask_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "ask", "--prompt=hi"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    fn extension_case_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "extension-case", "casefile"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    /// An `ask` or `extension-case` above the peer owns it without any
    /// session id: the argv-only predicate recognizes the session host.
    fn host_walk(argv: Vec<Vec<u8>>) -> OwnerScript {
        OwnerScript {
            current: 1000,
            parents: [(10, 20), (20, 1), (1, 0)].into_iter().collect(),
            euids: [(20, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(20, argv), (1, vec![b"/sbin/init".to_vec()])]
                .into_iter()
                .collect(),
            unreadable_euid: vec![],
            unreadable_argv: vec![],
            argv_read: std::cell::Cell::new(0),
        }
    }

    #[test]
    fn ask_argv_on_the_caller_walk_makes_the_peer_owned() {
        let attribution = attribute_peer(Ok(10), &host_walk(ask_argv()));
        let Attribution::Local { owners, .. } = attribution else {
            panic!("an ask descendant is a local owned peer");
        };
        assert!(!owners.is_empty(), "an ask argv owns the peer");
    }

    #[test]
    fn extension_case_argv_on_the_caller_walk_makes_the_peer_owned() {
        let attribution = attribute_peer(Ok(10), &host_walk(extension_case_argv()));
        let Attribution::Local { owners, .. } = attribution else {
            panic!("an extension-case descendant is a local owned peer");
        };
        assert!(!owners.is_empty(), "an extension-case argv owns the peer");
    }

    struct OwnerScript {
        current: u32,
        parents: HashMap<u32, u32>,
        euids: HashMap<u32, u32>,
        argvs: HashMap<u32, Vec<Vec<u8>>>,
        unreadable_euid: Vec<u32>,
        unreadable_argv: Vec<u32>,
        argv_read: std::cell::Cell<usize>,
    }

    impl support::peer::ProcessRead for OwnerScript {
        fn current_euid(&self) -> Result<u32, support::peer::ReadError> {
            Ok(self.current)
        }

        fn euid(&self, pid: u32) -> Result<u32, support::peer::ReadError> {
            if self.unreadable_euid.contains(&pid) {
                return Err(support::peer::ReadError::Unreadable { pid, what: "euid" });
            }
            self.euids
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "euid" })
        }

        fn parent(&self, pid: u32) -> Result<u32, support::peer::ReadError> {
            self.parents
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable {
                    pid,
                    what: "parent",
                })
        }

        fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, support::peer::ReadError> {
            self.argv_read.set(self.argv_read.get() + 1);
            if self.unreadable_argv.contains(&pid) {
                return Err(support::peer::ReadError::Unreadable { pid, what: "argv" });
            }
            self.argvs
                .get(&pid)
                .cloned()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "argv" })
        }
    }

    fn session_argv(id: &str) -> Vec<Vec<u8>> {
        [
            "/path/to/fiber",
            "session",
            &format!("--id={id}"),
            "--workspace=/w",
        ]
        .iter()
        .map(|word| word.as_bytes().to_vec())
        .collect()
    }

    #[test]
    fn session_argv_id_matches_only_the_session_shape() {
        let id = "s_0123456789abcdef";
        assert_eq!(
            session_argv_id(&session_argv(id)),
            Some(contract::SessionId(id.to_owned()))
        );
        let mut reordered = session_argv(id);
        reordered.swap(2, 3);
        assert_eq!(
            session_argv_id(&reordered),
            Some(contract::SessionId(id.to_owned()))
        );
        for argv in [
            vec![],
            vec![b"fiber".to_vec()],
            vec![b"fiber".to_vec(), b"ask".to_vec()],
            vec![b"fiber".to_vec(), b"session".to_vec()],
            vec![
                b"fiber".to_vec(),
                b"sessionx".to_vec(),
                b"--id=s_0123456789abcdef".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"session".to_vec(),
                b"--idx=s_0123456789abcdef".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"session".to_vec(),
                b"--id=s_SHORT".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"session".to_vec(),
                b"--id=s_0123456789abcde".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"session".to_vec(),
                b"--id=s_0123456789abcdef0".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"session".to_vec(),
                b"--id=s_0123456789ABCDEF".to_vec(),
            ],
            vec![
                b"fiber".to_vec(),
                b"sh".to_vec(),
                b"-c".to_vec(),
                b"session --id=s_0123456789abcdef".to_vec(),
            ],
        ] {
            assert_eq!(session_argv_id(&argv), None, "{argv:?}");
        }
    }

    #[test]
    fn session_owners_skips_foreign_argv_and_names_unreadable_pids() {
        let id = "s_0123456789abcdef";
        let read = OwnerScript {
            current: 1000,
            parents: [(10, 20), (20, 30), (30, 1), (1, 0)].into_iter().collect(),
            euids: [(20, 1000), (30, 2000), (1, 1000)].into_iter().collect(),
            argvs: [(20, session_argv(id)), (1, session_argv(id))]
                .into_iter()
                .collect(),
            unreadable_euid: vec![],
            unreadable_argv: vec![],
            argv_read: std::cell::Cell::new(0),
        };
        assert_eq!(
            session_owners(10, &read).unwrap(),
            [
                contract::SessionId(id.to_owned()),
                contract::SessionId(id.to_owned())
            ]
        );
        assert_eq!(read.argv_read.get(), 2);
        let unreadable = OwnerScript {
            current: 1000,
            parents: [(10, 20), (20, 1), (1, 0)].into_iter().collect(),
            euids: [(20, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(1, session_argv(id))].into_iter().collect(),
            unreadable_euid: vec![20],
            unreadable_argv: vec![],
            argv_read: std::cell::Cell::new(0),
        };
        assert_eq!(
            session_owners(10, &unreadable).unwrap_err(),
            support::peer::IncompleteWalk { pid: 20 }
        );
        let unreadable = OwnerScript {
            current: 1000,
            parents: [(10, 20), (20, 1), (1, 0)].into_iter().collect(),
            euids: [(20, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(1, session_argv(id))].into_iter().collect(),
            unreadable_euid: vec![],
            unreadable_argv: vec![20],
            argv_read: std::cell::Cell::new(0),
        };
        assert_eq!(
            session_owners(10, &unreadable).unwrap_err(),
            support::peer::IncompleteWalk { pid: 20 }
        );
    }

    #[test]
    fn refusal_messages_name_the_owner_target_pid_and_peer_error() {
        let owner = contract::SessionId("s_0123456789abcdef".to_owned());
        let target = contract::SessionId("s_fedcba9876543210".to_owned());
        let owned = Attribution::Local {
            peer_pid: 555,
            owners: vec![owner.clone()],
        };
        let approval = from_session_message(&owned, FromSessionRefusal::Approval);
        assert!(approval.contains("s_0123456789abcdef"), "{approval}");
        let command = from_session_message(
            &owned,
            FromSessionRefusal::SessionCommand {
                command: "steer",
                target: &target,
            },
        );
        assert!(command.contains("s_0123456789abcdef"), "{command}");
        assert!(command.contains("s_fedcba9876543210"), "{command}");
        let incomplete = Attribution::Incomplete {
            peer_pid: Some(555),
            pid: Some(999),
            peer_error: None,
        };
        let incomplete = from_session_message(&incomplete, FromSessionRefusal::Approval);
        assert!(incomplete.contains("999"), "{incomplete}");
        let unknown = Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some("injected credential failure".to_owned()),
        };
        let unknown = from_session_message(&unknown, FromSessionRefusal::Approval);
        assert!(unknown.contains("injected credential failure"), "{unknown}");
        let unreadable = unreadable_target_message(&target, "steer");
        assert!(unreadable.contains("s_fedcba9876543210"), "{unreadable}");
    }
}

/// Refusal-predicate tests at the dispatch seam: fabricated caller
/// attribution with scripted target reads, covering every command class on
/// the own session, a delegate, and an unrelated session.
mod refusal_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helpers; a failure is the test's"
    )]

    use std::collections::HashMap;

    use super::super::{Attribution, attribute_peer, session_command_refusal};
    use contract::RequestId;
    use contract::SessionId;
    use contract::commands::{Command, ContentArgs, Reply, ReplyAnswer, SentPart};
    use contract::events::Decision;

    const PEER: u32 = 555;
    const TARGET_PID: u32 = 777;
    const UNREADABLE: u32 = 999;
    const OWNED: &str = "s_0123456789abcdef";
    const OTHER: &str = "s_fedcba9876543210";

    fn owned_id() -> SessionId {
        SessionId(OWNED.to_owned())
    }

    fn other_id() -> SessionId {
        SessionId(OTHER.to_owned())
    }

    /// Scripted kernel reads: the refusal paths answer from these tables,
    /// and a missing entry reads as unreadable.
    #[derive(Default)]
    struct Script {
        current: u32,
        parents: HashMap<u32, u32>,
        euids: HashMap<u32, u32>,
        argvs: HashMap<u32, Vec<Vec<u8>>>,
    }

    impl support::peer::ProcessRead for Script {
        fn current_euid(&self) -> Result<u32, support::peer::ReadError> {
            Ok(self.current)
        }

        fn euid(&self, pid: u32) -> Result<u32, support::peer::ReadError> {
            self.euids
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "euid" })
        }

        fn parent(&self, pid: u32) -> Result<u32, support::peer::ReadError> {
            self.parents
                .get(&pid)
                .copied()
                .ok_or(support::peer::ReadError::Unreadable {
                    pid,
                    what: "parent",
                })
        }

        fn argv(&self, pid: u32) -> Result<Vec<Vec<u8>>, support::peer::ReadError> {
            self.argvs
                .get(&pid)
                .cloned()
                .ok_or(support::peer::ReadError::Unreadable { pid, what: "argv" })
        }
    }

    fn session_argv(id: &str) -> Vec<Vec<u8>> {
        [
            "/path/to/fiber",
            "session",
            &format!("--id={id}"),
            "--workspace=/w",
        ]
        .iter()
        .map(|word| word.as_bytes().to_vec())
        .collect()
    }

    fn ask_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "ask", "--prompt=hi"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    fn extension_case_argv() -> Vec<Vec<u8>> {
        ["/path/to/fiber", "extension-case", "casefile"]
            .iter()
            .map(|word| word.as_bytes().to_vec())
            .collect()
    }

    /// The target's walk with no session above it: the target session
    /// itself decides.
    fn plain_target() -> Script {
        Script {
            current: 1000,
            parents: [(TARGET_PID, 1), (1, 0)].into_iter().collect(),
            euids: [(TARGET_PID, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(1, vec![b"/sbin/init".to_vec()])].into_iter().collect(),
        }
    }

    fn owned(peer: u32, id: &str) -> Attribution {
        Attribution::Local {
            peer_pid: peer,
            owners: vec![SessionId(id.to_owned())],
        }
    }

    fn incomplete(peer: Option<u32>, pid: Option<u32>) -> Attribution {
        Attribution::Incomplete {
            peer_pid: peer,
            pid,
            peer_error: None,
        }
    }

    fn prompt() -> Command {
        Command::Prompt(ContentArgs {
            content: vec![SentPart::Text { text: "hi".into() }],
        })
    }

    fn steer() -> Command {
        Command::Steer(ContentArgs {
            content: vec![SentPart::Text { text: "hi".into() }],
        })
    }

    fn approval_reply() -> Command {
        Command::Reply(Reply {
            request_id: RequestId("r_1".into()),
            answer: ReplyAnswer::Approval {
                decision: Decision::Allow,
                feedback: None,
                remember: None,
            },
        })
    }

    fn question_reply() -> Command {
        Command::Reply(Reply {
            request_id: RequestId("r_1".into()),
            answer: ReplyAnswer::Confirmed { confirmed: true },
        })
    }

    fn labels_reply() -> Command {
        Command::Reply(Reply {
            request_id: RequestId("r_1".into()),
            answer: ReplyAnswer::Labels {
                labels: vec!["a".into()],
            },
        })
    }

    fn message() -> Command {
        Command::Message(contract::commands::Message {
            from_session_id: owned_id(),
            parent_session_id: None,
            text: "hi".into(),
        })
    }

    fn refused(
        attribution: &Attribution,
        command: &Command,
        target: &SessionId,
        read: &Script,
    ) -> Option<super::super::Refused> {
        session_command_refusal(attribution, command, target, TARGET_PID, read)
    }

    #[test]
    fn an_approval_reply_from_an_owned_peer_is_refused_on_any_session() {
        let script = plain_target();
        for target in [owned_id(), other_id()] {
            let refused = refused(&owned(PEER, OWNED), &approval_reply(), &target, &script)
                .expect("an approval answer is refused");
            assert_eq!(refused.code, contract::ErrorCode::FromSession);
            assert!(refused.message.contains(OWNED), "{}", refused.message);
        }
    }

    #[test]
    fn an_approval_reply_has_no_parent_exception() {
        // The peer is the target's kernel parent, which exempts refusal 2:
        // refusal 1 still fires.
        let script = Script {
            parents: [(TARGET_PID, PEER)].into_iter().collect(),
            ..Default::default()
        };
        let refused = refused(&owned(PEER, OWNED), &approval_reply(), &other_id(), &script)
            .expect("an approval answer is refused without exception");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
        assert!(refused.message.contains(OWNED), "{}", refused.message);
    }

    #[test]
    fn a_question_reply_to_the_callers_own_session_is_refused() {
        let script = plain_target();
        for command in [question_reply(), labels_reply()] {
            let refused = refused(&owned(PEER, OWNED), &command, &owned_id(), &script)
                .expect("a question answer to the own session is refused");
            assert_eq!(refused.code, contract::ErrorCode::FromSession);
            assert!(refused.message.contains(OWNED), "{}", refused.message);
        }
    }

    #[test]
    fn prompt_and_steer_to_a_delegate_are_refused() {
        let script = Script {
            current: 1000,
            parents: [(TARGET_PID, 1), (1, 0)].into_iter().collect(),
            euids: [(TARGET_PID, 1000), (1, 1000)].into_iter().collect(),
            argvs: [(1, session_argv(OWNED))].into_iter().collect(),
        };
        for command in [prompt(), steer(), labels_reply()] {
            let refused = refused(&owned(PEER, OWNED), &command, &other_id(), &script)
                .expect("a command to a delegate is refused");
            assert_eq!(refused.code, contract::ErrorCode::FromSession);
            assert!(refused.message.contains(OWNED), "{}", refused.message);
            assert!(refused.message.contains(OTHER), "{}", refused.message);
        }
    }

    #[test]
    fn unrelated_and_unowned_commands_are_allowed() {
        let script = plain_target();
        // An approval answer is refused on every session, related or not;
        // everything else stays reachable on an unrelated one.
        let unrelated = [
            prompt(),
            steer(),
            question_reply(),
            labels_reply(),
            message(),
        ];
        for command in unrelated {
            assert!(
                refused(&owned(PEER, OWNED), &command, &other_id(), &script).is_none(),
                "an unrelated session stays reachable"
            );
        }
        let unrelated_approval =
            refused(&owned(PEER, OWNED), &approval_reply(), &other_id(), &script)
                .expect("an approval answer is refused on an unrelated session");
        assert_eq!(unrelated_approval.code, contract::ErrorCode::FromSession);
        let unowned = Attribution::Local {
            peer_pid: PEER,
            owners: vec![],
        };
        for command in [prompt(), steer(), approval_reply(), message()] {
            assert!(
                refused(&unowned, &command, &owned_id(), &script).is_none(),
                "an unowned connection is never refused"
            );
        }
        assert!(
            refused(&owned(PEER, OWNED), &message(), &owned_id(), &script).is_none(),
            "a message is never refused"
        );
    }

    /// The target walk with a session-hosting `ask` above the target: the
    /// target shares a session process with a caller below that host.
    fn ask_target() -> Script {
        const ASK: u32 = 440;
        Script {
            current: 1000,
            parents: [(TARGET_PID, ASK), (ASK, 1), (1, 0)].into_iter().collect(),
            euids: [(TARGET_PID, 1000), (ASK, 1000), (1, 1000)]
                .into_iter()
                .collect(),
            argvs: [
                (ASK, ask_argv()),
                (TARGET_PID, vec![b"/sbin/init".to_vec()]),
                (1, vec![b"/sbin/init".to_vec()]),
            ]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn steer_refused_when_the_target_walk_passes_an_ask_host() {
        let script = ask_target();
        let refused = refused(&owned(PEER, OWNED), &steer(), &other_id(), &script)
            .expect("a steer to a target below an ask host is refused");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
    }

    #[test]
    fn question_reply_refused_when_the_target_walk_passes_an_ask_host() {
        let script = ask_target();
        let refused = refused(&owned(PEER, OWNED), &question_reply(), &other_id(), &script)
            .expect("a reply to a target below an ask host is refused");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
    }

    #[test]
    fn other_commands_are_never_refused() {
        use contract::commands::{SubscribeArgs, SubscribeLevel};
        let script = plain_target();
        let subscribe = Command::Subscribe(SubscribeArgs {
            level: SubscribeLevel::Full,
            credential: None,
        });
        assert!(
            refused(&owned(PEER, OWNED), &subscribe, &owned_id(), &script).is_none(),
            "subscribe is never refused"
        );
    }

    #[test]
    fn the_direct_parent_reaches_its_session_but_never_an_approval() {
        let script = Script {
            parents: [(TARGET_PID, PEER)].into_iter().collect(),
            ..Default::default()
        };
        for command in [prompt(), steer(), question_reply(), labels_reply()] {
            assert!(
                refused(&owned(PEER, OWNED), &command, &other_id(), &script).is_none(),
                "the delegate's own client reaches it"
            );
        }
        let refused = refused(&owned(PEER, OWNED), &approval_reply(), &other_id(), &script)
            .expect("an approval answer has no parent exception");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
    }

    #[test]
    fn an_incomplete_caller_without_the_exception_names_its_pid() {
        let script = plain_target();
        let refused = refused(
            &incomplete(Some(PEER), Some(UNREADABLE)),
            &steer(),
            &other_id(),
            &script,
        )
        .expect("an incomplete walk fails closed");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
        assert!(
            refused.message.contains(&UNREADABLE.to_string()),
            "{}",
            refused.message
        );
    }

    #[test]
    fn an_incomplete_caller_with_the_parent_exception_still_reaches() {
        let script = Script {
            parents: [(TARGET_PID, PEER)].into_iter().collect(),
            ..Default::default()
        };
        assert!(
            refused(
                &incomplete(Some(PEER), Some(UNREADABLE)),
                &steer(),
                &other_id(),
                &script
            )
            .is_none(),
            "the exception holds on an incomplete walk"
        );
    }

    #[test]
    fn a_failed_credential_read_renders_its_error() {
        let attribution = Attribution::Incomplete {
            peer_pid: None,
            pid: None,
            peer_error: Some("injected credential failure".to_owned()),
        };
        let script = plain_target();
        let refused = refused(&attribution, &steer(), &other_id(), &script)
            .expect("a failed credential read fails closed");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
        assert!(
            refused.message.contains("injected credential failure"),
            "{}",
            refused.message
        );
    }

    #[test]
    fn an_unreadable_target_names_the_target_session() {
        let script = Script::default();
        let refused = refused(&owned(PEER, OWNED), &steer(), &other_id(), &script)
            .expect("an unreadable target fails closed");
        assert_eq!(refused.code, contract::ErrorCode::FromSession);
        assert!(refused.message.contains(OTHER), "{}", refused.message);
    }

    #[test]
    fn attribute_peer_names_the_unreadable_owner_and_keeps_the_error() {
        let read = Script {
            current: 1000,
            parents: [(PEER, UNREADABLE)].into_iter().collect(),
            ..Default::default()
        };
        assert_eq!(
            attribute_peer(Ok(PEER), &read),
            Attribution::Incomplete {
                peer_pid: Some(PEER),
                pid: Some(UNREADABLE),
                peer_error: None,
            }
        );
        let error = support::peer::PeerCredentialError::Unavailable(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "no peering",
        ));
        let kept = error.to_string();
        assert_eq!(
            attribute_peer(Err(error), &read),
            Attribution::Incomplete {
                peer_pid: None,
                pid: None,
                peer_error: Some(kept),
            }
        );
    }

    #[test]
    fn an_unreadable_owner_walk_fails_closed_with_its_pid() {
        let read = Script {
            current: 1000,
            parents: [(1, 0)].into_iter().collect(),
            euids: [(1, 1000)].into_iter().collect(),
            ..Default::default()
        };
        let attribution = attribute_peer(Ok(PEER), &read);
        let script = plain_target();
        let refused = refused(&attribution, &steer(), &other_id(), &script)
            .expect("an unreadable owner fails closed");
        assert!(
            refused.message.contains(&PEER.to_string()),
            "{}",
            refused.message
        );
    }
}
