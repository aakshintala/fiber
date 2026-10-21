//! `fiber sessions export`'s resolver and printer, and `fiber sessions
//! delete`'s confirmation and its one `delete` to an in-process fake hub,
//! with the session directories built by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use contract::HubLine;
use serde_json::{Map, Value, json};

use super::{Ask, delete, delete_run, export, matched, run};
use crate::CliError;
use fakes::Deadline;

/// One hub failure per `doors::Error` shape, with its listed code.
fn hub_cases() -> Vec<(doors::Error, ErrorCode)> {
    vec![
        (
            doors::Error::Local(io::Error::other("down")),
            ErrorCode::IoFailed,
        ),
        (
            doors::Error::UnknownHub { name: "box".into() },
            ErrorCode::Usage,
        ),
        (
            doors::Error::MissingDefault { name: "box".into() },
            ErrorCode::ConfigInvalid,
        ),
        (
            doors::Error::MissingToken { name: "box".into() },
            ErrorCode::CredentialMissing,
        ),
        (
            doors::Error::Hello {
                hub: "box".into(),
                source: io::Error::other("down"),
            },
            ErrorCode::ConnectionFailed,
        ),
        (
            doors::Error::Refused {
                hub: "box".into(),
                message: "revoked".into(),
            },
            ErrorCode::AuthenticationFailed,
        ),
    ]
}

#[test]
fn hub_failure_preserves_local_text_and_listed_error_codes() {
    for (error, code) in hub_cases() {
        let expected = if matches!(error, doors::Error::Local(_)) {
            "the hub: down".into()
        } else {
            error.to_string()
        };
        let failure = super::hub_failure(error);
        assert_eq!(failure.code, code);
        assert_eq!(failure.message, expected);
    }
    for (error, code) in hub_cases() {
        let expected = if matches!(error, doors::Error::Local(_)) {
            "the hub: down".to_owned()
        } else {
            error.to_string()
        };
        let error = CliError::from(error);
        assert_eq!(error.code(), code);
        assert_eq!(error.to_string(), expected);
    }
}

/// A `session_started` first line for session `id` in `workspace`: a full
/// envelope, so `resolve` keeps it, in this project's workspace. `from`
/// continues a session, as a fork does.
fn started(id: &str, workspace: &Path, from: Option<&str>) -> String {
    let mut payload = json!({"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    format!(
        "{}\n",
        json!({"kind": "session_started", "session_id": id, "ts": 0,
            "schema_version": 1, "seq": 0, "payload": payload})
    )
}

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
    /// artifact and a lock file. The first line starts the session in
    /// this workspace, so `resolve` keeps it.
    fn session(&self, id: &str) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            format!("{}{SECOND}{TORN}", started(id, &self.workspace(), None)),
        )
        .unwrap();
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
        format!(
            "{}{SECOND}",
            started("s_exportaa01", &setup.workspace(), None)
        )
        .into_bytes()
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
        format!(
            "{}{SECOND}",
            started("s_exportbb01", &setup.workspace(), None)
        )
        .into_bytes()
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
    let error = run(
        &setup.home(),
        &setup.workspace(),
        "s_missing",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, CliError::Log(log::Error::NotFound(_))),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::SessionNotFound);
}

#[test]
fn an_ambiguous_prefix_names_both_matches() {
    let setup = Setup::new();
    setup.session("s_amb11111");
    setup.session("s_amb22222");
    let error = run(
        &setup.home(),
        &setup.workspace(),
        "s_amb",
        None,
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::Usage);
    let CliError::Log(log::Error::Ambiguous { matches, .. }) = &error else {
        panic!("expected an ambiguous log error, got {error:?}");
    };
    assert_eq!(matches, &["s_amb11111".to_owned(), "s_amb22222".to_owned()]);
}

/// The child's marker: set, the test runs `export` and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the re-run child may run before the test kills its group and
/// fails: half of nextest's per-test timeout less the cleanup bound.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

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
    let output = fakes::rerun_prepared(
        &format!("{name}::export_exits_zero_and_writes_the_export"),
        &[(CHILD, "1")],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command
                .env("FIBER_HOME", setup.home())
                .current_dir(setup.workspace());
        },
    );
    assert_eq!(output.status.code(), Some(0), "{}", output.status);
    assert_eq!(
        fs::read(setup.workspace().join(CHILD_ID).join("events.jsonl")).unwrap(),
        format!("{}{SECOND}", started(CHILD_ID, &setup.workspace(), None)).into_bytes()
    );
}

