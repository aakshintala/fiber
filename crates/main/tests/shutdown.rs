//! Binary-level tests of a shutdown (`docs/invocation.md`, "Shutdown"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! and the test sends the signal to its pid alone, never to a group.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, KillGroup, Setup, function_call, hello_item_done, stream};

impl Setup {
    fn new_with_fs_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fs");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

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
            raw: Vec::new(),
            stderr: stderr_text,
            deadline: self.deadline,
        }
    }

    /// Every session directory (`s_…`) under Fiber home.
    fn session_dirs(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut dirs = vec![self.home()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if !path.is_dir() {
                    continue;
                }
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("s_"))
                {
                    found.push(path.clone());
                }
                dirs.push(path);
            }
        }
        found
    }

    /// The one session's `events.jsonl` under Fiber home, if any.
    fn logs(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut dirs = vec![self.home()];
        while let Some(dir) = dirs.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.file_name().is_some_and(|name| name == "events.jsonl") {
                    found.push(path);
                }
            }
        }
        found
    }
}

/// How many leading lines of each leftover session log a failure message
/// carries: paths and lengths are complete, content is a bounded head, so
/// a large log cannot flood the CI log.
const LEFTOVER_LOG_HEAD_LINES: usize = 5;

/// How many bytes of that head a failure message carries at most: one JSONL
/// line can itself be megabytes and is emitted whole, so a line cap alone
/// does not bound the dump.
const LEFTOVER_LOG_HEAD_BYTES: usize = 1024;

