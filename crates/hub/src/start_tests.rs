//! Tests for `start`: workspace checks, the wait for `run/<id>`, the
//! process that exits first, and the `content` handshake.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use serde_json::{Value, json};

use super::*;
use crate::connection::Hub;
use crate::fake::{FakeStarter, Handshake, failure};

/// One named deadline per wait: `start` answers before it.
const DEADLINE: Duration = Duration::from_secs(30);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hs");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn hub(&self, starter: FakeStarter) -> Hub {
        Hub::new(
            &self.dir,
            "0.0.0",
            Arc::new(starter),
            fakes::clock::FakeClock::new(),
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
fn started(hub: Hub, workspace: String, model: Option<String>, content: Option<Value>) -> Outcome {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-start".to_owned())
        .spawn(move || {
            let outcome = run(&hub, &workspace, model.as_deref(), content.as_ref());
            done_tx.send(outcome).unwrap_or(());
        })
        .unwrap();
    done_rx
        .recv_timeout(DEADLINE)
        .expect("start answers before its deadline")
}

#[test]
fn a_relative_workspace_is_invalid_arguments() {
    let temp = Temp::new();
    let hub = temp.hub(FakeStarter::hang(&temp.dir));
    let outcome = started(hub, "relative/path".to_owned(), None, None);
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
    let outcome = started(hub, workspace, None, None);
    let Outcome::Accepted { session_id } = outcome else {
        panic!("the start is accepted");
    };
    assert!(is_hex_id(&session_id.0));
    assert!(temp.dir.join("run").join(&session_id.0).exists());
}

#[test]
fn a_session_that_exits_first_rejects_with_its_fiber_exited() {
    let temp = Temp::new();
    let exited = failure(ErrorCode::NoModel, "No model is configured.");
    let hub = temp.hub(FakeStarter::exit_with(&temp.dir, exited));
    let workspace = temp.workspace();
    let outcome = started(hub, workspace, None, None);
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
    let outcome = started(hub, workspace, None, None);
    let Outcome::Rejected { code, .. } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::IoFailed);
}

#[test]
fn content_is_delivered_as_the_first_prompt_and_accepted() {
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: true,
            code: String::new(),
            message: String::new(),
        },
    );
    let hub = temp.hub(starter.clone());
    let workspace = temp.workspace();
    let content = json!([{"type": "text", "text": "hi"}]);
    let outcome = started(hub, workspace, None, Some(content.clone()));
    let Outcome::Accepted { .. } = outcome else {
        panic!("the start is accepted");
    };
    let received = starter.received();
    assert_eq!(received.len(), 1);
    let prompt: Value = serde_json::from_str(&received[0]).unwrap();
    assert_eq!(prompt.get("command"), Some(&json!("prompt")));
    assert_eq!(
        prompt.get("args").and_then(|args| args.get("content")),
        Some(&content)
    );
}

#[test]
fn a_rejected_first_prompt_rejects_the_start_with_its_code() {
    let temp = Temp::new();
    let starter = FakeStarter::with_handshake(
        &temp.dir,
        Handshake {
            accept: false,
            code: "invalid_arguments".to_owned(),
            message: "The prompt is empty.".to_owned(),
        },
    );
    let hub = temp.hub(starter);
    let workspace = temp.workspace();
    let content = json!([{"type": "text", "text": "hi"}]);
    let outcome = started(hub, workspace, None, Some(content));
    let Outcome::Rejected { code, message } = outcome else {
        panic!("the start is rejected");
    };
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(message, "The prompt is empty.");
}
