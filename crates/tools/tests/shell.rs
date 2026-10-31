//! The shell tool through [`tools::Shell::run`] (`docs/tools.md`, "Shell").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use contract::ActionId;
use contract::ErrorCode;
use contract::clock::Wake;
use contract::events::{JobStarted, Outcome};
use contract::jobs::{JobRecord, Jobs as _};
use contract::shapes::{ContentPart, Process};
use contract::tool::{Answered, Ask, Asking, Cancel, Tool};
use fakes::Deadline;
use fakes::children::{Ready, escapes_group, ignores_sigterm, leaves_descendants};
use fakes::clock::FakeClock;
use fakes::jobs::FakeJobs;
use fakes::{CancelToken, Recorder, Watchdog, group_alive, kill_group, kill_pid, pid_alive};
use serde_json::{Map, Value, json};
#[cfg(target_os = "macos")]
use tools::SandboxSettings;
use tools::Shell;

/// The bound on a child's startup-marker ready lines. How long a child takes to start
/// follows the machine's load, not the test: at a load of 40 to 100 on 11
/// cores 5 s was not enough. 20 s is the per-wait bound `crates/main/tests`
/// use. A later startup marker (for example the second line proving `sleep`
/// exec'd before the exit) keeps `CHILD_START`; other later lines come from
/// the running command and keep `fakes::MUST_SUCCEED_WITHIN`.
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

#[path = "support/sandbox_home.rs"]
mod sandbox_home;

fn args(command: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("command".into(), Value::String(command.into()));
    map
}

/// A caller that never asks: integration calls run through
/// `run_with_effects` with prepared effects and no interaction.
struct NoAsk;

impl Ask for NoAsk {
    fn action(&self) -> ActionId {
        ActionId("a_1".into())
    }

    fn answerable(&self) -> bool {
        false
    }

    fn ask(&self, _asking: Asking) -> Answered {
        Answered::NoAnswer
    }
}

/// Runs the call with the prepared effects, as the loop would: the shell
/// classifies the arguments, then runs with that exact decision. The
/// integration suite drives this entry so every shell behavior runs
/// through the same seam the loop uses.
fn run_prepared(
    shell: &Shell,
    arguments: &Map<String, Value>,
    cancel: &dyn Cancel,
    emit: &dyn contract::emit::Emit,
) -> contract::tool::Output {
    let effects = shell
        .effects(arguments)
        .expect("the test arguments classify");
    shell.run_with_effects(arguments, &effects, cancel, emit, &NoAsk)
}

#[track_caller]
fn run(dir: &Path, command: &str) -> contract::tool::Output {
    let shell = Arc::new(Shell::new(dir.to_path_buf(), FakeClock::new()));
    run_on(&shell, args(command), &Arc::new(Recorder::default()))
}

/// Runs one shell call on its own thread and returns its output. Calling
/// code that blocks is a wait too (`docs/testing.md`, "Waits and
/// timeouts"): on expiry the test fails naming the command.
#[track_caller]
fn run_on(
    shell: &Arc<Shell>,
    arguments: Map<String, Value>,
    recorder: &Arc<Recorder>,
) -> contract::tool::Output {
    let shell = Arc::clone(shell);
    let recorder = Arc::clone(recorder);
    fakes::within(
        &format!("the shell command {}", arguments["command"]),
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || run_prepared(&shell, &arguments, &CancelToken::new(), recorder.as_ref()),
    )
}

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
    #[track_caller]
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
        match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&rx) {
            Ok(()) => {}
            Err(_) => panic!(
                "waited {:?} for a holder of {}",
                fakes::MUST_SUCCEED_WITHIN,
                self.path.display()
            ),
        }
        Holders(rx)
    }
}

struct Holders(mpsc::Receiver<()>);

impl Holders {
    #[track_caller]
    fn gone(self, what: &str) {
        match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&self.0) {
            Ok(()) => {}
            Err(_) => panic!("waited {:?} for {what} to exit", fakes::MUST_SUCCEED_WITHIN),
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
        tx.send(run_prepared(
            &shell,
            &arguments,
            &cancel_for_run,
            &Recorder::default(),
        ))
        .unwrap();
    });
    Running {
        clock,
        start,
        output: rx,
    }
}

