//! The shell tool through [`tools::Shell::run`] (`docs/tools.md`, "Shell").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Wake;
use contract::events::{JobStarted, Outcome};
use contract::jobs::{JobRecord, Jobs as _};
use contract::shapes::{ContentPart, Process};
use contract::tool::{Cancel, Tool};
use fakes::children::{Ready, escapes_group, ignores_sigterm, leaves_descendants};
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use fakes::{CancelToken, Recorder, Watchdog, kill_group, kill_pid};
use serde_json::{Map, Value, json};
use tools::Shell;

const DEADLINE: Duration = Duration::from_secs(5);

/// The bound on a child's first ready line. How long a child takes to start
/// follows the machine's load, not the test: at a load of 40 to 100 on 11
/// cores 5 s was not enough. 20 s is the per-wait bound `crates/main/tests`
/// use. Later lines come from the running command and keep `DEADLINE`.
const CHILD_START: Duration = Duration::from_secs(20);

/// One bound for a whole `print_steps` sequence.
const STEPS: Duration = Duration::from_secs(15);

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
    let shell = Arc::new(Shell::new(dir.to_path_buf(), FakeClock::new()));
    run_on(&shell, args(command), &Arc::new(Recorder::default()))
}

/// Runs one shell call on its own thread and returns its output. Calling
/// code that blocks is a wait too (`docs/testing.md`, "Waits and
/// timeouts"): on expiry the test fails naming the command.
fn run_on(
    shell: &Arc<Shell>,
    arguments: Map<String, Value>,
    recorder: &Arc<Recorder>,
) -> contract::tool::Output {
    let shell = Arc::clone(shell);
    let recorder = Arc::clone(recorder);
    fakes::within(
        &format!("the shell command {}", arguments["command"]),
        DEADLINE,
        move || shell.run(&arguments, &CancelToken::new(), recorder.as_ref()),
    )
}

fn group_alive(group: u32) -> bool {
    kill_group(group, "0").unwrap()
}

fn pid_alive(pid: u32) -> bool {
    kill_pid(pid, "0").unwrap()
}

const LIFELINE: Duration = Duration::from_secs(2);

/// A FIFO the command's processes hold open: end-of-file on the read end
/// means no process holds a write end any more.
///
/// Limited to the `leaves_descendants`, `escapes_group` and
/// `holds_the_pipe_and_exits` fixtures: the script's shell and every
/// process it starts after the first line inherit fd 9, and none of them
/// closes it. End-of-file therefore means each of them has exited, not
/// that anything has reaped them.
struct Lifeline {
    path: PathBuf,
}

impl Lifeline {
    /// Only computes `<dir>/life.fifo`: runs nothing and does not block.
    fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("life.fifo"),
        }
    }

    /// Prefixes `script` with making the FIFO and holding it on fd 9. The
    /// script makes the FIFO itself, so no test-side `mkfifo` wait exists.
    /// A failure exits before the ready line, and `ready.wait` names it.
    /// The read-write open never blocks.
    fn hold(&self, script: &str) -> String {
        format!(
            "mkfifo {} && exec 9<>{} || exit 1\n{script}",
            quote(&self.path),
            quote(&self.path),
        )
    }

    /// Watches the FIFO: a thread opens the read end read-only and reports
    /// the open, then reads to end-of-file and reports that. Call only
    /// when a holder exists, after its ready line and before anything
    /// kills it: a read-only open with no writer blocks.
    fn watch(&self) -> Holders {
        let path = self.path.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(_) => return,
            };
            match tx.send(()) {
                Ok(()) | Err(_) => {}
            }
            let mut buf = [0u8; 1024];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
            match tx.send(()) {
                Ok(()) | Err(_) => {}
            }
        });
        match rx.recv_timeout(LIFELINE) {
            Ok(()) => {}
            Err(_) => panic!(
                "waited {LIFELINE:?} for a holder of {}",
                self.path.display()
            ),
        }
        Holders(rx)
    }
}

struct Holders(mpsc::Receiver<()>);