/// Formats leftover session logs for a failure message: every path, each
/// file's byte and line counts, and a bounded head of its first lines, cut
/// to the byte cap at a character boundary. A file that cannot be read
/// reports its error beside its path, never panicking, so the message still
/// names every log left behind.
fn describe_leftover_logs(logs: &[PathBuf]) -> String {
    logs.iter()
        .map(|path| match fs::read_to_string(path) {
            Ok(content) => {
                let total = content.lines().count();
                let mut head = content
                    .lines()
                    .take(LEFTOVER_LOG_HEAD_LINES)
                    .collect::<Vec<_>>()
                    .join("\n");
                let marker = if head.len() > LEFTOVER_LOG_HEAD_BYTES {
                    head.truncate(head.floor_char_boundary(LEFTOVER_LOG_HEAD_BYTES));
                    format!("\n…[head truncated to {LEFTOVER_LOG_HEAD_BYTES} bytes]")
                } else {
                    String::new()
                };
                format!(
                    "{} ({} bytes, {} lines):\n{}{}",
                    path.display(),
                    content.len(),
                    total,
                    head,
                    marker
                )
            }
            Err(err) => format!("{} (unreadable: {err})", path.display()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
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
    /// The same lines as written, newlines kept.
    raw: Vec<Vec<u8>>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

/// How a run ended.
struct Ended {
    code: Option<i32>,
    lines: Vec<Value>,
    /// The same lines as written, newlines kept.
    raw: Vec<Vec<u8>>,
    stderr: String,
}

impl Fiber {
    /// Reads stdout until a line of `kind`, the `nth` of it (from 1).
    #[track_caller]
    fn wait_for(&mut self, kind: &str, nth: usize) {
        loop {
            if self.seen.iter().filter(|line| line["kind"] == kind).count() >= nth {
                return;
            }
            let Ok(line) = self.deadline.recv(&self.lines) else {
                panic!("waited until the deadline for {kind}; saw {:?}", self.seen)
            };
            self.seen.push(serde_json::from_slice(&line).unwrap());
            self.raw.push(line);
        }
    }

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
            mut raw,
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
                    raw.push(line);
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
            raw: raw
                .into_iter()
                .zip(&seen)
                .filter(|(_, line)| !is_status_line(line))
                .map(|(text, _)| text)
                .collect(),
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
/// is not what these tests pin.
fn is_status_line(line: &Value) -> bool {
    line["kind"] == "session_status"
}

/// Stdout's event kinds are `kinds`, in order; its durable lines, byte for
/// byte, are everything the session log gained past `before`, its bytes
/// before the run; and the last is `fiber_exited` with `code` and no error
/// or final message.
fn assert_exited(setup: &Setup, ended: &Ended, code: i32, kinds: &[&str], before: &[u8]) {
    assert_eq!(ended.code, Some(code), "stderr: {}", ended.stderr);
    // A signal carries no `error`, so nothing is printed on stderr.
    assert_eq!(ended.stderr, "");
    let seen: Vec<&str> = ended
        .lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(seen, kinds);
    let last = ended.lines.last().expect("a line on stdout");
    assert_eq!(last["kind"], "fiber_exited", "{:?}", ended.lines);
    assert_eq!(last["payload"]["exit_code"], code);
    assert_eq!(last["payload"].get("error"), None);
    assert_eq!(last["payload"].get("text"), None);
    assert_eq!(last["payload"].get("final_action_id"), None);
    let logs = setup.logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let durable: Vec<u8> = ended
        .raw
        .iter()
        .zip(&ended.lines)
        .filter(|(_, line)| line.get("seq").is_some())
        .flat_map(|(bytes, _)| bytes.iter().copied())
        .collect();
    let mut expected = before.to_vec();
    expected.extend(durable);
    assert_eq!(fs::read(&logs[0]).unwrap(), expected);
}

/// The one early diagnostic line a pre-`fiber_started` signal leaves:
/// exactly one pid-named file under `home`'s `logs/` with one line, keys
/// in wire order, naming `signal` and the exit `code`.
#[track_caller]
fn assert_early_line(home: &Path, pid: u32, signal: &str, code: i32) {
    let entries: Vec<PathBuf> = fs::read_dir(home.join("logs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one diagnostic file: {entries:?}");
    assert_eq!(
        entries[0].file_name().unwrap(),
        format!("ask-{pid}.log").as_str()
    );
    let text = fs::read_to_string(&entries[0]).unwrap();
    assert_eq!(text.lines().count(), 1, "exactly one line");
    let line = text.lines().next().unwrap();
    for (first, second) in [
        ("ts", "level"),
        ("level", "process"),
        ("process", "code"),
        ("code", "message"),
    ] {
        assert!(
            line.find(format!("\"{first}\":").as_str()).unwrap()
                < line.find(format!("\"{second}\":").as_str()).unwrap(),
            "keys in order: {line}"
        );
    }
    let value: Value = serde_json::from_str(line).unwrap();
    assert!(value["ts"].as_u64().is_some_and(|ts| ts > 0));
    assert_eq!(value["level"], "error");
    assert_eq!(value["process"], "ask");
    assert!(value.get("session_id").is_none());
    assert_eq!(value["code"], "signal");
    assert_eq!(
        value["message"],
        format!("{signal} arrived before the session started; the process exits {code}.")
    );
}

/// `fiber ask` whose model request the fake server holds open, signalled
/// with `signal` once the request arrived.
#[track_caller]
fn held_ask(signal: &'static str, code: i32) {
    let setup = Setup::new_with_fs_root();
    let server = ProviderServer::start([hello_item_done()]).unwrap();
    server.hold();
    setup.provider(&server);
    let fiber = setup.start(&["ask", "hi"], Stdio::null());
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived"
    );
    fiber.signal(signal);
    let ended = fiber.end();
    server.release();

    assert_exited(
        &setup,
        &ended,
        code,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "turn_completed",
            "fiber_exited",
        ],
        &[],
    );
    let completed: Vec<&Value> = ended
        .lines
        .iter()
        .filter(|line| line["kind"] == "turn_completed")
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["payload"]["outcome"], "interrupted");
    assert_eq!(server.requests().len(), 1, "no further model request");
    // A started shutdown writes no diagnostic line: the writer's file is
    // created only by its first line.
    assert!(
        !setup.home().join("logs").exists(),
        "a started shutdown left no logs/"
    );
}

#[test]
fn sigterm_mid_request_exits_143_with_the_turn_interrupted() {
    held_ask("TERM", 143);
}

#[test]
fn sigint_mid_request_exits_130_with_the_turn_interrupted() {
    held_ask("INT", 130);
}

#[test]
fn sighup_mid_request_exits_129_with_the_turn_interrupted() {
    held_ask("HUP", 129);
}

#[test]
fn a_shutdown_stops_a_background_job_before_fiber_exited() {
    let setup = Setup::new_with_fs_root();
    let ready = ready_fifo(&setup);
    // The job writes its group id, then runs until it is stopped.
    fs::write(
        setup.workspace().join("job.sh"),
        format!(
            "echo $$ > '{}'\nwhile :; do sleep 0.05; done\n",
            ready.path().display()
        ),
    )
    .unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_bg",
            "shell",
            &json!({"command": "sh job.sh", "run_in_background": true}),
        )]),
        hello_item_done(),
        hello_item_done(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "sh job.sh"})
        ),
    )
    .unwrap();
    let mut fiber = setup.start(&["ask", "start the job"], Stdio::null());
    // The prompt's turn, then the ending notice's: the session now waits
    // for the job.
    fiber.wait_for("turn_completed", 2);
    let job_group = ready.wait(setup.deadline)[0];
    let job_watchdog = Watchdog::group(job_group);
    fiber.signal("TERM");
    let ended = fiber.end();

    assert_exited(
        &setup,
        &ended,
        143,
        &[
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
            "permission_resolved",
            "tool_call_started",
            "job_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "jobs_pending_notified",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "job_completed",
            "fiber_exited",
        ],
        &[],
    );
    let kinds: Vec<&str> = ended
        .lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let job_end = kinds
        .iter()
        .position(|kind| *kind == "job_completed")
        .unwrap_or_else(|| panic!("no job_completed in {kinds:?}"));
    assert_eq!(ended.lines[job_end]["payload"]["status"], "cancelled");
    assert!(job_end < kinds.len() - 1);
    assert!(
        !fakes::group_alive(job_group),
        "the job's group outlived fiber"
    );
    job_watchdog.stand_down(setup.deadline.cleanup_phase());
}