/// The ids of sessions `delete` tests name.
const ROOT: &str = "s_00000000000000a1";
const FORK: &str = "s_00000000000000b2";

/// One named deadline for the fake hub's read and the test's receive.
const HUB_DEADLINE: Duration = Duration::from_secs(10);

impl Setup {
    /// Session `id` in the workspace's project, continuing `from`.
    fn started(&self, id: &str, from: Option<&str>) {
        let dir = self.sessions().join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("events.jsonl"),
            started(id, &self.workspace(), from),
        )
        .unwrap();
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

    fn connect(&mut self) -> Result<doors::hub::Hub, doors::Error> {
        self.connects += 1;
        let (client, hub) = UnixStream::pair().map_err(doors::Error::Local)?;
        hub.set_read_timeout(Some(HUB_DEADLINE))
            .map_err(doors::Error::Local)?;
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
    #[track_caller]
    fn sent(&self) -> Value {
        let line = Deadline::after(HUB_DEADLINE)
            .recv(self.got.as_ref().unwrap())
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

/// Runs `delete_run` for `selector` against a fake hub answering `answers`,
/// on a worker thread (edge: `thread::spawn`): its stdout, its stderr, its
/// outcome, how often it connected, and the command the hub read when it
/// connected. The test thread receives the outcome under `HUB_DEADLINE`.
fn run_delete(
    setup: &Setup,
    answers: &[Value],
    selector: &str,
    cascade: bool,
    yes: bool,
    terminal: bool,
    input: impl BufRead + Send + 'static,
) -> (String, String, Result<(), CliError>, usize, Option<Value>) {
    let home = setup.home();
    let workspace = setup.workspace();
    let answers = answers.to_owned();
    let selector = selector.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&answers);
        let mut input = input;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let ask = Ask {
            yes,
            terminal,
            input: &mut input,
            err: &mut err,
        };
        let got = delete_run(
            &home,
            &workspace,
            &selector,
            cascade,
            ask,
            &mut out,
            &mut || hub.connect(),
        );
        let sent = if hub.connects > 0 {
            Some(hub.sent())
        } else {
            None
        };
        tx.send((
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            got,
            hub.connects,
            sent,
        ))
        .unwrap_or(());
    });
    Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

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
        let output = fakes::rerun_prepared(
            &format!("{name}::delete_exits_zero_on_success_two_on_usage_and_one_on_refusal"),
            &[("FIBER_CLI_DELETE_CHILD", case)],
            Deadline::after(HUB_DEADLINE),
            |command| {
                command
                    .env("FIBER_HOME", setup.home())
                    .current_dir(setup.workspace());
            },
        );
        assert_eq!(
            output.status.code(),
            Some(code),
            "{case}: {}",
            output.status
        );
    }
}

#[test]
fn yes_sends_one_delete_for_the_full_id_and_prints_it() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    // A line for another command comes first, and is skipped.
    let other = json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": {"command_id": "c_other"}});
    let (out, err, got, connects, sent) = run_delete(
        &setup,
        &[other, accepted()],
        "s_00000000000000a",
        false,
        true,
        false,
        Unread,
    );
    got.unwrap();
    assert_eq!(
        sent.unwrap(),
        json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT}})
    );
    assert_eq!(out, format!("{ROOT}\n"));
    assert_eq!(err, "");
    assert_eq!(connects, 1);
}

#[test]
fn cascade_sends_cascade_and_lists_the_dependents() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let (out, _, got, _, sent) = run_delete(&setup, &[accepted()], ROOT, true, true, false, Unread);
    got.unwrap();
    assert_eq!(
        sent.unwrap(),
        json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT, "cascade": true}})
    );
    assert_eq!(out, format!("{ROOT}\n{FORK}\n"));
}

