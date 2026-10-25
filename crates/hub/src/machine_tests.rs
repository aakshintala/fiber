//! Machine commands use the connection's machine, without a session.
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::thread;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::commands::hub::{PruneExpect, PruneSession};
use contract::shapes::Failure;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::connection::{lock, serve_connection};
use crate::fake::{FakeMachine, FakeStarter};
use crate::machine::{PruneFailure, PruneOutcome};
use crate::testkit::{DEADLINE, hub, id};

fn exchange(machine: Option<Arc<FakeMachine>>, commands: Vec<Value>) -> Vec<Value> {
    let temp = fakes::TempDir::new("hm");
    let clock: Arc<dyn Clock> = FakeClock::new();
    let h = hub(temp.path(), Arc::new(FakeStarter::hang(temp.path())), clock);
    let h = Arc::new(match machine {
        Some(machine) => h.with_machine(machine),
        None => h,
    });
    let (mut client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(DEADLINE)).unwrap();
    client.set_write_timeout(Some(DEADLINE)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        serve_connection(server, h);
        done.send(()).unwrap();
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["kind"],
        "hub_hello"
    );
    let mut answers = Vec::new();
    for args in commands {
        writeln!(client, "{args}").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        answers.push(serde_json::from_str(&line).unwrap());
    }
    client.shutdown(Shutdown::Both).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&finished, "connection ended");
    answers
}

fn command(name: &str, args: Value) -> Value {
    json!({"id":"m", "command":name, "args":args})
}

#[test]
fn configure_routes_args_and_every_outcome() {
    let machine = Arc::new(FakeMachine::default());
    lock(&machine.answers).extend([
        Ok(Some(json!({"rows":[1]}))),
        Ok(None),
        Err(Failure {
            code: ErrorCode::Usage,
            message: "refused".into(),
            provider: None,
            retry_after_ms: None,
        }),
    ]);
    let answers = exchange(
        Some(machine.clone()),
        vec![
            command(
                "configure",
                json!({"operation":"settings","workspace":"/w"})
            );
            3
        ],
    );
    assert_eq!(
        answers
            .iter()
            .map(|v| v["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["command_accepted", "command_accepted", "command_rejected"]
    );
    assert_eq!(answers[0]["payload"]["result"], json!({"rows":[1]}));
    assert!(answers[1]["payload"].get("result").is_none());
    assert_eq!(answers[2]["payload"]["code"], "usage");
    assert_eq!(answers[2]["payload"]["message"], "refused");
    let calls = lock(&machine.calls);
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0],
        json!({"operation":"settings","args":{"workspace":"/w"}})
    );
}

#[test]
fn configure_requires_a_string_operation_without_calling_machine() {
    let machine = Arc::new(FakeMachine::default());
    let answers = exchange(
        Some(machine.clone()),
        [json!({}), json!({"operation":null}), json!({"operation":3})]
            .into_iter()
            .map(|a| command("configure", a))
            .collect(),
    );
    for answer in answers {
        assert_eq!(answer["payload"]["code"], "invalid_arguments");
    }
    assert!(lock(&machine.calls).is_empty());
}

#[test]
fn config_file_commands_route_typed_arguments() {
    let machine = Arc::new(FakeMachine::default());
    let answers = exchange(
        Some(machine.clone()),
        vec![
            command(
                "read_config",
                json!({"workspace":"/w","path":"/h/config.json"}),
            ),
            command(
                "write_config",
                json!({"workspace":"/w","path":"/h/config.json","text":"{","version":"v"}),
            ),
        ],
    );
    assert_eq!(
        answers[0]["payload"]["result"],
        json!({"text":"text","version":"version"})
    );
    assert_eq!(answers[1]["kind"], "command_accepted");
    assert!(answers[1]["payload"].get("result").is_none());
    assert_eq!(
        *lock(&machine.calls),
        vec![
            json!({"read":["/w","/h/config.json"]}),
            json!({"write":["/w","/h/config.json","{","v"]})
        ]
    );
}

