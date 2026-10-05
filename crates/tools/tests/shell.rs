//! The shell tool through [`tools::Shell::run`] (`docs/tools.md`, "Shell").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::events::{JobStarted, Outcome};
use contract::jobs::JobRecord;
use contract::shapes::{ContentPart, Process};
use contract::tool::{Cancel, Tool};
use fakes::children::{Ready, escapes_group, ignores_sigterm, leaves_descendants};
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use fakes::{CancelToken, Recorder, Watchdog, kill_group, kill_pid};
use serde_json::{Map, Value, json};
use tools::Shell;

const DEADLINE: Duration = Duration::from_secs(10);

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn args(command: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("command".into(), Value::String(command.into()));
    map
}

fn run(dir: &Path, command: &str) -> contract::tool::Output {
    let shell = Shell::new(dir.to_path_buf(), FakeClock::new());
    shell.run(&args(command), &CancelToken::new(), &Recorder::default())
}

fn group_alive(group: u32) -> bool {
    kill_group(group, "0").unwrap()
}

fn pid_alive(pid: u32) -> bool {
    kill_pid(pid, "0").unwrap()
}

/// `kill -0` succeeds on a zombie until its new parent reaps it.
fn wait_until_pid_gone(pid: u32) {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        while pid_alive(pid) {}
        done.send(()).unwrap();
    });
    assert!(
        finished.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for pid {pid} to be gone"
    );
}

struct Running {
    clock: Arc<FakeClock>,
    start: Instant,
    output: mpsc::Receiver<contract::tool::Output>,
}

fn start(dir: PathBuf, command: String, timeout_ms: Option<u64>, cancel: CancelToken) -> Running {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>);
    let mut arguments = args(&command);
    if let Some(timeout_ms) = timeout_ms {
        arguments.insert("timeout_ms".into(), json!(timeout_ms));
    }
    let (tx, rx) = mpsc::channel();
    let cancel_for_run = cancel.clone();
    thread::spawn(move || {
        tx.send(shell.run(&arguments, &cancel_for_run, &Recorder::default()))
            .unwrap();
    });
    Running {
        clock,
        start,
        output: rx,
    }
}

fn join(running: Running) -> contract::tool::Output {
    match running.output.recv_timeout(DEADLINE) {
        Ok(output) => output,
        Err(_) => panic!("waited {DEADLINE:?} for the command to finish"),
    }
}

#[test]
fn stdout_and_stderr_are_one_stream_and_exit_zero_completes() {
    let dir = fakes::TempDir::new("fiber-shell-echo");
    let output = run(dir.path(), "echo a; echo b >&2; echo c");
    assert_eq!(text(&output), "a\nb\nc\nExit code 0.\n");
    assert!(output.error.is_none());
    assert_eq!(
        output.process,
        Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        })
    );
}

#[test]
fn a_nonzero_exit_is_nonzero_exit() {
    let dir = fakes::TempDir::new("fiber-shell-exit");
    let output = run(dir.path(), "exit 3");
    assert_eq!(code(&output), Some(ErrorCode::NonzeroExit));
    assert!(text(&output).contains("Exit code 3."));
    assert_eq!(
        output.process,
        Some(Process {
            exit_code: Some(3),
            signal: None,
            timed_out: false,
        })
    );
}

#[test]
fn a_signal_fiber_did_not_send_is_signal() {
    let dir = fakes::TempDir::new("fiber-shell-segv");
    let output = run(dir.path(), "kill -SEGV $$");
    assert_eq!(code(&output), Some(ErrorCode::Signal));
    assert!(
        text(&output).contains("Killed by SIGSEGV."),
        "{}",
        text(&output)
    );
    let process = output.process.unwrap();
    assert_eq!(process.signal.as_deref(), Some("SIGSEGV"));
    assert!(!process.timed_out);
    assert!(process.exit_code.is_none());
}

#[test]
fn stdin_is_dev_null_so_cat_returns_at_once() {
    let dir = fakes::TempDir::new("fiber-shell-cat");
    let output = run(dir.path(), "cat");
    assert_eq!(text(&output), "Exit code 0.\n");
    assert!(output.error.is_none());
}

