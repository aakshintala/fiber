//! `fiber sessions export`'s resolver and printer, and `fiber sessions
//! delete`'s confirmation and its one `delete` to an in-process fake hub,
//! with the session directories built by hand.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::ErrorCode;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use contract::{HubEventKind, HubLine};
use serde_json::{Map, Value, json};

use super::{Ask, HubAnswer, delete, delete_listed, delete_run, export, matched, run};
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
    /// same project as `worktree::project` derives inside `run`.
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

/// An in-process hub holding a shared answer queue: every `connect`
/// hands out one end of a pair, and a thread on the other answers each
/// command line it reads with the next queued answer, recording every
/// line it read. The queue and the log are shared across connects, so a
/// resolve on one connection and its dry run and confirm on the next
/// read successive answers in order.
struct FakeHub {
    connects: usize,
    answers: Arc<Mutex<VecDeque<String>>>,
    sent: Arc<Mutex<Vec<String>>>,
    got: Option<mpsc::Receiver<String>>,
}

impl FakeHub {
    fn new(answers: &[Value]) -> Self {
        Self {
            connects: 0,
            answers: Arc::new(Mutex::new(
                answers.iter().map(|line| format!("{line}\n")).collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
            got: None,
        }
    }

    fn connect(&mut self) -> Result<doors::hub::Hub, doors::Error> {
        self.connects += 1;
        let (client, hub) = UnixStream::pair().map_err(doors::Error::Local)?;
        hub.set_read_timeout(Some(HUB_DEADLINE))
            .map_err(doors::Error::Local)?;
        let (tx, rx) = mpsc::channel();
        let answers = Arc::clone(&self.answers);
        let sent = Arc::clone(&self.sent);
        thread::spawn(move || {
            let mut lines = BufReader::new(&hub);
            let mut first = Some(tx);
            loop {
                let mut line = String::new();
                match lines.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                sent.lock().unwrap().push(line.clone());
                if let Some(tx) = first.take() {
                    tx.send(line.clone()).unwrap();
                }
                // Answers queued lines until an accepted or rejected line
                // for the command just read, so a future kind with the same
                // command id is emitted but never ends the batch, as
                // `request_value` skips it and reads on; a stray line for
                // another command is skipped the same way. An empty queue
                // ends the connection, as a dropped hub end does.
                let want = request_id(&line);
                loop {
                    let Some(answer) = answers.lock().unwrap().pop_front() else {
                        break;
                    };
                    let done = match (want.as_deref(), answer_command_id(&answer)) {
                        (Some(want), Some(got)) => {
                            want == got
                                && matches!(
                                    answer_kind(&answer).as_deref(),
                                    Some("command_accepted" | "command_rejected")
                                )
                        }
                        _ => true,
                    };
                    if (&hub).write_all(answer.as_bytes()).is_err() {
                        break;
                    }
                    if done {
                        break;
                    }
                }
                if answers.lock().unwrap().is_empty() {
                    break;
                }
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

    /// The first command line the hub read, as JSON.
    #[track_caller]
    fn sent(&self) -> Value {
        let line = Deadline::after(HUB_DEADLINE)
            .recv(self.got.as_ref().unwrap())
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Every command line the hub read, as JSON, in read order.
    fn sent_all(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// The request `id` of a command line the hub read, when it parses.
fn request_id(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|line| line.get("id")?.as_str().map(str::to_owned))
}

/// The answered `command_id` of a queued answer line, when it parses.
fn answer_command_id(answer: &str) -> Option<String> {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|answer| {
            answer
                .get("payload")?
                .get("command_id")?
                .as_str()
                .map(str::to_owned)
        })
}

/// The `kind` of a queued answer line, when it parses.
fn answer_kind(answer: &str) -> Option<String> {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|answer| answer.get("kind")?.as_str().map(str::to_owned))
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
        let code = delete(ROOT, false, case != "usage", None, &mut || hub.connect());
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

fn hub_answer(kind: HubEventKind, payload: Value) -> HubAnswer {
    HubAnswer {
        kind,
        payload: payload.as_object().unwrap().clone(),
    }
}

#[test]
fn an_answer_for_another_command_id_is_no_answer() {
    let line = hub_answer(
        HubEventKind::CommandAccepted,
        json!({"command_id": "c_other", "result": 1}),
    );
    assert!(matched(line, "c_1", || "delete".to_owned()).is_none());
}

#[test]
fn an_accepted_answer_carries_its_result() {
    let line = hub_answer(
        HubEventKind::CommandAccepted,
        json!({"command_id": "c_1", "result": {"token": "t"}}),
    );
    assert_eq!(
        matched(line, "c_1", || "pair".to_owned()).unwrap().unwrap(),
        json!({"token": "t"})
    );
}

#[test]
fn an_accepted_answer_without_a_result_is_null() {
    let line = hub_answer(HubEventKind::CommandAccepted, json!({"command_id": "c_1"}));
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
        HubEventKind::CommandRejected,
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
        HubEventKind::CommandRejected,
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
        let error = matched(
            hub_answer(HubEventKind::CommandRejected, payload),
            "c_1",
            || "delete".to_owned(),
        )
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
        HubEventKind::CommandRejected,
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
    let line = hub_answer(HubEventKind::SessionLeft, json!({"command_id": "c_1"}));
    assert!(matched(line, "c_1", || "delete".to_owned()).is_none());
}

/// One complete hub JSON line enters as its typed kind.
fn entered(kind: &str, payload: Value) -> HubAnswer {
    serde_json::from_value(json!({"kind": kind, "ts": 1, "schema_version": 1, "payload": payload}))
        .unwrap()
}

#[test]
fn hub_json_enters_as_its_typed_kind() {
    for (name, expected) in [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("stream_closed", HubEventKind::StreamClosed),
        ("stream_reopened", HubEventKind::StreamReopened),
        ("command_accepted", HubEventKind::CommandAccepted),
        ("command_rejected", HubEventKind::CommandRejected),
    ] {
        let line = entered(name, json!({}));
        assert_eq!(line.kind, expected, "{name}");
    }
}

#[test]
fn an_unfamiliar_wire_kind_keeps_its_text_at_entry_and_answers_nothing() {
    let line: HubAnswer = serde_json::from_str(
        r#"{"kind":"future_kind","ts":1,"schema_version":1,"payload":{"command_id":"c_1","result":1}}"#,
    )
    .unwrap();
    assert_eq!(line.kind, HubEventKind::Unknown("future_kind".into()));
    assert!(matched(line, "c_1", || "delete".to_owned()).is_none());
}

#[test]
fn a_typed_answer_keeps_the_wire_shape_validation() {
    for line in [
        json!({"ts": 1, "schema_version": 1, "payload": {}}),
        json!({"kind": "command_accepted", "schema_version": 1, "payload": {}}),
        json!({"kind": "command_accepted", "ts": 1, "payload": {}}),
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1}),
        json!({"kind": 7, "ts": 1, "schema_version": 1, "payload": {}}),
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": null}),
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": []}),
    ] {
        assert!(
            serde_json::from_value::<HubAnswer>(line.clone()).is_err(),
            "{line}"
        );
    }
    let line: HubAnswer = serde_json::from_value(
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1, "payload": {}, "future": true}),
    )
    .unwrap();
    assert_eq!(line.kind, HubEventKind::CommandAccepted);
}

#[test]
fn every_known_non_answer_kind_with_the_matching_id_is_no_answer() {
    for (name, kind) in [
        ("hub_hello", HubEventKind::HubHello),
        ("attention", HubEventKind::Attention),
        ("device_changed", HubEventKind::DeviceChanged),
        ("session_left", HubEventKind::SessionLeft),
        ("stream_closed", HubEventKind::StreamClosed),
        ("stream_reopened", HubEventKind::StreamReopened),
    ] {
        let line = hub_answer(kind, json!({"command_id": "c_1"}));
        assert!(
            matched(line, "c_1", || "delete".to_owned()).is_none(),
            "{name}"
        );
    }
}

#[test]
fn an_accepted_answer_never_runs_the_fallback() {
    let line = hub_answer(
        HubEventKind::CommandAccepted,
        json!({"command_id": "c_1", "result": 1}),
    );
    assert_eq!(
        matched(line, "c_1", || panic!(
            "an accepted answer names no command"
        ),)
        .unwrap()
        .unwrap(),
        json!(1)
    );
}

#[test]
fn an_unknown_line_before_an_accepted_answer_reads_the_answer() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let unknown = json!({"kind": "future_kind", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "result": {"token": "wrong"},
    }});
    assert_eq!(
        serde_json::from_value::<HubAnswer>(unknown.clone())
            .unwrap()
            .kind,
        HubEventKind::Unknown("future_kind".into()),
    );
    let (out, _, got, _, _) = run_delete(
        &setup,
        &[unknown, accepted()],
        ROOT,
        false,
        true,
        false,
        Unread,
    );
    got.unwrap();
    assert_eq!(out, format!("{ROOT}\n"));
}

#[test]
fn an_unknown_line_before_a_rejection_reads_the_refusal() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let unknown = json!({"kind": "future_kind", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "code": "session_held", "message": "held",
    }});
    assert_eq!(
        serde_json::from_value::<HubAnswer>(unknown.clone())
            .unwrap()
            .kind,
        HubEventKind::Unknown("future_kind".into()),
    );
    let (out, _, got, _, _) = run_delete(
        &setup,
        &[unknown, refused("c_delete", "busy", "run")],
        ROOT,
        false,
        true,
        false,
        Unread,
    );
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
    assert_eq!(error.to_string(), "run");
    assert_eq!(out, "", "nothing is printed as deleted");
}