impl Holders {
    fn gone(self, what: &str) {
        match self.0.recv_timeout(LIFELINE) {
            Ok(()) => {}
            Err(_) => panic!("waited {LIFELINE:?} for {what} to exit"),
        }
    }
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
    let shell = Arc::new(Shell::new(workspace.clone(), FakeClock::new()));
    let mut arguments = args("pwd -P");
    arguments.insert("workdir".into(), json!("sub"));
    let output = run_on(&shell, arguments, &Arc::new(Recorder::default()));
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
    let shell = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let first = run_on(
        &shell,
        args(&format!("cd {} && pwd -P", quote(&elsewhere))),
        &Arc::new(Recorder::default()),
    );
    assert!(text(&first).contains("elsewhere"), "{}", text(&first));
    let second = run_on(&shell, args("pwd -P"), &Arc::new(Recorder::default()));
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
fn exported_functions_are_not_carried() {
    const PROBE: &str = "FIBER_SHELL_BASH_FUNC_PROBE";
    if std::env::var(PROBE).is_ok() {
        let dir = fakes::TempDir::new("fiber-shell-bash-func-child");
        let output = run(dir.path(), "fiber_probe; true");
        assert!(output.error.is_none(), "{}", text(&output));
        assert!(!text(&output).contains("carried"), "{}", text(&output));
        return;
    }
    // How bash exports a function: `export -f fiber_probe` sets this.
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "exported_functions_are_not_carried",
            "--nocapture",
        ])
        .env(PROBE, "1")
        .env("BASH_FUNC_fiber_probe%%", "() {  echo carried\n}")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(DEADLINE) {
        Ok(output) => output.unwrap(),
        Err(_) => panic!("waited {DEADLINE:?} for the BASH_FUNC probe"),
    };
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let shell = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    let mut arguments = args("echo hi");
    arguments.insert("timeout_ms".into(), json!(0));
    let output = run_on(&shell, arguments, &Arc::new(Recorder::default()));
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let life = Lifeline::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        life.hold(&leaves_descendants(ready.path())),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let pids = ready.wait(DEADLINE);
    let holders = life.watch();
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
    holders.gone("the shell and its descendant");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_pipe_held_open_after_a_normal_end_keeps_the_exit() {
    let dir = fakes::TempDir::new("fiber-shell-held");
    let ready = Ready::new(dir.path());
    let life = Lifeline::new(dir.path());
    let running = start(
        dir.path().to_path_buf(),
        life.hold(&holds_the_pipe_and_exits(ready.path())),
        None,
        CancelToken::new(),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let holder = ready.wait(DEADLINE)[0];
    let holders = life.watch();
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
    holders.gone("the pipe holder");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn an_escapee_that_holds_the_pipe_is_indeterminate() {
    let dir = fakes::TempDir::new("fiber-shell-escape");
    let ready = Ready::new(dir.path());
    let life = Lifeline::new(dir.path());
    let cancel = CancelToken::new();
    let running = start(
        dir.path().to_path_buf(),
        life.hold(&escapes_group(ready.path())),
        None,
        cancel.clone(),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let escapee = ready.wait(DEADLINE)[0];
    let holders = life.watch();
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
    holders.gone("the escapee");
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
    let shell = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    let recorder = Arc::new(Recorder::default());
    let output = run_on(&shell, args("printf 'hi\\n'"), &recorder);
    assert!(output.error.is_none());
    assert_eq!(text(&output), "hi\nExit code 0.\n");
    assert_eq!(recorder.text(), "hi\n");
}

#[test]
fn a_failing_command_streams_what_it_printed() {
    let dir = fakes::TempDir::new("fiber-shell-stream-failing");
    let shell = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    let recorder = Arc::new(Recorder::default());
    let output = run_on(&shell, args("printf 'oops\\n'; exit 3"), &recorder);
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
    let pgid = ready.wait(CHILD_START)[0];
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
fn a_running_job_emits_what_it_prints_after_the_move_as_job_deltas() {
    let dir = fakes::TempDir::new("fiber-shell-bg-delta");
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
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    let deltas = jobs.deltas();
    // The FIFO open waits for the shell's read, so it runs on its own thread.
    let block = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&block);
        let _sent = released.send(());
    });
    release
        .recv_timeout(DEADLINE)
        .expect("waited for the command's read of the block fifo");
    let ended = jobs.ended(DEADLINE).expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    // The move races the command's first line, so "a" is either in the copy
    // at the move or a delta. Every byte after the copy is a delta, and all
    // of it is out before the end was reported.
    let text = deltas.text();
    assert!(text.ends_with("b\n"), "{text:?}");
    assert!("a\nb\n".ends_with(&text), "{text:?}");
    assert!(
        deltas.deltas().iter().all(|(id, _)| *id == job.job_id),
        "{:?}",
        deltas.deltas()
    );
    assert_eq!(job_output(dir.path(), &job), "a\nb\n");
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
    let pgid = ready.wait(CHILD_START)[0];
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
fn the_background_command_moves_a_running_call_before_thirty_seconds() {
    let dir = fakes::TempDir::new("fiber-shell-commanded");
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
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(30), DEADLINE),
        "the run did not park at 30 seconds"
    );
    assert_eq!(jobs.background(), 1);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    assert!(
        text(&output).starts_with("Moved to the background by the `background` command.\n"),
        "{}",
        text(&output)
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(output.process.is_none());
    assert_eq!(jobs.started(), vec![job.clone()]);
    assert!(group_alive(pgid), "the command stopped the group");
    assert_eq!(jobs.background(), 0, "a moved call was counted again");
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(600_000), DEADLINE),
        "the job did not keep the timeout from the command's start"
    );
    release_block(ready.path());
    let ended = jobs.ended(DEADLINE).expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(job_output(dir.path(), &job), "BEFORE\nAFTER\n");
    assert_eq!(jobs.started().len(), 1, "the command moved twice");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_finished_call_is_not_registered_as_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-finished");
    let jobs = FakeJobs::new(dir.path());
    let running = start_jobs(
        dir.path().to_path_buf(),
        "echo done".to_owned(),
        None,
        false,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let output = running.output.recv_timeout(DEADLINE).expect("the result");
    assert!(output.jobs.is_empty());
    assert_eq!(jobs.background(), 0);
    assert!(jobs.started().is_empty());
}