#[test]
fn the_workdir_defaults_to_the_workspace_and_a_relative_path_is_resolved() {
    let dir = fakes::TempDir::new("fiber-shell-pwd");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    let here = run(&workspace, "pwd -P");
    assert_eq!(
        Path::new(text(&here).lines().next().unwrap())
            .canonicalize()
            .unwrap(),
        workspace.canonicalize().unwrap()
    );
    let shell = Shell::new(workspace.clone(), FakeClock::new());
    let mut arguments = args("pwd -P");
    arguments.insert("workdir".into(), json!("sub"));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(
        Path::new(text(&output).lines().next().unwrap())
            .canonicalize()
            .unwrap(),
        workspace.join("sub").canonicalize().unwrap()
    );
}

#[test]
fn a_cd_does_not_carry_to_the_next_call() {
    let dir = fakes::TempDir::new("fiber-shell-cd");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let first = shell.run(
        &args(&format!("cd {} && pwd -P", quote(&elsewhere))),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(text(&first).contains("elsewhere"), "{}", text(&first));
    let second = shell.run(&args("pwd -P"), &CancelToken::new(), &Recorder::default());
    assert_eq!(
        Path::new(text(&second).lines().next().unwrap())
            .canonicalize()
            .unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn the_command_is_its_own_process_group() {
    let dir = fakes::TempDir::new("fiber-shell-pgid");
    let output = run(dir.path(), "echo $$; ps -o pgid= -p $$");
    let body = text(&output);
    let mut lines = body.lines();
    let pid = lines.next().unwrap().trim();
    let pgid = lines.next().unwrap().trim();
    assert_eq!(pid, pgid, "{body}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_command_is_its_own_session() {
    let dir = fakes::TempDir::new("fiber-shell-sid");
    let output = run(dir.path(), "echo $$; ps -o sid= -p $$");
    let body = text(&output);
    let mut lines = body.lines();
    let pid = lines.next().unwrap().trim();
    let sid = lines.next().unwrap().trim();
    assert_eq!(pid, sid, "{body}");
}

#[test]
fn non_utf8_bytes_come_back_lossy() {
    let dir = fakes::TempDir::new("fiber-shell-utf8");
    let output = run(dir.path(), "printf '\\377'");
    assert!(text(&output).starts_with('\u{FFFD}'), "{}", text(&output));
    assert!(text(&output).contains("Exit code 0."));
}

#[test]
fn the_tool_does_not_cut_the_output() {
    let dir = fakes::TempDir::new("fiber-shell-bound");
    let output = run(dir.path(), "printf '%20000s' x");
    let body = text(&output);
    assert!(body.len() > 16_384, "{}", body.len());
    assert!(body.contains("Exit code 0."));
}

#[test]
fn bash_env_is_not_read() {
    const PROBE: &str = "FIBER_SHELL_BASH_ENV_PROBE";
    if std::env::var(PROBE).is_ok() {
        let dir = PathBuf::from(std::env::var("FIBER_SHELL_PROBE_DIR").unwrap());
        let output = run(&dir, "echo hi");
        assert!(output.error.is_none(), "{}", text(&output));
        assert!(!dir.join("marker").exists());
        return;
    }
    let dir = fakes::TempDir::new("fiber-shell-bash-env");
    let script = dir.path().join("env.sh");
    let marker = dir.path().join("marker");
    std::fs::write(&script, format!("touch {}\n", quote(&marker))).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "bash_env_is_not_read", "--nocapture"])
        .env(PROBE, "1")
        .env("BASH_ENV", &script)
        .env("FIBER_SHELL_PROBE_DIR", dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(output) => output.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the BASH_ENV probe"),
    };
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists(), "bash read BASH_ENV");
}

#[test]
fn the_call_waits_until_the_group_is_empty() {
    let dir = fakes::TempDir::new("fiber-shell-group");
    let ready = Ready::new(dir.path());
    let hold = dir.path().join("hold");
    Command::new("mkfifo").arg(&hold).status().unwrap();
    let command = format!(
        "echo $$ > {ready}\n(read -r _ < {hold}) &\necho $! >> {ready}\n",
        ready = quote(ready.path()),
        hold = quote(&hold),
    );
    let running = start(dir.path().to_path_buf(), command, None, CancelToken::new());
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _child = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(600), DEADLINE),
        "the run did not park while the group still had a member"
    );
    assert!(group_alive(pgid));
    let mut release = std::fs::OpenOptions::new().write(true).open(&hold).unwrap();
    writeln!(release, "go").unwrap();
    drop(release);
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Exit code 0."));
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_timeout_is_not_stopped_until_the_deadline() {
    let dir = fakes::TempDir::new("fiber-shell-timeout");
    let ready = Ready::new(dir.path());
    let command = waits(ready.path());
    let running = start(
        dir.path().to_path_buf(),
        command,
        Some(1000),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(group_alive(pgid));
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(1), DEADLINE),
        "the run did not park at the timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance(Duration::from_secs(1));
    let output = join(running);
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(text(&output).contains("Timed out after 1000 ms and stopped."));
    assert!(output.process.unwrap().timed_out);
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn with_no_timeout_the_run_parks_at_ten_minutes() {
    let dir = fakes::TempDir::new("fiber-shell-default-timeout");
    let ready = Ready::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        waits(ready.path()),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(600_000), DEADLINE),
        "the run did not park at the default timeout"
    );
    cancel.cancel();
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_zero_timeout_stops_at_once() {
    let dir = fakes::TempDir::new("fiber-shell-zero");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let mut arguments = args("echo hi");
    arguments.insert("timeout_ms".into(), json!(0));
    let output = shell.run(&arguments, &CancelToken::new(), &Recorder::default());
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    assert!(text(&output).contains("Timed out after 0 ms and stopped."));
}

