//! Binary-level test of a shutdown landing inside an extension callback
//! (`docs/invocation.md`, "Shutdown"): an `after_tool` hook in flight
//! calling `host.exec` must stop like a tool call's group does, so the
//! shutdown's SIGTERM reaches the exec program before any SIGKILL, the
//! process exits 143 with `fiber_exited` last, and the bound's kill point
//! is never reached.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use fakes::MUST_SUCCEED_WITHIN;
use std::fs;
use std::io::{BufRead, BufReader, Read as _};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, KillGroup, Setup, function_call, hello_item_done, stream};

impl Setup {
    /// Starts `fiber` with `args` in its own process group, its stdout read
    /// line by line, and a watchdog that kills the group if this process
    /// dies first.
    #[track_caller]
    fn start(&self, args: &[&str], stdin: Stdio) -> Fiber {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            // Each line as written, its newline kept.
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let (err_tx, stderr_text) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            match stderr.read_to_string(&mut text) {
                Ok(_) | Err(_) => {}
            }
            match err_tx.send(text) {
                Ok(()) | Err(_) => {}
            }
        });
        Fiber {
            stdin: child.stdin.take(),
            child,
            group,
            guard,
            watchdog,
            lines,
            seen: Vec::new(),
            stderr: stderr_text,
            deadline: self.deadline,
        }
    }
}

/// A running `fiber`.
struct Fiber {
    child: Child,
    stdin: Option<ChildStdin>,
    group: u32,
    guard: KillGroup,
    watchdog: Watchdog,
    lines: mpsc::Receiver<Vec<u8>>,
    /// Every stdout line read so far, parsed.
    seen: Vec<Value>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

/// How a run ended.
struct Ended {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Fiber {
    /// Sends `signal` to `fiber`'s pid alone.
    #[track_caller]
    fn signal(&self, signal: &'static str) {
        assert!(fakes::kill_pid(self.child.id(), signal).unwrap());
    }

    /// Waits for the exit under the test's [`Deadline`], reads the rest of
    /// stdout, and asserts nothing is left in the group.
    #[track_caller]
    fn end(self) -> Ended {
        let Fiber {
            mut child,
            stdin,
            group,
            guard,
            watchdog,
            lines,
            mut seen,
            stderr,
            deadline,
        } = self;
        drop(stdin);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(_) => support::expired(deadline, group, &finished, "fiber to exit"),
        };
        // The pipe's end comes once the process is gone.
        loop {
            match deadline.recv(&lines) {
                Ok(line) => {
                    seen.push(serde_json::from_slice(&line).unwrap());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the rest of stdout")
                }
            }
        }
        assert!(
            !fakes::group_alive(group),
            "fiber left a process in its group"
        );
        std::mem::forget(guard);
        watchdog.stand_down(deadline.cleanup_phase());
        Ended {
            code: status.code(),
            lines: seen
                .into_iter()
                .filter(|line| !is_status_line(line))
                .collect(),
            stderr: match deadline.recv(&stderr) {
                Ok(text) => text,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stderr")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the stderr reader ended without its text")
                }
            },
        }
    }
}

/// `ready.fifo` in the test's root, read as a [`fakes::children::Ready`]
/// whose open takes what remains of the test's [`Deadline`].
#[track_caller]
fn ready_fifo(setup: &Setup) -> fakes::children::Ready {
    let fifo = setup.root.path().join("ready.fifo");
    fakes::fifo(&fifo);
    fakes::children::Ready::at(&fifo, &|| setup.deadline.left())
}

/// A `session_status` line, written by an observer thread: where it falls
/// is not what this test pins.
fn is_status_line(line: &Value) -> bool {
    line["kind"] == "session_status"
}

/// Shell single-quote of `path`.
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Builds the exec program's script from its mark, ready and hold paths:
/// it arms its SIGTERM trap, writes its pid to the ready FIFO, then blocks
/// in `read` on the hold FIFO. `prefix` runs first inside the blocking
/// command's expansion: the stand-in below passes `kill -STOP $$; `, so
/// the shell stops inside the expansion and a SIGTERM sent then lands
/// inside the read.
fn exec_script(mark: &Path, ready: &Path, hold: &Path, prefix: &str) -> String {
    format!(
        "trap 'echo term > {}; exit 0' TERM\necho $$ > {}\nread -r _ < \"$({}echo {})\" & wait\n",
        quote(mark),
        quote(ready),
        prefix,
        quote(hold),
    )
}

