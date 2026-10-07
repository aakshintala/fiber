//! Tests for the internal hub command's process handling: the exit code of
//! a failed start, what `exited` reports, the stdout drain keeping the last
//! `fiber_exited` line, and parsing that line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hub::Started;

use super::*;

/// One named deadline per wait: the drain finishes before it.
const DRAIN_DEADLINE: Duration = Duration::from_secs(10);

fn exited_line(code: &str, message: &str) -> String {
    serde_json::json!({
        "kind": "fiber_exited",
        "ts": 1,
        "schema_version": 1,
        "payload": {"error": {"code": code, "message": message}},
    })
    .to_string()
}

/// A child printing `lines` on stdout, then exiting 0.
fn printing(lines: &[String]) -> Child {
    let mut script = String::from("printf '%s\\n'");
    for line in lines {
        script.push_str(" '");
        script.push_str(&line.replace('\'', "'\\''"));
        script.push('\'');
    }
    Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn drained(child: Child) -> Arc<Mutex<State>> {
    let state = Arc::new(Mutex::new(State {
        exited: false,
        failure: None,
    }));
    let child = Arc::new(Mutex::new(child));
    // `drain` blocks reading stdout to EOF and reaping the child: a wait,
    // so the test receives it with a wall-clock deadline.
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-drain".to_owned())
        .spawn({
            let child = Arc::clone(&child);
            let state = Arc::clone(&state);
            move || {
                drain(&child, &state);
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    done_rx
        .recv_timeout(DRAIN_DEADLINE)
        .expect("drain finishes before its deadline");
    state
}

fn failure_of(state: &Arc<Mutex<State>>) -> Option<Failure> {
    lock(state).failure.clone()
}

#[test]
fn fail_exits_1() {
    assert_eq!(
        fail(failure(
            ErrorCode::IoFailed,
            "the current directory: gone".to_owned()
        )),
        1
    );
}

#[test]
fn a_usage_failure_exits_2() {
    assert_eq!(
        fail(failure(ErrorCode::Usage, "FIBER_HOME is empty.".to_owned())),
        2
    );
}

#[test]
fn finish_passes_the_hub_exit_code_through() {
    assert_eq!(finish(Ok(0)), 0);
    assert_eq!(finish(Ok(143)), 143);
}

#[test]
fn finish_exits_2_for_a_too_long_home_and_1_for_an_io_failure() {
    assert_eq!(finish(Err(hub::StartError::HomeTooLong { max: 103 })), 2);
    let io = hub::StartError::Io {
        path: PathBuf::from("/h/run"),
        source: std::io::Error::other("refused"),
    };
    assert_eq!(finish(Err(io)), 1);
}

/// A Fiber home and a workspace under one temporary root.
fn home_and_workspace(root: &fakes::TempDir) -> (PathBuf, PathBuf) {
    let home = root.path().join("h");
    let workspace = root.path().join("w");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    (home, workspace)
}

#[test]
fn configure_without_a_current_directory_is_io_failed_naming_it() {
    let root = fakes::TempDir::new("hcfg");
    let (home, _) = home_and_workspace(&root);
    let Err(failure) = configure(&home, Err(std::io::Error::other("gone"))) else {
        panic!("no current directory cannot configure");
    };
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert_eq!(failure.message, "the current directory: gone");
}

#[test]
fn configure_with_invalid_json_is_config_invalid() {
    let root = fakes::TempDir::new("hcfg");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(home.join("config.json"), "{").unwrap();
    let Err(failure) = configure(&home, Ok(workspace)) else {
        panic!("invalid JSON cannot configure");
    };
    assert_eq!(failure.code, ErrorCode::ConfigInvalid);
    assert!(
        failure.message.contains("config.json"),
        "{}",
        failure.message
    );
}

#[test]
fn configure_reads_the_idle_exit() {
    let root = fakes::TempDir::new("hcfg");
    let (home, workspace) = home_and_workspace(&root);
    std::fs::write(
        home.join("config.json"),
        r#"{"hub": {"idle_exit_ms": 200}}"#,
    )
    .unwrap();
    assert_eq!(
        configure(&home, Ok(workspace)).unwrap(),
        Duration::from_millis(200)
    );
}

#[test]
fn exited_is_none_while_the_session_runs() {
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state: Arc::new(Mutex::new(State {
            exited: false,
            failure: None,
        })),
    };
    assert!(spawned.exited().is_none());
}

#[test]
fn exited_reports_the_drained_failure() {
    let child = printing(&[exited_line("no_model", "No model is configured.")]);
    let state = drained(child);
    assert!(lock(&state).exited);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state,
    };
    let failure = spawned.exited().expect("the drained failure");
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, "No model is configured.");
}

#[test]
fn exited_without_a_verdict_is_io_failed() {
    let child = printing(&["not a verdict".to_owned()]);
    let state = drained(child);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        state,
    };
    let failure = spawned.exited().expect("the fallback failure");
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(failure.message.contains("s_0123456789abcdef"));
}