#[test]
fn a_cancel_after_the_timeout_fired_stays_a_timeout() {
    let dir = fakes::TempDir::new("fiber-shell-timeout-cancel");
    let ready = Ready::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        waits(ready.path()),
        Some(1000),
        cancel.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(1), DEADLINE)
    );
    running.clock.advance(Duration::from_secs(1));
    cancel.cancel();
    let output = join(running);
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    assert!(text(&output).contains("Timed out after 1000 ms and stopped."));
    assert!(!text(&output).contains("Cancelled"));
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_command_that_ignores_sigterm_is_killed_after_the_grace() {
    let dir = fakes::TempDir::new("fiber-shell-ignore");
    let ready = Ready::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        ignores_sigterm(ready.path()),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    cancel.cancel();
    let grace = running.start + Duration::from_millis(800);
    assert!(
        running.clock.await_parked(grace, DEADLINE),
        "the run did not park for the grace period"
    );
    assert!(group_alive(pgid), "killed before the grace elapsed");
    running.clock.advance(Duration::from_millis(800));
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_descendant_that_ignores_sigterm_is_killed_with_the_group() {
    let dir = fakes::TempDir::new("fiber-shell-descendants");
    let ready = Ready::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        leaves_descendants(ready.path()),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let pids = ready.wait(DEADLINE);
    cancel.cancel();
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(800), DEADLINE)
    );
    for pid in &pids {
        assert!(pid_alive(*pid), "{pid} died before the grace");
    }
    running.clock.advance(Duration::from_millis(800));
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    for pid in pids {
        assert!(!pid_alive(pid), "{pid} survived");
    }
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_pipe_held_open_after_a_normal_end_keeps_the_exit() {
    let dir = fakes::TempDir::new("fiber-shell-held");
    let ready = Ready::new(dir.path());
    let running = start(
        dir.path().to_path_buf(),
        holds_the_pipe_and_exits(ready.path()),
        None,
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let holder = ready.wait(DEADLINE)[0];
    let _guard = KillPid(holder);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(2), DEADLINE),
        "the run did not park at the drain bound"
    );
    running.clock.advance(Duration::from_secs(2));
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Exit code 0."), "{}", text(&output));
    assert!(
        text(&output).contains("Output was still held open."),
        "{}",
        text(&output)
    );
    kill_pid(holder, "KILL").unwrap();
    wait_until_pid_gone(holder);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn an_escapee_that_holds_the_pipe_is_indeterminate() {
    let dir = fakes::TempDir::new("fiber-shell-escape");
    let ready = Ready::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        escapes_group(ready.path()),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let escapee = ready.wait(DEADLINE)[0];
    let _guard = KillPid(escapee);
    cancel.cancel();
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(2), DEADLINE),
        "the run did not park at the drain bound"
    );
    running.clock.advance(Duration::from_secs(2));
    let output = join(running);
    assert_eq!(code(&output), Some(ErrorCode::Indeterminate));
    assert!(
        text(&output).contains("cannot tell whether it completed"),
        "{}",
        text(&output)
    );
    assert!(!output.process.unwrap().timed_out);
    assert!(!group_alive(pgid));
    kill_pid(escapee, "KILL").unwrap();
    wait_until_pid_gone(escapee);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn two_calls_at_once_do_not_share_a_command() {
    let dir = fakes::TempDir::new("fiber-shell-concurrent");
    let shell = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    let left = Arc::clone(&shell);
    let right = Arc::clone(&shell);
    let (tx, rx) = mpsc::channel();
    let tx_right = tx.clone();
    thread::spawn(move || {
        tx.send(left.run(
            &args("echo left"),
            &CancelToken::new(),
            &Recorder::default(),
        ))
        .unwrap();
    });
    thread::spawn(move || {
        tx_right
            .send(right.run(
                &args("echo right"),
                &CancelToken::new(),
                &Recorder::default(),
            ))
            .unwrap();
    });
    let texts = [
        text(&rx.recv_timeout(DEADLINE).unwrap()),
        text(&rx.recv_timeout(DEADLINE).unwrap()),
    ];
    assert!(texts.iter().any(|line| line.contains("left\nExit code 0.")));
    assert!(
        texts
            .iter()
            .any(|line| line.contains("right\nExit code 0."))
    );
}