#[test]
fn an_unknown_line_then_a_close_is_io_failed() {
    let setup = Setup::new();
    setup.started(ROOT, None);
    let unknown = json!({"kind": "future_kind", "ts": 1, "schema_version": 1, "payload": {
        "command_id": "c_delete", "result": 1,
    }});
    let (out, _, got, _, _) = run_delete(&setup, &[unknown], ROOT, false, true, false, Unread);
    let error = got.unwrap_err();
    assert!(
        matches!(&error, CliError::HubClosed { command } if command == "delete"),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(out, "");
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

/// Sessions only the hub answers with: they live on the hub, never in the
/// local sessions directory, so a passing hub test proves the hub path
/// reads no local log.
const HUB_ROOT: &str = "s_11111111111111a1";
const HUB_FORK: &str = "s_11111111111111b2";
const HUB_OTHER: &str = "s_11111111111111c3";

/// A `sessions` answer naming `ids` as exited sessions.
fn sessions_answer(ids: &[&str]) -> Value {
    let exited: Vec<Value> = ids
        .iter()
        .map(|id| json!({"session_id": id, "name": "n", "how": "exited"}))
        .collect();
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_sessions", "result": {"live": [], "exited": exited}}})
}

/// A dry-run answer naming `ids` as the set the delete would remove.
fn dry_answer(ids: &[&str]) -> Value {
    dry_answer_with_held(ids, &[])
}

/// A dry-run answer with an explicit set of busy ids.
fn dry_answer_with_held(ids: &[&str], held: &[&str]) -> Value {
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_delete", "result": {"sessions": ids, "held": held}}})
}

/// A rejection of `command_id` with `code` and `message`.
fn refused(command_id: &str, code: &str, message: &str) -> Value {
    json!({"kind": "command_rejected", "ts": 1, "schema_version": 1,
        "payload": {"command_id": command_id, "code": code, "message": message}})
}

/// Runs `delete_listed` on hub `box` against scripted `answers`, on a
/// worker thread: its stdout, its stderr, its outcome, how often it
/// connected, and every command the hub read. The test thread receives
/// the outcome under `HUB_DEADLINE`.
fn run_delete_listed(
    answers: &[Value],
    selector: &str,
    cascade: bool,
    yes: bool,
    terminal: bool,
    input: impl BufRead + Send + 'static,
) -> (String, String, Result<(), CliError>, usize, Vec<Value>) {
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
        let got = delete_listed("box", &selector, cascade, ask, &mut out, &mut || {
            hub.connect()
        });
        tx.send((
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
            got,
            hub.connects,
            hub.sent_all(),
        ))
        .unwrap_or(());
    });
    Deadline::after(HUB_DEADLINE).recv(&rx).unwrap()
}

