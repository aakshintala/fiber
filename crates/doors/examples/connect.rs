//! The `connect` jig (`docs/testing.md`, "Jigs"): connects to a session's
//! socket and sends the JSON commands typed on stdin, printing what comes back.
//!
//! `cargo run -p doors --example connect -- <socket path>`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::thread;

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: cargo run -p doors --example connect -- <socket path>");
        return ExitCode::from(2);
    };
    match connect(Path::new(&path), io::stdin(), &mut io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("connect: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Sends each line of `stdin` to the socket at `path`, as typed, and writes
/// each line received to `stdout`. Returns when the socket closes. It does
/// not subscribe on its own.
fn connect(
    path: &Path,
    stdin: impl Read + Send + 'static,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let stream = UnixStream::connect(path)?;
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

    use super::connect;

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
            .run(Vec::new(), |_inbox| {
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
}
