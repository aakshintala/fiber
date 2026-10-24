//! Tests for the hub's session starter: the command lines, the exit
//! code of a failed start, what `exited` reports, the stdout drain
//! keeping the last `fiber_exited` line, and parsing that line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::VariablesSource;
use contract::{ErrorCode, SessionId};
use fakes::Deadline;

use super::*;
use crate::testkit::await_true;
use crate::{Environment, Environments, Started, Starter};

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

/// What `drained` hands back: the shared child and its drain state.
type Drained = (Arc<Mutex<Child>>, Arc<(Mutex<State>, Condvar)>);

#[track_caller]
fn drained(child: Child) -> Drained {
    let state: Arc<(Mutex<State>, Condvar)> = Arc::new((
        Mutex::new(State {
            exited: false,
            failure: None,
            reading: None,
        }),
        Condvar::new(),
    ));
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
    Deadline::after(DRAIN_DEADLINE)
        .recv(&done_rx)
        .expect("drain finishes before its deadline");
    (child, state)
}

fn failure_of(state: &Arc<(Mutex<State>, Condvar)>) -> Option<Failure> {
    support::lock(&state.0).failure.clone()
}

#[test]
fn exited_is_none_while_the_session_runs() {
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        child: Arc::new(Mutex::new(printing(&[]))),
        state: Arc::new((
            Mutex::new(State {
                exited: false,
                failure: None,
                reading: None,
            }),
            Condvar::new(),
        )),
    };
    assert!(spawned.exited().is_none());
    // Reap the only child this test made: it printed nothing and exited.
    support::lock(&spawned.child).wait().unwrap();
}

#[test]
fn exited_reports_the_drained_failure() {
    let child = printing(&[exited_line("no_model", "No model is configured.")]);
    let (child, state) = drained(child);
    assert!(support::lock(&state.0).exited);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        child,
        state,
    };
    let failure = spawned.exited().expect("the drained failure");
    assert_eq!(failure.code, ErrorCode::NoModel);
    assert_eq!(failure.message, "No model is configured.");
}

#[test]
fn exited_without_a_verdict_is_io_failed() {
    let child = printing(&["not a verdict".to_owned()]);
    let (child, state) = drained(child);
    let spawned = Spawned {
        id: SessionId("s_0123456789abcdef".to_owned()),
        child,
        state,
    };
    let failure = spawned.exited().expect("the fallback failure");
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(failure.message.contains("s_0123456789abcdef"));
}

/// `docs/invocation.md`, "What a caller gets back": `fiber_exited` is "the
/// last line".
#[test]
fn drain_keeps_the_last_fiber_exited_line() {
    let child = printing(&[
        exited_line("no_model", "No model is configured."),
        exited_line("invalid_request", "The request was bad."),
    ]);
    let (_, state) = drained(child);
    assert!(support::lock(&state.0).exited);
    let failure = failure_of(&state).expect("the last verdict");
    assert_eq!(failure.code, ErrorCode::InvalidRequest);
    assert_eq!(failure.message, "The request was bad.");
}

/// `docs/invocation.md`, "What a caller gets back": `fiber_exited` is "the
/// last line", so an earlier one is not the verdict.
#[test]
fn drain_ignores_a_fiber_exited_line_that_is_not_last() {
    let child = printing(&[
        exited_line("no_model", "No model is configured."),
        "noise".to_owned(),
    ]);
    let (_, state) = drained(child);
    assert!(support::lock(&state.0).exited);
    assert!(failure_of(&state).is_none());
}

/// `docs/invocation.md`, "What a caller gets back": `fiber_exited` is "the
/// last line", so an earlier one is not the verdict when the last is malformed.
#[test]
fn drain_ignores_a_malformed_last_line() {
    let child = printing(&[
        exited_line("no_model", "No model is configured."),
        "{\"kind\":\"fiber_exited\"".to_owned(),
    ]);
    let (_, state) = drained(child);
    assert!(support::lock(&state.0).exited);
    assert!(failure_of(&state).is_none());
}

/// `docs/invocation.md`, "What a caller gets back": `fiber_exited` is "the
/// last line", so an earlier one is not the verdict when the last cannot be read.
#[test]
fn drain_ignores_an_unreadable_last_line() {
    let verdict = exited_line("no_model", "No model is configured.").replace('\'', "'\\''");
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s\\n' '{verdict}'; printf '\\377\\n'"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (_, state) = drained(child);
    assert!(support::lock(&state.0).exited);
    assert!(failure_of(&state).is_none());
}

#[test]
fn drain_marks_exited_once_stdout_closes() {
    let child = printing(&[]);
    let (_, state) = drained(child);
    assert!(support::lock(&state.0).exited);
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
        false,
        None,
        None,
        false,
    );
    assert_eq!(command.get_program(), "/bin/fiber");
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--model=fake/m"
        ]
    );
}