#[test]
fn hub_delete_without_yes_confirms_the_dry_run_set_exactly() {
    let (out, err, got, _, sent) = run_delete_listed(
        &[
            sessions_answer(&[HUB_ROOT, HUB_FORK]),
            dry_answer(&[HUB_ROOT, HUB_FORK]),
            accepted(),
        ],
        "s_11111111111111a",
        true,
        false,
        true,
        io::Cursor::new(b"y\n".to_vec()),
    );
    got.unwrap();
    // The answered set prints before the question and once accepted.
    assert_eq!(
        out,
        format!("{HUB_ROOT}\n{HUB_FORK}\n{HUB_ROOT}\n{HUB_FORK}\n")
    );
    assert_eq!(err, "delete? [y/N] ");
    // Order and count per docs/invocation.md:1421-1426: the id resolves
    // against the hub's `sessions` answer, then a dry run prints and asks,
    // then `delete` carries that exact set as `expect` (docs/invocation.md:1415).
    assert_eq!(
        sent,
        vec![
            json!({"id": "c_sessions", "command": "sessions", "args": {}}),
            json!({"id": "c_delete", "command": "delete",
                "args": {"session": HUB_ROOT, "cascade": true, "dry_run": true}}),
            json!({"id": "c_delete", "command": "delete",
                "args": {"session": HUB_ROOT, "cascade": true,
                    "expect": [HUB_ROOT, HUB_FORK]}}),
        ]
    );
}