#[test]
fn a_call_that_moved_by_thirty_seconds_is_not_moved_again() {
    let dir = fakes::TempDir::new("fiber-shell-moved-once");
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
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(30), DEADLINE),
        "the run did not park at 30 seconds"
    );
    running.clock.advance(Duration::from_secs(30));
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    assert!(
        text(&output).starts_with("Still running after 30 seconds"),
        "{}",
        text(&output)
    );
    assert_eq!(jobs.background(), 0);
    assert_eq!(jobs.started().len(), 1);
    release_block(ready.path());
    jobs.ended(DEADLINE).expect("the job to finish");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_run_in_background_call_is_not_registered_as_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-bg-unregistered");
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
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    running.output.recv_timeout(DEADLINE).expect("the receipt");
    assert_eq!(jobs.background(), 0);
    release_block(ready.path());
    jobs.ended(DEADLINE).expect("the job to finish");
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
    let pgid = ready.wait(CHILD_START)[0];
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
        "echo $$ > {ready}; sleep 1000 & until ps -o comm= -p $! | grep -q sleep; do :; done; exit 3",
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
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let output = running
        .output
        .recv_timeout(DEADLINE)
        .expect("waited for the shell-exited receipt after sleep exec'd");
    let body = text(&output);
    assert!(output.error.is_none(), "{body}");
    assert!(output.process.is_none());
    assert!(body.contains("exited with code 3"), "{body}");
    assert!(body.contains("sleep ("), "{body}");
    assert!(body.contains("moved to the background"), "{body}");
    assert!(group_alive(pgid));
    let timeout_at = running.start + Duration::from_millis(600_000);
    let mark = running
        .clock
        .mark_parked(timeout_at, DEADLINE)
        .expect("the moved job did not park at its timeout");
    // A later park at the timeout means another group check found sleep
    // alive and the job waits on.
    assert!(
        running
            .clock
            .await_parked_since(&mark, Some(timeout_at), DEADLINE),
        "the job did not wait again while sleep was in the group"
    );
    assert!(
        jobs.ended(Duration::ZERO).is_none(),
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
    let pgid = ready.wait(CHILD_START)[0];
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
    let pgid = ready.wait(CHILD_START)[0];
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
    // Marked after the cancel, then woken: a later park at the timeout ran a
    // whole pass after the turn cancel and did not stop.
    let mark = running
        .clock
        .mark_parked(timeout_at, DEADLINE)
        .expect("the job left its timeout park");
    running.clock.advance(Duration::ZERO);
    assert!(
        running
            .clock
            .await_parked_since(&mark, Some(timeout_at), DEADLINE),
        "the job did not wait again after the turn cancel"
    );
    assert!(
        jobs.ended(Duration::ZERO).is_none(),
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
fn a_background_command_whose_open_fails_leaves_no_registration() {
    let dir = fakes::TempDir::new("fiber-shell-commanded-open-fails");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::failing();
    let running = start_jobs(
        dir.path().to_path_buf(),
        blocking(ready.path(), "BEFORE", "AFTER"),
        None,
        false,
        Arc::clone(&jobs),
        CancelToken::new(),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_secs(30), DEADLINE),
        "the run did not park at 30 seconds"
    );
    assert_eq!(jobs.background(), 1);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(600_000), DEADLINE),
        "the command did not fall back to the foreground"
    );
    assert_eq!(jobs.background(), 0, "a fallen-back call stayed registered");
    release_block(ready.path());
    let output = running.output.recv_timeout(DEADLINE).expect("the result");
    assert!(
        text(&output).contains("It could not move to the background"),
        "{}",
        text(&output)
    );
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
    let pgid = ready.wait(CHILD_START)[0];
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

// `tty`: the command runs in a pseudo-terminal and moves to the background
// at once (`docs/tools.md`, "Terminal (`tty`)").

