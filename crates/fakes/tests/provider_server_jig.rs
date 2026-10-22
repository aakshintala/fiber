//! The `provider-server` jig through its built binary: it prints
//! `listening on <url>`, serves one fixture file, and prints the request
//! it received.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers outside a #[test]; a failure is the test's"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

#[path = "support/provider_server.rs"]
mod support;

/// The deadline on each wait: the jig printing its address, the client's
/// exchange, and the jig printing the request. A stall fails naming the
/// wait (`docs/testing.md`, "Waits and timeouts").
const DEADLINE: Duration = fakes::MUST_SUCCEED_WITHIN;

/// How long the jig may take to print its listening line, process start
/// included: the first exec of a freshly linked binary can stall under
/// load. This is the bound `crates/loop/tests/review_jig.rs` gives a built
/// binary's first start. The exchange after the line keeps [`DEADLINE`].
const START_BOUND: Duration = Duration::from_secs(30);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/provider-server`), so the test starts no
/// cargo and waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("provider-server")
}

/// Reads the jig's stdout: first its `listening on` line, then one request
/// block, which ends at the blank line. Each send is best-effort: the test
/// may already have failed its deadline and gone away. A jig that closes its
/// stdout before the line drops the sender, so the wait fails at once.
fn spawn_reader(stdout: ChildStdout, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut lines = BufReader::new(stdout).lines();
        let listening = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(_)) | None => return,
        };
        match tx.send(listening) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
        let mut block = String::new();
        for line in lines {
            let line = line.unwrap_or_default();
            if line.is_empty() {
                break;
            }
            block.push_str(&line);
            block.push('\n');
        }
        match tx.send(block) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
}

/// One exchange over a plain client: connect, write the whole request, read
/// the reply to the end of the connection. A client thread does the I/O
/// and the test receives it under one deadline, so a stall names the wait
/// instead of hanging the test. The sockets take timeouts, so a failure
/// path never leaves a blocked client behind.
#[track_caller]
fn exchange(addr: SocketAddr, request: Vec<u8>) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    let client = thread::spawn(move || {
        let steps = || -> std::io::Result<Vec<u8>> {
            let mut stream = TcpStream::connect_timeout(&addr, DEADLINE)?;
            stream.set_read_timeout(Some(DEADLINE))?;
            stream.set_write_timeout(Some(DEADLINE))?;
            stream.write_all(&request)?;
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw)?;
            Ok(raw)
        };
        match tx.send(steps().map_err(|e| e.to_string())) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    let wait = Deadline::after(DEADLINE);
    let raw = match wait.recv(&rx) {
        Ok(Ok(raw)) => raw,
        Ok(Err(e)) => panic!("the jig's reply failed: {e}"),
        Err(_) => panic!("the jig's reply: not done within {DEADLINE:?}"),
    };
    match client.join() {
        Ok(()) | Err(_) => {}
    }
    raw
}

#[test]
fn it_serves_one_fixture_and_prints_the_request_it_received() {
    let dir = fakes::TempDir::new("provider-server-jig");
    let fixture = dir.path().join("reply.sse");
    std::fs::write(&fixture, "data: hi\n\n").unwrap();
    let mut command = Command::new(example_path());
    command.arg(&fixture);
    serve_one_fixture(command);
}

/// The reported flake: the built binary's first start outlasts the wait for
/// its listening line. `/bin/sh` holds the jig on a FIFO until the test
/// releases it. The release comes after the old bound, so the wait for the
/// line expires first.
#[test]
fn a_slow_process_start_still_prints_the_listening_line() {
    let dir = fakes::TempDir::new("provider-server-jig");
    let fixture = dir.path().join("reply.sse");
    std::fs::write(&fixture, "data: hi\n\n").unwrap();
    let gate = dir.path().join("start");
    fakes::fifo(&gate);
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("read line < \"$0\"; exec \"$@\"")
        .arg(&gate)
        .arg(example_path())
        .arg(&fixture);
    // The deadline is made here, before the jig starts, so the release
    // follows the listening wait's own bound. The sender stays alive, so the
    // receive ends at the deadline and not at a disconnect.
    let hold = Deadline::after(fakes::MUST_SUCCEED_WITHIN + Duration::from_secs(1));
    thread::spawn(move || {
        let (_keep, never) = mpsc::channel::<()>();
        match hold.recv(&never) {
            Ok(()) | Err(_) => {}
        }
        let mut release = std::fs::OpenOptions::new().write(true).open(&gate).unwrap();
        release.write_all(b"go\n").unwrap();
    });
    serve_one_fixture(command);
}

/// A jig that exits before its line fails the listening wait at once, and
/// the failure names the stderr it wrote. The child exits, so its output is
/// in the pipe before the wait fails: no timing decides the expected text.
#[test]
#[should_panic(expected = "its stderr: boom")]
fn a_jig_that_exits_before_its_line_names_its_stderr() {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("echo boom >&2");
    serve_one_fixture(command);
}

/// Starts the jig through `command`, which runs it with its fixture, then
/// checks one exchange and the request the jig printed.
fn serve_one_fixture(mut command: Command) {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // The child is in its own process group, so dropping this kills the
    // group and never the test's own; no pid or `pkill` names it.
    let watchdog = fakes::Watchdog::group(child.id());
    let (tx, rx) = mpsc::channel();
    spawn_reader(child.stdout.take().unwrap(), tx);
    let mut stderr = child.stderr.take().unwrap();

    let printed = match Deadline::after(START_BOUND).recv(&rx) {
        Ok(printed) => printed,
        Err(err) => {
            // The group kill closes the jig's stderr, so its read ends.
            drop(watchdog);
            let errors = fakes::within(
                "the killed jig's stderr",
                Deadline::after(DEADLINE),
                move || {
                    let mut text = String::new();
                    stderr.read_to_string(&mut text).unwrap();
                    text
                },
            );
            panic!(
                "the provider-server jig printed its listening line: {err:?} \
                 within {START_BOUND:?}\nits stderr: {errors}"
            );
        }
    };
    assert!(printed.starts_with("listening on http://"), "{printed}");
    let addr: SocketAddr = printed
        .trim_start_matches("listening on http://")
        .parse()
        .unwrap();

    let request =
        b"POST /v1/messages HTTP/1.1\r\nHost: fake\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
            .to_vec();
    let raw = exchange(addr, request);
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let status: u16 = head
        .split("\r\n")
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(raw[split + 4..].to_vec(), b"data: hi\n\n".to_vec());

    let block = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the provider-server jig printed the request");
    assert_eq!(
        block,
        "POST /v1/messages\nhost: fake\ncontent-length: 2\nconnection: close\n{}\n"
    );

    // The child served its fixture: killing its group ends it.
    drop(watchdog);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => panic!("reaping the provider-server jig failed: {e}"),
        Err(_) => panic!("the provider-server jig: not reaped within {DEADLINE:?}"),
    }
    // The child has exited, so every write end is closed and this reads
    // the buffered remainder to end-of-file.
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();
    assert!(errors.is_empty(), "{errors}");
}