#[track_caller]
fn join(running: Running) -> contract::tool::Output {
    match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&running.output) {
        Ok(output) => output,
        Err(_) => panic!(
            "waited {:?} for the command to finish",
            fakes::MUST_SUCCEED_WITHIN
        ),
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
    let dir = fakes::TempDir::new("fiber-shell-usr1");
    let output = run(dir.path(), "kill -USR1 $$");
    assert_eq!(code(&output), Some(ErrorCode::Signal));
    assert!(
        text(&output).contains("Killed by SIGUSR1."),
        "{}",
        text(&output)
    );
    let process = output.process.unwrap();
    assert_eq!(process.signal.as_deref(), Some("SIGUSR1"));
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
    let output = match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => panic!(
            "waited {:?} for the BASH_ENV probe",
            fakes::MUST_SUCCEED_WITHIN
        ),
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
    let output = match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => panic!(
            "waited {:?} for the BASH_FUNC probe",
            fakes::MUST_SUCCEED_WITHIN
        ),
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
    fakes::fifo(&hold);
    let command = format!(
        "echo $$ > {ready}\n(read -r _ < {hold}) &\necho $! >> {ready}\n",
        ready = quote(ready.path()),
        hold = quote(&hold),
    );
    let running = start(dir.path().to_path_buf(), command, None, CancelToken::new());
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _child = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(600),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(group_alive(pgid));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(1),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at the timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(1)),
        None,
        Duration::from_secs(1),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = join(running);
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(text(&output).contains("Timed out after 1000 ms and stopped."));
    assert!(output.process.unwrap().timed_out);
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_millis(600_000),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at the default timeout"
    );
    cancel.cancel();
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(1)),
        None,
        Duration::from_secs(1),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    cancel.cancel();
    let output = join(running);
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    assert!(text(&output).contains("Timed out after 1000 ms and stopped."));
    assert!(!text(&output).contains("Cancelled"));
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    cancel.cancel();
    let grace = running.start + Duration::from_millis(800);
    assert!(
        running
            .clock
            .await_parked(grace, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the run did not park for the grace period"
    );
    assert!(group_alive(pgid), "killed before the grace elapsed");
    running.clock.advance_when_parked(
        Some(grace),
        None,
        Duration::from_millis(800),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let pids = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let holders = life.watch();
    cancel.cancel();
    assert!(running.clock.await_parked(
        running.start + Duration::from_millis(800),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    for pid in &pids {
        assert!(pid_alive(*pid), "{pid} died before the grace");
    }
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_millis(800)),
        None,
        Duration::from_millis(800),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = join(running);
    assert!(output.error.is_none(), "{}", text(&output));
    assert!(text(&output).contains("Cancelled and stopped."));
    holders.gone("the shell and its descendant");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let holder = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let holders = life.watch();
    let _guard = KillPid(holder);
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(2),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at the drain bound"
    );
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(2)),
        None,
        Duration::from_secs(2),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let escapee = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let holders = life.watch();
    let _guard = KillPid(escapee);
    cancel.cancel();
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(2),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at the drain bound"
    );
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(2)),
        None,
        Duration::from_secs(2),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    let texts = [
        text(&wait.recv(&rx).unwrap()),
        text(&wait.recv(&rx).unwrap()),
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
        tx.send(run_prepared(
            &shell,
            &arguments,
            &CancelToken::new(),
            tapped.as_ref(),
        ))
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
    fakes::fifo(&fifo);
    let shell = Shell::new(dir.path().to_path_buf(), FakeClock::new());
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let command = format!("echo line; read x < {}; echo got:$x", quote(&fifo));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(run_prepared(
            &shell,
            &args(&command),
            &CancelToken::new(),
            tapped.as_ref(),
        ))
        .unwrap();
    });
    assert!(
        recorder.wait_for_text("line\n", fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "waited {:?} for the line to stream",
        fakes::MUST_SUCCEED_WITHIN
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
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the call to finish");
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
        running.recorder.wait_for_text(
            "partial\n",
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "waited {:?} for the partial line to stream",
        fakes::MUST_SUCCEED_WITHIN
    );
    let due = running.start + Duration::from_secs(1);
    running.clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_secs(1),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the command to finish");
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(text(&output).starts_with("partial\n"), "{}", text(&output));
    assert_eq!(running.recorder.text(), "partial\n");
}

struct JobRun {
    call: thread::ThreadId,
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
    let call = thread::spawn(move || {
        tx.send(run_prepared(
            &shell,
            &arguments,
            &cancel_for_run,
            tapped.as_ref(),
        ))
        .unwrap();
    })
    .thread()
    .id();
    JobRun {
        call,
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(
        ended.process.as_ref().and_then(|process| process.exit_code),
        Some(0)
    );
    assert!(ended.output_tail.is_none());
    assert_eq!(job_output(dir.path(), &job), "a\nb\n");
    assert_eq!(running.recorder.text(), streamed);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let job = started(&output);
    let deltas = jobs.deltas();
    // The FIFO open waits for the shell's read, so it runs on its own thread.
    let block = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&block);
        let _sent = released.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("waited for the command's read of the block fifo");
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let move_at = running.start + Duration::from_secs(30);
    assert!(
        running
            .clock
            .await_parked(move_at, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the run did not park at 30 seconds"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the command moved before 30 seconds"
    );
    assert!(group_alive(pgid));
    running.clock.advance_when_parked(
        Some(move_at),
        None,
        Duration::from_secs(30),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
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
        running.clock.await_parked(
            timeout_at,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the job did not keep the timeout from the command's start"
    );
    let streamed = running.recorder.text();
    assert!(!streamed.contains("AFTER"), "{streamed}");
    release_block(ready.path());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(job_output(dir.path(), &job), "BEFORE\nAFTER\n");
    assert_eq!(running.recorder.text(), streamed);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(30),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at 30 seconds"
    );
    assert_eq!(jobs.background(), 1);
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
        running.clock.await_parked(
            running.start + Duration::from_millis(600_000),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the job did not keep the timeout from the command's start"
    );
    release_block(ready.path());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(job_output(dir.path(), &job), "BEFORE\nAFTER\n");
    assert_eq!(jobs.started().len(), 1, "the command moved twice");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the result");
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(30)),
        None,
        Duration::from_secs(30),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    assert!(
        text(&output).starts_with("Still running after 30 seconds"),
        "{}",
        text(&output)
    );
    assert_eq!(jobs.background(), 0);
    assert_eq!(jobs.started().len(), 1);
    release_block(ready.path());
    jobs.ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    assert_eq!(jobs.background(), 0);
    release_block(ready.path());
    jobs.ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(30),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at the timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_secs(30)),
        None,
        Duration::from_secs(30),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the timeout");
    assert_eq!(code(&output), Some(ErrorCode::Timeout));
    assert!(output.process.as_ref().unwrap().timed_out);
    assert!(text(&output).contains("Timed out after 30000 ms and stopped."));
    assert!(output.jobs.is_empty());
    assert!(jobs.started().is_empty());
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn a_shell_that_exits_with_members_moves_and_names_them() {
    let dir = fakes::TempDir::new("fiber-shell-orphans");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let command = format!(
        "echo $$ > {ready}; sleep 1000 & until ps -o comm= -p $! | grep -q sleep; do :; done; echo $$ >> {ready}; exit 3",
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let _started = ready.wait(fakes::Deadline::after(CHILD_START));
    let watchdog = Watchdog::group(pgid);
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
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
        .mark_parked(
            timeout_at,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        )
        .expect("the moved job did not park at its timeout");
    // A later park at the timeout means another group check found sleep
    // alive and the job waits on.
    assert!(
        running.clock.await_parked_since(
            &mark,
            Some(timeout_at),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the job did not wait again while sleep was in the group"
    );
    assert!(
        jobs.ended(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "the job ended while sleep was still in the group"
    );
    assert!(kill_group(pgid, "KILL").unwrap());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let due = running.start + Duration::from_secs(5);
    assert!(
        running
            .clock
            .await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the job did not park at its timeout"
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_secs(5),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to time out");
    assert_eq!(ended.status, Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Timeout)
    );
    assert!(ended.process.as_ref().unwrap().timed_out);
    let tail = ended.output_tail.unwrap();
    assert!(tail.contains("partial"), "{tail}");
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let job = started(&output);
    assert_eq!(
        watched.live(),
        0,
        "the call's cancel still reaches the moved job"
    );
    let timeout_at = running.start + Duration::from_millis(600_000);
    assert!(
        running.clock.await_parked(
            timeout_at,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the job did not park"
    );
    cancel.cancel();
    // Marked after the cancel, then woken: a later park at the timeout ran a
    // whole pass after the turn cancel and did not stop.
    let mark = running.clock.advance_when_parked(
        Some(timeout_at),
        None,
        Duration::ZERO,
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    assert!(
        running.clock.await_parked_since(
            &mark,
            Some(timeout_at),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the job did not wait again after the turn cancel"
    );
    assert!(
        jobs.ended(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "the turn cancel stopped the job"
    );
    assert!(group_alive(pgid));
    assert!(
        running.clock.parked().contains(&Some(timeout_at)),
        "the turn cancel left the timeout park: {:?}",
        running.clock.parked()
    );
    jobs.stop(&job.job_id);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the stop to finish the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    assert!(ended.error.is_none());
    assert!(ended.output_tail.is_none());
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_secs(30),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the run did not park at 30 seconds"
    );
    assert_eq!(jobs.background(), 1);
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_millis(600_000),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the command did not fall back to the foreground"
    );
    assert_eq!(jobs.background(), 0, "a fallen-back call stayed registered");
    release_block(ready.path());
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the result");
    assert!(
        text(&output).contains("It could not move to the background"),
        "{}",
        text(&output)
    );
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_millis(600_000),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the command did not stay in the foreground"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "open failing returned before the command ended"
    );
    assert!(group_alive(pgid));
    assert!(running.jobs.started().is_empty());
    release_block(ready.path());
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the foreground result");
    let body = text(&output);
    assert!(body.contains("BEFORE\n"), "{body}");
    assert!(body.contains("AFTER\n"), "{body}");
    assert!(body.contains("Exit code 0."), "{body}");
    assert!(
        body.ends_with("It could not move to the background: the job's output file jobs/unavailable.log could not be created: background jobs are unavailable.\nExit code 0.\n"),
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let call = thread::spawn(move || {
        tx.send(run_prepared(
            &shell,
            &arguments,
            &CancelToken::new(),
            tapped.as_ref(),
        ))
        .unwrap();
    })
    .thread()
    .id();
    JobRun {
        call,
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
fn tty_needs_jobs() {
    let dir = fakes::TempDir::new("fiber-shell-tty-args");
    let marker = dir.path().join("marker");
    let touch = format!("touch {}", quote(&marker));
    let without = Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(
        running.clock.await_parked(
            running.start + Duration::from_millis(250),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the call did not park for the first output"
    );
    let block = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&block);
        let _sent = released.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("waited for the command's read of the block fifo");
    assert!(
        jobs.deltas().wait_for_text(
            "yes:/dev/",
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the output did not arrive: {:?}",
        jobs.deltas().text()
    );
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_millis(250)),
        Some(running.call),
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
        jobs.deltas().wait_for_text(
            ":controlling\r\n",
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
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
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the result");
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let due = running.start + Duration::from_millis(250);
    assert!(
        running
            .clock
            .await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the call did not park for the first output"
    );
    assert!(
        matches!(running.output.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the receipt returned before 250 ms"
    );
    running.clock.advance_when_parked(
        Some(due),
        Some(running.call),
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let job = started(&output);
    assert!(
        !text(&output).contains("Output so far"),
        "{}",
        text(&output)
    );
    jobs.stop(&job.job_id);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let due = running.start + Duration::from_millis(250);
    running.clock.advance_when_parked(
        Some(due),
        Some(running.call),
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    let file = job_output(dir.path(), &job);
    assert!(file.contains("got:hi"), "{file:?}");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn a_tty_job_past_its_timeout_fails_and_stop_cancels_it() {
    let dir = fakes::TempDir::new("fiber-shell-tty-timeout");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let block = block_of(ready.path());
    // The command prints `S` once the test has seen the call park for its
    // first output, which it does only after the job opened, so `S` is
    // `job_delta` text and the only chunk.
    // One byte goes out in one `job_delta`, so no second chunk can arm
    // the pacing hold: the loop's next park after the send is the timeout.
    let body = format!(
        "mkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\nprintf S\nread x\n",
        block = quote(&block),
        ready = quote(ready.path()),
    );
    let running = start_tty(
        dir.path().to_path_buf(),
        on_terminal(ready.path(), &body),
        Some(5_000),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    assert!(running.clock.await_parked(
        running.start + Duration::from_millis(250),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    let block = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&block);
        let _sent = released.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("waited for the command's read of the block fifo");
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_millis(250)),
        Some(running.call),
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    // The send carried the only byte, so the loop is out of output.
    assert!(
        jobs.deltas()
            .wait_for_text("S", fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the byte never went out in a job_delta: {:?}",
        jobs.deltas().text()
    );
    let timeout_at = running.start + Duration::from_secs(5);
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance_when_parked(
        Some(timeout_at),
        None,
        Duration::from_millis(4_750),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to time out");
    assert_eq!(ended.status, Outcome::Failed);
    assert_eq!(
        ended.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Timeout)
    );
    assert!(ended.process.as_ref().unwrap().timed_out);
    assert!(!group_alive(pgid));
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    running.clock.advance_when_parked(
        Some(running.start + Duration::from_millis(250)),
        Some(running.call),
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
        clock.await_parked(
            clock.origin() + Duration::from_millis(10),
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the write did not wait for room"
    );
    assert!(matches!(written.try_recv(), Err(mpsc::TryRecvError::Empty)));
    cancel.cancel();
    let count = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&written)
        .expect("the cancelled write to return")
        .unwrap();
    assert!(count < length, "the whole input fit: {count}");
    jobs.stop(&job.job_id);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the stop to end the job");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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

/// The limit the monitor tests give, so it never interferes.
const LONG_DEADLINE_MS: u64 = 1_800_000;

fn start_monitor(
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
    arguments.insert("monitor".into(), json!(true));
    if let Some(timeout_ms) = timeout_ms {
        arguments.insert("timeout_ms".into(), json!(timeout_ms));
    }
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let (tx, rx) = mpsc::channel();
    let call = thread::spawn(move || {
        tx.send(run_prepared(
            &shell,
            &arguments,
            &CancelToken::new(),
            tapped.as_ref(),
        ))
        .unwrap();
    })
    .thread()
    .id();
    JobRun {
        call,
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
#[track_caller]
fn open_feed(ready: &Path) -> std::fs::File {
    let feed = block_of(ready);
    let (opened, open) = mpsc::channel();
    thread::spawn(move || {
        let _sent = opened.send(std::fs::OpenOptions::new().write(true).open(feed).unwrap());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&open)
        .expect("waited for the command's read of the feed fifo")
}

/// Every step in order: the drive thread offers what a delta carried before
/// it parks again, so the next step's park means the line was offered at the
/// step's instant. One [`STEPS`] bound for the whole sequence.
#[track_caller]
fn print_steps(
    run: &JobRun,
    deadline: Instant,
    feed: std::fs::File,
    steps: Vec<(Duration, String)>,
) -> std::fs::File {
    let clock = Arc::clone(&run.clock);
    let jobs = Arc::clone(&run.jobs);
    fakes::within("monitor steps", Deadline::after(STEPS), move || {
        let mut feed = feed;
        for (step, line) in steps {
            clock.advance_when_parked(
                Some(deadline),
                None,
                step,
                &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            );
            // One write: `writeln!` can split the newline into its own write, and
            // a delta of the line alone would hold the newline past the step.
            feed.write_all(format!("{line}\n").as_bytes()).unwrap();
            assert!(
                jobs.deltas().wait_for_text(
                    &format!("{line}\n"),
                    fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
                ),
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
    assert!(running.clock.await_parked(
        running.start + Duration::from_millis(300_000),
        fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
    ));
    jobs.stop(&job.job_id);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the stop to end the monitor");
    assert_eq!(ended.status, Outcome::Cancelled);
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("the release");
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed, "{ended:?}");
    let texts: Vec<String> = jobs
        .lines()
        .lines()
        .into_iter()
        .map(|line| line.lines)
        .collect();
    assert_eq!(texts, ["out"]);
    assert!(errors.is_dir());
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn a_monitor_with_a_zero_deadline_times_out_in_the_foreground() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-zero");
    let jobs = FakeJobs::new(dir.path());
    let shell =
        Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_jobs(jobs.clone()));
    let mut arguments = args("echo hi");
    arguments.insert("monitor".into(), json!(true));
    arguments.insert("timeout_ms".into(), json!(0));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let output = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let job = started(&output);
    let path = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&path);
        let _sent = released.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("the release");
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the monitor to end");
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let path = ready.path().to_path_buf();
    let (released, release) = mpsc::channel();
    thread::spawn(move || {
        release_block(&path);
        let _sent = released.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&release)
        .expect("the release");
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the monitor to end");
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let deadline = running.start + Duration::from_millis(LONG_DEADLINE_MS);
    let feed = open_feed(ready.path());
    // Twelve deliveries 100 ms apart: ten spend the budget, two are dropped.
    let steps: Vec<(Duration, String)> = (1..=12)
        .map(|k| (Duration::from_millis(100), format!("line{k}")))
        .collect();
    let feed = print_steps(&running, deadline, feed, steps);
    drop(feed);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the monitor to end");
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let deadline = running.start + Duration::from_millis(LONG_DEADLINE_MS);
    let feed = open_feed(ready.path());
    // One delivery every 500 ms, faster than the refill: the budget runs
    // out at the 14th (7 s), so the drop at the 74th (37 s) floods.
    let steps: Vec<(Duration, String)> = (1..=73)
        .map(|k| (Duration::from_millis(500), format!("line{k}")))
        .collect();
    let feed = print_steps(&running, deadline, feed, steps);
    assert!(
        jobs.ended(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "flooded early"
    );
    assert!(group_alive(pgid));
    let feed = print_steps(
        &running,
        deadline,
        feed,
        vec![(Duration::from_millis(500), "line74".to_owned())],
    );
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the flood to stop the monitor");
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
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
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
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
    assert!(
        jobs.ended(fakes::Deadline::after(Duration::ZERO)).is_none(),
        "the monitor flooded"
    );
    drop(feed);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the monitor to end");
    assert_eq!(ended.status, Outcome::Completed, "{ended:?}");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

#[test]
fn a_monitor_ends_at_its_deadline_as_a_timeout() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-deadline");
    let ready = Ready::new(dir.path());
    let jobs = FakeJobs::new(dir.path());
    let running = start_monitor(
        dir.path().to_path_buf(),
        blocking(ready.path(), "partial", "later"),
        Some(1_000),
        Arc::clone(&jobs),
    );
    let pgid = ready.wait(fakes::Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let _receipt = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.output)
        .expect("the receipt");
    let due = running.start + Duration::from_millis(1_000);
    assert!(
        running
            .clock
            .await_parked(due, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
    );
    assert!(group_alive(pgid), "stopped before the deadline");
    running.clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_millis(1_000),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the deadline to end the monitor");
    assert_eq!(ended.status, Outcome::Failed);
    let error = ended.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(
        error.message,
        "The monitor's deadline of 1000 ms passed; start it again if you still need it."
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
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

/// One shell call with `artifacts`, on its own thread under the deadline.
#[track_caller]
fn run_spilling(dir: &Path, artifacts: &Path, command: &str) -> contract::tool::Output {
    let shell = Arc::new(
        Shell::new(dir.to_path_buf(), FakeClock::new()).with_artifacts(artifacts.to_path_buf()),
    );
    run_on(&shell, args(command), &Arc::new(Recorder::default()))
}

#[test]
fn output_at_a_mebibyte_is_whole_with_no_spill() {
    let dir = fakes::TempDir::new("fiber-shell-public-whole");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let output = run_spilling(
        dir.path(),
        &artifacts,
        "head -c 1048576 /dev/zero | tr '\\0' a",
    );
    assert!(output.error.is_none());
    assert!(output.spilled.is_none());
    let text = text(&output);
    assert_eq!(text.len(), 1_048_576 + 14);
    assert_eq!(&text[..100], &"a".repeat(100));
    assert!(text.ends_with("\nExit code 0.\n"));
    assert!(std::fs::read_dir(&artifacts).unwrap().next().is_none());
}

#[test]
fn output_past_a_mebibyte_spills_into_the_call_artifact_file() {
    let dir = fakes::TempDir::new("fiber-shell-public-spill");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let output = run_spilling(
        dir.path(),
        &artifacts,
        "head -c 1048577 /dev/zero | tr '\\0' a",
    );
    assert!(output.error.is_none());
    assert!(output.content.is_empty());
    let spilled = output.spilled.expect("spilled is set");
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert_eq!(kept.size, 1_048_577 + 14);
    assert_eq!(std::fs::read(&kept.path).unwrap().len(), 1_048_577 + 14);
    let entries: Vec<_> = std::fs::read_dir(&artifacts).unwrap().collect();
    assert_eq!(entries.len(), 1);
    assert!(kept.path.starts_with(&artifacts));
}

#[test]
fn a_shell_without_artifacts_never_spills() {
    let dir = fakes::TempDir::new("fiber-shell-public-no-spill");
    let output = run(dir.path(), "head -c 1048577 /dev/zero | tr '\\0' a");
    assert!(output.error.is_none());
    assert!(output.spilled.is_none());
    assert_eq!(text(&output).len(), 1_048_577 + 14);
}

#[test]
fn a_relative_artifacts_directory_spills_under_its_absolute_form() {
    let dir = fakes::TempDir::new("fiber-shell-public-relative");
    // Test-owned and unique: created under the process's directory, which
    // the relative path resolves against, and removed on drop, so no
    // concurrent run shares it and no failure leaks it.
    let owned = fakes::TempDir::under(
        &std::env::current_dir().unwrap(),
        "fiber-shell-public-relative-artifacts",
    );
    let name: PathBuf = owned.path().file_name().unwrap().into();
    let shell =
        Arc::new(Shell::new(dir.path().to_path_buf(), FakeClock::new()).with_artifacts(name));
    let output = run_on(
        &shell,
        args("head -c 1048577 /dev/zero | tr '\\0' a"),
        &Arc::new(Recorder::default()),
    );
    assert!(output.error.is_none());
    let spilled = output.spilled.expect("spilled is set");
    let kept = spilled.file.as_ref().expect("the file is kept");
    assert!(kept.path.is_absolute());
    assert!(kept.path.starts_with(owned.path()));
}

fn configured_shell(dir: &Path, entries: Vec<(String, Vec<String>)>) -> Shell {
    Shell::new(dir.to_path_buf(), FakeClock::new()).with_read_only(Arc::new(Mutex::new(entries)))
}

fn effects_of(shell: &Shell, command: &str) -> contract::tool::Effects {
    shell.effects(&args(command)).expect("effects")
}

/// The destructive-command guard (`docs/tools.md`, "What a tool declares"):
/// the shell declares `review_when_confined` when any component of a
/// multi-part command holds a built-in destructive form, matched by command
/// text at any word offset within the component, so a wrapper such as
/// `sudo` does not hide it. Each of the six forms stands alone below so
/// deleting one matcher predicate fails this test.
#[test]
fn the_destructive_guard_matches_each_form_in_every_placement() {
    let dir = fakes::TempDir::new("fiber-shell-destructive-guard");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = Shell::new(workspace, FakeClock::new());
    let guarded = [
        "rm -r tmp",
        "rm -rf tmp",
        "git reset --hard",
        "git clean -fd",
        "git checkout -- README.md",
        "git restore README.md",
    ];
    let mut cases: Vec<String> = Vec::new();
    for form in guarded {
        cases.push(form.to_owned());
        for sep in ["&&", "||", ";", "|"] {
            cases.push(format!("{form} {sep} echo hi"));
            cases.push(format!("echo hi {sep} {form} {sep} echo hi"));
            cases.push(format!("echo hi {sep} {form}"));
        }
    }
    cases.push("/usr/bin/rm -rf tmp".to_owned());
    cases.push("/usr/bin/git reset --hard".to_owned());
    cases.push("/usr/bin/git clean -fd && echo hi".to_owned());
    // A wrapper before the form does not hide it: each form guarded above
    // stays guarded under `sudo`, standalone and in multi-part placements.
    let wrapped = [
        "sudo rm -r tmp",
        "sudo rm -rf tmp",
        "sudo git reset --hard",
        "sudo git clean -fd",
        "sudo git checkout -- README.md",
        "sudo git restore README.md",
    ];
    for form in wrapped {
        cases.push(form.to_owned());
        cases.push(format!("echo hi && {form}"));
        cases.push(format!("{form} && echo hi"));
    }
    cases.push("sudo /usr/bin/rm -rf tmp".to_owned());
    cases.push("echo hi && sudo /usr/bin/git clean -fd".to_owned());
    // Long and quoted spellings take the same matcher paths: `--recursive`
    // is the long form of `-r`, and quotes around the command name do not
    // hide it.
    cases.push("rm --recursive tmp".to_owned());
    cases.push("'rm' -rf tmp".to_owned());
    cases.push("'git' clean -fd".to_owned());
    // Best-effort text matching over-matches: a quoted or echoed form still
    // counts, since only review is at stake.
    cases.push("echo rm -rf tmp".to_owned());
    cases.push("echo \"rm -rf tmp\"".to_owned());
    for command in cases {
        let effects = effects_of(&shell, &command);
        assert!(
            effects.review_when_confined,
            "a guarded component stays reviewed: {command}"
        );
        assert!(
            !effects.always_reviewed,
            "the guard is not a full review: {command}"
        );
    }
}

/// Near-misses and benign commands declare no guard: the flag stays false
/// and the call keeps its ordinary `always_reviewed` answer.
#[test]
fn the_destructive_guard_ignores_near_misses() {
    let dir = fakes::TempDir::new("fiber-shell-destructive-guard-benign");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = Shell::new(workspace, FakeClock::new());
    for command in [
        "rm tmp",
        "rm -f tmp",
        "git reset",
        "git reset --soft HEAD",
        "git checkout main",
        "git checkout -b feature",
        "git status",
        "echo hi",
    ] {
        let effects = effects_of(&shell, command);
        assert!(
            !effects.review_when_confined,
            "no guarded form appears: {command}"
        );
        assert!(
            !effects.always_reviewed,
            "the guard changes nothing benign: {command}"
        );
    }
}

/// The guard reads the raw command text, so it holds where the parser
/// gives up: a newline the recogniser refuses, an opaque grouped stage,
/// and a command past the parser's length limit.
#[test]
fn the_destructive_guard_covers_text_the_parser_rejects() {
    let dir = fakes::TempDir::new("fiber-shell-destructive-guard-raw");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = Shell::new(workspace, FakeClock::new());
    let long = format!("{} && git restore f", "true ".repeat(3_000));
    assert!(
        long.len() > 10_000,
        "the long case stays past the parser limit"
    );
    for command in [
        "echo hi\ngit clean -fd".to_owned(),
        "true && (git clean -fd)".to_owned(),
        long,
    ] {
        let effects = effects_of(&shell, &command);
        assert!(
            effects.review_when_confined,
            "raw text still guards: {command:?}"
        );
        assert!(
            !effects.always_reviewed,
            "the guard is not a full review: {command:?}"
        );
    }
}

fn jq_r() -> Vec<(String, Vec<String>)> {
    vec![("jq".to_owned(), vec!["-r".to_owned()])]
}

#[test]
fn a_shell_with_a_read_only_entry_reads_a_listed_command() {
    let dir = fakes::TempDir::new("fiber-shell-read-only");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = configured_shell(&workspace, jq_r());
    let effects = effects_of(&shell, "jq -r . f");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads]
    );
    assert!(effects.declared.reversible);
    assert_eq!(
        effects.declared.paths,
        Some(vec![
            workspace.join(".").to_str().unwrap().to_owned(),
            workspace.join("f").to_str().unwrap().to_owned(),
        ])
    );
}

#[test]
fn a_plain_shell_executes_what_a_configured_shell_reads() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-default");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let plain = Shell::new(workspace.clone(), FakeClock::new());
    let effects = effects_of(&plain, "jq -r . f");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
    assert!(!effects.declared.reversible);
    assert!(effects.declared.paths.is_none());
    // An unlisted flag executes on the configured instance too.
    let configured = configured_shell(&workspace, jq_r());
    let effects = effects_of(&configured, "jq -c . f");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
    assert!(effects.declared.paths.is_none());
}

#[test]
fn a_shell_reads_the_new_git_forms_with_no_paths() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-git");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = Shell::new(workspace, FakeClock::new());
    for command in [
        "git status",
        "git stash list",
        "git branch --show-current",
        "LC_ALL=C git status",
    ] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Reads],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
    // A writing form executes: `git init` creates or updates `.git` state,
    // `git push --dry-run` reaches the remote, and a leading assignment
    // with an unlisted name (#1497).
    for command in ["git init", "git push --dry-run", "FOO=1 ls"] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
    // A discard redirect or an input file reads.
    let effects = effects_of(&shell, "ls 2>/dev/null");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads]
    );
}

#[test]
fn a_shell_reads_a_cd_call_with_paths_past_the_join() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-cd");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(workspace.join("sub")).unwrap();
    std::fs::write(workspace.join("sub").join("n.md"), "The note.\n").unwrap();
    std::fs::write(workspace.join("f"), "Top.\n").unwrap();
    let shell = Shell::new(workspace.clone(), FakeClock::new());
    let sub = workspace.join("sub").to_str().unwrap().to_owned();
    // A `cd` is a read-only step: later paths resolve past the join, and
    // an operand-less command declares each directory it may run in. The
    // doc promises which directories, not their order.
    for (command, paths) in [
        (
            "cd sub && cat n.md 2>/dev/null",
            vec![sub.clone(), format!("{sub}/n.md")],
        ),
        (
            "cd sub ; ls",
            vec![sub.clone(), workspace.to_str().unwrap().to_owned()],
        ),
        (
            "cd sub | cat f",
            vec![
                sub.clone(),
                workspace.join("f").to_str().unwrap().to_owned(),
            ],
        ),
        ("git status && cd sub", vec![sub.clone()]),
    ] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Reads],
            "{command}"
        );
        let declared: BTreeSet<&str> = effects
            .declared
            .paths
            .as_ref()
            .map(|paths| paths.iter().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            declared,
            BTreeSet::from_iter(paths.iter().map(String::as_str)),
            "{command}"
        );
    }
    // A `git` after any `cd`, and a `cd` outside the allow-list, execute.
    for command in [
        "cd sub && git status",
        "cd sub ; git status",
        "cd sub | git status",
        "cd",
        "cd -",
        "cd -P",
        "cd a b",
        "timeout 5 cd sub",
    ] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn a_shell_reads_wrapper_calls_with_paths_from_the_inner_command() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-wrap");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("f"), "Top.\n").unwrap();
    let shell = Shell::new(workspace.clone(), FakeClock::new());
    let file = workspace.join("f").to_str().unwrap().to_owned();
    // A wrapper is classified by the command it runs: an assignment for
    // a listed name, `env` with listed assignments, and `timeout` with a
    // duration all read, declaring the inner command's paths. A
    // path-reading `xargs` target reads only on proven static stdin, so
    // names from command output stay reviewed (fail-closed).
    for command in ["LC_ALL=C cat f", "env LC_ALL=C cat f", "timeout 5 cat f"] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Reads],
            "{command}"
        );
        assert!(effects.declared.reversible, "{command}");
    }
    assert_eq!(
        effects_of(&shell, "LC_ALL=C cat f").declared.paths,
        Some(vec![file.clone()])
    );
    // Any other assignment name, a flag of `env`'s own, a refused
    // duration, an `xargs` target outside its six commands, and an
    // `xargs` reader fed names from command output execute.
    for command in [
        "FOO=1 cat f",
        "env PATH=/x cat f",
        "env -i cat f",
        "timeout -k 1 5 cat f",
        "timeout 5x cat f",
        "xargs rm f",
        "xargs /usr/bin/wc -l f",
        "xargs grep -R x f",
        "git ls-files | xargs wc -l",
    ] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
}

#[test]
fn one_shells_list_does_not_leak_into_another() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-isolation");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let first = configured_shell(&workspace, jq_r());
    let second = configured_shell(&workspace, vec![("yq".to_owned(), Vec::new())]);
    assert_eq!(
        effects_of(&first, "yq . f").declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
    assert_eq!(
        effects_of(&second, "jq -r . f").declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
}

#[test]
fn replacing_the_handles_contents_reaches_the_shell_and_its_clone() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-replace");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let handle = Arc::new(Mutex::new(jq_r()));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_read_only(Arc::clone(&handle));
    let clone = shell.clone();
    *handle.lock().unwrap() = vec![("yq".to_owned(), Vec::new())];
    // The same shell and its clone both answer from the replaced list.
    for shell in [&shell, &clone] {
        assert_eq!(
            effects_of(shell, "yq . f").declared.effects,
            vec![contract::shapes::Effect::Reads]
        );
        assert_eq!(
            effects_of(shell, "jq -r . f").declared.effects,
            vec![contract::shapes::Effect::Executes]
        );
    }
}

#[test]
fn a_builtin_basename_stays_closed_through_the_public_path() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-basename");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let shell = configured_shell(
        &workspace,
        vec![("/usr/bin/grep".to_owned(), vec!["-R".to_owned()])],
    );
    let effects = effects_of(&shell, "/usr/bin/grep -R x ws");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
    assert!(effects.declared.paths.is_none());
}

#[test]
fn a_capped_clone_shares_the_read_only_handle() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-cap");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let handle = Arc::new(Mutex::new(jq_r()));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_read_only(Arc::clone(&handle));
    let capped = shell.with_cap(1024).expect("a cap");
    // The capped clone the loop holds answers from the same handle.
    let effects = capped.effects(&args("jq -r . f")).expect("effects");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads]
    );
    // A later replacement reaches the capped clone too: a copy in
    // `with_cap` would keep answering from the old list.
    *handle.lock().unwrap() = Vec::new();
    let effects = capped.effects(&args("jq -r . f")).expect("effects");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Executes]
    );
}

#[test]
fn a_shell_guards_git_reads_by_repository() {
    let dir = fakes::TempDir::new("fiber-shell-read-only-repo");
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(workspace.join(".git")).unwrap();
    std::fs::write(workspace.join("f"), "Top.\n").unwrap();
    let shell = Shell::new(workspace.clone(), FakeClock::new());
    // A normal repository reads, declaring operand paths, and the
    // repository-independent `gh` form declares the default host.
    for command in ["git status", "git log --oneline f"] {
        let effects = effects_of(&shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Reads],
            "{command}"
        );
    }
    let effects = effects_of(&shell, "gh pr view 123");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Network]
    );
    assert_eq!(
        effects.hosts,
        Some(vec!["https://api.github.com/".to_owned()])
    );
    assert_eq!(
        effects_of(&shell, "git log --oneline f").declared.paths,
        Some(vec![workspace.join("f").to_str().unwrap().to_owned()])
    );
    // A bare repository executes.
    let bare = dir.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::create_dir_all(bare.join("objects")).unwrap();
    std::fs::create_dir_all(bare.join("refs")).unwrap();
    let bare_shell = Shell::new(bare.clone(), FakeClock::new());
    for command in ["git status", "git log --oneline"] {
        let effects = effects_of(&bare_shell, command);
        assert_eq!(
            effects.declared.effects,
            vec![contract::shapes::Effect::Executes],
            "{command}"
        );
        assert!(effects.declared.paths.is_none(), "{command}");
    }
    // A worktree checkout whose `.git` file names a target under another
    // `.git/` reads.
    let checkout = dir.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    let target = workspace.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", target.to_str().unwrap()),
    )
    .unwrap();
    let checkout_shell = Shell::new(checkout.clone(), FakeClock::new());
    let effects = effects_of(&checkout_shell, "git status");
    assert_eq!(
        effects.declared.effects,
        vec![contract::shapes::Effect::Reads]
    );
}