#[test]
fn hub_delete_refuses_successful_dry_run_with_held_before_asking() {
    for held in [HUB_ROOT, HUB_FORK] {
        let (out, err, got, _, sent) = run_delete_listed(
            &[
                sessions_answer(&[HUB_ROOT, HUB_FORK]),
                dry_answer_with_held(&[HUB_ROOT, HUB_FORK], &[held]),
            ],
            HUB_ROOT,
            true,
            false,
            true,
            Unread,
        );
        let error = got.unwrap_err();
        assert!(
            matches!(
                &error,
                CliError::Rejected {
                    code: ErrorCode::SessionHeld,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("Session `{held}` is held by another process.")
        );
        assert_eq!(out, "", "a held dry run prints no preview");
        assert_eq!(err, "", "a held dry run prints no prompt");
        assert_eq!(
            sent,
            vec![
                json!({"id": "c_sessions", "command": "sessions", "args": {}}),
                json!({"id": "c_delete", "command": "delete",
                    "args": {"session": HUB_ROOT, "cascade": true, "dry_run": true}}),
            ],
            "no real delete follows a held dry run"
        );
    }
}

#[test]
fn hub_delete_reports_the_first_held_session_in_preview_order() {
    let (out, err, got, _, sent) = run_delete_listed(
        &[
            sessions_answer(&[HUB_ROOT, HUB_FORK]),
            dry_answer_with_held(&[HUB_ROOT, HUB_FORK], &[HUB_FORK, HUB_ROOT]),
        ],
        HUB_ROOT,
        true,
        false,
        true,
        Unread,
    );
    let error = got.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("Session `{HUB_ROOT}` is held by another process.")
    );
    assert_eq!(out, "");
    assert_eq!(err, "");
    assert_eq!(sent.len(), 2);
}

#[test]
fn hub_delete_malformed_held_answer_fails_before_printing_or_asking() {
    let malformed = json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_delete", "result": {"sessions": [HUB_ROOT], "held": "bad"}}});
    let (out, err, got, _, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_ROOT]), malformed],
        HUB_ROOT,
        false,
        false,
        true,
        Unread,
    );
    assert!(matches!(got, Err(CliError::SessionsAnswer(_))));
    assert_eq!(out, "");
    assert_eq!(err, "");
    assert_eq!(sent.len(), 2, "only resolve and dry run are sent");
}

/// A dry-run answer that fails validation prints nothing, asks nothing
/// and sends nothing after the dry run: the `Unread` input panics if the
/// question is ever read.
fn bad_dry_run_is_unusable(result: Value) {
    let (out, err, got, _, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_ROOT]), result],
        HUB_ROOT,
        true,
        false,
        true,
        Unread,
    );
    let error = got.unwrap_err();
    assert!(matches!(error, CliError::SessionsAnswer(_)), "{error:?}");
    assert_eq!(out, "", "a bad dry run prints no preview");
    assert_eq!(err, "", "a bad dry run prints no prompt");
    assert_eq!(sent.len(), 2, "only resolve and dry run are sent");
}

#[test]
fn hub_delete_rejects_dry_run_missing_its_target() {
    bad_dry_run_is_unusable(dry_answer(&[HUB_FORK]));
}

#[test]
fn hub_delete_rejects_empty_dry_run_set() {
    bad_dry_run_is_unusable(dry_answer(&[]));
}

#[test]
fn hub_delete_rejects_duplicate_dry_run_ids() {
    bad_dry_run_is_unusable(dry_answer(&[HUB_ROOT, HUB_ROOT]));
}

#[test]
fn hub_delete_rejects_unminted_dry_run_ids() {
    for id in [
        "s_xyz",
        "s_ABCDEF1234567890",
        "s_123",
        "s_11111111111111a1x",
        "11111111111111a1",
        "../s_11111111111111a1",
    ] {
        // Keep the requested session present so rejecting an unminted
        // cascade id cannot pass merely because the target is missing.
        bad_dry_run_is_unusable(dry_answer(&[HUB_ROOT, id]));
    }
}

#[test]
fn hub_delete_rejects_held_outside_the_set() {
    bad_dry_run_is_unusable(dry_answer_with_held(&[HUB_ROOT], &[HUB_FORK]));
}

