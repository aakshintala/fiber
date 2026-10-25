//! Tests for `prune`: the argument shapes it refuses without calling the
//! machine, the dry-run preview and confirmed answers it passes through,
//! the seam failures it maps, and the session-delete closure it hands the
//! machine, which deletes through the hub's own `delete`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
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
use crate::machine::{PruneFailure, PruneOutcome, ShowCode};
use crate::testkit::{DEADLINE, args, hub, id};

/// One named deadline per receive: the hub answers before it.
fn exchange(machine: Arc<dyn crate::Machine>, commands: Vec<Value>) -> Vec<Value> {
    let temp = fakes::TempDir::new("hp");
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = hub(temp.path(), Arc::new(FakeStarter::hang(temp.path())), clock);
    let hub = Arc::new(hub.with_machine(machine));
    let (mut client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(DEADLINE)).unwrap();
    client.set_write_timeout(Some(DEADLINE)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        serve_connection(server, hub);
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
    for command in commands {
        writeln!(client, "{command}").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        answers.push(serde_json::from_str(&line).unwrap());
    }
    client.shutdown(Shutdown::Both).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&finished, "connection ended");
    answers
}

fn command(id: &str, args: Value) -> Value {
    json!({"id": id, "command": "prune", "args": args})
}

/// The empty previewed-items object.
fn empty_expect() -> PruneExpect {
    PruneExpect {
        sessions: Vec::new(),
        worktrees: Vec::new(),
        pinned: Vec::new(),
        diagnostics: Vec::new(),
    }
}

#[test]
fn unknown_keys_wrong_types_and_nulls_are_invalid_without_a_machine_call() {
    let machine = Arc::new(FakeMachine::default());
    // `{"expect": {}}` reads every list absent, so it is well-shaped and
    // reaches the machine; every other shape below is refused first.
    let bad = vec![
        json!({"older_than": 1}),
        json!({"older_than": null}),
        json!({"older_than": true}),
        json!({"cascade": "yes"}),
        json!({"cascade": null}),
        json!({"cascade": 1}),
        json!({"force": "yes"}),
        json!({"force": null}),
        json!({"dry_run": "yes"}),
        json!({"dry_run": null}),
        json!({"dry_run": 1}),
        json!({"expect": null}),
        json!({"expect": 1}),
        json!({"expect": "s_1"}),
        json!({"expect": []}),
        json!({"expect": [[]]}),
        json!({"expect": {"sessions": {}}}),
        json!({"expect": {"sessions": null}}),
        json!({"expect": {"sessions": "s_1"}}),
        json!({"expect": {"sessions": [null]}}),
        json!({"expect": {"sessions": [1]}}),
        json!({"expect": {"sessions": [{"id": 1, "cascade": []}]}}),
        json!({"expect": {"sessions": [[id(1), []]]}}),
        json!({"expect": {"sessions": [{"cascade": []}]}}),
        json!({"expect": {"sessions": [{"id": id(1)}]}}),
        json!({"expect": {"sessions": [{"id": [id(1)], "cascade": []}]}}),
        json!({"expect": {"sessions": [{"id": id(1), "cascade": null}]}}),
        json!({"expect": {"sessions": [{"id": id(1), "cascade": [null]}]}}),
        json!({"expect": {"sessions": [{"id": id(1), "cascade": [], "extra": 1}]}}),
        json!({"expect": {"worktrees": "w"}}),
        json!({"expect": {"worktrees": [null]}}),
        json!({"expect": {"pinned": [1]}}),
        json!({"expect": {"diagnostics": null}}),
        json!({"expect": {"bogus": []}}),
        json!({"expect": {"sessions": [], "bogus": []}}),
        json!({"older_than": "30d", "extra": true}),
    ];
    let scripted: Arc<dyn crate::Machine> = machine.clone();
    let answers = exchange(
        scripted,
        bad.iter()
            .map(|shape| command("p", shape.clone()))
            .collect(),
    );
    for (answer, shape) in answers.iter().zip(bad.iter()) {
        assert_eq!(answer["kind"], "command_rejected", "{shape}");
        assert_eq!(answer["payload"]["code"], "invalid_arguments", "{shape}");
    }
    assert!(lock(&machine.calls).is_empty());
}

#[test]
fn a_dry_run_answers_the_preview_and_deletes_nothing() {
    let machine = Arc::new(FakeMachine::default());
    let preview = PruneExpect {
        sessions: vec![PruneSession {
            id: contract::SessionId(id(1)),
            cascade: vec![contract::SessionId(id(1))],
        }],
        worktrees: vec!["projects/p/worktrees/w1".to_owned()],
        pinned: vec![],
        diagnostics: vec!["logs/old.log".to_owned()],
    };
    lock(&machine.prunes).extend([Ok(PruneOutcome {
        lines: vec!["session  s_1  41d  1.2 MiB".to_owned(), "...".to_owned()],
        items: preview.clone(),
        freed: 1048576,
        total: 0,
        failures: Vec::new(),
        started: false,
    })]);
    let scripted: Arc<dyn crate::Machine> = machine.clone();
    let answers = exchange(scripted, vec![command("p_1", json!({"dry_run": true}))]);
    assert_eq!(answers[0]["kind"], "command_accepted");
    assert_eq!(
        answers[0]["payload"]["result"],
        json!({
            "lines": ["session  s_1  41d  1.2 MiB", "..."],
            "would_free": 1048576,
            "items": {
                "sessions": [{"id": id(1), "cascade": [id(1)]}],
                "worktrees": ["projects/p/worktrees/w1"],
                "pinned": [],
                "diagnostics": ["logs/old.log"],
            },
        })
    );
    let calls = lock(&machine.calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["prune"]["dry_run"], json!(true));
    assert!(calls[0]["prune"].get("expect").unwrap().is_null());
}