fn sandbox_settings_for_integration() -> (sandbox_home::SandboxHome, tools::SandboxSettings) {
    let home = sandbox_home::SandboxHome::protected();
    let settings = tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: home.fiber_home().to_path_buf(),
    };
    (home, settings)
}

#[test]
fn a_confined_shell_runs_and_marks_its_effects() {
    let dir = fakes::TempDir::new("fiber-tools-shell-sandbox-run");
    let workspace = dir.path().to_path_buf();
    let (_home, settings) = sandbox_settings_for_integration();
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(settings));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox);
    let mut args = Map::new();
    args.insert("command".into(), Value::String("echo hi".into()));
    let effects = shell.effects(&args).unwrap();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(effects.declared.confined);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert!(!effects.declared.confined);
    let output = run_prepared(&shell, &args, &CancelToken::new(), &Recorder::default());
    assert!(output.error.is_none());
    assert!(text(&output).contains("hi"));
}

/// Guarded and unguarded confined effects side by side: both declare
/// confined, and only the destructive form carries the guard, so the
/// loop's fast path takes one and reviews the other.
#[test]
fn confined_effects_mark_the_guard_only_on_destructive_forms() {
    let dir = fakes::TempDir::new("fiber-tools-shell-confined-guard-forms");
    let workspace = dir.path().to_path_buf();
    let (_home, settings) = sandbox_settings_for_integration();
    let shell =
        Shell::new(workspace, FakeClock::new()).with_sandbox(Arc::new(Mutex::new(settings)));
    let guarded = shell
        .effects(&args("rm -rf tmp"))
        .expect("the guarded command classifies");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(
        guarded.declared.confined,
        "the sandbox still confines the guarded call"
    );
    assert!(
        guarded.review_when_confined,
        "the destructive form carries the guard"
    );
    let plain = shell
        .effects(&args("echo hi"))
        .expect("the plain command classifies");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(
        plain.declared.confined,
        "the sandbox confines the plain call"
    );
    assert!(
        !plain.review_when_confined,
        "the plain form carries no guard"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_confined_shell_restores_protected_checkout_paths_and_reports_them() {
    let root = fakes::TempDir::new("fiber-shell-protected-paths");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fakes::git::git(&workspace, &["init", "-q"]);
    // A valid config the discovery reads: the baseline records the shared
    // config and hooks through git, never as a fallback directory.
    fakes::git::git(&workspace, &["config", "fiber.probe", "config baseline"]);
    fakes::git::git(
        &workspace,
        &["commit", "-q", "--allow-empty", "-m", "baseline"],
    );
    let linked = root.path().join("linked");
    let linked_arg = linked.to_string_lossy().into_owned();
    fakes::git::git(&workspace, &["worktree", "add", "--detach", &linked_arg]);
    std::fs::create_dir_all(workspace.join(".git/hooks")).unwrap();
    std::fs::create_dir_all(workspace.join(".fiber")).unwrap();
    std::fs::create_dir_all(linked.join(".fiber")).unwrap();
    std::fs::write(workspace.join(".git/hooks/pre-commit"), "hook baseline\n").unwrap();
    std::fs::write(workspace.join(".fiber/state"), "fiber baseline\n").unwrap();
    std::fs::write(linked.join(".fiber/state"), "linked fiber baseline\n").unwrap();
    std::fs::write(linked.join("mutation.txt"), "gitdir: /evil\n").unwrap();
    let linked_pointer = std::fs::read(linked.join(".git")).unwrap();
    let (home, sandbox) = sandbox_settings_for_integration();
    let fiber_home = home.fiber_home().to_path_buf();
    let (protection, recovery) =
        tools::ProtectedPaths::open(&fiber_home, &workspace).expect("the project baseline opens");
    assert!(
        recovery.is_none(),
        "the first session has no stale baseline"
    );
    let shell = Shell::new(workspace.clone(), FakeClock::new())
        .with_sandbox(Arc::new(Mutex::new(sandbox)))
        .with_protection(protection);

    let arguments = args(
        "tee .git/config .git/hooks/pre-commit .fiber/state ../linked/.git ../linked/.fiber/state < ../linked/mutation.txt",
    );
    assert!(
        shell.effects(&arguments).unwrap().declared.confined,
        "the test needs a confined Linux call"
    );
    let output = run_prepared(
        &shell,
        &arguments,
        &CancelToken::new(),
        &Recorder::default(),
    );

    assert!(
        output.error.is_none(),
        "the command completed: {}",
        text(&output)
    );
    assert!(
        std::fs::read_to_string(workspace.join(".git/config"))
            .unwrap()
            .contains("config baseline"),
        "the shared config is restored"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".git/hooks/pre-commit")).unwrap(),
        "hook baseline\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".fiber/state")).unwrap(),
        "fiber baseline\n"
    );
    assert_eq!(std::fs::read(linked.join(".git")).unwrap(), linked_pointer);
    assert_eq!(
        std::fs::read_to_string(linked.join(".fiber/state")).unwrap(),
        "linked fiber baseline\n"
    );
    let report = output
        .protected_paths
        .as_ref()
        .expect("the restore is in the result");
    // `docs/sandbox.md:64-73` promises restoration and reporting for the
    // shared Git paths and every registered worktree's pointer and `.fiber/`;
    // `docs/events.md:683` defines restored paths as workspace-relative.
    let restored: BTreeSet<&str> = report.restored.iter().map(String::as_str).collect();
    assert_eq!(
        restored,
        BTreeSet::from_iter([
            ".git/config",
            ".git/hooks",
            ".fiber",
            "../linked/.git",
            "../linked/.fiber",
        ])
    );
    // `docs/sandbox.md:100-102` promises an unsuccessful restore appears in
    // the report's failed list.
    assert!(report.failed.is_empty());
    // `docs/events.md:697-701` carries warning lines from the report into the
    // model result, and `docs/sandbox.md:64-73` includes `.git/config`.
    assert!(text(&output).contains(".git/config"));
}

