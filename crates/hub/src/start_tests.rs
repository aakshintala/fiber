//! Tests for `start`: workspace checks, the wait for `run/<id>`, the
//! process that exits first, and the `content` handshake.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::{CommandId, SessionId};
use serde_json::{Value, json};

use super::*;
use crate::connection::Hub;
use crate::diag::Diag;
use crate::fake::{FakeStarter, Handshake, failure};

/// One named deadline per wait: `start` answers before it.
const DEADLINE: Duration = Duration::from_secs(30);

/// The handshake tests' wall-clock deadline: under ten seconds, below the
/// twenty a stuck acknowledgement would hang, so a mutant that never
/// matches the ack fails here instead of at the harness.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
    clock: Arc<fakes::clock::FakeClock>,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hs");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            held,
            clock: fakes::clock::FakeClock::new(),
        }
    }

    fn hub(&self, starter: FakeStarter) -> Hub {
        self.hub_with(starter)
    }

    fn hub_with(&self, starter: impl crate::Starter + 'static) -> Hub {
        let clock = Arc::clone(&self.clock);
        let timed: Arc<dyn Clock> = clock;
        Hub::new(
            &self.dir,
            "0.0.0",
            Arc::new(starter),
            Arc::clone(&timed),
            Diag::open(&self.dir, timed),
        )
    }

    fn workspace(&self) -> String {
        let workspace = self.dir.join("w");
        fs::create_dir_all(&workspace).unwrap();
        workspace.to_string_lossy().into_owned()
    }
}

fn is_hex_id(id: &str) -> bool {
    id.len() == 18
        && id.starts_with("s_")
        && id.bytes().skip(2).all(|byte| byte.is_ascii_hexdigit())
}

/// Runs `start` on a thread: calling code that blocks is a wait, so the
/// test receives its result with a wall-clock deadline.
fn started(
    hub: Hub,
    workspace: String,
    model: Option<String>,
    worktree: bool,
    content: Option<Value>,
) -> Outcome {
    started_before(hub, workspace, model, worktree, content, DEADLINE)
}

fn started_before(
    hub: Hub,
    workspace: String,
    model: Option<String>,
    worktree: bool,
    content: Option<Value>,
    deadline: Duration,
) -> Outcome {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-start".to_owned())
        .spawn(move || {
            let outcome = run(
                &hub,
                &CommandId("c_start".to_owned()),
                &workspace,
                model.as_deref(),
                worktree,
                content.as_ref(),
            );
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(deadline)
        .expect("start answers before its deadline")
}

#[test]
fn a_relative_workspace_is_invalid_arguments() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let outcome = started(hub, "relative/path".to_owned(), None, false, None);
    let Outcome::Rejected { code, .. } = outcome else {
        panic!("a relative workspace is rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
}

#[test]
fn a_workspace_that_is_no_directory_is_invalid_arguments() {
    let temp = Temp::new();
    fs::write(temp.dir.join("file"), b"x").unwrap();
    for workspace in [
        "/absent/fiber-hub-test".to_owned(),
        temp.dir.join("file").display().to_string(),
    ] {
        let outcome = started(
            temp.hub(FakeStarter::hang(&temp.dir)),
            workspace.clone(),
            None,
            false,
            None,
        );
        let Outcome::Rejected { code, .. } = outcome else {
            panic!("{workspace} is rejected");
        };
        assert_eq!(code, ErrorCode::InvalidArguments);
    }
}

#[test]
fn an_accepted_start_mints_a_session_id_and_binds_its_socket() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, false, None);
    let Outcome::Accepted { session_id, first } = outcome else {
        panic!("the start is accepted");
    };
    assert!(first.is_none(), "a start without content holds no prompt");
    assert!(is_hex_id(&session_id.0));
    assert!(temp.dir.join("run").join(&session_id.0).exists());
}

#[test]
fn worktree_reaches_the_starter() {
    let temp = Temp::new();
    let starter = FakeStarter::bind_and_hold(&temp.dir);
    let workspace = temp.workspace();
    let outcome = started(temp.hub(starter.clone()), workspace, None, true, None);
    let Outcome::Accepted { .. } = outcome else {
        panic!("the start is accepted");
    };
    assert_eq!(starter.started_worktrees(), [true]);
}

#[test]
fn a_session_that_exits_first_rejects_with_its_fiber_exited() {
    let temp = Temp::new();
    let exited = failure(ErrorCode::NoModel, "No model is configured.");
    let hub = temp.hub(FakeStarter::exit_with(&temp.dir, exited));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, false, None);
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::NoModel);
    assert_eq!(message, "No model is configured.");
}