#[test]
fn a_started_run_answers_freed_total_and_failures() {
    let machine = Arc::new(FakeMachine::default());
    lock(&machine.prunes).extend([Ok(PruneOutcome {
        lines: Vec::new(),
        items: empty_expect(),
        freed: 1048576,
        total: 3,
        failures: vec![
            PruneFailure {
                item: id(2),
                code: ErrorCode::StaleRequest,
                message: "changed".to_owned(),
            },
            PruneFailure {
                item: "worktree w1".to_owned(),
                code: ErrorCode::IoFailed,
                message: "gone".to_owned(),
            },
        ],
        started: true,
    })]);
    let expect = json!({
        "sessions": [{"id": id(1), "cascade": [id(1)]}],
        "worktrees": ["w1"],
        "pinned": [],
        "diagnostics": [],
    });
    let scripted: Arc<dyn crate::Machine> = machine.clone();
    let answers = exchange(
        scripted,
        vec![command(
            "p_1",
            json!({"older_than": "30d", "cascade": true, "force": false, "expect": expect}),
        )],
    );
    assert_eq!(answers[0]["kind"], "command_accepted");
    assert_eq!(
        answers[0]["payload"]["result"],
        json!({
            "freed": 1048576,
            "total": 3,
            "failures": [
                {"item": id(2), "code": "stale_request", "message": "changed"},
                {"item": "worktree w1", "code": "io_failed", "message": "gone"},
            ],
        })
    );
    let calls = lock(&machine.calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["prune"]["older_than"], json!("30d"));
    assert_eq!(calls[0]["prune"]["cascade"], json!(true));
    assert_eq!(calls[0]["prune"]["force"], json!(false));
    assert_eq!(calls[0]["prune"]["dry_run"], json!(false));
    assert_eq!(
        calls[0]["prune"]["expect"]["sessions"],
        json!([{"id": id(1), "cascade": [id(1)]}])
    );
}

#[test]
fn an_unstarted_failure_is_rejected_with_its_code() {
    let machine = Arc::new(FakeMachine::default());
    lock(&machine.prunes).extend([Err(Failure {
        code: ErrorCode::InvalidArguments,
        message: "nope".to_owned(),
        provider: None,
        retry_after_ms: None,
    })]);
    let scripted: Arc<dyn crate::Machine> = machine.clone();
    let answers = exchange(scripted, vec![command("p_1", json!({}))]);
    assert_eq!(answers[0]["kind"], "command_rejected");
    assert_eq!(answers[0]["payload"]["command_id"], "p_1");
    assert_eq!(answers[0]["payload"]["code"], "invalid_arguments");
    assert_eq!(answers[0]["payload"]["message"], "nope");
}

/// A machine that deletes through the passed callback, as the seam will:
/// each previewed session with its previewed cascade set as `expect`.
struct InvokeMachine {
    fake: FakeMachine,
}

impl crate::Machine for InvokeMachine {
    fn device_login(
        &self,
        name: &str,
        shown: ShowCode,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn crate::DeviceLogin> {
        self.fake.device_login(name, shown, clock)
    }
    fn file_search(
        &self,
        workspace: &std::path::Path,
        home: Option<&std::path::Path>,
    ) -> Box<dyn crate::FileSearch> {
        self.fake.file_search(workspace, home)
    }
    fn configure(
        &self,
        operation: &str,
        args: &serde_json::Map<String, Value>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure> {
        self.fake.configure(operation, args, clock)
    }
    fn import(
        &self,
        home: &std::path::Path,
        source: Option<&str>,
        clock: Arc<dyn Clock>,
    ) -> Result<Option<Value>, Failure> {
        self.fake.import(home, source, clock)
    }
    fn read_config(
        &self,
        workspace: &std::path::Path,
        path: &std::path::Path,
    ) -> Result<(String, String), Failure> {
        self.fake.read_config(workspace, path)
    }
    fn write_config(
        &self,
        workspace: &std::path::Path,
        path: &std::path::Path,
        text: &str,
        version: &str,
    ) -> Result<(), Failure> {
        self.fake.write_config(workspace, path, text, version)
    }
    #[allow(
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "one seam call carries the whole prune: its flags, the previewed items and the session-delete callback"
    )]
    fn prune(
        &self,
        _home: &std::path::Path,
        _older_than: Option<String>,
        _cascade: bool,
        _force: bool,
        _dry_run: bool,
        expect: Option<PruneExpect>,
        _clock: Arc<dyn Clock>,
        delete: &mut (
                 dyn FnMut(
            &contract::SessionId,
            bool,
            Option<&BTreeSet<contract::SessionId>>,
        ) -> Result<(), Failure>
                     + Send
             ),
    ) -> Result<PruneOutcome, Failure> {
        let Some(expect) = expect else {
            return Ok(PruneOutcome {
                lines: Vec::new(),
                items: empty_expect(),
                freed: 0,
                total: 0,
                failures: Vec::new(),
                started: true,
            });
        };
        let mut total = 0;
        for session in &expect.sessions {
            let set: BTreeSet<contract::SessionId> = session.cascade.iter().cloned().collect();
            delete(&session.id, !session.cascade.is_empty(), Some(&set))?;
            total += 1;
        }
        Ok(PruneOutcome {
            lines: Vec::new(),
            items: empty_expect(),
            freed: 0,
            total,
            failures: Vec::new(),
            started: true,
        })
    }
}