#[test]
fn drain_keeps_the_last_fiber_exited_line() {
    let child = printing(&[
        exited_line("no_model", "No model is configured."),
        exited_line("invalid_request", "The request was bad."),
        "noise".to_owned(),
    ]);
    let state = drained(child);
    assert!(lock(&state).exited);
    let failure = failure_of(&state).expect("the last verdict");
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert_eq!(failure.message, "The request was bad.");
}

#[test]
fn drain_marks_exited_once_stdout_closes() {
    let child = printing(&[]);
    let state = drained(child);
    assert!(lock(&state).exited);
    assert!(failure_of(&state).is_none());
}

#[test]
fn fiber_exited_error_parses_code_and_message() {
    let failure = fiber_exited_error(&exited_line("no_model", "No model is configured."))
        .expect("the verdict");
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, "No model is configured.");
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
}

#[test]
fn fiber_exited_error_rejects_anything_but_a_verdict() {
    for line in [
        "not json".to_owned(),
        serde_json::json!({"kind": "turn_completed", "payload": {}}).to_string(),
        serde_json::json!({
            "kind": "fiber_exited",
            "payload": {"error": {"code": 7, "message": "x"}},
        })
        .to_string(),
        serde_json::json!({
            "kind": "fiber_exited",
            "payload": {"error": {"code": "no_model"}},
        })
        .to_string(),
        serde_json::json!({"kind": "fiber_exited", "payload": {}}).to_string(),
    ] {
        assert!(fiber_exited_error(&line).is_none(), "{line}");
    }
}

fn args_of(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_session_command_starts_with_a_model_and_never_a_prompt() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let command = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        Some("fake/m"),
        false,
    );
    assert_eq!(command.get_program(), "/bin/fiber");
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--model",
            "fake/m"
        ]
    );
}

#[test]
fn the_session_command_resumes_with_the_recorded_workspace_and_no_model() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let command = session_command(Path::new("/bin/fiber"), &id, Path::new("/w"), None, true);
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id",
            "s_0123456789abcdef",
            "--workspace",
            "/w",
            "--resume"
        ]
    );
}

/// An executable that prints a `fiber_exited` line whose message is
/// `marker`, so a start through it is told apart from any other binary.
fn stub_binary(root: &fakes::TempDir, marker: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.path().join("fiber-stub");
    let line = exited_line("io_failed", marker).replace('\'', "'\\''");
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{line}'\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The failure a started session reports once it has exited.
fn exit_of(started: Box<dyn hub::Started>) -> Failure {
    let (done_tx, done_rx) = mpsc::channel();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    thread::Builder::new()
        .name("hub-test-exit".to_owned())
        .spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(failure) = started.exited() {
                        done_tx.send(failure).unwrap_or(());
                        return;
                    }
                    thread::yield_now();
                }
            }
        })
        .unwrap();
    let exit = done_rx.recv_timeout(DRAIN_DEADLINE);
    // The poll thread ends with the wait, whichever way the wait ended.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    exit.expect("the session exits before its deadline")
}

#[test]
fn start_runs_the_recorded_path() {
    let root = fakes::TempDir::new("hstart");
    let (_, workspace) = home_and_workspace(&root);
    let starter = SpawnStarter {
        exe: stub_binary(&root, "recorded start"),
    };
    let started =
        hub::Starter::start(&starter, &SessionId("s1".to_owned()), &workspace, None).unwrap();
    assert_eq!(exit_of(started).message, "recorded start");
}

#[test]
fn resume_runs_the_recorded_path() {
    let root = fakes::TempDir::new("hresume");
    let (_, workspace) = home_and_workspace(&root);
    let starter = SpawnStarter {
        exe: stub_binary(&root, "recorded resume"),
    };
    let started = hub::Starter::resume(&starter, &SessionId("s1".to_owned()), &workspace).unwrap();
    assert_eq!(exit_of(started).message, "recorded resume");
}