#[test]
fn an_unavailable_sandbox_does_not_declare_the_shell_confined() {
    let dir = fakes::TempDir::new("fiber-tools-shell-sandbox-fail");
    let workspace = dir.path().to_path_buf();
    let home_root = fakes::TempDir::new("fiber-tools-shell-sandbox-protected");
    let fiber_home = home_root.path().join("fiber-home");
    assert!(fiber_home.starts_with(std::env::temp_dir()));
    assert!(fiber_home.starts_with(home_root.path()));
    let sandbox = std::sync::Arc::new(std::sync::Mutex::new(tools::SandboxSettings {
        enabled: true,
        allow_write: vec![workspace.join("missing-write")],
        deny_read: Vec::new(),
        fiber_home,
    }));
    let shell = Shell::new(workspace, FakeClock::new()).with_sandbox(sandbox);
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");

    assert!(
        !effects.declared.confined,
        "an unavailable sandbox leaves the call on the reviewed path"
    );
}

/// Through the loop's seam, an unconfined call reads what a confined call
/// is refused: the declaration leaves the sandbox and the run follows it.
#[test]
fn an_unconfined_call_reads_what_a_confined_call_is_refused() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let secret = root.path().join("secret");
    std::fs::write(&secret, "shh").unwrap();
    let sandbox = Arc::new(Mutex::new(tools::SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: vec![secret.clone()],
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox);
    // The confined call is refused the denied file.
    let refused_args = args(&format!("cat {}", quote(&secret)));
    let refused_effects = shell
        .effects(&refused_args)
        .expect("the shell command classifies");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(refused_effects.declared.confined);
    let refused = run_prepared(
        &shell,
        &refused_args,
        &CancelToken::new(),
        &Recorder::default(),
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        assert!(
            refused.error.is_some(),
            "a confined read of the denied file fails"
        );
        assert!(
            !text(&refused).contains("shh"),
            "no denied byte reaches the caller"
        );
    }
    // The explicit unconfined call declares reviewed and reads it.
    let mut unconfined_args = args(&format!("cat {}", quote(&secret)));
    unconfined_args.insert("unconfined".into(), json!(true));
    let unconfined_effects = shell
        .effects(&unconfined_args)
        .expect("the shell command classifies");
    assert!(
        !unconfined_effects.declared.confined,
        "an unconfined call is reviewed as with the sandbox off"
    );
    let read = run_prepared(
        &shell,
        &unconfined_args,
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(
        read.error.is_none(),
        "an unconfined read succeeds: {}",
        text(&read)
    );
    assert!(
        text(&read).contains("shh"),
        "the denied file stays readable outside the sandbox"
    );
}

/// A confined spawn failure fails once: the call ends with the sandbox
/// error and its command never runs, so no unconfined retry happens.
#[test]
fn a_confined_spawn_failure_fails_once_without_an_unconfined_retry() {
    let dir = fakes::TempDir::new("fiber-tools-shell-fail-once");
    let workspace = dir.path().to_path_buf();
    let (_home, settings) = sandbox_settings_for_integration();
    let sandbox = Arc::new(Mutex::new(settings));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
    let marker = workspace.join("marker");
    let command = format!("touch {} && echo hi", quote(&marker));
    let effects = shell
        .effects(&args(&command))
        .expect("the shell command classifies");
    assert!(
        effects.declared.confined,
        "the failed call still declares confined: the start carries it"
    );
    // The reload turns the sandbox off between the declaration and the run.
    sandbox.lock().unwrap().enabled = false;
    let output = shell.run_with_effects(
        &args(&command),
        &effects,
        &CancelToken::new(),
        &Recorder::default(),
        &NoAsk,
    );
    assert_eq!(
        code(&output),
        Some(ErrorCode::ToolError),
        "the failed spawn fails with a tool error"
    );
    assert!(
        text(&output).contains("The sandbox could not be applied"),
        "the failure names the sandbox: {}",
        text(&output)
    );
    assert!(
        text(&output).contains("the sandbox is off"),
        "the failure names the cause: {}",
        text(&output)
    );
    assert!(
        !marker.exists(),
        "the failed call ran nothing once, and never reran unconfined"
    );
}

/// A confined shell on macOS: an enabled sandbox with no overlap. The
/// fixture root sits under the owner's home, never the temp dir, and the
/// deny sits beside the workspace.
#[cfg(target_os = "macos")]
struct MacosConfined {
    root: fakes::TempDir,
    workspace: PathBuf,
    shell: Shell,
}

#[cfg(target_os = "macos")]
impl MacosConfined {
    fn new(deny: &[&str]) -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let sandbox = Arc::new(Mutex::new(SandboxSettings {
            enabled: true,
            allow_write: Vec::new(),
            deny_read: deny.iter().map(|name| root.path().join(name)).collect(),
            fiber_home: root.path().join("fiber-home"),
        }));
        let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
        Self {
            root,
            workspace,
            shell,
        }
    }

    fn run(&self, command: &str) -> contract::tool::Output {
        let shell = self.shell.clone();
        let command = command.to_owned();
        let what = format!("confined command {command:?}");
        fakes::within(
            &what,
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                run_prepared(
                    &shell,
                    &args(&command),
                    &CancelToken::new(),
                    &Recorder::default(),
                )
            },
        )
    }
}