fn holds_the_pipe_and_exits(ready: &Path) -> String {
    let marker = ready.with_extension("marker");
    format!(
        "echo $$ > {ready}\nperl -MPOSIX -e 'POSIX::setsid(); $SIG{{HUP}} = \"IGNORE\"; $SIG{{TERM}} = \"IGNORE\"; open my $m, \">\", $ARGV[0] or die $!; print $m \"$$\\n\"; close $m; sleep 3600 while 1' {marker} &\nwhile [ ! -s {marker} ]; do :; done\nread -r pid < {marker}\necho \"$pid\" >> {ready}\nexit 0\n",
        ready = quote(ready),
        marker = quote(&marker),
    )
}

fn waits(ready: &Path) -> String {
    let block = ready.with_file_name(format!(
        "{}.block",
        ready.file_name().unwrap().to_string_lossy()
    ));
    format!(
        "echo $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\n",
        ready = quote(ready),
        block = quote(&block),
    )
}

/// Kills `pid` on drop so a failed test does not leave the escapee behind.
struct KillPid(u32);

impl Drop for KillPid {
    fn drop(&mut self) {
        match kill_pid(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

struct Streamed {
    clock: Arc<FakeClock>,
    start: Instant,
    output: mpsc::Receiver<contract::tool::Output>,
    recorder: Arc<Recorder>,
}

/// As `start`, but tapping the call's streamed output: the test reads
/// `recorder` while the command runs.
fn streamed(dir: PathBuf, command: String, timeout_ms: Option<u64>) -> Streamed {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>);
    let mut arguments = args(&command);
    if let Some(timeout_ms) = timeout_ms {
        arguments.insert("timeout_ms".into(), json!(timeout_ms));
    }
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(shell.run(&arguments, &CancelToken::new(), tapped.as_ref()))
            .unwrap();
    });
    Streamed {
        clock,
        start,
        output: rx,
        recorder,
    }
}

#[test]
fn output_streams_before_the_call_returns() {
    let dir = fakes::TempDir::new("fiber-shell-stream");
    let fifo = dir.path().join("pipe");
    Command::new("mkfifo").arg(&fifo).status().unwrap();
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let command = format!("echo line; read x < {}; echo got:$x", quote(&fifo));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(shell.run(&args(&command), &CancelToken::new(), tapped.as_ref()))
            .unwrap();
    });
    assert!(
        recorder.wait_for_text("line\n", DEADLINE),
        "waited {DEADLINE:?} for the line to stream"
    );
    // The command is still blocked reading the pipe, so the line streamed
    // before the call returned.
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the call returned before streaming"
    );
    // Opening the pipe blocks until the command opens its end, so the
    // write runs on its own thread and the wait below carries the deadline.
    thread::spawn(move || std::fs::write(&fifo, "go\n"));
    let output = rx.recv_timeout(DEADLINE).expect("the call to finish");
    assert_eq!(text(&output), "line\ngot:go\nExit code 0.\n");
    assert_eq!(recorder.text(), "line\ngot:go\n");
}