#[test]
fn sigterm_stops_an_after_tool_callback_and_its_exec_group() {
    let setup = Setup::new();
    let ready = ready_fifo(&setup);
    let mark = setup.root.path().join("mark");
    // The ticket's `sh -c 'trap ...; sleep 60'` runs the callback until
    // SIGTERM: the trap's mark proves SIGTERM arrived before any SIGKILL.
    // The trap is set before the pid line, so the ready wait proves it is
    // armed; the `read` below blocks on a FIFO the test holds open instead
    // of the ticket's `sleep 60`, parked with no CPU until the signal.
    let hold = setup.root.path().join("hold.fifo");
    fakes::fifo(&hold);
    // This write-end open blocks until the exec program's `read` opens the
    // FIFO: it runs on a thread, and the receive below carries the
    // deadline. The binding holds the write end until the test ends.
    let (held, writing) = mpsc::channel();
    thread::spawn({
        let hold = hold.clone();
        move || {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(&hold) {
                match held.send(file) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    });
    let script = exec_script(&mark, ready.path(), &hold, "");
    let init = format!(
        "fiber.hook(\"after_tool\", {{ timeout = 30000, on_failure = \"blocking\",\n  run = function(call)\n    host.exec(\"sh\", {{\"-c\", [[{script}]]}})\n    return {{ content = \"hooked\" }}\n  end }})\n"
    );
    setup.lua("shutdown-exec", &init);
    fs::write(setup.workspace().join("note.txt"), "the note\n").unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello_item_done(),
    ])
    .unwrap();
    setup.provider(&server);
    let fiber = setup.start(&["ask", "read the note"], Stdio::null());
    // The exec program runs past its trap, and its `read` opened the FIFO,
    // so the hook is in flight and parked before the shutdown starts.
    let _started = ready.wait(setup.deadline);
    let _writer = setup
        .deadline
        .recv(&writing)
        .expect("the hold FIFO opened for writing");
    fiber.signal("TERM");
    let ended = fiber.end();

    // SIGTERM reached the exec program before any SIGKILL.
    assert_eq!(
        fs::read_to_string(&mark).unwrap_or_default().trim(),
        "term",
        "the exec program saw SIGTERM"
    );
    // The process exits 143 with `fiber_exited` last and no error.
    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert_eq!(ended.stderr, "");
    let kinds: Vec<&str> = ended
        .lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
            "extension_exec",
            "fiber_exited",
        ],
    );
    let completed = ended
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .expect("the interrupted turn ended");
    assert_eq!(completed["payload"]["outcome"], "interrupted");
    let last = ended.lines.last().expect("a line on stdout");
    assert_eq!(last["kind"], "fiber_exited", "{:?}", ended.lines);
    assert_eq!(last["payload"]["exit_code"], 143);
    assert_eq!(last["payload"].get("error"), None);
    assert_eq!(last["payload"].get("text"), None);
    assert_eq!(last["payload"].get("final_action_id"), None);
    // The shutdown never reached its kill point, in any diagnostic file.
    let logs_dir = setup.home().join("logs");
    if logs_dir.is_dir() {
        for entry in fs::read_dir(&logs_dir).unwrap() {
            let text = fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(
                !text.contains("kill point"),
                "the bound's kill line was written: {text}"
            );
        }
    }
}