#[test]
fn a_session_that_neither_binds_nor_exits_is_rejected_io_failed() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, false, None);
    let Outcome::Rejected { code, .. } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    // The waitout lasts the whole deadline: returning at once would fail
    // the start before the session could bind.
    assert!(temp.clock.now() >= temp.clock.origin() + START_DEADLINE);
}

#[test]
fn an_unreachable_socket_error_fails_without_retrying() {
    let temp = Temp::new();
    // `run/` is a file, so connecting fails before any bind: not a missing
    // socket, so there is nothing to wait for.
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, false, None);
    let Outcome::Rejected { code, .. } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(temp.clock.now() < temp.clock.origin() + START_DEADLINE);
}

/// A starter that fails with `message` as its io error text.
struct FailStarter {
    message: String,
}

impl crate::Starter for FailStarter {
    fn start(
        &self,
        _id: &SessionId,
        _workspace: &Path,
        _model: Option<&str>,
        _worktree: bool,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other(self.message.clone()))
    }

    fn resume(&self, _id: &SessionId, _workspace: &Path) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other(self.message.clone()))
    }

    fn rewind(
        &self,
        _id: &SessionId,
        _workspace: &Path,
        _from: &SessionId,
    ) -> io::Result<Box<dyn crate::Started>> {
        Err(io::Error::other(self.message.clone()))
    }
}

#[test]
fn a_starter_error_keeps_its_detail_out_of_the_log() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let starter = FailStarter {
        message: format!("starter blew up on {SECRET}."),
    };
    let hub = temp.hub_with(starter);
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, Some(SECRET.to_owned()), false, None);
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(
        message.contains(SECRET),
        "the client keeps the starter detail"
    );
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("\"code\":\"io_failed\""));
    assert!(log.contains("could not start."));
    assert!(!log.contains(SECRET), "no starter text in the log");
}

#[test]
fn an_unexpected_connect_error_keeps_its_detail_out_of_the_log() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    // `run/` is a file, so connecting fails NotADirectory on every
    // platform: not a missing socket, so there is nothing to wait for.
    // The workspace name plants a path secret the log must never hold.
    fs::write(temp.dir.join("run"), b"x").unwrap();
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    fs::create_dir_all(&workspace).unwrap();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let outcome = started(
        hub,
        workspace.to_string_lossy().into_owned(),
        Some(SECRET.to_owned()),
        false,
        None,
    );
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(
        message.contains("could not start:"),
        "the client keeps the connect detail"
    );
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("\"code\":\"io_failed\""));
    assert!(log.contains("could not start."));
    assert!(!log.contains(SECRET), "no path or model text in the log");
    assert!(
        !log.contains("could not start:"),
        "no connect detail in the log"
    );
}

#[test]
fn a_failed_start_logs_the_code_and_a_fixed_sentence() {
    let temp = Temp::new();
    let exited = failure(ErrorCode::NoModel, "No model is configured.");
    let hub = temp.hub(FakeStarter::exit_with(&temp.dir, exited));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, false, None);
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::NoModel);
    assert_eq!(message, "No model is configured.");
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("\"code\":\"no_model\""));
    assert!(!log.contains("No model is configured."));
}

#[test]
fn an_exited_failure_keeps_session_text_out_of_the_log() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let exited = failure(ErrorCode::NoModel, &format!("No model for {SECRET}."));
    let hub = temp.hub(FakeStarter::exit_with(&temp.dir, exited));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, Some(SECRET.to_owned()), false, None);
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::NoModel);
    assert!(
        message.contains(SECRET),
        "the client keeps what the session said"
    );
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("\"code\":\"no_model\""));
    assert!(!log.contains(SECRET), "no session text in the log");
}

#[test]
fn session_started_keeps_workspace_and_model_out_of_the_log() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::bind_and_hold(&temp.dir));
    let workspace = temp.dir.join(format!("w-{SECRET}"));
    fs::create_dir_all(&workspace).unwrap();
    let outcome = started(
        hub,
        workspace.to_string_lossy().into_owned(),
        Some(SECRET.to_owned()),
        false,
        None,
    );
    let Outcome::Accepted { .. } = outcome else {
        panic!("the start is accepted");
    };
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(log.contains("\"code\":\"session_started\""));
    assert!(
        !log.contains(SECRET),
        "no workspace or model text in the log"
    );
}

fn accepting() -> Handshake {
    Handshake {
        accept: true,
        code: String::new(),
        message: String::new(),
    }
}