/// Writes stay inside the workspace, reads of the denied file fail, and a
/// sibling reads fine. Every output carries user bytes only: the marker
/// never reaches the caller.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_shell_enforces_read_write_boundaries() {
    let confined = MacosConfined::new(&["secret"]);
    std::fs::write(confined.root.path().join("secret"), "shh").unwrap();
    std::fs::write(confined.root.path().join("sibling"), "ok").unwrap();
    let effects = confined
        .shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let wrote = confined.run("echo hi > hello");
    assert!(wrote.error.is_none(), "a workspace write succeeds");
    assert!(
        !text(&wrote).contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );
    assert_eq!(
        std::fs::read_to_string(confined.workspace.join("hello")).unwrap(),
        "hi\n"
    );

    let outside = confined.root.path().join("outside");
    let escaped = confined.run(&format!("echo evil > {}", quote(&outside)));
    assert!(escaped.error.is_some(), "a write outside the grants fails");
    assert!(!outside.exists(), "nothing escaped the sandbox");

    let unread = confined.run(&format!(
        "cat {}",
        quote(&confined.root.path().join("secret"))
    ));
    assert!(unread.error.is_some(), "a denied read fails");
    assert!(
        !text(&unread).contains("shh"),
        "no protected byte reaches the caller"
    );

    let sibling = confined.run(&format!(
        "cat {}",
        quote(&confined.root.path().join("sibling"))
    ));
    assert!(sibling.error.is_none(), "a sibling reads fine");
    assert!(text(&sibling).contains("ok"));
}