#[test]
fn sigterm_while_the_prompt_is_read_exits_143_writing_nothing() {
    let setup = Setup::new_with_fs_root();
    let server = ProviderServer::start([hello_item_done()]).unwrap();
    setup.provider(&server);
    let mut fiber = setup.start(&["ask", "-"], Stdio::piped());
    // Past a pipe's buffer: the write returns only once `fiber` is reading
    // stdin, after its signals are installed.
    let mut stdin = fiber.stdin.take().unwrap();
    let (written, result) = mpsc::channel();
    thread::spawn(move || {
        let outcome = stdin.write_all(&vec![b'x'; 1 << 20]);
        written.send((stdin, outcome)).unwrap();
    });
    let (stdin, outcome) = setup
        .deadline
        .recv(&result)
        .expect("fiber read stdin in time");
    outcome.unwrap();
    let pid = fiber.child.id();
    fiber.signal("TERM");
    let ended = fiber.end();
    drop(stdin);

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    // No session log here: a signal before `fiber_started` writes no durable
    // event (`docs/invocation.md`, "What is written") and a failure before
    // `fiber_started` has no session log (`docs/errors.md`, "Before a session
    // exists").
    let leftover = setup.logs();
    assert!(
        leftover.is_empty(),
        "a session was left behind:\n{}",
        describe_leftover_logs(&leftover)
    );
    assert_early_line(&setup.home(), pid, "SIGTERM", 143);
    assert!(server.requests().is_empty());
}

#[test]
fn sigterm_mid_turn_of_a_resumed_session_exits_143() {
    let setup = Setup::new_with_fs_root();
    let server = ProviderServer::start([hello_item_done(), hello_item_done()]).unwrap();
    setup.provider(&server);
    let first = setup.start(&["ask", "hi"], Stdio::null()).end();
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.lines[0]["session_id"].as_str().unwrap().to_owned();
    let logs = setup.logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let before = fs::read(&logs[0]).unwrap();

    server.hold();
    let fiber = setup.start(&["ask", "--resume", &id, "again"], Stdio::null());
    assert!(
        server.await_requests(2, setup.deadline),
        "the resumed request arrived"
    );
    fiber.signal("TERM");
    let ended = fiber.end();
    server.release();

    assert_exited(
        &setup,
        &ended,
        143,
        &[
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "turn_completed",
            "fiber_exited",
        ],
        &before,
    );
    assert_eq!(ended.lines[0]["kind"], "fiber_started");
    assert_eq!(ended.lines[0]["payload"]["resumed"], true);
    let completed = ended
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .expect("the resumed turn ended");
    assert_eq!(completed["payload"]["outcome"], "interrupted");
}

