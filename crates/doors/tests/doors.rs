//! The doors crate through its public API: the prompt rules, the line a
//! process prints before any session exists, the project's identity, and a
//! session process's boundary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use contract::events::{
    AssistantMessageCompleted, Empty, Event, InputItem, MessageOutcome, TurnCompleted, TurnOutcome,
    TurnStarted, UsageRecorded,
};
use contract::shapes::{ContentPart, Failure, Origin, Sender, Tokens};
use contract::{ActionId, CommandId, ErrorCode, GenerationId, TurnId};
use doors::{Session, exit_before_session, failure, project, prompt};
use serde_json::Value;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory, removed on drop, with a short name: a session's
/// socket path must fit in 103 bytes on macOS.
struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fd{}-{n}", std::process::id()));
        fs::remove_dir_all(&dir).unwrap_or(());
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap_or(());
    }
}

/// A writer the test reads back after the session is done with it.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// A reader that fails the test if read: stdin on a terminal is never read.
struct Untouched;

impl io::Read for Untouched {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("stdin on a terminal was read");
    }
}

fn code(result: Result<String, Failure>) -> ErrorCode {
    result.unwrap_err().code
}

#[test]
fn the_prompt_is_the_argument_or_stdin_and_never_both_or_neither() {
    assert_eq!(
        prompt(Some("hi".into()), &mut Untouched, true).unwrap(),
        "hi"
    );
    assert_eq!(
        prompt(Some("hi".into()), &mut &b""[..], false).unwrap(),
        "hi"
    );
    assert_eq!(
        prompt(None, &mut &b"brief\n"[..], false).unwrap(),
        "brief\n"
    );
    assert_eq!(
        code(prompt(Some("hi".into()), &mut &b"brief"[..], false)),
        ErrorCode::Usage
    );
    assert_eq!(code(prompt(None, &mut Untouched, true)), ErrorCode::Usage);
    assert_eq!(
        code(prompt(None, &mut &b" \n"[..], false)),
        ErrorCode::Usage
    );
    assert_eq!(
        code(prompt(Some(" ".into()), &mut Untouched, true)),
        ErrorCode::Usage
    );
    assert_eq!(
        code(prompt(None, &mut &[0xff, 0xfe][..], false)),
        ErrorCode::Usage
    );
}

#[test]
fn a_failure_before_any_session_prints_one_line_with_no_session_and_one_sentence() {
    for (failure, exit) in [
        (failure(ErrorCode::Usage, "No prompt."), 2),
        (failure(ErrorCode::NoModel, "No model was chosen."), 1),
    ] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let message = failure.message.clone();
        let error = serde_json::to_value(&failure).unwrap();

        assert_eq!(exit_before_session(failure, &mut out, &mut err), exit);

        let line: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(line["kind"], "fiber_exited");
        assert_eq!(line.get("session_id"), None);
        assert_eq!(line["payload"]["exit_code"], exit);
        assert_eq!(line["payload"]["error"], error);
        assert!(out.ends_with(b"}\n"));
        assert_eq!(
            String::from_utf8(err).unwrap(),
            format!("fiber: {message}\n")
        );
    }
}

#[test]
fn the_project_is_gits_shared_directory_or_the_launch_directory() {
    let temp = Temp::new();
    let plain = temp.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(project(&plain), fs::canonicalize(&plain).unwrap());

    let repo = temp.0.join("repo");
    fs::create_dir_all(repo.join("docs")).unwrap();
    let init = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&repo)
        .status();
    assert!(init.unwrap().success());
    let common = fs::canonicalize(repo.join(".git")).unwrap();
    assert_eq!(project(&repo), common);
    assert_eq!(project(&repo.join("docs")), common);
}

fn sessions(temp: &Temp) -> PathBuf {
    temp.0.join("h/projects/p/sessions")
}

fn start(temp: &Temp, out: &Shared) -> Session {
    Session::start(&temp.0.join("h"), &sessions(temp), Box::new(out.clone())).unwrap()
}

fn socket(temp: &Temp, id: &str) -> PathBuf {
    temp.0.join("h/run").join(id)
}

#[test]
fn a_session_binds_its_socket_and_one_that_never_got_a_prompt_leaves_nothing() {
    let temp = Temp::new();
    let out = Shared::default();
    let session = start(&temp, &out);
    let dir = only_session(&sessions(&temp));
    let id = dir.file_name().unwrap().to_str().unwrap().to_owned();
    let path = socket(&temp, &id);
    UnixStream::connect(&path).expect("the session's socket accepts a connection");
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let run = fs::metadata(temp.0.join("h/run"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(run & 0o777, 0o700);

    assert_eq!(session.exit(Ok(())), 0);

    assert!(!path.exists(), "the socket is unlinked on exit");
    assert!(
        !dir.exists(),
        "a session with no turn deletes its directory"
    );
    let lines = out.lines();
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["fiber_started", "fiber_exited"]);
    assert_eq!(lines[0]["session_id"], id.as_str());
    assert_eq!(lines[0]["payload"]["resumed"], false);
    assert_eq!(lines[1]["payload"]["exit_code"], 0);
    assert_eq!(lines[1]["payload"]["usage"]["cost"], 0.0);
}

fn only_session(sessions: &Path) -> PathBuf {
    let dirs: Vec<PathBuf> = fs::read_dir(sessions)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1);
    dirs[0].clone()
}

