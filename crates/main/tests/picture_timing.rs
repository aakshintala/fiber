//! Binary-level `picture.run` silence (see #1212, Part 12): the built
//! `fiber` runs `ask` against the fake provider server, and one session
//! over its socket. The production composition does not forward a timing
//! yet, so no run here can emit `picture.run`; these journeys pin that
//! silence through the installed composition. A text-only read emits
//! nothing at any level, and a paste the child never enters emits nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, Run, Setup, function_call, hello, is_status, read_kinds, run_to_exit, stream,
    write_json,
};

/// Runs `fiber ask <prompt>` in the workspace to exit and parses its stdout.
#[track_caller]
fn ask(setup: &Setup, prompt: &str) -> Run {
    let mut command = setup.fiber(&["ask", prompt]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// A `read` of `note.txt` answered, then a closing answer.
fn read_script() -> [fakes::Response; 2] {
    [
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ]
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// The `picture.run` lines among `lines`: never an argument, a path or any
/// content, only the operation and the milliseconds.
fn picture_runs(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["code"] == "op_timing" && line["data"]["op"] == "picture.run")
        .collect()
}

#[test]
fn at_default_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn at_info_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "info"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(server.requests().len(), 2, "the read really ran");
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn an_ignored_repository_debug_level_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.all("notice")
            .iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value stays ignored: {:?}",
        run.kinds()
    );
    assert!(
        run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the read really ran"
    );
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

#[test]
fn at_debug_a_text_read_writes_no_picture_run() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(read_script()).unwrap();
    setup.provider(&server);

    let run = ask(&setup, "read the note");

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert!(
        run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the read really ran"
    );
    // Other operations may time at debug; the imageless read times none.
    assert!(
        picture_runs(&log_lines(&setup)).is_empty(),
        "no picture.run without an image"
    );
}

impl Setup {
    fn new_with_fm_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    /// Installs the provider with the fake model declaring `input`
    /// `["text", "image"]`, so a pasted image is sent as an image part.
    fn provider_with_images(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000, "input": ["text", "image"]}]
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
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// The session's directory, from its id.
    fn session_dir(&self, id: &str) -> PathBuf {
        log::sessions_dir(&self.home(), &doors::project(&self.workspace())).join(id)
    }

    fn socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    /// Starts `fiber session --id <id> --workspace <workspace>` in its own
    /// process group, its stdout drained on a thread and its stderr kept
    /// for a failure.
    #[track_caller]
    fn start_session(&self, id: &str) -> Running {
        let mut command = self.fiber(&[
            "session",
            "--id",
            id,
            "--workspace",
            self.workspace().to_str().unwrap(),
        ]);
        command.stdin(Stdio::null());
        let mut child = command.spawn().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match tx.send(line.unwrap()) {
                    Ok(()) => {}
                    Err(mpsc::SendError(_)) => break,
                }
            }
        });
        let (err_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            match std::io::Read::read_to_string(&mut BufReader::new(stderr_pipe), &mut text) {
                Ok(_) | Err(_) => {}
            }
            match err_tx.send(text) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        Running {
            child,
            watchdog,
            group,
            guard,
            lines,
            stderr: stderr_rx,
            first: Vec::new(),
            deadline: self.deadline,
        }
    }
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// A running `fiber session`: its stdout drained line by line, its stderr
/// kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    /// Stdout lines taken as the readiness signal, as written, prepended
    /// to what [`Running::wait`] returns.
    first: Vec<String>,
    deadline: Deadline,
}

impl Running {
    /// Waits under the test's [`Deadline`] for the child's first stdout line, then
    /// connects once. The socket is bound before the loop writes that
    /// line, so the line is the signal the socket accepts
    /// (`docs/testing.md`, "Waits and timeouts").
    #[track_caller]
    fn connect(&mut self, socket: &Path) -> Socket {
        let line = match self.deadline.recv(&self.lines) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        };
        self.first.push(line);
        Socket::connect(self.deadline, socket)
            .expect("the session's socket accepted before the deadline")
    }

    /// Reads stdout lines until one of `kind` arrives, each taking what
    /// remains of the test's [`Deadline`]. Only kinds the loop writes
    /// before waiting for a prompt qualify.
    #[track_caller]
    fn wait_for(&mut self, kind: &str) {
        loop {
            let line = match self.deadline.recv(&self.lines) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for {kind} on the session's stdout")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the session's stdout closed before {kind}")
                }
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            let done = value["kind"] == kind;
            self.first.push(line);
            if done {
                return;
            }
        }
    }

    /// Waits under the test's [`Deadline`] for the process to exit, and asserts that
    /// nothing it started is left in its group.
    #[track_caller]
    fn wait(mut self) -> (ExitStatus, Vec<Value>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match self.deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                support::expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !support::group_alive(self.deadline, self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        let mut out = std::mem::take(&mut self.first);
        loop {
            match self.deadline.recv(&self.lines) {
                Ok(line) => out.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = match self.deadline.recv(&self.stderr) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => String::new(),
        };
        self.watchdog.stand_down(self.deadline.cleanup());
        let out = out
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (status, out, stderr)
    }
}

/// A client on the session's socket. Every read and write takes what
/// remains of the test's [`Deadline`]: expiry panics naming what was
/// awaited, while the session closing the socket ends [`until_close`].
struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
    deadline: Deadline,
}