#[test]
fn sigterm_while_an_mcp_server_starts_kills_it_and_exits_143_writing_nothing() {
    let setup = Setup::new_with_fs_root();
    let server = ProviderServer::start([hello_item_done()]).unwrap();
    setup.provider(&server);
    let ready = ready_fifo(&setup);
    // A FIFO only the server holds open for writing: its read end sees
    // end-of-file as soon as the server dies, while a pid probe still
    // answers during the short window the kernel needs to tear it down.
    // The blocking open and read run on a thread, and the wait below
    // carries the deadline, so nothing polls and no clock is read.
    let death = setup.root.path().join("death.fifo");
    fakes::fifo(&death);
    // A server that writes its pid, then never answers `initialize`. The
    // recorded signal gives the starting server the documented stop:
    // stdin closed, SIGTERM, then SIGKILL 800 ms later. It ignores SIGTERM
    // (`trap '' TERM` is inherited across its exec of `sleep`), so the
    // stop's kill after the grace is the path exercised, and the server is
    // reaped before fiber exits. The process itself exits without waiting
    // out the bound on this path. Its startup deadline is far past the
    // 5 s bound. It holds the death FIFO open across the exec, so
    // the read end above sees end-of-file when it dies. Fiber never opens
    // that FIFO (`crates/mcp/src/server.rs` pipes only stdin and stdout):
    // only this server holds its write end.
    let script = format!(
        "trap '' TERM\nexec 3>'{}'\necho $$ > '{}'\nexec sleep 3600\n",
        death.display(),
        ready.path().display()
    );
    let (dead, died) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(mut fifo) = fs::File::open(&death) {
            let mut sink = Vec::new();
            if fifo.read_to_end(&mut sink).is_ok() {
                match dead.send(()) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
    });
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"slow": {
            "command": "/bin/sh",
            "args": ["-c", script],
            "startup_timeout_ms": 600_000,
        }}}}),
    );
    let fiber = setup.start(&["ask", "hi"], Stdio::null());
    // The server runs, so the signals were armed before it started.
    let _pid = ready.wait(setup.deadline)[0];
    let pid = fiber.child.id();
    fiber.signal("TERM");
    // Before `end`, whose group check would otherwise catch a server
    // left alive first: this wait is the one that pins the stop's kill.
    setup
        .deadline
        .recv(&died)
        .expect("the MCP server outlived fiber");
    let ended = fiber.end();

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    // No session log here: a signal before `fiber_started` writes no durable
    // event (`docs/invocation.md`, "What is written") and a failure before
    // `fiber_started` has no session log (`docs/errors.md`, "Before a session
    // exists").
    let leftover = setup.logs();
    assert!(
        leftover.is_empty(),
        "a session was left behind:\n{}",
        describe_leftover_logs(&leftover)
    );
    assert_early_line(&setup.home(), pid, "SIGTERM", 143);
    assert!(
        setup.session_dirs().is_empty(),
        "a session directory was created"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn sigterm_while_an_mcp_server_starts_sends_it_sigterm() {
    let setup = Setup::new_with_fs_root();
    let server = ProviderServer::start([hello_item_done()]).unwrap();
    setup.provider(&server);
    let ready = ready_fifo(&setup);
    let marker = setup.root.path().join("marker");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    // Never answers `initialize`. The trap is set before the pid line,
    // so the ready wait proves SIGTERM will be caught. The `read` runs
    // in the background and the shell parks in `wait`. The vulnerable
    // window is after the FIFO open returns and before `read(2)` blocks,
    // where a foreground `read` left TERM pending until the write end
    // dropped; `wait` is interruptible there, so a TERM that lands while
    // the `read` is starting still runs the trap, which kills the `read`
    // (no child outlives the group) and writes the marker. The parked
    // server burns no CPU, and no `exec sleep` drops the trap. Dropping
    // the write end at the test's end (even on panic) releases the read
    // if still parked.
    // The counts below cite their promises: exit 143 (`docs/invocation.md`,
    // "Three signals, one path"); no event lines (a signal before
    // `fiber_started` writes no durable event: `docs/invocation.md`, "What
    // is written"); no session log or directory (`docs/errors.md`, "Before
    // a session exists"); no provider request (`docs/invocation.md`, "What
    // happens, all at once": "No model request is made").
    // On the base commit the server gets only the bound's SIGKILL, so
    // the marker is never written and this fails there.
    let hold = setup.root.path().join("hold.fifo");
    fakes::fifo(&hold);
    // The server's `read` opens the FIFO, and this write-end open
    // blocks until it does: it runs on a thread, and the receive
    // below carries the deadline. The binding holds the write end
    // until the test ends.
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
    let script = format!(
        "trap 'kill \"$child\"; echo term > {}; exit 0' TERM\nread -r _ < {} & child=$!\necho $$ > {}\nwait\n",
        quote(&marker),
        quote(&hold),
        quote(ready.path()),
    );
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"slow": {
            "command": "/bin/sh",
            "args": ["-c", script],
            "startup_timeout_ms": 600_000,
        }}}}),
    );
    let fiber = setup.start(&["ask", "hi"], Stdio::null());
    // The server runs past its trap, so the signals were armed before
    // it started.
    let _pid = ready.wait(setup.deadline)[0];
    // The server's `read` opened the FIFO, so the write end is open:
    // holding it parks the server until the signal or the test's end.
    let _writer = setup
        .deadline
        .recv(&writing)
        .expect("the hold FIFO opened for writing");
    fiber.signal("TERM");
    // The group check inside fails first when a server is left alive.
    let ended = fiber.end();

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    // No session log here: a signal before `fiber_started` writes no durable
    // event (`docs/invocation.md`, "What is written") and a failure before
    // `fiber_started` has no session log (`docs/errors.md`, "Before a session
    // exists").
    let leftover = setup.logs();
    assert!(
        leftover.is_empty(),
        "a session was left behind:\n{}",
        describe_leftover_logs(&leftover)
    );
    assert!(
        setup.session_dirs().is_empty(),
        "a session directory was created"
    );
    assert!(server.requests().is_empty());
    assert_eq!(
        fs::read_to_string(&marker).unwrap_or_default().trim(),
        "term",
        "the starting server saw SIGTERM"
    );
}