/// Whether `ps` shows process `pid` stopped: its state holds `T`. `ps`
/// runs in its own watched group and its output arrives under a short
/// bound, so a `ps` that never exits is killed and reported, never read
/// as running.
// debt: stopped-state poll shells out to `ps` instead of `rustix::process::waitid`: waitid needs siginfo/ECHILD plumbing and drops the kill-and-report hang-proofing above. Upgrade trigger: a second stopped-state poll in main tests, which moves both onto one shared waitid helper.
#[track_caller]
fn is_stopped(deadline: Deadline, pid: u32) -> bool {
    let mut command = Command::new("ps");
    command
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let (child, watchdog) = support::spawn_watched(&mut command);
    let ps_group = child.id();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let wait = deadline.left().min(MUST_SUCCEED_WITHIN);
    let output = match Deadline::after(wait).recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            match fakes::kill_group(ps_group, "KILL") {
                Ok(_) | Err(_) => {}
            }
            watchdog.stand_down(deadline.cleanup_phase());
            panic!("ps did not exit within {wait:?}");
        }
    };
    watchdog.stand_down(deadline.cleanup_phase());
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).contains('T')
}

/// Waits until `ps` shows process `pid` stopped, polling with no sleep to
/// the test's [`Deadline`]: proof the shell parked inside the blocking
/// command's expansion, so the SIGTERM below lands inside the read.
#[track_caller]
fn wait_stopped(deadline: Deadline, pid: u32) {
    loop {
        assert!(
            !deadline.left().is_zero(),
            "waited until the deadline for process {pid} to stop"
        );
        if is_stopped(deadline, pid) {
            return;
        }
        std::thread::yield_now();
    }
}

/// Sends SIGCONT to process `pid` through `fakes::kill_pid`, starting
/// no process: the shell stopped itself, and SIGTERM stays pending until
/// SIGCONT resumes it. An id of 1 or less panics before anything runs,
/// since the signal would reach processes the test does not own.
#[track_caller]
fn continue_pid(pid: u32) {
    assert!(fakes::kill_pid(pid, "CONT").unwrap());
}

#[test]
fn a_sigterm_landing_inside_the_blocking_read_still_runs_the_trap() {
    let setup = Setup::new();
    let ready = ready_fifo(&setup);
    let mark = setup.root.path().join("mark");
    let hold = setup.root.path().join("hold.fifo");
    fakes::fifo(&hold);
    // The expansion stops the shell itself before it blocks, so the
    // SIGTERM below lands inside the blocking read on every run.
    let script = exec_script(&mark, ready.path(), &hold, "kill -STOP $$; ");
    // debt: manual process-group, drop guard and watchdog instead of `support::spawn_watched`, which forgets its guard at once and kills only on process death. Upgrade trigger: spawn_watched keeps a drop guard, or a third manual setup appears, which moves all three onto the helper.
    let mut command = Command::new("sh");
    command
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    // The pid line proves the trap is armed before any signal goes out.
    let pid = ready.wait(setup.deadline)[0];
    // The shell stopped inside the blocking command's expansion: SIGTERM
    // stays pending until SIGCONT resumes it into the read.
    wait_stopped(setup.deadline, pid);
    assert!(fakes::kill_pid(pid, "TERM").unwrap());
    continue_pid(pid);
    // This write-end open blocks until the exec program's `read` opens the
    // FIFO: it runs on a thread, and the receive below carries the
    // deadline. The binding holds the write end until the test ends.
    let (held, writing) = mpsc::channel();
    thread::spawn({
        let hold = hold.clone();
        move || {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(&hold) {
                match held.send(file) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    });
    let _writer = setup
        .deadline
        .recv(&writing)
        .expect("the hold FIFO opened for writing");
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    match setup.deadline.recv(&finished) {
        Ok(status) => assert!(
            status.unwrap().success(),
            "the exec program exited through its trap"
        ),
        Err(_) => support::expired(
            setup.deadline,
            group,
            &finished,
            "the exec program to run its trap",
        ),
    }
    // SIGTERM reached the exec program: the trap wrote its mark.
    assert_eq!(
        fs::read_to_string(&mark).unwrap_or_default().trim(),
        "term",
        "the exec program saw SIGTERM"
    );
    // The background `read` outlives the shell: kill the group, check it
    // emptied, and stand the watchdog down.
    match fakes::kill_group(group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(group, setup.deadline.cleanup_phase()),
        "the exec program's group emptied"
    );
    std::mem::forget(guard);
    watchdog.stand_down(setup.deadline.cleanup_phase());
}