fn start_tty(
    dir: PathBuf,
    command: String,
    timeout_ms: Option<u64>,
    jobs: Arc<FakeJobs>,
) -> JobRun {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>)
        .with_jobs(jobs.clone());
    let mut arguments = args(&command);
    if let Some(timeout_ms) = timeout_ms {
        arguments.insert("timeout_ms".into(), json!(timeout_ms));
    }
    arguments.insert("tty".into(), json!(true));
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(shell.run(&arguments, &CancelToken::new(), tapped.as_ref()))
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

/// Writes the group id to `ready` first, then runs `body`.
fn on_terminal(ready: &Path, body: &str) -> String {
    format!("echo $$ > {}\n{body}", quote(ready))
}

#[test]
fn tty_is_a_boolean_and_needs_jobs() {
    let dir = fakes::TempDir::new("fiber-shell-tty-args");
    let marker = dir.path().join("marker");
    let touch = format!("touch {}", quote(&marker));
    let schema = Shell::new(dir.path().to_path_buf(), FakeClock::new())
        .definition()
        .input_schema;
    assert_eq!(schema["properties"]["tty"]["type"], "boolean");
    let with_jobs = Arc::new(
        Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(FakeJobs::new(dir.path())),
    );
    let without = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
    for value in [json!("yes"), json!(1)] {
        let mut arguments = args(&touch);
        arguments.insert("tty".into(), value);
        let output = run_on(&with_jobs, arguments, &Arc::new(Recorder::default()));
        assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
        assert!(
            text(&output).contains("`tty` must be a boolean"),
            "{}",
            text(&output)
        );
    }
    let mut arguments = args(&touch);
    arguments.insert("tty".into(), json!(true));
    let output = run_on(&without, arguments, &Arc::new(Recorder::default()));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(
        text(&output).contains("Background jobs are not available in this session."),
        "{}",
        text(&output)
    );
    assert!(!marker.exists(), "a refused call started its command");
}

#[test]
fn a_tty_command_has_a_terminal_for_all_three_streams_and_as_its_controlling_terminal() {
    let dir = fakes::TempDir::new("fiber-shell-tty-terminal");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let block = block_of(ready.path());
    // The command prints once the test has seen the call park for its first
    // output, which it does only after the job moved, so the output is
    // `job_delta` text and is in the file when the delta arrives.
    // One write, so one `job_delta`: the next is held until the fake clock
    // moves.
    let body = format!(
        "mkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\n\
         c=none\n{{ : </dev/tty; }} 2>/dev/null && c=controlling\n\
         test -t 0 && test -t 1 && test -t 2 && printf 'yes:%s:%s\\n' \"$(tty)\" $c\nread x\n",
        block = quote(&block),
        ready = quote(ready.path()),
    );
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), &body),
        None,
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(250), DEADLINE),
        "the call did not park for the first output"
    );
    let block = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&block);
        let _sent = released.send(());
    });
    release
        .recv_timeout(DEADLINE)
        .expect("waited for the command's read of the block fifo");
    assert!(
        jobs.deltas().wait_for_text("yes:/dev/", DEADLINE),
        "the output did not arrive: {:?}",
        jobs.deltas().text()
    );
    running.clock.advance(Duration::from_millis(250));
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    let body = text(&output);
    assert!(output.error.is_none(), "{body}");
    assert!(
        body.starts_with("Started in a terminal and moved to the background.\n"),
        "{body}"
    );
    assert!(body.contains("jobs write"), "{body}");
    assert!(body.contains("Output so far:\nyes:/dev/"), "{body}");
    // The terminal may hand the line's end to the reader in a later read,
    // so the receipt is checked for the line and the file for its end.
    assert!(body.contains(":controlling"), "{body}");
    assert!(!body.contains("not a tty"), "{body}");
    // The bytes also stay in the output file, as a pipe job's do, with the
    // terminal's `\r\n` line end. A line end read later goes out in a
    // later delta once the clock has passed the pacing interval, and the
    // reader writes the file before it hands bytes to a delta.
    assert!(
        jobs.deltas().wait_for_text(":controlling\r\n", DEADLINE),
        "{:?}",
        jobs.deltas().text()
    );
    let file = job_output(dir.path(), &job);
    assert!(
        file.contains("yes:/dev/") && file.contains(":controlling\r\n"),
        "{file:?}"
    );
    assert!(group_alive(pgid));
    jobs.stop(&job.job_id);
    let ended = jobs.ended(DEADLINE).expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_command_that_ends_at_once_on_a_terminal_still_returns_its_output() {
    let dir = fakes::TempDir::new("fiber-shell-tty-quick");
    let jobs = FakeJobs::new(dir.path());
    let running = start_tty(
        dir.path().to_path_buf(),
        "test -t 0 && echo quick".to_owned(),
        None,
        jobs,
    );
    // Moved or finished, the output is in the result: the call raced the
    // command's exit.
    let output = running.output.recv_timeout(DEADLINE).expect("the result");
    assert!(text(&output).contains("quick"), "{}", text(&output));
    assert!(output.error.is_none(), "{}", text(&output));
}