#[test]
fn config_file_commands_refuse_every_bad_shape_without_a_call() {
    let machine = Arc::new(FakeMachine::default());
    let mut commands = Vec::new();
    for name in ["read_config", "write_config"] {
        let mut valid = json!({"workspace":"/w","path":"/h/config.json"});
        if name == "write_config" {
            valid["text"] = json!("text");
            valid["version"] = json!("v");
        }
        for key in valid.as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(key);
            commands.push(command(name, missing));
            for bad in [Value::Null, json!(true), json!(5), json!([]), json!({})] {
                let mut args = valid.clone();
                args[key] = bad;
                commands.push(command(name, args));
            }
        }
        let mut extra = valid.clone();
        extra["extra"] = json!(true);
        commands.push(command(name, extra));
        valid["workspace"] = json!("relative");
        commands.push(command(name, valid));
    }
    for answer in exchange(Some(machine.clone()), commands) {
        assert_eq!(answer["payload"]["code"], "invalid_arguments");
    }
    assert!(lock(&machine.calls).is_empty());
}

#[test]
fn a_hub_without_a_machine_keeps_unknown_command() {
    // Deliberately leave the fake uninstalled: exchange(None) never calls with_machine.
    let machine = FakeMachine::default();
    for answer in exchange(
        None,
        [
            "search_files",
            "device_login",
            "device_login_result",
            "device_login_cancel",
            "configure",
            "read_config",
            "write_config",
            "prune",
        ]
        .into_iter()
        .map(|n| command(n, json!({})))
        .collect(),
    ) {
        assert_eq!(answer["kind"], "command_rejected");
        assert_eq!(answer["payload"]["command_id"], "m");
        assert_eq!(answer["payload"]["code"], "unknown_command");
    }
    assert!(lock(&machine.calls).is_empty());
}

#[test]
fn prune_routes_args_and_maps_every_outcome() {
    let machine = Arc::new(FakeMachine::default());
    let preview = PruneExpect {
        sessions: vec![PruneSession {
            id: contract::SessionId(id(1)),
            cascade: vec![contract::SessionId(id(1))],
        }],
        worktrees: Vec::new(),
        pinned: Vec::new(),
        diagnostics: Vec::new(),
    };
    lock(&machine.prunes).extend([
        Ok(PruneOutcome {
            lines: vec!["session  s_1  41d  1.2 MiB".to_owned()],
            items: preview,
            freed: 1048576,
            total: 0,
            failures: Vec::new(),
            started: false,
        }),
        Ok(PruneOutcome {
            lines: Vec::new(),
            items: PruneExpect {
                sessions: Vec::new(),
                worktrees: Vec::new(),
                pinned: Vec::new(),
                diagnostics: Vec::new(),
            },
            freed: 7,
            total: 2,
            failures: vec![PruneFailure {
                item: id(2),
                code: ErrorCode::StaleRequest,
                message: "changed".to_owned(),
            }],
            started: true,
        }),
        Err(Failure {
            code: ErrorCode::IoFailed,
            message: "busy".to_owned(),
            provider: None,
            retry_after_ms: None,
        }),
    ]);
    let answers = exchange(
        Some(machine.clone()),
        vec![
            command("prune", json!({"older_than": "30d", "dry_run": true})),
            command(
                "prune",
                json!({"cascade": true, "expect": {"sessions": [{"id": id(1), "cascade": [id(1)]}]}}),
            ),
            // Missing `args` reads as `{}`: a confirm with nothing previewed.
            json!({"id": "m", "command": "prune"}),
        ],
    );
    assert_eq!(answers[0]["kind"], "command_accepted");
    assert_eq!(
        answers[0]["payload"]["result"]["would_free"],
        json!(1048576)
    );
    assert_eq!(answers[1]["kind"], "command_accepted");
    assert_eq!(answers[1]["payload"]["result"]["freed"], json!(7));
    assert_eq!(answers[1]["payload"]["result"]["total"], json!(2));
    assert_eq!(
        answers[1]["payload"]["result"]["failures"],
        json!([{"item": id(2), "code": "stale_request", "message": "changed"}])
    );
    assert_eq!(answers[2]["kind"], "command_rejected");
    assert_eq!(answers[2]["payload"]["code"], "io_failed");
    assert_eq!(answers[2]["payload"]["message"], "busy");
    let calls = lock(&machine.calls);
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0]["prune"]["older_than"], json!("30d"));
    assert_eq!(calls[0]["prune"]["dry_run"], json!(true));
    assert!(calls[0]["prune"].get("expect").unwrap().is_null());
    assert_eq!(
        calls[1]["prune"]["expect"]["sessions"],
        json!([{"id": id(1), "cascade": [id(1)]}])
    );
    assert_eq!(calls[2]["prune"]["dry_run"], json!(false));
}
