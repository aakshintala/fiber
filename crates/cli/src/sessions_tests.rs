//! `fiber sessions export`'s resolver and printer, and `fiber sessions
//! delete`'s confirmation and its one `delete` to an in-process fake hub,
//! with the session directories built by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;

use contract::HubLine;
use serde_json::{Map, Value, json};

use super::{Ask, delete, delete_run, export, run};

const FIRST: &str = "{\"seq\":0,\"kind\":\"a\"}\n";
const SECOND: &str = "{\"seq\":1,\"kind\":\"b\"}\n";
const TORN: &str = "{\"seq\":2,\"kin";

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-sessions"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// The workspace as the command sees it: resolved, so it names the
    /// same project as `doors::project` derives inside `run`.
    fn workspace(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("workspace")).unwrap()
    }

    /// The project's sessions directory, as `run` derives it: a workspace
    /// outside any repository is its own project.
    fn sessions(&self) -> PathBuf {
        log::sessions_dir(&self.home(), &self.workspace())
    }

    /// A session directory with two complete lines, a torn tail, one
    /// artifact and a lock file.
    fn session(&self, id: &str) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(dir.join("events.jsonl"), format!("{FIRST}{SECOND}{TORN}")).unwrap();
        fs::write(dir.join("artifacts/a_1.txt"), "artifact\n").unwrap();
        fs::write(dir.join("session.lock"), "held").unwrap();
    }
}

#[test]
fn a_unique_prefix_exports_the_complete_lines_and_the_artifact() {
    let setup = Setup::new();
    setup.session("s_exportaa01");
    let mut out = Vec::new();
    let target = run(
        &setup.home(),
        &setup.workspace(),
        "s_exportaa",
        None,
        &mut out,
    )
    .unwrap();
    assert_eq!(target, setup.workspace().join("s_exportaa01"));
    assert_eq!(out, format!("{}\n", target.display()).into_bytes());
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
    assert!(!target.join("session.lock").exists());
}

#[test]
fn an_explicit_relative_path_lands_under_the_workspace() {
    let setup = Setup::new();
    setup.session("s_exportbb01");
    let mut out = Vec::new();
    let target = run(
        &setup.home(),
        &setup.workspace(),
        "s_exportbb01",
        Some(Path::new("deep/out")),
        &mut out,
    )
    .unwrap();
    assert_eq!(target, setup.workspace().join("deep/out"));
    assert_eq!(out, format!("{}\n", target.display()).into_bytes());
    assert_eq!(
        fs::read(target.join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
    assert_eq!(
        fs::read(target.join("artifacts/a_1.txt")).unwrap(),
        b"artifact\n"
    );
}

#[test]
fn an_unknown_id_is_not_found() {
    let setup = Setup::new();
    setup.session("s_exportdd01");
    let e = run(
        &setup.home(),
        &setup.workspace(),
        "s_missing",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SessionNotFound);
}

#[test]
fn an_ambiguous_prefix_names_both_matches() {
    let setup = Setup::new();
    setup.session("s_amb11111");
    setup.session("s_amb22222");
    let e = run(
        &setup.home(),
        &setup.workspace(),
        "s_amb",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(e.message.contains("s_amb11111"), "{}", e.message);
    assert!(e.message.contains("s_amb22222"), "{}", e.message);
}

/// The child's marker: set, the test runs `export` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

/// The session the child exports.
const CHILD_ID: &str = "s_childexport01";

#[test]
fn export_exits_zero_and_writes_the_export() {
    // Runs in a child with the parent's Fiber home and workspace, so the
    // exit code is `export`'s own. The parent checks the code and the
    // export: a code alone would not catch an `export` that returns 0
    // without copying.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(export(CHILD_ID, None));
    }
    let setup = Setup::new();
    setup.session(CHILD_ID);
    let name = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::export_exits_zero_and_writes_the_export"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait().unwrap()));
    let Ok(status) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for `fiber sessions export` to exit");
    };
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert_eq!(
        fs::read(setup.workspace().join(CHILD_ID).join("events.jsonl")).unwrap(),
        format!("{FIRST}{SECOND}").into_bytes()
    );
}

/// The ids of sessions `delete` tests name.
const ROOT: &str = "s_00000000000000a1";
const FORK: &str = "s_00000000000000b2";

/// One named deadline for the fake hub's read and the test's receive.
const HUB_DEADLINE: Duration = Duration::from_secs(10);

/// A `session_started` first line, continuing `from` when given.
fn started(from: Option<&str>) -> String {
    let mut payload = json!({"workspace": "/w"});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    format!(
        "{}\n",
        json!({"kind": "session_started", "seq": 0, "payload": payload})
    )
}

impl Setup {
    /// Session `id` in the workspace's project, continuing `from`.
    fn started(&self, id: &str, from: Option<&str>) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("events.jsonl"), started(from)).unwrap();
    }
}

