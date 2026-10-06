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

use std::fs;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run, or one of its lines, may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fs");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Starts `fiber` with `args` in its own process group, its stdout read
    /// line by line, and a watchdog that kills the group if this process
    /// dies first.
    fn start(&self, args: &[&str], stdin: Stdio) -> Fiber {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
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
    fn wait_for(&mut self, kind: &str, nth: usize) {
        loop {
            if self.seen.iter().filter(|line| line["kind"] == kind).count() >= nth {
                return;
            }
            let line = self
                .lines
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for {kind}; saw {:?}", self.seen));
            self.seen.push(serde_json::from_slice(&line).unwrap());
            self.raw.push(line);
        }
    }

    /// Sends `signal` to `fiber`'s pid alone.
    fn signal(&self, signal: &str) {
        assert!(fakes::kill_pid(self.child.id(), signal).unwrap());
    }

    /// Waits for the exit under [`DEADLINE`], reads the rest of stdout, and
    /// asserts nothing is left in the group.
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
        } = self;
        drop(stdin);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = finished
            .recv_timeout(DEADLINE)
            .expect("fiber exited in time")
            .unwrap();
        // The pipe's end comes once the process is gone.
        while let Ok(line) = lines.recv_timeout(DEADLINE) {
            seen.push(serde_json::from_slice(&line).unwrap());
            raw.push(line);
        }
        assert!(
            !fakes::kill_group(group, "0").unwrap(),
            "fiber left a process in its group"
        );
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Ended {
            code: status.code(),
            raw: raw
                .into_iter()
                .zip(&seen)
                .filter(|(_, line)| !is_status(line))
                .map(|(text, _)| text)
                .collect(),
            lines: seen.into_iter().filter(|line| !is_status(line)).collect(),
            stderr: stderr.recv_timeout(DEADLINE).unwrap_or_default(),
        }
    }
}

/// A `session_status` line, written by an observer thread: where it falls
/// is not what these tests pin.
fn is_status(line: &Value) -> bool {
    line["kind"] == "session_status"
}

/// Kills process group `group` on drop; forgotten once the group is empty.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// An `openai-responses` stream of `events`, then its completion.
fn stream(events: &[Value]) -> Response {
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    let body: String = events
        .iter()
        .chain([&done])
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect();
    Response::stream(body)
}

fn hello() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
    }})])
}

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
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

/// `fiber ask` whose model request the fake server holds open, signalled
/// with `signal` once the request arrived.
fn held_ask(signal: &str, code: i32) {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    let fiber = setup.start(&["ask", "hi"], Stdio::null());
    assert!(
        server.await_requests(1, DEADLINE),
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
    let setup = Setup::new();
    let ready = fakes::children::Ready::new(setup.root.path());
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
        hello(),
        hello(),
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
    let job_group = ready.wait(DEADLINE)[0];
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
        !fakes::kill_group(job_group, "0").unwrap(),
        "the job's group outlived fiber"
    );
    job_watchdog.stand_down(DEADLINE);
}

#[test]
fn sigterm_while_the_prompt_is_read_exits_143_writing_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
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
    let (stdin, outcome) = result
        .recv_timeout(DEADLINE)
        .expect("fiber read stdin in time");
    outcome.unwrap();
    fiber.signal("TERM");
    let ended = fiber.end();
    drop(stdin);

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    assert!(setup.logs().is_empty(), "a session was left behind");
    assert!(server.requests().is_empty());
}

#[test]
fn sigterm_mid_turn_of_a_resumed_session_exits_143() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
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
        server.await_requests(2, DEADLINE),
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
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let ready = fakes::children::Ready::new(setup.root.path());
    // A FIFO only the server holds open for writing: its read end sees
    // end-of-file as soon as the server dies, while a pid probe still
    // answers during the short window the kernel needs to tear it down.
    // The blocking open and read run on a thread, and the wait below
    // carries the deadline, so nothing polls and no clock is read.
    let death = setup.root.path().join("death.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&death)
            .status()
            .unwrap()
            .success(),
        "mkfifo {} failed",
        death.display()
    );
    // A server that writes its pid, then never answers `initialize`. The
    // recorded signal gives the starting server the documented stop:
    // stdin closed, SIGTERM, then SIGKILL 800 ms later. It ignores SIGTERM
    // (`trap '' TERM` is inherited across its exec of `sleep`), so the
    // stop's kill after the grace is the path exercised, and the server is
    // reaped before fiber exits. The process itself still exits at the
    // bound on this path (see #830). Its startup deadline is far past the
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
    let _pid = ready.wait(DEADLINE)[0];
    fiber.signal("TERM");
    // Before `end`, whose group check would otherwise catch a server
    // left alive first: this wait is the one that pins the stop's kill.
    died.recv_timeout(DEADLINE)
        .expect("the MCP server outlived fiber");
    let ended = fiber.end();

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    assert!(setup.logs().is_empty(), "a session was left behind");
    assert!(
        setup.session_dirs().is_empty(),
        "a session directory was created"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn sigterm_while_an_mcp_server_starts_sends_it_sigterm() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let ready = fakes::children::Ready::new(setup.root.path());
    let marker = setup.root.path().join("marker");
    let quote =
        |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    // Never answers `initialize`. The trap is set before the pid line,
    // so the ready wait proves SIGTERM will be caught. Builtins only:
    // no `exec sleep` to drop the trap, no background child to outlive
    // the group. On the base commit the server gets only the bound's
    // SIGKILL, so the marker is never written and this fails there.
    let script = format!(
        "trap 'echo term > {}; exit 0' TERM\necho $$ > {}\nwhile :; do :; done\n",
        quote(&marker),
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
    let _pid = ready.wait(DEADLINE)[0];
    fiber.signal("TERM");
    // The group check inside fails first when a server is left alive.
    let ended = fiber.end();

    assert_eq!(ended.code, Some(143), "stderr: {}", ended.stderr);
    assert!(ended.lines.is_empty(), "{:?}", ended.lines);
    assert!(setup.logs().is_empty(), "a session was left behind");
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