#[test]
fn the_session_command_passes_worktree_only_when_set() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let with = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        true,
        false,
        None,
        None,
        false,
    );
    assert!(args_of(&with).contains(&"--worktree".to_owned()));
    let without = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        false,
        false,
        None,
        None,
        false,
    );
    assert!(!args_of(&without).contains(&"--worktree".to_owned()));
}

#[test]
fn the_session_command_resumes_with_the_recorded_workspace_and_no_model() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let command = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        false,
        true,
        None,
        None,
        false,
    );
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--resume"
        ]
    );
}

/// The failure a started session reports once it has exited.
#[track_caller]
fn exit_of(started: Box<dyn Started>) -> Failure {
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-exit".to_owned())
        .spawn(move || {
            done_tx.send(started.wait_exited()).unwrap_or(());
        })
        .unwrap();
    // `wait_exited` blocks: a wait, so the test receives it with a
    // wall-clock deadline.
    Deadline::after(DRAIN_DEADLINE)
        .recv(&done_rx)
        .expect("the session exits before its deadline")
        .expect("the session exits before its deadline")
}

#[test]
fn wait_exited_blocks_until_the_child_exits() {
    // The stub blocks reading stdin until the test releases it, so a
    // `wait_exited` that answers without waiting returns `None` and a
    // `wait_exited` that never wakes misses the deadline: both fail here.
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("read line; printf '%s\\n' \"$line\"")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let state: Arc<(Mutex<State>, Condvar)> = Arc::new((
        Mutex::new(State {
            exited: false,
            failure: None,
            reading: None,
        }),
        Condvar::new(),
    ));
    let child = Arc::new(Mutex::new(child));
    thread::Builder::new()
        .name("hub-test-drain".to_owned())
        .spawn({
            let child = Arc::clone(&child);
            let state = Arc::clone(&state);
            move || drain(&child, &state)
        })
        .unwrap();
    let spawned = Spawned {
        id: SessionId("s_wait".to_owned()),
        child: Arc::clone(&child),
        state,
    };
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-exit".to_owned())
        .spawn(move || {
            started_tx.send(()).unwrap_or(());
            done_tx.send(spawned.wait_exited()).unwrap_or(());
        })
        .unwrap();
    Deadline::after(DRAIN_DEADLINE)
        .recv(&started_rx)
        .expect("the waiter reports before it waits");
    // The child still holds stdin, so the waiter must still be blocked:
    // a `wait_exited` that answers without waiting has sent by now.
    assert!(
        Deadline::after(Duration::from_millis(500))
            .recv(&done_rx)
            .is_err(),
        "wait_exited answered while stdin was held"
    );
    std::io::Write::write_all(&mut stdin, b"go\n").unwrap();
    drop(stdin);
    let failure = Deadline::after(DRAIN_DEADLINE)
        .recv(&done_rx)
        .expect("the session exits before its deadline")
        .expect("the session exits before its deadline");
    assert_eq!(failure.code, ErrorCode::IoFailed);
}

#[test]
fn kill_returns_while_the_drain_still_holds_the_pipe_open() {
    // `cat` holds stdout open until killed or stdin closes: stdin stays
    // open here, so even a panic leaves no sleeper behind.
    let id = SessionId("s_aaaabbbbccccdddd".to_owned());
    let (reading_tx, reading_rx) = mpsc::channel();
    *DRAIN_READING.lock().unwrap() = Some((id.clone(), reading_tx));
    let mut command = Command::new("cat");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let spawned = spawn(&id, command).expect("cat spawns");
    // The drain fires just before its first read, the pipe still open:
    // a `kill` that waited on the drain's lock would hang here.
    Deadline::after(DRAIN_DEADLINE)
        .recv(&reading_rx)
        .expect("the drain reaches its first read");
    // `kill` returns while the drain still holds the pipe: received
    // through a deadline, so a lock-scoped regression fails here.
    // The kill thread hands the session back; the test thread then
    // reaps it directly.
    let (killed_tx, killed_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-kill".to_owned())
        .spawn(move || {
            spawned.kill();
            killed_tx.send(spawned).unwrap_or(());
        })
        .unwrap();
    let spawned = Deadline::after(DRAIN_DEADLINE)
        .recv(&killed_rx)
        .expect("kill returns while the drain reads");
    // The kill ends `cat`: the drain sees EOF, reaps, and notifies.
    // Polled to a deadline, so a `kill` that never signals fails here
    // instead of hanging the shard: `exited` is set only by the drain
    // after it reaps.
    await_true("the drain to reap the killed child", move || {
        spawned.exited().is_some()
    });
}

#[test]
fn start_runs_the_recorded_path() {
    let root = fakes::TempDir::new("hstart");
    let workspace = workspace(&root);
    let starter = starter_with(stub_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::start(
        &starter,
        &SessionId("recorded start".to_owned()),
        &workspace,
        None,
        &[],
        false,
        None,
        false,
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded start");
}

#[test]
fn start_command_passes_each_override_as_its_own_dash_c_in_order() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let command = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        Some("fake/m"),
        &["retry.attempts=2", "model=fake/m2"],
        false,
        None,
        false,
        false,
    );
    assert_eq!(command.get_program(), "/bin/fiber");
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--model=fake/m",
            "-c",
            "retry.attempts=2",
            "-c",
            "model=fake/m2",
        ]
    );
    let bare = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        &[],
        false,
        None,
        false,
        false,
    );
    assert_eq!(
        args_of(&bare),
        ["session", "--id=s_0123456789abcdef", "--workspace=/w"]
    );
}