/// An in-process hub: `connect` hands out one end of a pair, and a thread
/// on the other reads the one command line, then writes `answers`.
struct FakeHub {
    connects: usize,
    answers: Vec<String>,
    got: Option<mpsc::Receiver<String>>,
}

impl FakeHub {
    fn new(answers: &[Value]) -> Self {
        Self {
            connects: 0,
            answers: answers.iter().map(|line| format!("{line}\n")).collect(),
            got: None,
        }
    }

    fn connect(&mut self) -> io::Result<doors::hub::Hub> {
        self.connects += 1;
        let (client, hub) = UnixStream::pair()?;
        hub.set_read_timeout(Some(HUB_DEADLINE))?;
        let (tx, rx) = mpsc::channel();
        let answers = self.answers.clone();
        thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(&hub).read_line(&mut line).unwrap();
            tx.send(line).unwrap();
            for answer in answers {
                (&hub).write_all(answer.as_bytes()).unwrap();
            }
            // Dropping `hub` ends the connection.
        });
        self.got = Some(rx);
        let hello = HubLine {
            kind: "hub_hello".to_owned(),
            ts: 1,
            schema_version: 1,
            payload: Map::new(),
        };
        Ok((client, hello))
    }

    /// The command line the hub read, as JSON.
    fn sent(&self) -> Value {
        let line = self
            .got
            .as_ref()
            .unwrap()
            .recv_timeout(HUB_DEADLINE)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

fn accepted() -> Value {
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": {"command_id": "c_delete"}})
}

/// Input that fails the test if anything reads it.
struct Unread;

impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("nothing is read");
    }
}

impl BufRead for Unread {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        panic!("nothing is read");
    }

    fn consume(&mut self, _: usize) {}
}

/// Runs `delete_run` for `selector` against `hub`: its stdout, its
/// stderr, and the result.
fn run_delete(
    setup: &Setup,
    hub: &mut FakeHub,
    selector: &str,
    cascade: bool,
    yes: bool,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, String, Result<(), contract::shapes::Failure>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ask = Ask {
        yes,
        terminal,
        input,
        err: &mut err,
    };
    let got = delete_run(
        &setup.home(),
        &setup.workspace(),
        selector,
        cascade,
        ask,
        &mut out,
        &mut || hub.connect(),
    );
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        got,
    )
}

/// How long a killed child or its watchdog may take to be reaped.
const REAP_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn delete_exits_zero_on_success_two_on_usage_and_one_on_refusal() {
    if let Ok(case) = std::env::var("FIBER_CLI_DELETE_CHILD") {
        let answers = if case == "refusal" {
            vec![
                json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
                    "command_id": "c_delete", "code": "session_held", "message": "Session is held.",
                }}),
            ]
        } else {
            vec![accepted()]
        };
        let mut hub = FakeHub::new(&answers);
        let code = delete(ROOT, false, case != "usage", &mut || hub.connect());
        if case == "usage" {
            assert_eq!(hub.connects, 0, "usage must not reach the hub");
        } else {
            assert_eq!(hub.connects, 1);
            assert_eq!(
                hub.sent(),
                json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT}})
            );
        }
        std::process::exit(code);
    }
    let setup = Setup::new();
    setup.started(ROOT, None);
    let name = module_path!().split_once("::").unwrap().1;
    for (case, code) in [("success", 0), ("usage", 2), ("refusal", 1)] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{name}::delete_exits_zero_on_success_two_on_usage_and_one_on_refusal"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FIBER_HOME", setup.home())
            .env("FIBER_CLI_DELETE_CHILD", case)
            .current_dir(setup.workspace())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let watchdog = fakes::Watchdog::group(group);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(child.wait()));
        let Ok(status) = rx.recv_timeout(HUB_DEADLINE) else {
            assert!(fakes::kill_group(group, "KILL").unwrap());
            rx.recv_timeout(REAP_DEADLINE)
                .expect("the killed delete child must be reaped")
                .unwrap();
            panic!("waited {HUB_DEADLINE:?} for `fiber sessions delete` ({case}) to exit");
        };
        let status = status.unwrap();
        assert!(!fakes::kill_group(group, "0").unwrap(), "a child remains");
        watchdog.stand_down(REAP_DEADLINE);
        assert_eq!(status.code(), Some(code), "{case}: {status}");
    }
}