/// Symlink, `..` and child-process escapes stay inside the grants: a link
/// to an outside file, a `..` write and a grandchild `python3` write all
/// fail with nothing created outside, while an inside write succeeds.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_shell_blocks_symlink_and_child_process_escapes() {
    let confined = MacosConfined::new(&[]);
    let effects = confined
        .shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let inside = confined.run("echo ok > inside.txt");
    assert!(inside.error.is_none(), "an inside write succeeds");
    assert_eq!(
        std::fs::read_to_string(confined.workspace.join("inside.txt")).unwrap(),
        "ok\n"
    );

    let target = confined.root.path().join("escape-target");
    let linked = confined.run(&format!(
        "ln -s {} link && echo evil > link",
        quote(&target)
    ));
    assert!(linked.error.is_some(), "a symlink write outside fails");
    assert!(!target.exists(), "nothing escaped through the link");

    let dotdot = confined.root.path().join("escape-dotdot");
    let traversed = confined.run("echo evil > ../escape-dotdot");
    assert!(traversed.error.is_some(), "a `..` write outside fails");
    assert!(!dotdot.exists(), "nothing escaped through `..`");

    let py = confined.root.path().join("escape-py");
    let grandchild = confined.run(&format!(
        "python3 -c \"open('{}', 'w').write('evil')\"",
        py.display()
    ));
    assert!(
        grandchild.error.is_some(),
        "a grandchild write outside fails"
    );
    assert!(!py.exists(), "nothing escaped through the grandchild");
}