#[test]
fn the_receipt_waits_250_ms_for_output_on_the_clock() {
    let dir = fakes::TempDir::new("fiber-shell-tty-250");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), "read x\n"),
        None,
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let due = running.start + Duration::from_millis(250);
    assert!(
        running.clock.await_parked(due, DEADLINE),
        "the call did not park for the first output"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the receipt returned before 250 ms"
    );
    running.clock.advance(Duration::from_millis(250));
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    assert!(
        !text(&output).contains("Output so far"),
        "{}",
        text(&output)
    );
    jobs.stop(&job.job_id);
    let ended = jobs.ended(DEADLINE).expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn what_a_job_is_typed_reaches_the_program_and_its_answer_is_in_the_file() {
    let dir = fakes::TempDir::new("fiber-shell-tty-write");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), "read line\necho got:$line\n"),
        None,
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let due = running.start + Duration::from_millis(250);
    assert!(running.clock.await_parked(due, DEADLINE));
    running.clock.advance(Duration::from_millis(250));
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    assert!(
        running
            .jobs
            .type_into(
                &job.job_id,
                b"hi\n",
                running.clock.as_ref(),
                &CancelToken::new()
            )
            .expect("a terminal to type into")
            .is_ok()
    );
    let ended = jobs.ended(DEADLINE).expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    let file = job_output(dir.path(), &job);
    assert!(file.contains("got:hi"), "{file:?}");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_tty_job_past_its_timeout_fails_and_stop_cancels_it() {
    let dir = fakes::TempDir::new("fiber-shell-tty-timeout");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), "echo partial\nread x\n"),
        Some(5_000),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(250), DEADLINE)
    );
    running.clock.advance(Duration::from_millis(250));
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let timeout_at = running.start + Duration::from_secs(5);
    assert!(
        running.clock.await_parked(timeout_at, CHILD_START),
        "the job did not park at its timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance(Duration::from_millis(4_750));
    let ended = jobs.ended(DEADLINE).expect("the job to time out");
    assert_eq!(ended.status, Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Timeout)
    );
    assert!(ended.process.as_ref().unwrap().timed_out);
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_cancel_stops_a_write_to_a_terminal_whose_program_never_reads() {
    let dir = fakes::TempDir::new("fiber-shell-tty-cancel-write");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let block = block_of(ready.path());
    // Raw mode with no echo, so the terminal's input queue fills and does
    // not drop what does not fit; the program then waits on a fifo.
    let body = format!(
        "stty raw -echo\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\n",
        block = quote(&block),
        ready = quote(ready.path()),
    );
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), &body),
        None,
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(250), DEADLINE)
    );
    running.clock.advance(Duration::from_millis(250));
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);

    let clock = FakeClock::new();
    let cancel = CancelToken::new();
    let input = vec![b'x'; 1 << 20];
    let (done, written) = mpsc::channel();
    let (typing_jobs, typing_clock, typing_cancel) =
        (Arc::clone(&jobs), Arc::clone(&clock), cancel.clone());
    let id = job.job_id.clone();
    let length = input.len();
    thread::spawn(move || {
        let _sent = done.send(
            typing_jobs
                .type_into(&id, &input, typing_clock.as_ref(), &typing_cancel)
                .expect("a terminal to type into"),
        );
    });
    // Parked on the clock: the queue is full and the program does not read.
    assert!(
        clock.await_parked(clock.origin() + Duration::from_millis(10), DEADLINE),
        "the write did not wait for room"
    );
    assert!(matches!(written.try_recv(), Err(mpsc::TryRecvError::Empty)));
    cancel.cancel();
    let count = written
        .recv_timeout(DEADLINE)
        .expect("the cancelled write to return")
        .unwrap();
    assert!(count < length, "the whole input fit: {count}");
    jobs.stop(&job.job_id);
    let ended = jobs.ended(DEADLINE).expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_tty_command_is_still_a_bare_wait() {
    let dir = fakes::TempDir::new("fiber-shell-tty-sleep");
    let marker = dir.path().join("marker");
    let jobs = FakeJobs::new(dir.path());
    let shell =
        Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone()));
    // Only `run_in_background` skips the refusal. A zero timeout, so a
    // command that wrongly starts stops at once.
    let mut arguments = args(&format!("sleep 30; touch {}", quote(&marker)));
    arguments.insert("tty".into(), json!(true));
    arguments.insert("timeout_ms".into(), json!(0));
    let output = run_on(&shell, arguments, &Arc::new(Recorder::default()));
    assert_eq!(code(&output), Some(ErrorCode::InvalidArguments));
    assert!(text(&output).contains("jobs wait"), "{}", text(&output));
    assert!(jobs.started().is_empty());
    assert!(!marker.exists());
}

