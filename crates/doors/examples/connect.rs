//! The `connect` jig (`docs/testing.md`, "Jigs"): connects to a session's
//! socket and sends the JSON commands typed on stdin, printing what comes back.
//!
//! `cargo run -p doors --example connect -- <socket path>`
//! `cargo run -p doors --example connect -- --hub <fiber binary>`: home
//! from `FIBER_HOME`, else `$HOME/.fiber`. It starts the hub when none
//! runs, prints `hub_hello`, then behaves as with a socket path.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

const USAGE: &str = "usage: cargo run -p doors --example connect -- <socket path>\n       cargo run -p doors --example connect -- --hub <fiber binary>";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--hub") => match args.next() {
            Some(binary) => match hub_main(&binary) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("connect: {error}");
                    ExitCode::FAILURE
                }
            },
            None => {
                eprintln!("{USAGE}");
                ExitCode::from(2)
            }
        },
        Some(path) => match connect(Path::new(path), io::stdin(), &mut io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("connect: {error}");
                ExitCode::FAILURE
            }
        },
        None => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The process clock behind `contract::clock::Clock`.
struct SystemClock;

impl contract::clock::Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: std::time::Duration) {
        thread::sleep(d);
    }

    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<std::time::Duration>),
    ) {
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// Home from `FIBER_HOME`, else `$HOME/.fiber`.
fn fiber_home() -> io::Result<PathBuf> {
    if let Some(home) = std::env::var_os("FIBER_HOME") {
        return Ok(PathBuf::from(home));
    }
    match std::env::var_os("HOME") {
        Some(home) => {
            let mut dir = PathBuf::from(home);
            dir.push(".fiber");
            Ok(dir)
        }
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither FIBER_HOME nor HOME names Fiber home",
        )),
    }
}

fn hub_main(binary: &str) -> io::Result<()> {
    let home = fiber_home()?;
    hub_session(&home, binary, &SystemClock, io::stdin(), &mut io::stdout())
}

/// Connects to the hub in `home`, starting `<binary> hub serve` when none
/// runs, prints `hub_hello`, then behaves as with a socket path.
fn hub_session(
    home: &Path,
    binary: &str,
    clock: &dyn contract::clock::Clock,
    stdin: impl Read + Send + 'static,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let binary = binary.to_owned();
    let hub = doors::hub::connect(home, &mut move || start_hub(&binary), clock)?;
    let mut hello = serde_json::to_vec(&hub.hello)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    hello.push(b'\n');
    stdout.write_all(&hello)?;
    stdout.flush()?;
    relay(hub.stream, stdin, stdout)
}

/// Starts `<binary> hub serve` in its own process group with null stdio.
/// The hub it starts is reaped by a thread.
fn start_hub(binary: &str) -> io::Result<()> {
    let mut child = Command::new(binary)
        .arg("hub")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    thread::Builder::new()
        .name("connect-reap".to_owned())
        .spawn(move || match child.wait() {
            Ok(_) | Err(_) => {}
        })
        .map_err(|error| io::Error::new(error.kind(), format!("reap: {error}")))?;
    Ok(())
}

/// Sends each line of `stdin` to the socket at `path`, as typed, and writes
/// each line received to `stdout`. Returns when the socket closes. It does
/// not subscribe on its own.
fn connect(
    path: &Path,
    stdin: impl Read + Send + 'static,
    stdout: &mut impl Write,
) -> io::Result<()> {
    relay(UnixStream::connect(path)?, stdin, stdout)
}

fn relay(
    stream: UnixStream,
    stdin: impl Read + Send + 'static,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    let incoming = thread::Builder::new()
        .name("connect-stdin".to_owned())
        .spawn(move || copy_stdin(stdin, &mut writer))?;
    // The socket closing must not wait on stdin: this thread is still blocked
    // on a read when the function returns.
    drop(incoming);

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => stdout.write_all(line.as_bytes())?,
        }
    }
    stdout.flush()
}