#[test]
fn a_completed_command_streams_its_whole_output() {
    let dir = fakes::TempDir::new("fiber-shell-stream-done");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let recorder = Recorder::default();
    let output = shell.run(&args("printf 'hi\\n'"), &CancelToken::new(), &recorder);
    assert!(output.error.is_none());
    assert_eq!(text(&output), "hi\nExit code 0.\n");
    assert_eq!(recorder.text(), "hi\n");
}

#[test]
fn a_failing_command_streams_what_it_printed() {
    let dir = fakes::TempDir::new("fiber-shell-stream-failing");
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let recorder = Recorder::default();
    let output = shell.run(
        &args("printf 'oops\\n'; exit 3"),
        &CancelToken::new(),
        &recorder,
    );
    assert_eq!(code(&output), Some(ErrorCode::NonzeroExit));
    assert_eq!(text(&output), "oops\nExit code 3.\n");
    assert_eq!(recorder.text(), "oops\n");
}

#[test]
fn a_timed_out_command_streams_what_it_printed_before_the_deadline() {
    let dir = fakes::TempDir::new("fiber-shell-stream-timeout");
    // Blocked in `read`, like `waits`: no child to reap, so the timeout's
    // SIGTERM empties the group at once.
    let block = dir.path().join("block");
    let command = format!(
        "echo partial; mkfifo {block}; read -r _ < {block}",
        block = quote(&block),
    );
    let running = streamed(dir.path().to_path_buf(), command, Some(1000));
    assert!(
        running.recorder.wait_for_text("partial\n", DEADLINE),
        "waited {DEADLINE:?} for the partial line to stream"
    );
    let due = running.start + Duration::from_secs(1);
    assert!(
        running.clock.await_parked(due, DEADLINE),
        "the run did not park at the timeout"
    );
    running.clock.advance(Duration::from_secs(1));
    let output = running
        .output
        .recv_timeout(DEADLINE)
        .expect("the command to finish");
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(text(&output).starts_with("partial\n"), "{}", text(&output));
    assert_eq!(running.recorder.text(), "partial\n");
}

struct JobRun {
    clock: Arc<FakeClock>,
    start: Instant,
    output: mpsc::Receiver<contract::tool::Output>,
    jobs: Arc<FakeJobs>,
    recorder: Arc<Recorder>,
}

fn start_jobs(
    dir: PathBuf,
    command: String,
    timeout_ms: Option<u64>,
    background: bool,
    jobs: Arc<FakeJobs>,
    cancel: impl Cancel + Clone + 'static,
) -> JobRun {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>)
        .with_jobs(jobs.clone());
    let mut arguments = args(&command);
    if let Some(timeout_ms) = timeout_ms {
        arguments.insert("timeout_ms".into(), json!(timeout_ms));
    }
    if background {
        arguments.insert("run_in_background".into(), json!(true));
    }
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let cancel_for_run = cancel;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(shell.run(&arguments, &cancel_for_run, tapped.as_ref()))
            .unwrap();
    });
    JobRun {
        clock,
        start,
        output: rx,
        jobs,
        recorder,
    }
}

fn blocking(ready: &Path, before: &str, after: &str) -> String {
    let block = block_of(ready);
    format!(
        "echo {before}\necho $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\necho {after}\n",
        ready = quote(ready),
        block = quote(&block),
    )
}

fn block_of(ready: &Path) -> PathBuf {
    ready.with_file_name(format!(
        "{}.block",
        ready.file_name().unwrap().to_string_lossy()
    ))
}

fn release_block(ready: &Path) {
    let mut release = std::fs::OpenOptions::new()
        .write(true)
        .open(block_of(ready))
        .unwrap();
    writeln!(release, "go").unwrap();
}