#[test]
fn yes_sends_one_delete_for_the_full_id_and_prints_it() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    // A line for another command comes first, and is skipped.
    let other = json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": {"command_id": "c_other"}});
    let mut hub = FakeHub::new(&[other, accepted()]);
    let (out, err, got) = run_delete(
        &setup,
        &mut hub,
        "s_00000000000000a",
        false,
        true,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert_eq!(
        hub.sent(),
        json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT}})
    );
    assert_eq!(out, format!("{ROOT}\n"));
    assert_eq!(err, "");
    assert_eq!(hub.connects, 1);
}

#[test]
fn cascade_sends_cascade_and_lists_the_dependents() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let mut hub = FakeHub::new(&[accepted()]);
    let (out, _, got) = run_delete(&setup, &mut hub, ROOT, true, true, false, &mut Unread);
    got.unwrap();
    assert_eq!(
        hub.sent(),
        json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT, "cascade": true}})
    );
    assert_eq!(out, format!("{ROOT}\n{FORK}\n"));
}

#[test]
fn a_terminal_lists_then_asks_and_yes_deletes() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let mut hub = FakeHub::new(&[accepted()]);
    let mut input = io::Cursor::new(b"y\n".to_vec());
    let (out, err, got) = run_delete(&setup, &mut hub, ROOT, true, false, true, &mut input);
    got.unwrap();
    // The list before the question, then each id sent once accepted.
    assert_eq!(out, format!("{ROOT}\n{FORK}\n{ROOT}\n{FORK}\n"));
    assert_eq!(err, "delete? [y/N] ");
    assert_eq!(hub.sent()["args"]["session"], ROOT);
}

#[test]
fn without_cascade_the_list_is_only_the_session() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let mut hub = FakeHub::new(&[accepted()]);
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (out, _, got) = run_delete(&setup, &mut hub, ROOT, false, false, true, &mut input);
    got.unwrap();
    assert_eq!(out, format!("{ROOT}\n"));
}

#[test]
fn a_declined_answer_sends_nothing() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let mut hub = FakeHub::new(&[accepted()]);
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (_, err, got) = run_delete(&setup, &mut hub, ROOT, false, false, true, &mut input);
    got.unwrap();
    assert!(err.ends_with("nothing deleted\n"), "{err}");
    assert_eq!(hub.connects, 0);
}

#[test]
fn no_terminal_without_yes_is_usage_and_neither_reads_nor_connects() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let mut hub = FakeHub::new(&[accepted()]);
    let (out, _, got) = run_delete(&setup, &mut hub, ROOT, false, false, false, &mut Unread);
    let failure = got.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("--yes"), "{}", failure.message);
    assert_eq!(out, "");
    assert_eq!(hub.connects, 0);
}

#[test]
fn a_rejection_is_a_failure_with_its_code_and_message() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let rejected = json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "code": "session_has_dependents", "message": "It has forks.",
    }});
    let mut hub = FakeHub::new(&[rejected]);
    let (out, _, got) = run_delete(&setup, &mut hub, ROOT, false, true, false, &mut Unread);
    let failure = got.unwrap_err();
    assert_eq!(failure.code, ErrorCode::SessionHasDependents);
    assert_eq!(failure.message, "It has forks.");
    assert_eq!(out, "", "nothing is printed as deleted");
}

#[test]
fn the_hub_closing_before_answering_is_io_failed() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let mut hub = FakeHub::new(&[]);
    let (out, _, got) = run_delete(&setup, &mut hub, ROOT, false, true, false, &mut Unread);
    assert_eq!(got.unwrap_err().code, ErrorCode::IoFailed);
    assert_eq!(out, "");
}

#[test]
fn an_unknown_selector_is_not_found_and_never_connects() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let mut hub = FakeHub::new(&[accepted()]);
    let (_, _, got) = run_delete(&setup, &mut hub, "s_zz", false, true, false, &mut Unread);
    assert_eq!(got.unwrap_err().code, ErrorCode::SessionNotFound);
    assert_eq!(hub.connects, 0);
}
