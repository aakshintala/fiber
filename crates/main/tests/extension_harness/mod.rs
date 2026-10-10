#![allow(dead_code, reason = "shared by several test targets, each using part")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;

use crate::support::{Deadline, group_alive};
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

pub(crate) struct Setup {
    pub(crate) root: fakes::TempDir,
    pub(crate) deadline: Deadline,
}

impl Setup {
    pub(crate) fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    pub(crate) fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    pub(crate) fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
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

    /// Installs `fiber.test/<short>`, whose entry script is `init`, with
    /// `manifest` merged into its `extension.json`.
    pub(crate) fn lua_with(&self, short: &str, init: &str, manifest: Value) {
        let source = self.root.path().join("src").join(short);
        fs::create_dir_all(&source).unwrap();
        let mut base = json!({"name": format!("fiber.test/{short}"), "version": "v2.0.0", "fiber": "0.1.0", "api": 1});
        for (key, value) in manifest.as_object().unwrap() {
            base[key] = value.clone();
        }
        write_json(&source.join("extension.json"), &base);
        fs::write(source.join("init.lua"), init).unwrap();
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    pub(crate) fn lua(&self, short: &str, init: &str) {
        self.lua_with(short, init, json!({}));
    }

    pub(crate) fn socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    pub(crate) fn fiber(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    pub(crate) fn start_session(&self, id: &str, extra: &[&str]) -> Running {
        let workspace = self.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        let mut command = self.fiber(&args);
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

pub(crate) fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

pub(crate) struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        crate::support::kill_group_detached(self.0, "KILL");
    }
}

pub(crate) struct Running {
    pub(crate) child: Child,
    pub(crate) watchdog: Watchdog,
    pub(crate) group: u32,
    pub(crate) guard: KillGroup,
    pub(crate) lines: mpsc::Receiver<String>,
    pub(crate) stderr: mpsc::Receiver<String>,
    pub(crate) first: Vec<String>,
    pub(crate) deadline: Deadline,
}

impl Running {
    pub(crate) fn connect(&mut self, socket: &Path) -> Socket {
        let line = self.first_line();
        self.first.push(line);
        Socket::connect(self.deadline, socket)
            .expect("the session's socket accepted before the deadline")
    }

    pub(crate) fn connect_client(&mut self, socket: &Path) -> fakes::Client {
        let line = self.first_line();
        self.first.push(line);
        let target = socket.to_owned();
        crate::support::bounded(
            self.deadline,
            &format!("a connection to {}", socket.display()),
            move || fakes::Client::connect(&target),
        )
        .expect("the session's socket accepted before the deadline")
    }

    fn first_line(&mut self) -> String {
        match self.lines.recv_timeout(self.deadline.left()) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        }
    }

    pub(crate) fn wait_for(&mut self, kind: &str) {
        loop {
            let line = match self.lines.recv_timeout(self.deadline.left()) {
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

    pub(crate) fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let (status, raw, stderr) = self.wait_raw();
        let out = raw
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (status, out, stderr)
    }

    fn wait_raw(mut self) -> (ExitStatus, Vec<String>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match finished.recv_timeout(self.deadline.left()) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                crate::support::expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended before it exited")
            }
        };
        let mut lines = std::mem::take(&mut self.first);
        while let Ok(line) = self.lines.try_recv() {
            lines.push(line);
        }
        let stderr = match self.stderr.recv_timeout(self.deadline.left()) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr reader ended without its text")
            }
        };
        assert!(
            !group_alive(self.deadline, self.group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(self.guard);
        self.watchdog.stand_down(self.deadline.cleanup());
        (status, lines, stderr)
    }
}

/// A client on a session's socket. Every read and write takes what remains
/// of the test's [`Deadline`].
pub(crate) struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
    deadline: Deadline,
}

impl Socket {
    /// Connects to `path` on a thread bounded by the deadline.
    pub(crate) fn connect(deadline: Deadline, path: &Path) -> std::io::Result<Self> {
        let target = path.to_owned();
        let write = crate::support::bounded(
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

    fn send(&self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        if !line.ends_with('\n') {
            bytes.push(b'\n');
        }
        let mut write = self.write.lock().unwrap();
        if let Err(error) =
            crate::support::write_line(&mut write, self.deadline, &bytes, "sending a line")
        {
            panic!("writing the session socket: {error}");
        }
    }

    fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        match crate::support::read_line(&mut self.read.lock().unwrap(), self.deadline, what) {
            Ok(None) => None,
            Ok(Some(buf)) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                panic!("waited until the deadline for {what}; got {got:?}")
            }
            Err(error) => {
                panic!("reading the session socket while waiting for {what}: {error}")
            }
        }
    }
}

pub(crate) fn send(client: &Socket, line: &str) {
    client.send(line);
}

pub(crate) fn until(
    client: &Socket,
    what: &str,
    mut done: impl FnMut(&Value) -> bool,
) -> Vec<Value> {
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

pub(crate) fn until_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the session to close the socket", &lines) {
        lines.push(line);
    }
    lines
}

/// The event kinds of `lines`, in order, without `session_status`: an
/// observer thread writes it, so where it falls among the loop's own
/// lines is not what these tests pin.
pub(crate) fn kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// An `openai-responses` stream of `events`, then a completed reply.
pub(crate) fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body.into_bytes())
}

pub(crate) fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// [`hold_server`]'s URL, the receiver told once the request head is read,
/// and the sender whose drop releases the held connection.
pub(crate) type Held = (String, mpsc::Receiver<()>, mpsc::Sender<()>);

/// Holds one HTTP connection open (head read, body never sent), so a
/// `host.http` against it stays parked. Each header read takes what remains
/// of `deadline`; the connection is held until the returned sender sends or
/// drops. The caller receives the accepted signal with the deadline, which
/// bounds the accept.
pub(crate) fn hold_server(deadline: Deadline) -> Held {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0; 1];
        let mut seen = Vec::new();
        loop {
            use std::io::Read;
            // A zero timeout is refused, so a deadline that passes between
            // these two reads also stops here.
            if deadline.left().is_zero() || sock.set_read_timeout(Some(deadline.left())).is_err() {
                return;
            }
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => seen.push(buf[0]),
            }
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        match accepted_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        match release_rx.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    (url, accepted_rx, release)
}