#[test]
fn hub_delete_rejects_duplicate_held_ids() {
    bad_dry_run_is_unusable(dry_answer_with_held(
        &[HUB_ROOT, HUB_FORK],
        &[HUB_FORK, HUB_FORK],
    ));
}

#[test]
fn hub_delete_rejects_unminted_held_ids() {
    bad_dry_run_is_unusable(dry_answer_with_held(&[HUB_ROOT], &["bogus"]));
}

#[test]
fn hub_delete_rejects_dry_run_missing_its_held_field() {
    bad_dry_run_is_unusable(
        json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_delete", "result": {"sessions": [HUB_ROOT]}}}),
    );
}

#[test]
fn hub_delete_yes_sends_one_delete_without_expect() {
    let (out, err, got, _, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_ROOT]), accepted()],
        HUB_ROOT,
        false,
        true,
        false,
        Unread,
    );
    got.unwrap();
    // One `delete` with no `expect` and no dry run: the exact line
    // proves both keys are absent (docs/invocation.md:1424-1426: with
    // `--yes` one `delete` with no `expect` follows the `sessions` resolve).
    assert_eq!(
        sent,
        vec![
            json!({"id": "c_sessions", "command": "sessions", "args": {}}),
            json!({"id": "c_delete", "command": "delete", "args": {"session": HUB_ROOT}}),
        ]
    );
    assert_eq!(out, format!("{HUB_ROOT}\n"));
    assert_eq!(err, "");
}

#[test]
fn hub_delete_unknown_and_ambiguous_ids_fail_after_resolving() {
    let (_, _, got, _, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_OTHER])],
        "s_zz",
        false,
        true,
        false,
        Unread,
    );
    let error = got.unwrap_err();
    assert!(
        matches!(&error, CliError::SessionNotFoundOnHub { .. }),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::SessionNotFound);
    // The resolve connected; no dry run follows a failed resolve
    // (docs/invocation.md:1421-1424: the dry run comes only after a resolve).
    assert_eq!(sent.len(), 1);

    let (_, _, got, _, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_ROOT, HUB_FORK])],
        "s_11111111111111",
        false,
        true,
        false,
        Unread,
    );
    let error = got.unwrap_err();
    assert!(
        matches!(&error, CliError::Log(log::Error::Ambiguous { .. })),
        "{error:?}"
    );
    assert_eq!(error.code(), ErrorCode::Usage);
    // One `sessions` resolve and nothing after the ambiguous match
    // (docs/invocation.md:1421-1424: the dry run comes only after a resolve).
    assert_eq!(sent.len(), 1);
}

#[test]
fn hub_delete_without_a_terminal_never_connects() {
    let (out, _, got, connects, sent) = run_delete_listed(
        &[sessions_answer(&[HUB_ROOT])],
        HUB_ROOT,
        false,
        false,
        false,
        Unread,
    );
    let error = got.unwrap_err();
    assert!(matches!(&error, CliError::DeleteNobody), "{error:?}");
    assert_eq!(error.code(), ErrorCode::Usage);
    assert_eq!(out, "");
    // Nobody to ask fails before connecting (docs/invocation.md:1418-1420:
    // with no terminal to ask on, asking is a usage error), so no request.
    assert_eq!(connects, 0);
    assert!(sent.is_empty());
}

#[test]
fn hub_delete_keeps_refusal_codes() {
    // A held session fails the dry run with its own code.
    let (out, _, got, _, _) = run_delete_listed(
        &[
            sessions_answer(&[HUB_ROOT]),
            refused("c_delete", "session_held", "Session is held."),
        ],
        HUB_ROOT,
        false,
        false,
        true,
        io::Cursor::new(b"y\n".to_vec()),
    );
    let error = got.unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::SessionHeld,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "Session is held.");
    assert_eq!(out, "", "a refused dry run prints nothing");

    // A set changed mid-prompt fails the confirm with its own code and
    // prints only the previewed set.
    let (out, _, got, _, _) = run_delete_listed(
        &[
            sessions_answer(&[HUB_ROOT]),
            dry_answer(&[HUB_ROOT]),
            refused("c_delete", "stale_request", "The set changed."),
        ],
        HUB_ROOT,
        false,
        false,
        true,
        io::Cursor::new(b"y\n".to_vec()),
    );
    let error = got.unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::Rejected {
                code: ErrorCode::StaleRequest,
                ..
            }
        ),
        "{error:?}"
    );
    assert_eq!(out, format!("{HUB_ROOT}\n"));
}