fn started(output: &contract::tool::Output) -> JobStarted {
    match output.jobs.as_slice() {
        [JobRecord::Started(started)] => started.clone(),
        other => panic!("expected one started job, got {other:?}"),
    }
}

fn job_output(dir: &Path, job: &JobStarted) -> String {
    std::fs::read_to_string(dir.join(&job.output_path)).unwrap()
}

#[test]
fn run_in_background_returns_a_receipt_while_the_command_runs() {
    let dir = fakes::TempDir::new("fiber-shell-bg");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "a", "b"),
        None,
        true,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(output.process.is_none());
    assert!(
        text(&output).starts_with("Started in the background.\n"),
        "{}",
        text(&output)
    );
    assert!(text(&output).contains(&job.job_id.0), "{}", text(&output));
    assert!(
        text(&output).contains(&dir.path().join(&job.output_path).display().to_string()),
        "{}",
        text(&output)
    );
    assert_eq!(job.description, "echo a");
    assert_eq!(job.tool.as_deref(), Some("shell"));
    assert_eq!(jobs.started(), vec![job.clone()]);
    assert!(group_alive(pgid));
    let streamed = running.recorder.text();
    assert!(!streamed.contains('b'), "{streamed}");
    release_block(ready.path());
    let ended = jobs.ended(DEADLINE).expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(
        ended.process.as_ref().and_then(|process| process.exit_code),
        Some(0)
    );
    assert!(ended.output_tail.is_none());
    assert_eq!(job_output(dir.path(), &job), "a\nb\n");
    assert_eq!(running.recorder.text(), streamed);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_command_moves_after_thirty_seconds_without_being_restarted() {
    let dir = fakes::TempDir::new("fiber-shell-thirty");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "BEFORE", "AFTER"),
        None,
        false,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let move_at = running.start + Duration::from_secs(30);
    assert!(
        running.clock.await_parked(move_at, DEADLINE),
        "the run did not park at 30 seconds"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the command moved before 30 seconds"
    );
    assert!(group_alive(pgid));
    running.clock.advance(Duration::from_secs(30));
    let output = running
        .output
        .recv_timeout(DEADLINE)
        .expect("the 30-second receipt");
    let job = started(&output);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(output.process.is_none());
    assert!(
        text(&output)
            .starts_with("Still running after 30 seconds, so it moved to the background.\n"),
        "{}",
        text(&output)
    );
    assert!(!text(&output).contains("BEFORE"), "{}", text(&output));
    assert!(!text(&output).contains("AFTER"), "{}", text(&output));
    assert_eq!(job.description, "echo BEFORE");
    assert!(group_alive(pgid), "the move stopped the command");
    let timeout_at = running.start + Duration::from_millis(600_000);
    assert!(
        running.clock.await_parked(timeout_at, DEADLINE),
        "the job did not keep the timeout from the command's start"
    );
    let streamed = running.recorder.text();
    assert!(!streamed.contains("AFTER"), "{streamed}");
    release_block(ready.path());
    let ended = jobs.ended(DEADLINE).expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(job_output(dir.path(), &job), "BEFORE\nAFTER\n");
    assert_eq!(running.recorder.text(), streamed);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_timeout_at_thirty_seconds_stops_in_the_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-thirty-timeout");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "BEFORE", "AFTER"),
        Some(30_000),
        false,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(30), DEADLINE),
        "the run did not park at the timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance(Duration::from_secs(30));
    let output = running.output.recv_timeout(DEADLINE).expect("the timeout");
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    assert!(text(&output).contains("Timed out after 30000 ms and stopped."));
    assert!(output.jobs.is_empty());
    assert!(jobs.started().is_empty());
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_shell_that_exits_with_members_moves_and_names_them() {
    let dir = fakes::TempDir::new("fiber-shell-orphans");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let command = format!(
        "echo $$ > {ready}; sleep 1000 & exit 3",
        ready = quote(ready.path()),
    );
    let running = start_jobs(
        dir.path().to_path_buf(),
        command,
        None,
        false,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let output = running
        .output
        .recv_timeout(DEADLINE)
        .expect("the shell-exited receipt");
    let body = text(&output);
    assert!(output.error.is_none(), "{body}");
    assert!(output.process.is_none());
    assert!(body.contains("exited with code 3"), "{body}");
    assert!(body.contains("sleep ("), "{body}");
    assert!(body.contains("moved to the background"), "{body}");
    assert!(group_alive(pgid));
    assert!(
        jobs.ended(Duration::from_secs(1)).is_none(),
        "the job ended while sleep was still in the group"
    );
    assert!(kill_group(pgid, "KILL").unwrap());
    let ended = jobs
        .ended(DEADLINE)
        .expect("the job to finish once the group was empty");
    assert_eq!(ended.status, Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NonzeroExit)
    );
    assert_eq!(
        ended.process.as_ref().and_then(|process| process.exit_code),
        Some(3)
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_moved_job_times_out_from_the_commands_start() {
    let dir = fakes::TempDir::new("fiber-shell-job-timeout");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "partial", "later"),
        Some(5_000),
        true,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let due = running.start + Duration::from_secs(5);
    assert!(
        running.clock.await_parked(due, DEADLINE),
        "the job did not park at its timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance(Duration::from_secs(5));
    let ended = jobs.ended(DEADLINE).expect("the job to time out");
    assert_eq!(ended.status, Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Timeout)
    );
    assert!(ended.process.as_ref().unwrap().timed_out);
    let tail = ended.output_tail.unwrap();
    assert!(tail.contains("partial"), "{tail}");
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

/// A call's cancel that keeps every waker subscribed to it, to show which
/// still reach a command.
#[derive(Clone, Default)]
struct Watched {
    token: CancelToken,
    wakers: Arc<Mutex<Vec<Weak<dyn Wake>>>>,
}

impl Watched {
    /// Subscribers that are still alive.
    fn live(&self) -> usize {
        self.wakers
            .lock()
            .unwrap()
            .iter()
            .filter(|waker| waker.upgrade().is_some())
            .count()
    }
}

impl Cancel for Watched {
    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.wakers.lock().unwrap().push(waker.clone());
        self.token.subscribe(waker);
    }
}

#[test]
fn stopping_a_job_cancels_it_and_a_turn_cancel_does_not() {
    let dir = fakes::TempDir::new("fiber-shell-job-stop");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let watched = Watched::default();
    let cancel = watched.token.clone();
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "a", "b"),
        None,
        true,
        Arc::clone(&jobs),
        watched.clone(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    assert_eq!(
        watched.live(),
        0,
        "the call's cancel still reaches the moved job"
    );
    let timeout_at = running.start + Duration::from_millis(600_000);
    assert!(
        running.clock.await_parked(timeout_at, DEADLINE),
        "the job did not park"
    );
    cancel.cancel();
    assert!(
        jobs.ended(Duration::from_secs(1)).is_none(),
        "the turn cancel stopped the job"
    );
    assert!(group_alive(pgid));
    assert!(
        running.clock.parked().contains(&Some(timeout_at)),
        "the turn cancel left the timeout park: {:?}",
        running.clock.parked()
    );
    jobs.stop(&job.job_id);
    let ended = jobs.ended(DEADLINE).expect("the stop to finish the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    assert!(ended.error.is_none());
    assert!(ended.output_tail.is_none());
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_failed_open_leaves_the_command_running_in_the_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-open-fails");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::failing();
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "BEFORE", "AFTER"),
        None,
        true,
        jobs,
        CancelToken::new(),
    );
    let pgid = ready.wait(DEADLINE)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(600_000), DEADLINE),
        "the command did not stay in the foreground"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "open failing returned before the command ended"
    );
    assert!(group_alive(pgid));
    assert!(running.jobs.started().is_empty());
    release_block(ready.path());
    let output = running
        .output
        .recv_timeout(DEADLINE)
        .expect("the foreground result");
    let body = text(&output);
    assert!(body.contains("BEFORE\n"), "{body}");
    assert!(body.contains("AFTER\n"), "{body}");
    assert!(body.contains("Exit code 0."), "{body}");
    assert!(
        body.ends_with("Exit code 0.\nIt could not move to the background: the job's output file jobs/unavailable.log could not be created: background jobs are unavailable.\n"),
        "{body}"
    );
    assert!(output.error.is_none(), "{body}");
    assert_eq!(
        output
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(0)
    );
    watchdog.stand_down(DEADLINE);
}
