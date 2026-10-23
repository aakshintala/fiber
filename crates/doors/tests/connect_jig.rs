//! The built connect jig's socket and named-hub journeys.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
use contract::{
    SessionId,
    clock::Clock,
    events::{ToolInfo, ToolSource, ToolState},
};
use doors::{Session, mint};
use fakes::MUST_SUCCEED_WITHIN;
use fakes::{Deadline, Watchdog, clock::FakeClock};
use log::Log;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::{net::UnixListener, process::CommandExt};
use std::path::Path;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

struct Jig {
    stdin: ChildStdin,
    lines: mpsc::Receiver<Value>,
    stderr: Option<mpsc::Receiver<String>>,
    exited: mpsc::Receiver<ExitStatus>,
    group: u32,
    watchdog: Option<Watchdog>,
    deadline: Deadline,
    reaped: bool,
}
/// The built `connect` example beside the test binary: the jig the tests drive.
fn example_executable() -> std::path::PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/connect")
}
impl Jig {
    fn spawn(home: &Path, args: &[&str]) -> Self {
        Self::spawn_inner(home, args, false)
    }
    /// [`spawn`](Self::spawn) with piped stderr: the fail-fast path's first
    /// line is what the silent-hub test waits for. Existing callers keep
    /// inheriting stderr.
    fn spawn_with_stderr(home: &Path, args: &[&str]) -> Self {
        Self::spawn_inner(home, args, true)
    }
    fn spawn_inner(home: &Path, args: &[&str], capture_stderr: bool) -> Self {
        let executable = example_executable();
        let mut child = Command::new(executable)
            .args(args)
            .env_clear()
            .envs(fakes::check_run())
            .env("FIBER_HOME", home)
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let watchdog = Some(Watchdog::group(group));
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (line_tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line_tx
                    .send(serde_json::from_str(&line.unwrap()).unwrap())
                    .is_err()
                {
                    break;
                }
            }
        });
        // Only the fail-fast path writes stderr, so the piped reader never
        // fills on success paths; the silent-hub test waits for its first line.
        let stderr = capture_stderr.then(|| {
            let pipe = child.stderr.take().unwrap();
            let (err_tx, stderr) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    if err_tx.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            stderr
        });
        let (exit_tx, exited) = mpsc::channel();
        thread::spawn(move || {
            let _sent = exit_tx.send(child.wait().unwrap());
        });
        Self {
            stdin,
            lines,
            stderr,
            exited,
            group,
            watchdog,
            deadline: Deadline::after(Duration::from_secs(10)),
            reaped: false,
        }
    }
    fn send(&mut self, command: Value) {
        writeln!(self.stdin, "{command}").unwrap();
    }
    /// The jig's first stderr line: the fail-fast line naming the blocked wait.
    fn first_stderr(&self) -> String {
        self.deadline.recv_or_fail(
            self.stderr
                .as_ref()
                .expect("a jig that names its blocked wait spawns with piped stderr"),
            "the jig's stderr names the blocked wait",
        )
    }
    fn until(&self, predicate: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut lines = Vec::new();
        loop {
            let line = self
                .deadline
                .recv_or_fail(&self.lines, "the jig's next output line");
            let last = predicate(&line);
            lines.push(line);
            if last {
                return lines;
            }
        }
    }
    fn wait(&mut self) {
        let status = self
            .deadline
            .recv_or_fail(&self.exited, "the jig exits when its socket closes");
        self.reaped = true;
        assert!(status.success(), "{status}");
        assert!(!fakes::kill_group(self.group, "0").unwrap());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    }
    /// Reaps a jig that must fail, as [`wait`](Self::wait) reaps one that must succeed.
    fn wait_fails(&mut self) {
        let status = self
            .deadline
            .recv_or_fail(&self.exited, "the jig exits after its stderr line");
        self.reaped = true;
        assert!(!status.success(), "a wedged hub is a failure: {status}");
        assert!(!fakes::kill_group(self.group, "0").unwrap());
        self.watchdog
            .take()
            .unwrap()
            .stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    }
}
impl Drop for Jig {
    fn drop(&mut self) {
        if !self.reaped {
            let _killed = fakes::kill_group(self.group, "KILL");
            let _reaped = Deadline::after(MUST_SUCCEED_WITHIN).recv(&self.exited);
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
        }
    }
}