/// Session `n` in `project`, continuing `from` when given: its log, an
/// artifact and an unheld lock file.
fn session(dir: &std::path::Path, project: &str, n: u64, from: Option<u64>) -> std::path::PathBuf {
    let session = log::session_dir(dir, project, &contract::SessionId(id(n)));
    fs::create_dir_all(session.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": "/w"});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": id(from), "seq": 2});
    }
    let first =
        json!({"kind": "session_started", "seq": 0, "session_id": id(n), "payload": payload});
    fs::write(session.join("events.jsonl"), format!("{first}\n")).unwrap();
    fs::write(session.join("artifacts").join("a_1.txt"), b"bytes").unwrap();
    fs::write(session.join("session.lock"), b"").unwrap();
    session
}

#[test]
fn the_passed_callback_deletes_through_the_hubs_own_delete() {
    let (held, dir) = crate::testkit::home("hp");
    let root = session(&dir, "-p", 1, None);
    let fork = session(&dir, "-p", 2, Some(1));
    let row = crate::RecentRow {
        session_id: contract::SessionId(id(1)),
        ts: 1,
        project: "-p".to_owned(),
        workspace: "/w".to_owned(),
        name: "n".to_owned(),
        how: crate::Left::Crashed,
        status: Some(serde_json::from_value(crate::fake::status("n", "/w", "idle", None)).unwrap()),
    };
    crate::append(&dir, &row).unwrap();
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = hub(&dir, Arc::new(FakeStarter::hang(&dir)), clock);
    hub.feed.start();
    let hub = Arc::new(hub.with_machine(Arc::new(InvokeMachine {
        fake: FakeMachine::default(),
    })));
    let fed = Arc::clone(&hub);
    let (mut client, server) = UnixStream::pair().unwrap();
    client.set_read_timeout(Some(DEADLINE)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        serve_connection(server, hub);
        done.send(()).unwrap();
    });
    let mut reader = BufReader::new(client.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let sent = command(
        "p_1",
        json!({"cascade": true, "expect": {"sessions": [{"id": id(1), "cascade": [id(1), id(2)]}]}}),
    );
    writeln!(client, "{sent}").unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let answer: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(answer["kind"], "command_accepted");
    assert_eq!(
        answer["payload"]["result"],
        json!({"freed": 0, "total": 1, "failures": []})
    );
    client.shutdown(Shutdown::Both).unwrap();
    Deadline::after(DEADLINE).recv_or_fail(&finished, "connection ended");
    assert!(!root.exists(), "the root is gone");
    assert!(!fork.exists(), "the dependent is gone");
    // The crashed entry is forgotten with the session it names.
    let forgotten = fed
        .feed
        .dismiss(&args(json!({"session": id(1)})))
        .unwrap_err();
    assert_eq!(forgotten.code, ErrorCode::StaleRequest);
    fed.feed.stop();
    drop(held);
}

#[test]
fn the_passed_callback_maps_a_forked_set_to_stale_request() {
    let (_held, dir) = crate::testkit::home("hp");
    let root = session(&dir, "-p", 1, None);
    let fork = session(&dir, "-p", 2, Some(1));
    let clock: Arc<dyn Clock> = FakeClock::new();
    let hub = hub(&dir, Arc::new(FakeStarter::hang(&dir)), clock);
    let hub = Arc::new(hub.with_machine(Arc::new(InvokeMachine {
        fake: FakeMachine::default(),
    })));
    // The preview named only the root; the fork landed while asking. The
    // hub's own delete refuses, and the prune maps its refusal.
    let got = crate::prune::prune(
        &hub,
        &args(
            json!({"cascade": true, "expect": {"sessions": [{"id": id(1), "cascade": [id(1)]}]}}),
        ),
    );
    let refused = got.unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert!(refused.message.contains(&id(1)), "{}", refused.message);
    assert!(refused.message.contains(&id(2)), "{}", refused.message);
    assert!(root.join("events.jsonl").is_file());
    assert!(fork.join("events.jsonl").is_file());
}
