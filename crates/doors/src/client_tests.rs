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