#[test]
fn typed_lines_are_acknowledged_and_it_exits_when_the_socket_closes() {
    let temp = fakes::TempDir::new("fj");
    let home = temp.path();
    let sessions = home.join("projects/p/sessions");
    let id = SessionId(mint("s_"));
    let dir = sessions.join(&id.0);
    let clock: Arc<dyn Clock> = FakeClock::new();
    let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&clock)).unwrap());
    let session = Session::open(
        home,
        &dir,
        &log,
        clock,
        vec![ToolInfo {
            name: "read".into(),
            source: ToolSource::Builtin,
            state: ToolState::Full,
            bytes: 4,
            tokens: None,
        }],
        Box::new(std::io::sink()),
    )
    .unwrap();
    let socket = home.join("run").join(&id.0);
    let mut jig = Jig::spawn(home, &[socket.to_str().unwrap()]);
    session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            jig.send(json!({"id":"c_sub","command":"subscribe","args":{"level":"full"}}));
            jig.until(|line| line["payload"]["command_id"] == "c_sub");
            jig.send(json!({"id":"c_tools","command":"tools"}));
            let lines = jig.until(|line| line["payload"]["command_id"] == "c_tools");
            assert!(
                lines
                    .iter()
                    .any(|line| line.to_string().contains("\"name\":\"read\""))
            );
            Ok(())
        })
        .unwrap();
    let (done, done_rx) = mpsc::channel();
    thread::spawn(move || {
        session.close(log);
        done.send(()).unwrap();
    });
    jig.deadline.recv_or_fail(&done_rx, "the session closes");
    jig.wait();
}

fn hub_journey(listed: bool) {
    let temp = fakes::TempDir::new("fj");
    let home = temp.path();
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let socket = if listed {
        home.join("forwarded")
    } else {
        run.join("hub")
    };
    let listener = UnixListener::bind(&socket).unwrap();
    if listed {
        std::fs::write(
            home.join("config.json"),
            json!({"hubs":{"fwd":{"address":format!("unix:{}", socket.display())}}}).to_string(),
        )
        .unwrap();
    }
    let (done, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{}", json!({"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}})).unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"id":"c_1","command":"status"})
        );
        writeln!(stream, "{}", json!({"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_1"}})).unwrap();
        done.send(()).unwrap();
    });
    let args = if listed {
        vec!["--hub", "fwd"]
    } else {
        vec!["--fiber", "never-spawned"]
    };
    let mut jig = Jig::spawn(home, &args);
    let hello = jig.until(|line| line["kind"] == "hub_hello");
    assert_eq!(hello.len(), 1);
    jig.send(json!({"id":"c_1","command":"status"}));
    let answer = jig.until(|line| line["payload"]["command_id"] == "c_1");
    assert_eq!(answer.len(), 1);
    assert_eq!(answer[0]["kind"], "command_accepted");
    jig.deadline
        .recv_or_fail(&done_rx, "the hub peer completes");
    jig.wait();
}

#[test]
fn hub_mode_prints_hello_then_relays_typed_lines() {
    hub_journey(false);
}

#[test]
fn a_named_forwarded_hub_uses_the_same_client() {
    hub_journey(true);
}

/// A listed `fwd` hub that accepts one connection and then stays silent
/// until the test releases it: the jig's hello wait, not a closed socket,
/// is what the tests below bound. Returns the accept signal and the release.
fn silent_hub(home: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let socket = home.join("silent");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::write(
        home.join("config.json"),
        json!({"hubs":{"fwd":{"address":format!("unix:{}", socket.display())}}}).to_string(),
    )
    .unwrap();
    let (accepted, accepted_rx) = mpsc::channel();
    let (done, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        let _stream = stream;
        let _waited = Deadline::after(Duration::from_secs(10)).recv(&done_rx);
    });
    (accepted_rx, done)
}

/// A listed hub that accepts and then stays silent: the jig fails fast with
/// a stderr line naming the wait instead of hanging past the hang guard.
#[test]
fn a_listed_hub_that_stays_silent_fails_fast_with_a_stderr_line() {
    let temp = fakes::TempDir::new("fj");
    let home = temp.path();
    let (accepted_rx, done) = silent_hub(home);
    let mut jig = Jig::spawn_with_stderr(home, &["--hub", "fwd"]);
    jig.deadline
        .recv_or_fail(&accepted_rx, "the silent hub accepts");
    let line = jig.first_stderr();
    assert!(
        line.contains("hub_hello"),
        "the stderr line names the wait: {line}"
    );
    jig.wait_fails();
    done.send(()).unwrap();
}