impl Socket {
    /// Connects to `path` on a thread bounded by the deadline.
    #[track_caller]
    fn connect(deadline: Deadline, path: &Path) -> std::io::Result<Self> {
        let target = path.to_owned();
        let write = support::bounded(
            deadline,
            &format!("a connection to {}", path.display()),
            move || UnixStream::connect(target),
        )?;
        let read = write.try_clone()?;
        Ok(Self {
            write: Mutex::new(write),
            read: Mutex::new(BufReader::new(read)),
            deadline,
        })
    }

    #[track_caller]
    fn send(&self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        if !line.ends_with('\n') {
            bytes.push(b'\n');
        }
        let mut write = self.write.lock().unwrap();
        if let Err(error) = support::write_line(&mut write, self.deadline, &bytes, "sending a line")
        {
            panic!("writing the session socket: {error}");
        }
    }

    /// One socket line: a line, or `None` when the session closed the
    /// socket. The deadline passing with neither panics naming `what`,
    /// with the lines before it.
    fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        match support::read_line(&mut self.read.lock().unwrap(), self.deadline, what) {
            Ok(None) => None,
            Ok(Some(buf)) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                panic!("waited until the deadline for {what}; got {got:?}")
            }
            Err(error) => {
                panic!("reading the session socket while waiting for {what}: {error}")
            }
        }
    }
}

#[track_caller]
fn send(client: &Socket, line: &str) {
    client.send(line);
}

fn recv(client: &Socket, what: &str) -> Value {
    match client.next(what, &[]) {
        Some(line) => line,
        None => panic!("the session closed the socket while waiting for {what}"),
    }
}

/// Collects lines until `done`, each taking what remains of the test's
/// [`Deadline`]: expiry panics naming `what`, and the socket closing first
/// panics too.
fn until(client: &Socket, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = match client.next(what, &lines) {
            Some(line) => line,
            None => {
                panic!("the session closed the socket while waiting for {what}; got {lines:?}")
            }
        };
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

fn answer(client: &Socket, id: &str) -> Value {
    until(client, "the answer", |line| {
        line["payload"].get("command_id") == Some(&json!(id))
    })
    .into_iter()
    .next_back()
    .unwrap()
}

/// Collects socket lines until the session closes the socket, each taking
/// what remains of the test's [`Deadline`].
fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the session to close the socket", &lines) {
        lines.push(line);
    }
    lines
}

#[test]
fn an_invalid_pasted_image_is_refused_with_no_picture_run() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_images(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id);

    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    recv(&client, "the subscribe acknowledgement");
    // Not base64 at all: the paste is refused before the child runs.
    send(
        &client,
        r#"{"id":"c_1","command":"prompt","args":{"content":[{"type":"image","data":"!!!not-base64!!!","mime_type":"image/png"}]}}"#,
    );
    let refused = answer(&client, "c_1");
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["code"], "invalid_arguments", "{refused}");
    assert!(
        refused["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("not base64"),
        "{refused}"
    );
    assert!(server.requests().is_empty(), "no turn ran for the refusal");
    // A text turn after the refusal: the session persists events and
    // answers, proving the run below scans logs the session really wrote.
    send(
        &client,
        r#"{"id":"c_2","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert_eq!(server.requests().len(), 1, "the text turn really ran");

    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let mut lines = log_lines(&setup);
    let events = fs::read_to_string(setup.session_dir(&id).join("events.jsonl")).unwrap();
    for line in events.lines() {
        if line.is_empty() {
            continue;
        }
        lines.push(serde_json::from_str(line).unwrap());
    }
    assert!(
        picture_runs(&lines).is_empty(),
        "a refused paste enters no scope"
    );
}