/// The leftover-log dump names every path with its lengths and a bounded
/// head: below, at, and above the line cap, plus a log that cannot be read.
#[test]
fn leftover_log_dump_names_paths_with_a_bounded_head() {
    let root = fakes::TempDir::new("leftover-logs");
    let write_log = |name: &str, count: usize| -> PathBuf {
        let path = root.path().join(name).join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body: String = (1..=count)
            .map(|n| format!("{{\"kind\":\"event-{n}\"}}\n"))
            .collect();
        fs::write(&path, &body).unwrap();
        path
    };
    let over = write_log("s_over", LEFTOVER_LOG_HEAD_LINES + 3);
    let exact = write_log("s_exact", LEFTOVER_LOG_HEAD_LINES);
    let under = write_log("s_under", 2);
    let missing = root.path().join("s_gone").join("events.jsonl");

    let dump =
        describe_leftover_logs(&[over.clone(), exact.clone(), under.clone(), missing.clone()]);

    for path in [&over, &exact, &under, &missing] {
        assert!(
            dump.contains(&path.display().to_string()),
            "names {path:?}:\n{dump}"
        );
    }
    // The head stops at the cap even when the file holds more.
    assert!(
        dump.contains(r#""kind":"event-5""#),
        "head line kept:\n{dump}"
    );
    assert!(
        !dump.contains(r#""kind":"event-6""#),
        "beyond the cap cut:\n{dump}"
    );
    // Lengths are reported for every readable log.
    for (path, count) in [
        (&over, LEFTOVER_LOG_HEAD_LINES + 3),
        (&exact, LEFTOVER_LOG_HEAD_LINES),
        (&under, 2),
    ] {
        let bytes = fs::metadata(path).unwrap().len();
        assert!(
            dump.contains(&format!("{bytes} bytes")),
            "byte length of {path:?}:\n{dump}"
        );
        assert!(
            dump.contains(&format!("{count} lines")),
            "line count of {path:?}:\n{dump}"
        );
    }
    // A vanished log still names its path with the read error, no panic.
    assert!(dump.contains("unreadable"), "read error kept:\n{dump}");
}

/// The leftover-log dump caps an oversized line by bytes while keeping the
/// full byte and line counts: a single megabyte-scale JSONL line is emitted
/// whole without the cap, so it would flood the CI log.
#[test]
fn leftover_log_dump_truncates_an_oversized_line_by_bytes() {
    let root = fakes::TempDir::new("leftover-logs-huge-line");
    let write_log = |name: &str, body: &str| -> PathBuf {
        let path = root.path().join(name).join("events.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    };
    // One JSONL line far past the byte cap.
    let huge = "x".repeat(LEFTOVER_LOG_HEAD_BYTES * 4);
    let huge_line = format!("{{\"kind\":\"huge\",\"pad\":\"{huge}\"}}\n");
    let huge_path = write_log("s_huge", &huge_line);
    let huge_dump = describe_leftover_logs(std::slice::from_ref(&huge_path));

    // Full lengths are kept even though the head is cut.
    assert!(
        huge_dump.contains(&format!("{} bytes", huge_line.len())),
        "full byte length kept:\n{huge_dump}"
    );
    assert!(
        huge_dump.contains("1 lines"),
        "full line count kept:\n{huge_dump}"
    );
    // The head is bounded: the whole line never lands in the dump.
    assert!(
        !huge_dump.contains(&huge),
        "oversized line cut:\n{huge_dump}"
    );
    assert!(
        huge_dump.len() < huge_line.len(),
        "dump smaller than the log line"
    );
    // A head exactly at the cap is kept whole, with no truncation marker:
    // this pins the `>` against a `>=` flip.
    let exact = "y".repeat(LEFTOVER_LOG_HEAD_BYTES);
    let exact_path = write_log("s_exact_bytes", &format!("{exact}\n"));
    let exact_dump = describe_leftover_logs(std::slice::from_ref(&exact_path));
    assert!(
        exact_dump.contains(&exact),
        "at-cap head kept:\n{exact_dump}"
    );
    assert!(
        !exact_dump.contains("truncated"),
        "no truncation marker at the cap:\n{exact_dump}"
    );
    // One byte below the cap is kept whole, with no truncation marker.
    let below = "y".repeat(LEFTOVER_LOG_HEAD_BYTES - 1);
    let below_path = write_log("s_below_bytes", &format!("{below}\n"));
    let below_dump = describe_leftover_logs(std::slice::from_ref(&below_path));
    assert!(
        below_dump.contains(&below),
        "below-cap head kept:\n{below_dump}"
    );
    assert!(
        !below_dump.contains("truncated"),
        "no truncation marker below the cap:\n{below_dump}"
    );
    // One byte over the cap is cut, keeping the full lengths and marking
    // the truncation: this pins the boundary from above.
    let just_over = "z".repeat(LEFTOVER_LOG_HEAD_BYTES + 1);
    let just_over_line = format!("{just_over}\n");
    let just_over_path = write_log("s_just_over_bytes", &just_over_line);
    let just_over_dump = describe_leftover_logs(std::slice::from_ref(&just_over_path));
    assert!(
        just_over_dump.contains(&format!("{} bytes", just_over_line.len())),
        "full byte length kept one over the cap:\n{just_over_dump}"
    );
    assert!(
        just_over_dump.contains("1 lines"),
        "full line count kept one over the cap:\n{just_over_dump}"
    );
    assert!(
        !just_over_dump.contains(&just_over),
        "one-over-cap line cut:\n{just_over_dump}"
    );
    assert!(
        just_over_dump.contains("truncated"),
        "truncation marker one over the cap:\n{just_over_dump}"
    );
}