/// A configured credential file outside the workspace reads as denied
/// while its sibling reads fine: the deny travels as a binding, not as
/// profile text.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_shell_refuses_a_configured_credential_file() {
    let confined = MacosConfined::new(&["credential"]);
    std::fs::write(confined.root.path().join("credential"), "token-shh").unwrap();
    std::fs::write(confined.root.path().join("sibling"), "ok").unwrap();

    let denied = confined.run(&format!(
        "cat {}",
        quote(&confined.root.path().join("credential"))
    ));
    assert!(denied.error.is_some(), "the credential read fails");
    let denied_text = text(&denied);
    assert!(
        !denied_text.contains("token-shh"),
        "no credential byte reaches the caller"
    );
    assert!(
        !denied_text.contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );

    let sibling = confined.run(&format!(
        "cat {}",
        quote(&confined.root.path().join("sibling"))
    ));
    assert!(sibling.error.is_none(), "a sibling reads fine");
    assert!(text(&sibling).contains("ok"));
}

/// A normal checkout keeps committing while `.git/config`, hooks and
/// `.fiber/` stay unwritable.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_git_commit_works_in_a_checkout() {
    let confined = MacosConfined::new(&[]);
    let repo = &confined.workspace;
    fakes::git::git(repo, &["init", "-q"]);
    fakes::git::git(repo, &["config", "user.email", "confined@example.com"]);
    fakes::git::git(repo, &["config", "user.name", "Confined"]);
    std::fs::write(repo.join("file.txt"), "one\n").unwrap();
    fakes::git::git(repo, &["add", "file.txt"]);
    fakes::git::git(repo, &["commit", "-qm", "initial"]);

    let status = confined.run("GIT_CONFIG_GLOBAL=/dev/null git status --short");
    assert!(status.error.is_none(), "status runs confined");

    let wrote = confined.run("echo two >> file.txt");
    assert!(wrote.error.is_none(), "a tracked write succeeds");
    let commit = confined.run("GIT_CONFIG_GLOBAL=/dev/null git commit -qam confined");
    assert!(commit.error.is_none(), "a confined commit lands");
    let log = fakes::git::git(repo, &["log", "--format=%s"]);
    assert!(log.contains("confined"), "the commit is in the log");

    let checkout = confined.run("GIT_CONFIG_GLOBAL=/dev/null git checkout -qb branch");
    assert!(checkout.error.is_none(), "a confined branch lands");

    let config = confined.run("GIT_CONFIG_GLOBAL=/dev/null git config user.email evil@example.com");
    assert!(config.error.is_some(), "a config write fails");
    let config_file = std::fs::read_to_string(repo.join(".git").join("config")).unwrap();
    assert!(
        !config_file.contains("evil@example.com"),
        "the config is unchanged"
    );

    let hook = confined.run("echo hook > .git/hooks/pre-commit");
    assert!(hook.error.is_some(), "a hook write fails");
    assert!(
        !repo.join(".git").join("hooks").join("pre-commit").exists(),
        "no hook was installed"
    );

    let fiber = confined.run("mkdir -p .fiber && echo x > .fiber/x");
    assert!(fiber.error.is_some(), "the .fiber/ write fails");
    assert!(!repo.join(".fiber").exists(), "no .fiber/ was created");
}

/// A worktree commits through its outside gitdir while its `.git` pointer
/// file and the shared hooks and config stay unwritable.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_git_commit_works_in_a_worktree() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    fakes::git::git(&repo, &["init", "-q"]);
    fakes::git::git(&repo, &["config", "user.email", "confined@example.com"]);
    fakes::git::git(&repo, &["config", "user.name", "Confined"]);
    std::fs::write(repo.join("file.txt"), "one\n").unwrap();
    fakes::git::git(&repo, &["add", "file.txt"]);
    fakes::git::git(&repo, &["commit", "-qm", "initial"]);
    let worktree = root.path().join("wt");
    fakes::git::git(&repo, &["worktree", "add", &worktree.to_string_lossy()]);

    let sandbox = Arc::new(Mutex::new(SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(worktree.clone(), FakeClock::new()).with_sandbox(sandbox);
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let wrote = call(&shell, "echo two >> file.txt");
    assert!(wrote.error.is_none());
    let commit = call(
        &shell,
        "GIT_CONFIG_GLOBAL=/dev/null git commit -qam worktree",
    );
    assert!(
        commit.error.is_none(),
        "a worktree commit lands: {}",
        text(&commit)
    );
    let log = fakes::git::git(&worktree, &["log", "--format=%s"]);
    assert!(log.contains("worktree"), "the commit is in the log");

    let pointer = std::fs::read_to_string(worktree.join(".git")).unwrap();
    let rewrite = call(&shell, "echo 'gitdir: /evil' > .git");
    assert!(rewrite.error.is_some(), "the pointer rewrite fails");
    assert_eq!(
        std::fs::read_to_string(worktree.join(".git")).unwrap(),
        pointer,
        "the pointer is unchanged"
    );
    let moved = call(&shell, "echo 'gitdir: /evil' > newgit && mv newgit .git");
    assert!(moved.error.is_some(), "the pointer replace fails");
    assert_eq!(
        std::fs::read_to_string(worktree.join(".git")).unwrap(),
        pointer,
        "the pointer is still unchanged"
    );

    let config = call(
        &shell,
        "GIT_CONFIG_GLOBAL=/dev/null git config user.email evil@example.com",
    );
    assert!(config.error.is_some(), "the shared config write fails");

    let common_hooks = root
        .path()
        .join("repo")
        .join(".git")
        .join("hooks")
        .join("evil");
    let hook = call(&shell, &format!("echo hook > {}", quote(&common_hooks)));
    assert!(hook.error.is_some(), "the shared hook write fails");
    assert!(!common_hooks.exists(), "no shared hook was installed");
}

/// Unix sockets stay unreachable: a bind inside the workspace fails, and a
/// connect to a listener outside the grants — including Fiber's own
/// `FIBER_HOME/run/` socket path — fails too. `launchd` stays unreachable
/// as well.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_shell_cannot_reach_sockets() {
    use std::os::unix::net::UnixListener;
    let confined = MacosConfined::new(&[]);
    let effects = confined
        .shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let bound = confined.run(&format!(
        "printf 'attempting bind\\n'; python3 -c \"import socket; s = socket.socket(socket.AF_UNIX); s.bind('{}')\"",
        confined.workspace.join("x.sock").display()
    ));
    assert!(
        text(&bound).contains("attempting bind"),
        "the confined command reached bind"
    );
    assert!(bound.error.is_some(), "a unix bind fails");
    assert!(
        !confined.workspace.join("x.sock").exists(),
        "no socket file was created"
    );
    assert!(!text(&bound).contains("FIBER_SANDBOX_CONFINED"));

    let outside = confined.root.path().join("outside.sock");
    let _listener = UnixListener::bind(&outside).expect("the test binds its own socket");
    let connected = confined.run(&format!(
        "printf 'attempting outside connect\\n'; python3 -c \"import socket; s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect('{}')\"",
        outside.display()
    ));
    assert!(
        text(&connected).contains("attempting outside connect"),
        "the confined command reached connect"
    );
    assert!(connected.error.is_some(), "a unix connect fails");

    // Fiber's own `run/` sockets stay unreachable too: a listener under the
    // session's `FIBER_HOME/run/` answers nothing to a confined call.
    let run_dir = confined.root.path().join("fiber-home").join("run");
    std::fs::create_dir_all(&run_dir).expect("the test makes its run dir");
    let run_sock = run_dir.join("test.sock");
    let _run_listener = UnixListener::bind(&run_sock).expect("the test binds its own run socket");
    let run_connected = confined.run(&format!(
        "printf 'attempting run-socket connect\\n'; python3 -c \"import socket; s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect('{}')\"",
        run_sock.display()
    ));
    assert!(
        text(&run_connected).contains("attempting run-socket connect"),
        "the confined command reached the run-socket connect"
    );
    assert!(run_connected.error.is_some(), "a run-socket connect fails");
    assert!(
        !text(&run_connected).contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );

    let launchd = confined.run("launchctl list");
    assert!(launchd.error.is_some(), "launchd stays unreachable");
}