fn usage(output: u64, cost: Option<f64>, subscription: Option<bool>) -> Event {
    Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId("g".into()),
        model: "fake/m".into(),
        tokens: Tokens {
            input: 10,
            cache_read: 4,
            cache_write: BTreeMap::from([("1h".to_owned(), 2)]),
            output,
        },
        web_searches: None,
        cost,
        subscription,
        extension: None,
        origin_session_id: None,
    })
}

fn message(text: &str) -> Event {
    Event::AssistantMessageCompleted(AssistantMessageCompleted {
        outcome: MessageOutcome::Completed,
        text: text.into(),
        error: None,
        attempt: None,
    })
}

fn turn_started() -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: CommandId("c_1".into()),
            },
            changed_by: None,
        }],
    })
}

fn turn_completed(outcome: TurnOutcome, error: Option<Failure>) -> Event {
    Event::TurnCompleted(TurnCompleted {
        outcome,
        error,
        questions: None,
    })
}

#[test]
fn fiber_exited_carries_the_final_message_and_the_usage_and_stdout_is_the_log() {
    let temp = Temp::new();
    let out = Shared::default();
    let session = start(&temp, &out);
    let log = session.log();
    let turn = || Some(TurnId("t_1".into()));
    let action = |id: &str| Some(ActionId(id.into()));
    log.append(&turn_started(), turn(), None).unwrap();
    log.append(&Event::StepStarted(Empty {}), turn(), None)
        .unwrap();
    log.append(&usage(3, Some(0.5), None), turn(), action("a_1"))
        .unwrap();
    log.append(&message("Let me check."), turn(), action("a_1"))
        .unwrap();
    log.append(&usage(5, Some(0.25), None), turn(), action("a_2"))
        .unwrap();
    log.append(&usage(7, None, None), turn(), action("a_2"))
        .unwrap();
    log.append(&usage(1, Some(2.0), Some(true)), turn(), action("a_2"))
        .unwrap();
    log.append(&message("Hello."), turn(), action("a_2"))
        .unwrap();
    log.append(&turn_completed(TurnOutcome::Completed, None), turn(), None)
        .unwrap();
    drop(log);
    let dir = only_session(&sessions(&temp));

    assert_eq!(session.exit(Ok(())), 0);

    let lines = out.lines();
    let exited = &lines.last().unwrap()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    assert_eq!(exited["final_action_id"], "a_2");
    assert_eq!(exited.get("error"), None);
    let usage = &exited["usage"];
    assert_eq!(usage["tokens"]["output"], 16);
    assert_eq!(usage["tokens"]["input"], 40);
    assert_eq!(usage["tokens"]["cache_read"], 16);
    assert_eq!(usage["tokens"]["cache_write"]["1h"], 8);
    assert_eq!(usage["cost"], 0.75);
    assert_eq!(usage["subscription_cost"], 2.0);

    // The directory stays, and stdout holds the log's lines byte for byte.
    let log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    let printed = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert_eq!(printed, log);
}

#[test]
fn a_failed_turn_or_a_failed_run_exits_1_with_its_error() {
    let temp = Temp::new();
    let out = Shared::default();
    let session = start(&temp, &out);
    let log = session.log();
    let turn = || Some(TurnId("t_1".into()));
    let cause = failure(ErrorCode::ProviderUnavailable, "down");
    log.append(&turn_started(), turn(), None).unwrap();
    log.append(&message("Earlier."), turn(), Some(ActionId("a_1".into())))
        .unwrap();
    log.append(
        &turn_completed(TurnOutcome::Failed, Some(cause.clone())),
        turn(),
        None,
    )
    .unwrap();
    drop(log);

    assert_eq!(session.exit(Ok(())), 1);
    let exited = &out.lines().last().unwrap()["payload"].clone();
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], serde_json::to_value(&cause).unwrap());
    assert_eq!(exited.get("text"), None);
    // No billed call: the cost is 0, not null.
    assert_eq!(exited["usage"]["cost"], 0.0);

    let out = Shared::default();
    let session = start(&temp, &out);
    session
        .log()
        .append(&turn_started(), Some(TurnId("t_1".into())), None)
        .unwrap();
    let broke = failure(ErrorCode::IoFailed, "disk full");

    assert_eq!(session.exit(Err(broke.clone())), 1);
    let exited = &out.lines().last().unwrap()["payload"].clone();
    assert_eq!(exited["error"], serde_json::to_value(&broke).unwrap());
}

#[test]
fn a_billed_call_with_no_known_cost_makes_the_cost_null() {
    let temp = Temp::new();
    let out = Shared::default();
    let session = start(&temp, &out);
    let log = session.log();
    log.append(&turn_started(), Some(TurnId("t_1".into())), None)
        .unwrap();
    log.append(&usage(1, None, None), Some(TurnId("t_1".into())), None)
        .unwrap();
    drop(log);

    assert_eq!(session.exit(Ok(())), 0);
    assert_eq!(
        out.lines().last().unwrap()["payload"]["usage"]["cost"],
        Value::Null
    );
}

#[test]
fn a_fiber_home_too_long_for_a_socket_is_a_usage_error_and_leaves_nothing() {
    let temp = Temp::new();
    let home = temp.0.join("h".repeat(110));
    let sessions = home.join("projects/p/sessions");

    let error = Session::start(&home, &sessions, Box::new(Shared::default()))
        .err()
        .unwrap();

    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.message.contains("FIBER_HOME"));
    assert!(!home.exists());
}