#[test]
fn resume_runs_the_recorded_path() {
    let root = fakes::TempDir::new("hresume");
    let workspace = workspace(&root);
    let starter = starter_with(stub_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::resume(
        &starter,
        &SessionId("recorded resume".to_owned()),
        &workspace,
        None,
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded resume");
}

/// The checked-in stand-in for the session binary: it reports the
/// arguments and the environment it ran with, then ends the drain.
fn recorder_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fixtures/fiber-recorder")
}

/// The checked-in stub for the session binary: it exits with the session
/// id as its message.
fn stub_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fixtures/fiber-stub")
}

/// A workspace under a temporary root.
fn workspace(root: &fakes::TempDir) -> PathBuf {
    let workspace = root.path().join("w");
    std::fs::create_dir_all(&workspace).unwrap();
    workspace
}

/// The starter over a published environment holding `MARK`, with the
/// record directory published for the recorder fixture.
fn starter_with(exe: PathBuf, source: VariablesSource, root: &fakes::TempDir) -> SpawnStarter {
    SpawnStarter::new(
        exe,
        Environments::ready(Environment {
            vars: vec![
                (OsString::from("MARK"), OsString::from("start")),
                (OsString::from("FIBER_HOME"), OsString::from("/h")),
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (
                    OsString::from("RECORD_DIR"),
                    root.path().as_os_str().to_owned(),
                ),
            ],
            source,
        }),
    )
}

/// The marker rides beside the snapshot it marks, with the names-only
/// value `1`, so the session keys its fixed startup environment off it
/// instead of querying the hub again.
#[test]
fn apply_marks_the_session_for_the_startup_snapshot() {
    let mut command = Command::new("fiber");
    apply(
        &mut command,
        &Environment {
            vars: vec![(OsString::from("MARK"), OsString::from("start"))],
            source: VariablesSource::Inherited,
        },
    );
    let marked = command
        .get_envs()
        .find(|(name, _)| name.to_str() == Some(HUB_SESSION_MARKER))
        .map(|(_, value)| value.map(|value| value.to_owned()));
    assert_eq!(marked, Some(Some(OsString::from("1"))));
}

/// The stand-in's reported arguments and environment.
fn reported(root: &fakes::TempDir) -> (Vec<String>, String) {
    let args = std::fs::read_to_string(root.path().join("args")).unwrap_or_default();
    let env = std::fs::read_to_string(root.path().join("env")).unwrap_or_default();
    (
        args.lines().map(str::to_owned).collect(),
        env.trim_end().to_owned(),
    )
}

#[test]
fn start_runs_with_the_published_environment_and_the_flag() {
    let root = fakes::TempDir::new("hstartenv");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::LoginShell, &root);
    let started = Starter::start(
        &starter,
        &SessionId("s1".to_owned()),
        &workspace,
        None,
        &[],
        false,
        None,
        false,
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, env) = reported(&root);
    assert_eq!(args.last().map(String::as_str), Some("--login-shell"));
    assert_eq!(env, "start /h unset");
}

#[test]
fn resume_runs_with_the_published_environment_and_the_flag() {
    let root = fakes::TempDir::new("hresumeenv");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::LoginShell, &root);
    let started = Starter::resume(&starter, &SessionId("s1".to_owned()), &workspace, None).unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, env) = reported(&root);
    assert_eq!(args.last().map(String::as_str), Some("--login-shell"));
    assert!(args.contains(&"--resume".to_owned()));
    assert!(
        !args.iter().any(|arg| arg.starts_with("--credential")),
        "no credential argument without a label: {args:?}"
    );
    assert_eq!(env, "start /h unset");
}

#[test]
fn resume_with_a_label_passes_it_as_one_credential_argument() {
    let root = fakes::TempDir::new("hresumecred");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::resume(
        &starter,
        &SessionId("s1".to_owned()),
        &workspace,
        Some("work"),
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, _) = reported(&root);
    assert!(args.contains(&"--resume".to_owned()), "{args:?}");
    let credential: Vec<String> = args
        .iter()
        .filter(|arg| arg.starts_with("--credential"))
        .cloned()
        .collect();
    assert_eq!(credential, ["--credential=work"], "{args:?}");
}

