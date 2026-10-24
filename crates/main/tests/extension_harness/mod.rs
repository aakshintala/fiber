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
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use crate::support::{Deadline, KillGroup, Setup};
use fakes::Watchdog;
use serde_json::{Value, json};

impl Setup {
    pub(crate) fn new_with_fm_root() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
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
        crate::support::write_json(&source.join("extension.json"), &base);
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

    #[track_caller]
    pub(crate) fn start_session(&self, id: &str, extra: &[&str]) -> Running {
        self.start_session_stdin(id, extra).0
    }

    /// [`start_session`](Setup::start_session) with stdin piped: a delegate
    /// child exits when its stdin closes, so the caller holds the lifeline
    /// until the child exits on its own.
    pub(crate) fn start_session_stdin(
        &self,
        id: &str,
        extra: &[&str],
    ) -> (Running, Option<ChildStdin>) {
        let workspace = self.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        let mut command = self.fiber(&args);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let (end_tx, stdout_end) = mpsc::channel();
        thread::spawn(move || {
            let mut end = StdoutEnd::Eof;
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(text) => match tx.send(text) {
                        Ok(()) => {}
                        Err(mpsc::SendError(_)) => return,
                    },
                    Err(error) => {
                        end = StdoutEnd::ReadError(error.to_string());
                        break;
                    }
                }
            }
            end_tx.send(end).unwrap_or(());
        });
        let (err_tx, stderr_rx) = mpsc::channel();
        let stderr_snapshot = Arc::new(Mutex::new(String::new()));
        let stderr_copy = Arc::clone(&stderr_snapshot);
        thread::spawn(move || {
            let mut pipe = BufReader::new(stderr_pipe);
            let text = crate::support::drain_stderr_to_snapshot(&mut pipe, &stderr_copy);
            match err_tx.send(text) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        (
            Running {
                child,
                watchdog,
                group,
                guard,
                lines,
                stdout_end,
                stderr: stderr_rx,
                stderr_snapshot,
                first: Vec::new(),
                deadline: self.deadline,
            },
            stdin,
        )
    }
}

/// How the session stdout reader ended: an explicit EOF on a clean read
/// end, or an explicit error on a read failure. The stdout drain succeeds
/// only on EOF; a disconnect without EOF fails, so a reader panic can
/// never pass as a clean drain. This rides beside `lines` (left as
/// `Receiver<String>` for its direct readers) so no reader outside this
/// file changes.
enum StdoutEnd {
    Eof,
    ReadError(String),
}

pub(crate) struct Running {
    pub(crate) child: Child,
    pub(crate) watchdog: Watchdog,
    pub(crate) group: u32,
    pub(crate) guard: KillGroup,
    pub(crate) lines: mpsc::Receiver<String>,
    stdout_end: mpsc::Receiver<StdoutEnd>,
    pub(crate) stderr: mpsc::Receiver<String>,
    /// The stderr text so far, appended by the drain thread: the timeout
    /// dump reads it while the session still runs.
    pub(crate) stderr_snapshot: Arc<Mutex<String>>,
    pub(crate) first: Vec<String>,
    pub(crate) deadline: Deadline,
}

impl Running {
    #[track_caller]
    pub(crate) fn connect(&mut self, socket: &Path) -> Socket {
        let line = self.first_line();
        self.first.push(line);
        Socket::connect(self.deadline, socket)
            .expect("the session's socket accepted before the deadline")
    }

    #[track_caller]
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

    #[track_caller]
    fn first_line(&mut self) -> String {
        match self.deadline.recv(&self.lines) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        }
    }

    #[track_caller]
    pub(crate) fn wait_for(&mut self, kind: &str) {
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

    #[track_caller]
    pub(crate) fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let (status, raw, stderr) = self.wait_raw();
        let out = raw
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (status, out, stderr)
    }

    #[track_caller]
    fn wait_raw(mut self) -> (ExitStatus, Vec<String>, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match self.deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                crate::support::expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended before it exited")
            }
        };
        let mut lines = std::mem::take(&mut self.first);
        // Drain stdout to its end: the reader thread sends every line it
        // reads, then an explicit EOF on `stdout_end`, so receiving the
        // lines until their sender disconnects keeps the lines it had not
        // sent when the session exited, and the drain succeeds only on
        // the EOF that follows. A timeout, a read error, or a disconnect
        // without EOF fails naming the drain; a partial read never passes.
        loop {
            match self.deadline.recv(&self.lines) {
                Ok(line) => lines.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's remaining stdout")
                }
            }
        }
        match self.deadline.recv(&self.stdout_end) {
            Ok(StdoutEnd::Eof) => {}
            Ok(StdoutEnd::ReadError(error)) => {
                panic!("reading the session's remaining stdout: {error}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's stdout reader ended without EOF")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stdout end")
            }
        }
        let stderr = match self.deadline.recv(&self.stderr) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr reader ended without its text")
            }
        };
        assert!(
            !fakes::group_alive(self.group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(self.guard);
        self.watchdog.stand_down(self.deadline.cleanup_phase());
        (status, lines, stderr)
    }
}

/// A client on a session's socket. Every read and write takes what remains
/// of the test's [`Deadline`].
pub(crate) struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
    /// Context for the timeout dump, set once after connect at the sites
    /// that hold a session's stderr snapshot and log directory.
    diag: Option<crate::support::Diag>,
    deadline: Deadline,
}

impl Socket {
    /// Connects to `path` on a thread bounded by the deadline.
    #[track_caller]
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
            diag: None,
            deadline,
        })
    }

    /// Registers the timeout dump's context: the stderr snapshot the drain
    /// thread keeps and the session's log directory. Without it the timeout
    /// branch keeps the old message, so other `Socket` users are unaffected.
    pub(crate) fn set_diag(&mut self, diag: crate::support::Diag) {
        self.diag = Some(diag);
    }

    /// Replaces the socket's deadline: a test that proved one wait expires
    /// proves it on a zero or short bound instead of spending what remains
    /// of the test's own deadline (`docs/testing.md`, "Waits and timeouts").
    pub(crate) fn set_deadline(&mut self, deadline: Deadline) {
        self.deadline = deadline;
    }

    #[track_caller]
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

    pub(crate) fn next(&self, what: &str, got: &[Value]) -> Option<Value> {
        match crate::support::read_line(&mut self.read.lock().unwrap(), self.deadline, what) {
            Ok(None) => None,
            Ok(Some(buf)) => {
                let line = buf.trim_end_matches(&['\r', '\n'][..]).to_owned();
                Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => match &self.diag {
                Some(diag) => panic!("{}", diag.report(what, got)),
                None => panic!("waited until the deadline for {what}; got {got:?}"),
            },
            Err(error) => {
                panic!("reading the session socket while waiting for {what}: {error}")
            }
        }
    }
}

#[track_caller]
pub(crate) fn send(client: &Socket, line: &str) {
    client.send(line);
}

pub(crate) fn until_session(
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

pub(crate) fn until_session_close(client: &Socket) -> Vec<Value> {
    let mut lines = Vec::new();
    while let Some(line) = client.next("the session to close the socket", &lines) {
        lines.push(line);
    }
    lines
}

/// Re-exports `support`'s provider wire builders and event-kinds reader
/// under the names the harness users call.
#[allow(
    unused_imports,
    reason = "each test binary uses its own subset of the harness names"
)]
pub(crate) use crate::support::{hello, kinds, stream};