/// `ps` and `pgrep` stay unavailable under the sandbox, while `stty` on the
/// inherited pty keeps working: the single ioctl allow covers the pty the
/// shell holds, not process listing.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_ps_and_pgrep_fail() {
    let confined = MacosConfined::new(&[]);

    let listed = confined.run("ps -p 1");
    assert!(listed.error.is_some(), "ps fails confined");
    assert!(!text(&listed).contains("FIBER_SANDBOX_CONFINED"));

    let matched = confined.run("pgrep -f .");
    assert!(matched.error.is_some(), "pgrep fails confined");
}

/// A declared-confined call whose settings break before spawn never runs
/// unconfined: the pre-spawn check fails it closed, and the side-effect
/// command never runs.
#[cfg(target_os = "macos")]
#[test]
fn macos_declared_confined_call_failing_before_spawn_never_runs() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sandbox = Arc::new(Mutex::new(SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined, "the call is declared confined");

    // The settings break between the declaration and the spawn: a deny
    // lands inside the workspace, tripping the overlap rule.
    let denied = workspace.join("secret");
    std::fs::write(&denied, "shh").unwrap();
    sandbox.lock().unwrap().deny_read.push(denied);

    let side_effect = workspace.join("side-effect");
    // The loop's exact seam: the call runs with the earlier declaration,
    // not a fresh classification.
    let output = shell.run_with_effects(
        &args(&format!(
            "touch {} && echo CONFINED_USER_OUTPUT_XYZ",
            quote(&side_effect)
        )),
        &effects,
        &CancelToken::new(),
        &Recorder::default(),
        &NoAsk,
    );
    assert_eq!(
        code(&output),
        Some(ErrorCode::ToolError),
        "the broken declaration fails with a tool error"
    );
    assert!(
        text(&output).contains("The sandbox could not be applied"),
        "the failure names the sandbox: {}",
        text(&output)
    );
    assert!(
        !text(&output).contains("CONFINED_USER_OUTPUT_XYZ"),
        "no user output escapes the failed call: {}",
        text(&output)
    );
    assert!(!side_effect.exists(), "the side-effect command never ran");
}

/// Starts one confined call on its own thread, mirroring `start_jobs`
/// with the sandbox attached: background and tty ride the same seam.
#[cfg(target_os = "macos")]
fn start_confined(
    dir: PathBuf,
    command: &str,
    background: bool,
    tty: bool,
    jobs: Arc<FakeJobs>,
    sandbox: Arc<Mutex<SandboxSettings>>,
) -> JobRun {
    let clock = FakeClock::new();
    let start = clock.origin();
    let shell = Shell::new(dir, Arc::clone(&clock) as Arc<dyn contract::clock::Clock>)
        .with_jobs(jobs.clone())
        .with_sandbox(sandbox);
    let mut arguments = args(command);
    if background {
        arguments.insert("run_in_background".into(), json!(true));
    }
    if tty {
        arguments.insert("tty".into(), json!(true));
    }
    let recorder = Arc::new(Recorder::default());
    let tapped = Arc::clone(&recorder);
    let (tx, rx) = mpsc::channel();
    let call = thread::spawn(move || {
        tx.send(run_prepared(
            &shell,
            &arguments,
            &CancelToken::new(),
            tapped.as_ref(),
        ))
        .unwrap();
    })
    .thread()
    .id();
    JobRun {
        call,
        clock,
        start,
        output: rx,
        jobs,
        recorder,
    }
}

/// An immediate Background move hands the confined call to a job: the
/// receipt arrives within the deadline, and the job finishes with the
/// user's output intact and no marker byte. The receipt follows the contract
/// in `docs/tools.md`, "Background jobs".
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_background_call_moves_only_after_its_marker() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let jobs = FakeJobs::new(root.path());
    let sandbox = Arc::new(Mutex::new(SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let ready = Ready::new(&workspace);
    let command = blocking(ready.path(), "hi", "done");
    let running = start_confined(
        workspace.clone(),
        &command,
        true,
        false,
        jobs.clone(),
        sandbox,
    );
    let pgid = ready.wait(Deadline::after(CHILD_START))[0];
    let watchdog = Watchdog::group(pgid);
    let _own = ready.wait(Deadline::after(fakes::MUST_SUCCEED_WITHIN));
    let receipt = match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&running.output) {
        Ok(receipt) => receipt,
        Err(_) => panic!(
            "waited {:?} for the confined move receipt",
            fakes::MUST_SUCCEED_WITHIN
        ),
    };
    assert!(receipt.error.is_none(), "{}", text(&receipt));
    let job = started(&receipt);
    assert_eq!(jobs.started(), vec![job.clone()]);
    assert!(group_alive(pgid));
    assert!(
        !running.recorder.text().contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );
    release_block(ready.path());
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    assert_eq!(job_output(root.path(), &job), "hi\ndone\n");
    watchdog.stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

/// A quiet confined call hands off right after its marker: with the fake
/// clock frozen and no output after the marker, only the marker
/// observation can wake the parked drive loop, so the receipt arrives
/// while the command still runs instead of at its end
/// (`docs/sandbox.md`, "On macOS": no move hands the call to a job before
/// the marker is observed).
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_quiet_call_hands_off_right_after_its_marker() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let jobs = FakeJobs::new(root.path());
    let sandbox = Arc::new(Mutex::new(SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    // The shell acknowledges after opening the FIFO, then blocks on its
    // read. A read-write opener never blocks, so hold one before the child
    // starts and use it to release the child after the acknowledgement.
    let hold = root.path().join("hold");
    let ready = workspace.join("hold-ready");
    fakes::fifo(&hold);
    let mut release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hold)
        .unwrap();
    let running = start_confined(
        workspace,
        &format!(
            "exec 9< {} && : > {} && read -r _ <&9",
            quote(&hold),
            quote(&ready)
        ),
        true,
        false,
        jobs.clone(),
        sandbox,
    );
    let receipt = match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&running.output) {
        Ok(receipt) => receipt,
        Err(_) => panic!(
            "waited {:?} for the quiet call's handoff",
            fakes::MUST_SUCCEED_WITHIN
        ),
    };
    let job = started(&receipt);
    assert!(receipt.error.is_none(), "{}", text(&receipt));
    let ready_for_wait = ready.clone();
    fakes::wait_until(
        "the confined child to acknowledge opening the held FIFO",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || ready_for_wait.exists(),
    );
    // The child acknowledged after opening the FIFO and is now blocked on
    // its read. The fake clock never moved: the handoff waited for the
    // marker, not for output, the end, or a deadline (`docs/sandbox.md`,
    // "On macOS": no move before the marker is observed).
    // The call creates one job object (`docs/tools.md`, "Background jobs").
    assert_eq!(jobs.started(), vec![job.clone()]);
    assert_eq!(contract::clock::Clock::now(&*running.clock), running.start);
    assert!(
        !running.recorder.text().contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );
    // Release the held read and reap the job: the test leaves no detached
    // call behind.
    writeln!(release, "go").unwrap();
    drop(release);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
}

/// `stty` on the inherited pty keeps working confined: the single ioctl
/// allow covers the pty the shell holds.
#[cfg(target_os = "macos")]
#[test]
fn macos_confined_stty_on_a_pty_succeeds() {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let root = fakes::TempDir::under(&home, "fiber-tools-shell-sandbox-protected");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let jobs = FakeJobs::new(root.path());
    let sandbox = Arc::new(Mutex::new(SandboxSettings {
        enabled: true,
        allow_write: Vec::new(),
        deny_read: Vec::new(),
        fiber_home: root.path().join("fiber-home"),
    }));
    let shell = Shell::new(workspace.clone(), FakeClock::new()).with_sandbox(sandbox.clone());
    let effects = shell
        .effects(&args("echo hi"))
        .expect("the shell command classifies");
    assert!(effects.declared.confined);

    let running = start_confined(workspace, "stty -g", false, true, jobs.clone(), sandbox);
    let receipt = match Deadline::after(fakes::MUST_SUCCEED_WITHIN).recv(&running.output) {
        Ok(receipt) => receipt,
        Err(_) => panic!(
            "waited {:?} for the terminal move receipt",
            fakes::MUST_SUCCEED_WITHIN
        ),
    };
    let job = started(&receipt);
    let ended = jobs
        .ended(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))
        .expect("the job to finish");
    assert_eq!(ended.status, Outcome::Completed);
    let output = job_output(root.path(), &job);
    assert!(output.contains("speed"), "stty printed settings: {output}");
    assert!(
        !output.contains("FIBER_SANDBOX_CONFINED"),
        "the marker never reaches the caller"
    );
}

/// Runs one confined call to completion on an existing shell.
#[cfg(target_os = "macos")]
fn call(shell: &Shell, command: &str) -> contract::tool::Output {
    let shell = shell.clone();
    let command = command.to_owned();
    let what = format!("confined command {command:?}");
    fakes::within(
        &what,
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            run_prepared(
                &shell,
                &args(&command),
                &CancelToken::new(),
                &Recorder::default(),
            )
        },
    )
}
