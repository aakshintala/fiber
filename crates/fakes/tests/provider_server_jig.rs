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
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

#[path = "support/provider_server.rs"]
mod support;

/// The deadline on each wait: the jig's exchange, the jig printing the
/// request, and the jig's exit. A stall fails naming the wait
/// (`docs/testing.md`, "Waits and timeouts").
const DEADLINE: Duration = fakes::MUST_SUCCEED_WITHIN;

/// How long the jig may take to print its listening line, process start
/// included. The first exec of a freshly linked binary can stall under load,
/// past [`DEADLINE`], so the line gets this bound. This is the bound
/// `crates/loop/tests/review_jig.rs` gives a built binary's first start. The
/// exchange after the line keeps [`DEADLINE`].
const START_BOUND: Duration = Duration::from_secs(30);

const _: () = assert!(START_BOUND.as_secs() > DEADLINE.as_secs());

/// The signal the watchdog's group kill sends, the same on Linux and macOS.
/// No jig signals itself, so a jig that dies of it was still running.
const SIGKILL: i32 = 9;

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

/// The listening wait takes [`START_BOUND`]. Its one statement is found in
/// this file's source; a wait on [`DEADLINE`] instead leaves the count at
/// zero. The needle is split so this test's own text does not match it.
#[test]
fn the_listening_wait_takes_the_start_bound() {
    let source = include_str!("provider_server_jig.rs");
    let wait = concat!("Deadline::after(", "START_BOUND).recv(&rx)");
    assert_eq!(source.matches(wait).count(), 1);
}

/// A jig that exits before its line fails the listening wait at once. The
/// failure names the exit status and the stderr the jig wrote.
#[test]
fn a_jig_that_exits_before_its_line_names_its_status_and_stderr() {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg("echo boom >&2");
    let message = failure_message(command);
    assert!(
        message.contains("its state: exited, exit status: 0\n"),
        "{message}"
    );
    assert!(message.contains("its stderr: boom\n"), "{message}");
}

/// The panic message of a jig that fails [`serve_one_fixture`].
fn failure_message(command: Command) -> String {
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        serve_one_fixture(command);
    }))
    .expect_err("the jig's listening wait should fail");
    payload
        .downcast_ref::<String>()
        .cloned()
        .expect("a formatted panic message")
}

/// Starts the jig through `command`, which runs it with its fixture, then
/// checks one exchange and the request the jig printed.
fn serve_one_fixture(mut command: Command) {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    // The child is in its own process group, so dropping this kills the
    // group and never the test's own; no pid or `pkill` names it.
    let mut watchdog = Some(fakes::Watchdog::group(child.id()));
    let (tx, rx) = mpsc::channel();
    spawn_reader(child.stdout.take().unwrap(), tx);
    // Held until the function returns, so a jig that reads its standard
    // input blocks until the group kill, not until end-of-file.
    let _stdin = child.stdin.take();
    // One thread reaps the child and reads its stderr to end-of-file, so
    // the failure path and the success path take the same result.
    let mut stderr = child.stderr.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let status = child.wait();
        let mut errors = String::new();
        stderr.read_to_string(&mut errors).unwrap();
        match done.send((status, errors)) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });

    let listening = Deadline::after(START_BOUND).recv(&rx);
    let printed = listening.unwrap_or_else(|err| {
        // The kill ends a jig still running. A SIGKILL status is the kill's
        // own; any other status is the jig's exit before the kill.
        drop(watchdog.take());
        let (status, errors) = Deadline::after(DEADLINE).recv_or_fail(
            &finished,
            "the killed provider-server jig's exit and stderr",
        );
        let status = status.expect("reaping the killed provider-server jig");
        let state = if status.signal() == Some(SIGKILL) {
            "still running at its deadline".to_owned()
        } else {
            format!("exited, {status}")
        };
        panic!(
            "the provider-server jig printed its listening line: {err:?} \
             (bound {START_BOUND:?})\nits state: {state}\nits stderr: {errors}"
        );
    });
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
    drop(watchdog.take());
    let (status, errors) =
        Deadline::after(DEADLINE).recv_or_fail(&finished, "the provider-server jig to exit");
    status.expect("reaping the provider-server jig");
    // The child has exited, so every write end is closed and the reaper
    // read the buffered remainder to end-of-file.
    assert!(errors.is_empty(), "{errors}");
}