#[test]
fn a_terminal_lists_then_asks_and_yes_deletes() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let (out, err, got, _, sent) = run_delete(
        &setup,
        &[accepted()],
        ROOT,
        true,
        false,
        true,
        io::Cursor::new(b"y\n".to_vec()),
    );
    got.unwrap();
    // The list before the question, then each id sent once accepted.
    assert_eq!(out, format!("{ROOT}\n{FORK}\n{ROOT}\n{FORK}\n"));
    assert_eq!(err, "delete? [y/N] ");
    assert_eq!(sent.unwrap()["args"]["session"], ROOT);
}

#[test]
fn without_cascade_the_list_is_only_the_session() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    setup.started(FORK, Some(ROOT));
    let (out, _, got, _, _) = run_delete(
        &setup,
        &[accepted()],
        ROOT,
        false,
        false,
        true,
        io::Cursor::new(b"n\n".to_vec()),
    );
    got.unwrap();
    assert_eq!(out, format!("{ROOT}\n"));
}

#[test]
fn a_declined_answer_sends_nothing() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let (_, err, got, connects, _) = run_delete(
        &setup,
        &[accepted()],
        ROOT,
        false,
        false,
        true,
        io::Cursor::new(b"n\n".to_vec()),
    );
    got.unwrap();
    assert!(err.ends_with("nothing deleted\n"), "{err}");
    assert_eq!(connects, 0);
}

#[test]
fn no_terminal_without_yes_is_usage_and_neither_reads_nor_connects() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let (out, _, got, connects, _) =
        run_delete(&setup, &[accepted()], ROOT, false, false, false, Unread);
    let error = got.unwrap_err();
    assert!(matches!(&error, CliError::DeleteNobody), "{error:?}");
    assert_eq!(error.code(), ErrorCode::Usage);
    assert!(error.to_string().contains("--yes"), "{error:?}");
    assert_eq!(out, "");
    assert_eq!(connects, 0);
}

#[test]
fn a_rejection_is_a_failure_with_its_code_and_message() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let rejected = json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "code": "session_has_dependents", "message": "It has forks.",
    }});
    let (out, _, got, _, _) = run_delete(&setup, &[rejected], ROOT, false, true, false, Unread);
    let error = got.unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::SessionHasDependents,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::SessionHasDependents);
    assert_eq!(error.to_string(), "It has forks.");
    assert_eq!(out, "", "nothing is printed as deleted");
}

#[test]
fn the_hub_closing_before_answering_is_io_failed() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let (out, _, got, _, _) = run_delete(&setup, &[], ROOT, false, true, false, Unread);
    let error = got.unwrap_err();
    assert!(
        matches!(&error, CliError::HubClosed { command } if command == "delete"),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(
        error.to_string(),
        "the hub closed the connection before answering `delete`"
    );
    assert_eq!(out, "");
}

#[test]
fn a_rejection_without_a_message_through_delete_names_delete() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let rejected = json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "code": "busy",
    }});
    let (_, _, got, _, _) = run_delete(&setup, &[rejected], ROOT, false, true, false, Unread);
    let error = got.unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::Busy,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Busy);
    assert_eq!(error.to_string(), "the hub refused `delete`");
}