// Monitors: a job whose standard output lines reach the model in batches
// (`docs/tools.md`, "Background jobs").

/// The deadline the monitor tests give, so it never interferes.
const LONG_DEADLINE_MS: u64 = 1_800_000;

fn start_monitor(
    dir: PathBuf,
    command: String,
    deadline_ms: Option<u64>,
    jobs: Arc<FakeJobs>,
) -> JobRun {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>)
        .with_jobs(jobs.clone());
    let mut arguments = args(&command);
    arguments.insert("monitor".into(), json!(true));
    if let Some(deadline_ms) = deadline_ms {
        arguments.insert("deadline_ms".into(), json!(deadline_ms));
    }
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(shell.run(&arguments, &CancelToken::new(), tapped.as_ref()))
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

/// Writes its group to `ready` twice, around making the feed fifo, then
/// copies the feed to standard output until the feed closes.
fn fed_by_fifo(ready: &Path, before: &str) -> String {
    format!(
        "{before}\necho $$ > {ready}\nmkfifo {feed}\necho $$ >> {ready}\ncat {feed}\n",
        ready = quote(ready),
        feed = quote(&block_of(ready)),
    )
}

/// The feed's write end. The open waits for the command's read, so it runs
/// on its own thread under the deadline.
fn open_feed(ready: &Path) -> std::fs::File {
    let feed = block_of(ready);
    let (opened, open) = mpsc::channel();
    thread::spawn(move || {
        let _sent = opened.send(std::fs::OpenOptions::new().write(true).open(feed).unwrap());
    });
    open.recv_timeout(DEADLINE)
        .expect("waited for the command's read of the feed fifo")
}

/// Every step in order: the drive thread offers what a delta carried before
/// it parks again, so the next step's park means the line was offered at the
/// step's instant. One [`STEPS`] bound for the whole sequence.
fn print_steps(
    run: &JobRun,
    deadline: Instant,
    feed: std::fs::File,
    steps: Vec<(Duration, String)>,
) -> std::fs::File {
    let clock = Arc::clone(&run.clock);
    let jobs = Arc::clone(&run.jobs);
    fakes::within("monitor steps", STEPS, move || {
        let mut feed = feed;
        for (step, line) in steps {
            assert!(
                clock.await_parked(deadline, DEADLINE),
                "the monitor did not park at its deadline before {line}"
            );
            clock.advance(step);
            // One write: `writeln!` can split the newline into its own write, and
            // a delta of the line alone would hold the newline past the step.
            feed.write_all(format!("{line}\n").as_bytes()).unwrap();
            assert!(
                jobs.deltas().wait_for_text(&format!("{line}\n"), DEADLINE),
                "the monitor did not take {line}"
            );
        }
        feed
    })
}

fn errors_file(dir: &Path, job: &JobStarted) -> PathBuf {
    let id = &job.job_id.0;
    dir.join(format!("{id}.stderr.log"))
}

#[test]
fn a_monitor_moves_at_once_with_its_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-receipt");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_monitor(
        dir.path().to_path_buf(),
        blocking(ready.path(), "a", "b"),
        None,
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    assert!(output.error.is_none(), "{}", text(&output));
    let job = started(&output);
    assert_eq!(
        text(&output),
        format!(
            "Started a monitor.\nJob {id}. Output: {out}. Errors: {err}. Lines it prints reach you in batches; its deadline is 300000 ms.\n",
            id = job.job_id.0,
            out = dir.path().join(&job.output_path).display(),
            err = errors_file(dir.path(), &job).display(),
        )
    );
    assert_eq!(jobs.started(), vec![job.clone()]);
    // The default deadline: 5 minutes from the command's start.
    assert!(
        running
            .clock
            .await_parked(running.start + Duration::from_millis(300_000), DEADLINE)
    );
    jobs.stop(&job.job_id);
    let ended = jobs.ended(DEADLINE).expect("the stop to end the monitor");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_monitor_whose_errors_file_cannot_be_created_says_its_standard_error_is_discarded() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-no-errors");
    // The first job's errors file is a directory, so it cannot be created.
    let errors = dir.path().join("j_0000000000000001.stderr.log");
    std::fs::create_dir(&errors).unwrap();
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let block = block_of(ready.path());
    let command = format!(
        "echo $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\necho err >&2\necho out\n",
        ready = quote(ready.path()),
        block = quote(&block),
    );
    let running = start_monitor(dir.path().to_path_buf(), command, None, Arc::clone(&jobs));
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    assert!(output.error.is_none(), "{}", text(&output));
    let job = started(&output);
    assert_eq!(errors_file(dir.path(), &job), errors);
    let receipt = text(&output);
    let prefix = format!(
        "Started a monitor.\nJob {id}. Output: {out}. Its standard error is discarded: {err} could not be created (",
        id = job.job_id.0,
        out = dir.path().join(&job.output_path).display(),
        err = errors.display(),
    );
    assert!(receipt.starts_with(&prefix), "{receipt}");
    assert!(
        receipt.ends_with("). Lines it prints reach you in batches; its deadline is 300000 ms.\n"),
        "{receipt}"
    );
    assert!(!receipt.contains("Errors:"), "{receipt}");
    let path = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&path);
        let _sent = released.send(());
    });
    release.recv_timeout(DEADLINE).expect("the release");
    let ended = jobs.ended(DEADLINE).expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed, "{ended:?}");
    let texts: Vec<String> = jobs
        .lines()
        .lines()
        .into_iter()
        .map(|line| line.lines)
        .collect();
    assert_eq!(texts, ["out"]);
    assert!(errors.is_dir());
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_monitor_with_a_zero_deadline_times_out_in_the_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-zero");
    let jobs = FakeJobs::new(dir.path());
    let shell =
        Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone()));
    let mut arguments = args("echo hi");
    arguments.insert("monitor".into(), json!(true));
    arguments.insert("deadline_ms".into(), json!(0));
    let output = run_on(&shell, arguments, &Arc::new(Recorder::default()));
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    let line = "The monitor's deadline of 0 ms passed; start it again if you still need it.";
    assert_eq!(output.error.as_ref().unwrap().message, line);
    assert!(
        text(&output).ends_with(&format!("{line}\n")),
        "{}",
        text(&output)
    );
    assert!(jobs.started().is_empty());
    assert!(jobs.lines().lines().is_empty());
}

