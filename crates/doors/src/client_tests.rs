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