/// Runs `start` with `content` on a thread and returns the hub and the
/// held first prompt.
fn held(temp: &Temp, starter: FakeStarter, content: &Value) -> (Arc<Hub>, Box<Held>) {
    let hub = Arc::new(temp.hub(starter));
    let workspace = temp.workspace();
    let (done_tx, done_rx) = mpsc::channel();
    let running = Arc::clone(&hub);
    let content = content.clone();
    thread::Builder::new()
        .name("hub-test-start".to_owned())
        .spawn(move || {
            let outcome = run(
                &running,
                &CommandId("c_start".to_owned()),
                &workspace,
                None,
                false,
                Some(&content),
            );
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    let outcome = done_rx
        .recv_timeout(HANDSHAKE_DEADLINE)
        .expect("start answers before its deadline");
    let Outcome::Accepted {
        first: Some(held), ..
    } = outcome
    else {
        panic!("the start is accepted with its prompt held");
    };
    (hub, held)
}

/// Sends the held prompt on a thread, within the handshake deadline.
fn prompted(hub: Arc<Hub>, held: Held) -> Result<(), Outcome> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-prompt".to_owned())
        .spawn(move || done_tx.send(prompt(held, &hub)).unwrap_or(()))
        .unwrap();
    done_rx
        .recv_timeout(HANDSHAKE_DEADLINE)
        .expect("the prompt is answered before its deadline")
}

#[test]
fn content_is_held_until_the_hub_sends_it() {
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(&temp.dir, accepting());
    let content = json!([{"type": "text", "text": "hi"}]);
    let (hub, held) = held(&temp, starter.clone(), &content);
    let received = starter.received();
    assert_eq!(received.len(), 1, "only the subscription before the answer");
    let subscribe: Value = serde_json::from_str(&received[0]).unwrap();
    assert_eq!(subscribe.get("command"), Some(&json!("subscribe")));
    assert_eq!(subscribe.get("args"), Some(&json!({"level": "summary"})));
    assert!(prompted(hub, *held).is_ok());
    let received = starter.received();
    assert_eq!(received.len(), 2);
    let prompt: Value = serde_json::from_str(&received[1]).unwrap();
    assert_eq!(prompt.get("command"), Some(&json!("prompt")));
    assert_eq!(
        prompt.get("args").and_then(|args| args.get("content")),
        Some(&content)
    );
}

#[test]
fn a_rejected_first_prompt_returns_the_session_code() {
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: false,
            code: "invalid_arguments".to_owned(),
            message: "The prompt is empty.".to_owned(),
        },
    );
    let content = json!([{"type": "text", "text": "hi"}]);
    let (hub, held) = held(&temp, starter, &content);
    let Err(Outcome::Rejected { code, message }) = prompted(hub, *held) else {
        panic!("the prompt is rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "The prompt is empty.");
}

#[test]
fn a_rejected_first_prompt_keeps_session_text_out_of_the_log() {
    const SECRET: &str = "the-volume-of-the-meeting-room";
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: false,
            code: "invalid_arguments".to_owned(),
            message: format!("The prompt {SECRET} is empty."),
        },
    );
    let content = json!([{"type": "text", "text": SECRET}]);
    let (hub, held) = held(&temp, starter, &content);
    let session = held.id.0.clone();
    let Err(Outcome::Rejected { code, message }) = prompted(hub, *held) else {
        panic!("the prompt is rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert!(
        message.contains(SECRET),
        "the result keeps what the session said"
    );
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    assert!(!log.contains(SECRET), "no prompt text in the log");
    let line = log
        .lines()
        .find(|line| line.contains("first prompt"))
        .expect("the rejection is logged");
    assert!(line.contains("\"code\":\"invalid_arguments\""));
    assert!(line.contains(&session), "the line names the session");
    assert!(line.contains("c_start"), "the line names the start command");
}

#[test]
fn a_session_gone_before_the_first_prompt_logs_io_failed() {
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(&temp.dir, accepting());
    let content = json!([{"type": "text", "text": "hi"}]);
    let (hub, held) = held(&temp, starter.clone(), &content);
    let session = held.id.clone();
    assert!(starter.stop(&session, HANDSHAKE_DEADLINE));
    let Err(Outcome::Rejected { code, .. }) = prompted(hub, *held) else {
        panic!("the prompt is not taken");
    };
    assert_eq!(code, ErrorCode::IoFailed);
    let log = fs::read_to_string(temp.dir.join("logs").join("hub.log")).unwrap();
    let line = log
        .lines()
        .find(|line| line.contains("did not take the first prompt of command c_start."))
        .expect("the lost prompt is logged");
    assert!(line.contains("\"code\":\"io_failed\""));
    assert!(line.contains(&session.0));
}