fn copy_stdin(stdin: impl Read, writer: &mut UnixStream) {
    let mut stdin = BufReader::new(stdin);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match stdin.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if writer.write_all(&buf).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    use contract::SessionId;
    use contract::clock::Clock;
    use contract::events::{ToolInfo, ToolSource, ToolState};
    use doors::{Session, mint};
    use fakes::clock::FakeClock;
    use log::Log;

    use super::{connect, hub_session};

    const DEADLINE: Duration = Duration::from_secs(10);

    struct Temp(
        PathBuf,
        #[expect(dead_code, reason = "Drop removes the directory")] fakes::TempDir,
    );

    impl Temp {
        fn new() -> Self {
            let held = fakes::TempDir::new("fd");
            let dir = held.path().to_path_buf();
            Self(dir, held)
        }
    }

    #[derive(Clone)]
    struct Out {
        buf: Arc<Mutex<Vec<u8>>>,
        ready: Arc<Condvar>,
    }

    impl Out {
        fn new() -> Self {
            Self {
                buf: Arc::new(Mutex::new(Vec::new())),
                ready: Arc::new(Condvar::new()),
            }
        }

        fn wait_for(&self, needle: &str) {
            let guard = self.buf.lock().unwrap();
            let (guard, _) = self
                .ready
                .wait_timeout_while(guard, DEADLINE, |buf| {
                    !String::from_utf8_lossy(buf).contains(needle)
                })
                .unwrap();
            assert!(
                String::from_utf8_lossy(&guard).contains(needle),
                "never printed {needle}"
            );
        }

        fn text(&self) -> String {
            String::from_utf8(self.buf.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for Out {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(buf);
            self.ready.notify_all();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn typed_lines_are_acknowledged_and_it_exits_when_the_socket_closes() {
        let temp = Temp::new();
        let home = temp.0.join("h");
        let sessions = home.join("projects/p/sessions");
        let id = SessionId(mint("s_"));
        let dir = sessions.join(&id.0);
        let clock = FakeClock::new();
        let timed = Arc::clone(&clock);
        let timed: Arc<dyn Clock> = timed;
        let log = Arc::new(Log::create(&sessions, id.clone(), Arc::clone(&timed)).unwrap());
        let session = Session::open(
            &home,
            &dir,
            &log,
            timed,
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
        let (mut typed, stdin) = std::os::unix::net::UnixStream::pair().unwrap();
        let printed = Out::new();
        let mut stdout = printed.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let result = connect(&socket, stdin, &mut stdout);
            if let Ok(()) = done_tx.send(result) {}
        });

        session
            .run(Vec::new(), Arc::new(|| false), |_inbox| {
                writeln!(
                    typed,
                    r#"{{"id":"c_sub","command":"subscribe","args":{{"level":"full"}}}}"#
                )
                .unwrap();
                printed.wait_for("\"command_id\":\"c_sub\"");
                writeln!(typed, r#"{{"id":"c_tools","command":"tools"}}"#).unwrap();
                printed.wait_for("\"command_id\":\"c_tools\"");
                Ok(())
            })
            .unwrap();
        let text = printed.text();
        assert!(text.contains("\"kind\":\"command_accepted\""));
        assert!(text.contains("\"name\":\"read\""));
        // `typed` is still open: stdin has not ended.
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            session.close(log);
            if let Ok(()) = closed_tx.send(()) {}
        });
        closed_rx.recv_timeout(DEADLINE).expect("close returned");
        done_rx
            .recv_timeout(DEADLINE)
            .expect("the jig returns when the socket closes")
            .expect("the jig returns when the socket closes");
        drop(typed);
        drop(temp);
    }

    #[test]
    fn hub_mode_prints_hello_then_relays_typed_lines() {
        let temp = Temp::new();
        let home = temp.0.join("h");
        let run = home.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let listener = UnixListener::bind(run.join("hub")).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"{\"kind\":\"hub_hello\",\"ts\":1,\"schema_version\":1,\"payload\":{\"fiber_version\":\"0.0.0\"}}\n")
                .unwrap();
            stream.flush().unwrap();
            // One typed line, answered, then the socket closes.
            let mut read = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            assert!(line.contains("\"command\":\"status\""));
            stream
                .write_all(b"{\"kind\":\"command_accepted\",\"ts\":1,\"schema_version\":1,\"payload\":{\"command_id\":\"c_1\"}}\n")
                .unwrap();
            stream.flush().unwrap();
        });
        let (mut typed, stdin) = std::os::unix::net::UnixStream::pair().unwrap();
        let printed = Out::new();
        let mut stdout = printed.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            // The hub already runs: the starter must rest.
            let result = hub_session(
                &home,
                "never-spawned",
                &*FakeClock::new(),
                stdin,
                &mut stdout,
            );
            if let Ok(()) = done_tx.send(result) {}
        });
        printed.wait_for("\"kind\":\"hub_hello\"");
        writeln!(typed, r#"{{"id":"c_1","command":"status"}}"#).unwrap();
        printed.wait_for("\"command_id\":\"c_1\"");
        done_rx
            .recv_timeout(DEADLINE)
            .expect("the jig returns when the socket closes")
            .expect("the jig returns when the socket closes");
        drop(typed);
        drop(temp);
    }
}