#[test]
fn a_delete_rejection_without_a_usable_code_is_io_failed() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    for payload in [
        json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
            "command_id": "c_delete", "message": "m",
        }}),
        json!({"kind": "command_rejected", "ts": 1, "schema_version": 1, "payload": {
            "command_id": "c_delete", "code": 7, "message": "m",
        }}),
    ] {
        let (_, _, got, _, _) = run_delete(&setup, &[payload], ROOT, false, true, false, Unread);
        let error = got.unwrap_err();
        assert!(
            matches!(
                &error,
                CliError::Rejected {
                    code: ErrorCode::IoFailed,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(error.code(), ErrorCode::IoFailed);
        assert_eq!(error.to_string(), "m");
    }
}

#[test]
fn an_unknown_selector_is_not_found_and_never_connects() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let (_, _, got, connects, _) =
        run_delete(&setup, &[accepted()], "s_zz", false, true, false, Unread);
    let error = got.unwrap_err();
    assert!(
        matches!(&error, CliError::Log(log::Error::NotFound(_))),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::SessionNotFound);
    assert_eq!(connects, 0);
}

fn hub_answer(kind: &str, payload: Value) -> HubLine {
    HubLine {
        kind: kind.into(),
        ts: 1,
        schema_version: 1,
        payload: payload.as_object().unwrap().clone(),
    }
}

#[test]
fn an_answer_for_another_command_id_is_no_answer() {
    let line = hub_answer(
        "command_accepted",
        json!({"command_id": "c_other", "result": 1}),
    );
    assert!(matched(line, "c_1", || "delete".to_owned()).is_none());
}

#[test]
fn an_accepted_answer_carries_its_result() {
    let line = hub_answer(
        "command_accepted",
        json!({"command_id": "c_1", "result": {"token": "t"}}),
    );
    assert_eq!(
        matched(line, "c_1", || "pair".to_owned()).unwrap().unwrap(),
        json!({"token": "t"})
    );
}

#[test]
fn an_accepted_answer_without_a_result_is_null() {
    let line = hub_answer("command_accepted", json!({"command_id": "c_1"}));
    assert_eq!(
        matched(line, "c_1", || "refresh".to_owned())
            .unwrap()
            .unwrap(),
        Value::Null
    );
}

#[test]
fn a_rejected_answer_carries_its_code_and_message() {
    let line = hub_answer(
        "command_rejected",
        json!({"command_id": "c_1", "code": "busy", "message": "run"}),
    );
    let error = matched(line, "c_1", || "delete".to_owned())
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::Busy,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Busy);
    assert_eq!(error.to_string(), "run");
}

#[test]
fn an_unknown_rejection_code_is_other() {
    let line = hub_answer(
        "command_rejected",
        json!({"command_id": "c_1", "code": "future", "message": "m"}),
    );
    let error = matched(line, "c_1", || "delete".to_owned())
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&error, CliError::Rejected { code: ErrorCode::Other(code), .. } if code == "future"),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Other("future".into()));
}

#[test]
fn a_rejection_without_a_usable_code_is_io_failed() {
    for payload in [
        json!({"command_id": "c_1", "message": "m"}),
        json!({"command_id": "c_1", "code": 7, "message": "m"}),
    ] {
        let error = matched(hub_answer("command_rejected", payload), "c_1", || {
            "delete".to_owned()
        })
        .unwrap()
        .unwrap_err();
        assert!(
            matches!(
                &error,
                CliError::Rejected {
                    code: ErrorCode::IoFailed,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(error.code(), ErrorCode::IoFailed);
        assert_eq!(error.to_string(), "m");
    }
}

#[test]
fn a_rejection_without_a_message_names_the_command() {
    let line = hub_answer(
        "command_rejected",
        json!({"command_id": "c_1", "code": "busy"}),
    );
    let error = matched(line, "c_1", || "delete".to_owned())
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::Busy,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "the hub refused `delete`");
}

#[test]
fn another_kind_with_the_matching_id_is_no_answer() {
    let line = hub_answer("session_left", json!({"command_id": "c_1"}));
    assert!(matched(line, "c_1", || "delete".to_owned()).is_none());
}

/// A writer that always fails.
struct FailWrite;

impl io::Write for FailWrite {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_accepted_delete_with_a_failing_writer_is_io_failed_and_stays_deleted() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let home = setup.home();
    let workspace = setup.workspace();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut hub = FakeHub::new(&[accepted()]);
        let mut unread = Unread;
        let mut err = Vec::new();
        let ask = Ask {
            yes: true,
            terminal: false,
            input: &mut unread,
            err: &mut err,
        };
        let mut out = FailWrite;
        let got = delete_run(&home, &workspace, ROOT, false, ask, &mut out, &mut || {
            hub.connect()
        });
        let sent = hub.sent();
        tx.send((got, sent, hub.connects)).unwrap_or(());
    });
    let (got, sent, connects): (Result<(), CliError>, Value, usize) =
        Deadline::after(HUB_DEADLINE).recv(&rx).unwrap();
    let error = got.unwrap_err();
    assert!(matches!(&error, CliError::Forwarded(_)), "{error:?}");
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(
        sent,
        json!({"id": "c_delete", "command": "delete", "args": {"session": ROOT}})
    );
    assert_eq!(connects, 1);
}