#[test]
fn standard_output_lines_reach_the_model_and_standard_error_its_own_file() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-streams");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let block = block_of(ready.path());
    let command = format!(
        "echo $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\necho out\necho err >&2\necho after\necho err2 >&2\n",
        ready = quote(ready.path()),
        block = quote(&block),
    );
    let running = start_monitor(dir.path().to_path_buf(), command, None, Arc::clone(&jobs));
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let output = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let job = started(&output);
    let path = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&path);
        let _sent = released.send(());
    });
    release.recv_timeout(DEADLINE).expect("the release");
    let ended = jobs.ended(DEADLINE).expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed, "{ended:?}");
    let delivered: Vec<String> = jobs
        .lines()
        .lines()
        .into_iter()
        .map(|line| {
            assert_eq!(line.job_id, job.job_id);
            assert_eq!(line.suppressed, None);
            line.lines
        })
        .collect();
    assert_eq!(delivered.join("\n"), "out\nafter");
    assert_eq!(job_output(dir.path(), &job), "out\nafter\n");
    assert_eq!(
        std::fs::read_to_string(errors_file(dir.path(), &job)).unwrap(),
        "err\nerr2\n"
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn an_incomplete_last_line_is_flushed_before_the_end() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-tail");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let (inbox, delivered) = mpsc::channel();
    jobs.deliver_to(inbox);
    let block = block_of(ready.path());
    let command = format!(
        "echo $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\nprintf 'a\\nb'\n",
        ready = quote(ready.path()),
        block = quote(&block),
    );
    let running = start_monitor(dir.path().to_path_buf(), command, None, Arc::clone(&jobs));
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let path = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&path);
        let _sent = released.send(());
    });
    release.recv_timeout(DEADLINE).expect("the release");
    let ended = jobs.ended(DEADLINE).expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed);
    let texts: Vec<String> = jobs
        .lines()
        .lines()
        .into_iter()
        .map(|line| line.lines)
        .collect();
    assert_eq!(texts, ["a", "b"]);
    // Every batch is in the inbox before the end.
    let kinds: Vec<&str> = delivered
        .try_iter()
        .map(|delivery| {
            if matches!(delivery, contract::inbox::Delivery::Job(_)) {
                "end"
            } else {
                assert!(
                    matches!(delivery, contract::inbox::Delivery::JobLine(_)),
                    "unexpected {delivery:?}"
                );
                "line"
            }
        })
        .collect();
    assert_eq!(kinds, ["line", "line", "end"]);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_suppressed_count_still_pending_is_sent_before_the_end() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-suppressed");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let (inbox, delivered) = mpsc::channel();
    jobs.deliver_to(inbox);
    let running = start_monitor(
        dir.path().to_path_buf(),
        fed_by_fifo(ready.path(), ":"),
        Some(LONG_DEADLINE_MS),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let deadline = running.start + Duration::from_millis(LONG_DEADLINE_MS);
    let feed = open_feed(ready.path());
    // Twelve deliveries 100 ms apart: ten spend the budget, two are dropped.
    let steps: Vec<(Duration, String)> = (1..=12)
        .map(|k| (Duration::from_millis(100), format!("line{k}")))
        .collect();
    let feed = print_steps(&running, deadline, feed, steps);
    drop(feed);
    let ended = jobs.ended(DEADLINE).expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed);
    let lines = jobs.lines().lines();
    let texts: Vec<&str> = lines.iter().map(|line| line.lines.as_str()).collect();
    let mut expected: Vec<String> = (1..=10).map(|k| format!("line{k}")).collect();
    expected.push(String::new());
    assert_eq!(texts, expected);
    assert_eq!(lines.last().unwrap().suppressed, Some(2));
    assert!(lines[..10].iter().all(|line| line.suppressed.is_none()));
    let kinds: Vec<bool> = delivered
        .try_iter()
        .map(|delivery| matches!(delivery, contract::inbox::Delivery::Job(_)))
        .collect();
    assert_eq!(kinds.iter().filter(|end| **end).count(), 1);
    assert_eq!(kinds.last(), Some(&true), "the end came last");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn sustained_output_for_thirty_seconds_floods_and_stops_the_monitor() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-flood");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_monitor(
        dir.path().to_path_buf(),
        fed_by_fifo(ready.path(), ":"),
        Some(LONG_DEADLINE_MS),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let deadline = running.start + Duration::from_millis(LONG_DEADLINE_MS);
    let feed = open_feed(ready.path());
    // One delivery every 500 ms, faster than the refill: the budget runs
    // out at the 14th (7 s), so the drop at the 74th (37 s) floods.
    let steps: Vec<(Duration, String)> = (1..=73)
        .map(|k| (Duration::from_millis(500), format!("line{k}")))
        .collect();
    let feed = print_steps(&running, deadline, feed, steps);
    assert!(jobs.ended(Duration::ZERO).is_none(), "flooded early");
    assert!(group_alive(pgid));
    let feed = print_steps(
        &running,
        deadline,
        feed,
        vec![(Duration::from_millis(500), "line74".to_owned())],
    );
    let ended = jobs.ended(DEADLINE).expect("the flood to stop the monitor");
    assert_eq!(ended.status, Outcome::Failed);
    let error = ended.error.unwrap();
    assert_eq!(error.code, ErrorCode::Flooded);
    assert_eq!(
        error.message,
        "The monitor's output was suppressed for 30 seconds, so it was stopped. Restart it with a more selective source."
    );
    assert!(!group_alive(pgid));
    let lines = jobs.lines().lines();
    assert_eq!(lines[13].lines, "line17");
    assert_eq!(lines[13].suppressed, Some(3));
    let last = lines.last().unwrap();
    assert_eq!(last.lines, "");
    assert!(last.suppressed.is_some_and(|count| count > 0), "{last:?}");
    drop(feed);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn output_that_pauses_for_two_seconds_ends_the_run_and_does_not_flood() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-pause");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_monitor(
        dir.path().to_path_buf(),
        fed_by_fifo(ready.path(), ":"),
        Some(LONG_DEADLINE_MS),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let deadline = running.start + Duration::from_millis(LONG_DEADLINE_MS);
    let feed = open_feed(ready.path());
    // Suppressed from 7 s to 25 s, then 2.5 s with nothing, then suppressed
    // again to 50 s: 43 s after the first drop, but no run lasts 30 s.
    let mut steps: Vec<(Duration, String)> = (1..=50)
        .map(|k| (Duration::from_millis(500), format!("line{k}")))
        .collect();
    steps.push((Duration::from_millis(2_500), "line51".to_owned()));
    steps.extend((52..=96).map(|k| (Duration::from_millis(500), format!("line{k}"))));
    let feed = print_steps(&running, deadline, feed, steps);
    assert!(jobs.ended(Duration::ZERO).is_none(), "the monitor flooded");
    drop(feed);
    let ended = jobs.ended(DEADLINE).expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed, "{ended:?}");
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_monitor_ends_at_its_deadline_as_a_timeout() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-deadline");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_monitor(
        dir.path().to_path_buf(),
        blocking(ready.path(), "partial", "later"),
        Some(5_000),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(CHILD_START)[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(DEADLINE);
    let _receipt = running.output.recv_timeout(DEADLINE).expect("the receipt");
    let due = running.start + Duration::from_secs(5);
    assert!(running.clock.await_parked(due, DEADLINE));
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance(Duration::from_secs(5));
    let ended = jobs
        .ended(DEADLINE)
        .expect("the deadline to end the monitor");
    assert_eq!(ended.status, Outcome::Failed);
    let error = ended.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(
        error.message,
        "The monitor's deadline of 5000 ms passed; start it again if you still need it."
    );
    assert!(ended.process.unwrap().timed_out);
    let texts: Vec<String> = jobs
        .lines()
        .lines()
        .into_iter()
        .map(|line| line.lines)
        .collect();
    assert_eq!(texts, ["partial"]);
    assert!(!group_alive(pgid));
    watchdog.stand_down(DEADLINE);
}