#[test]
fn resume_with_a_flag_looking_label_passes_it_as_one_value() {
    let root = fakes::TempDir::new("hresumecredflag");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::resume(
        &starter,
        &SessionId("s1".to_owned()),
        &workspace,
        Some("--resume"),
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, _) = reported(&root);
    let credential: Vec<String> = args
        .iter()
        .filter(|arg| arg.starts_with("--credential"))
        .cloned()
        .collect();
    assert_eq!(credential, ["--credential=--resume"], "{args:?}");
}

#[test]
fn rewind_runs_with_the_published_environment_and_the_flag() {
    let root = fakes::TempDir::new("hrewindenv");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::LoginShell, &root);
    let started = Starter::rewind(
        &starter,
        &SessionId("s1".to_owned()),
        &workspace,
        &SessionId("s0".to_owned()),
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, env) = reported(&root);
    assert_eq!(args.last().map(String::as_str), Some("--login-shell"));
    assert!(args.contains(&"--rewound-from=s0".to_owned()));
    assert_eq!(env, "start /h unset");
}

#[test]
fn an_inherited_environment_runs_without_the_flag() {
    let root = fakes::TempDir::new("hstartinh");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::start(
        &starter,
        &SessionId("s1".to_owned()),
        &workspace,
        None,
        &[],
        false,
        None,
        false,
    )
    .unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, env) = reported(&root);
    assert!(!args.contains(&"--login-shell".to_owned()));
    assert_eq!(env, "start /h unset");
}

#[test]
fn the_session_command_rewinds_with_the_old_session_and_no_model() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let from = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let command = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        false,
        false,
        Some(&from),
        None,
        false,
    );
    assert_eq!(
        args_of(&command),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--rewound-from=s_aaaaaaaaaaaaaaaa"
        ]
    );
}

#[test]
fn resume_of_a_missing_target_fails_naming_the_path_before_binding() {
    let root = fakes::TempDir::new("hresumegone");
    // The target is never created: the resume fails before spawning, so
    // the recorder fixture never runs and writes nothing.
    let target = root.path().join("gone");
    let starter = starter_with(recorder_fixture(), VariablesSource::Inherited, &root);
    let error = match Starter::resume(&starter, &SessionId("s1".to_owned()), &target, None) {
        Ok(_) => panic!("a missing launch target fails before binding"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        error.to_string(),
        format!("{} no longer exists", target.display())
    );
    assert!(
        !root.path().join("args").exists(),
        "no process spawned behind the refusal"
    );
}

#[test]
fn resume_spawns_with_the_given_workspace() {
    let root = fakes::TempDir::new("hresumews");
    let workspace = workspace(&root);
    let starter = starter_with(recorder_fixture(), VariablesSource::Inherited, &root);
    let started = Starter::resume(&starter, &SessionId("s1".to_owned()), &workspace, None).unwrap();
    assert_eq!(exit_of(started).message, "recorded");
    let (args, _) = reported(&root);
    assert!(
        args.contains(&format!("--workspace={}", workspace.display())),
        "{args:?}"
    );
}

#[test]
fn start_command_passes_continues_only_when_set() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let with = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        &[],
        false,
        Some(&old),
        false,
        false,
    );
    assert_eq!(
        args_of(&with),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--continues=s_aaaaaaaaaaaaaaaa"
        ]
    );
    let without = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        &[],
        false,
        None,
        false,
        false,
    );
    assert!(
        !args_of(&without)
            .iter()
            .any(|arg| arg.starts_with("--continues"))
    );
}

#[test]
fn start_command_passes_worktree_only_when_set() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let with = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        &[],
        true,
        None,
        false,
        false,
    );
    assert!(args_of(&with).contains(&"--worktree".to_owned()));
    let without = start_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        &[],
        false,
        None,
        false,
        false,
    );
    assert!(!args_of(&without).contains(&"--worktree".to_owned()));
}

#[test]
fn session_command_passes_continues_only_when_set() {
    let id = SessionId("s_0123456789abcdef".to_owned());
    let old = SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let with = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        false,
        false,
        None,
        Some(&old),
        false,
    );
    assert_eq!(
        args_of(&with),
        [
            "session",
            "--id=s_0123456789abcdef",
            "--workspace=/w",
            "--continues=s_aaaaaaaaaaaaaaaa"
        ]
    );
    let without = session_command(
        Path::new("/bin/fiber"),
        &id,
        Path::new("/w"),
        None,
        false,
        false,
        None,
        None,
        false,
    );
    assert!(
        !args_of(&without)
            .iter()
            .any(|arg| arg.starts_with("--continues"))
    );
}
