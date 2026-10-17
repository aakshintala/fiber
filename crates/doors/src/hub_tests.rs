//! Tests for the hub client: the handshake classifiers, the closed-peer
//! read and the trickle. The entry points are covered through the public
//! API in `tests/hub.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;
use fakes::Deadline;

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::{DEADLINE, hello_line};

fn error(kind: io::ErrorKind) -> io::Error {
    io::Error::new(kind, "injected for the test")
}

#[test]
fn absent_is_not_found_or_connection_refused() {
    assert!(is_absent(io::ErrorKind::NotFound));
    assert!(is_absent(io::ErrorKind::ConnectionRefused));
    for kind in [
        io::ErrorKind::NotADirectory,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::InvalidData,
    ] {
        assert!(!is_absent(kind), "{kind:?}");
    }
}

#[test]
fn a_hello_timeout_is_would_block_or_timed_out() {
    assert!(is_hello_timeout(io::ErrorKind::WouldBlock));
    assert!(is_hello_timeout(io::ErrorKind::TimedOut));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::NotADirectory,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::InvalidData,
    ] {
        assert!(!is_hello_timeout(kind), "{kind:?}");
    }
}

#[test]
fn time_is_left_until_the_deadline() {
    assert!(!has_time_left(&Duration::ZERO));
    assert!(has_time_left(&Duration::from_nanos(1)));
    assert!(has_time_left(&Duration::from_secs(5)));
}

#[test]
fn a_carriage_return_is_stripped() {
    assert!(strips_cr(Some(&b'\r')));
    assert!(!strips_cr(Some(&b'\n')));
    assert!(!strips_cr(Some(&b'x')));
    assert!(!strips_cr(None));
}

#[test]
fn only_another_kind_is_rejected() {
    assert!(!rejects_hello("hub_hello"));
    assert!(rejects_hello("command_accepted"));
    assert!(rejects_hello("hub_hello "));
    assert!(rejects_hello(""));
}

#[test]
fn only_another_schema_version_is_rejected() {
    assert!(!rejects_schema(contract::SCHEMA_VERSION));
    assert!(rejects_schema(contract::SCHEMA_VERSION + 1));
    assert!(rejects_schema(contract::SCHEMA_VERSION.wrapping_sub(1)));
}

#[test]
fn only_the_closed_peer_refusal_is_ignored() {
    assert!(is_closed_peer(io::ErrorKind::InvalidInput));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::PermissionDenied,
    ] {
        assert!(!is_closed_peer(kind), "{kind:?}");
    }
}

#[test]
fn a_handshake_read_is_classified() {
    assert!(matches!(classify_hello_read(Ok(0)), HelloRead::Race));
    assert!(matches!(classify_hello_read(Ok(1)), HelloRead::Byte));
    assert!(matches!(classify_hello_read(Ok(100)), HelloRead::Byte));
    assert!(matches!(
        classify_hello_read(Err(error(io::ErrorKind::WouldBlock))),
        HelloRead::TimedOut
    ));
    assert!(matches!(
        classify_hello_read(Err(error(io::ErrorKind::TimedOut))),
        HelloRead::TimedOut
    ));
    assert!(matches!(
        classify_hello_read(Err(error(io::ErrorKind::ConnectionReset))),
        HelloRead::Race
    ));
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::UnexpectedEof,
    ] {
        assert!(
            matches!(classify_hello_read(Err(error(kind))), HelloRead::Failed(_)),
            "{kind:?}"
        );
    }
}

#[test]
fn only_the_closed_peer_refusal_is_folded_away() {
    assert!(ignore_closed_peer(Ok(())).is_ok());
    assert!(ignore_closed_peer(Err(error(io::ErrorKind::InvalidInput))).is_ok());
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
    ] {
        let folded = ignore_closed_peer(Err(error(kind)));
        assert_eq!(folded.unwrap_err().kind(), kind, "{kind:?}");
    }
}

#[test]
fn a_hello_from_a_peer_that_already_closed_is_read() {
    // Setting or clearing the read timeout on a socket whose peer has
    // closed fails on macOS, while the buffered hello stays readable.
    let (mut peer, reader) = UnixStream::pair().unwrap();
    peer.write_all(&hello_line()).unwrap();
    peer.write_all(b"\n").unwrap();
    drop(peer);
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now().checked_add(DEADLINE);
    let got = read_hello(
        reader,
        Path::new("run/hub"),
        deadline,
        DEADLINE,
        &*clock,
        &mut || {},
    );
    let Ok((stream, hello)) = got else {
        panic!("the buffered hello is read");
    };
    assert_eq!(hello.kind, "hub_hello");
    assert_eq!(stream.read_timeout().unwrap(), None);
}

#[test]
fn a_trickling_hub_cannot_extend_the_handshake_deadline() {
    let (mut peer, reader) = UnixStream::pair().unwrap();
    let clock = fakes::clock::FakeClock::new();
    let (read_tx, read_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-trickle".to_owned())
        .spawn({
            let clock = Arc::clone(&clock);
            move || {
                let mut before_read = || read_tx.send(()).unwrap_or(());
                let deadline = clock.now().checked_add(DEADLINE);
                let got = read_hello(
                    reader,
                    Path::new("run/hub"),
                    deadline,
                    DEADLINE,
                    &*clock,
                    &mut before_read,
                );
                done_tx.send(got).unwrap_or(());
            }
        })
        .unwrap();
    let line = hello_line();
    peer.write_all(&line[..1]).unwrap();
    let wait = Deadline::after(Duration::from_secs(5));
    for n in 1..=2 {
        wait.recv(&read_rx)
            .unwrap_or_else(|_| panic!("the reader is about to read for the {n}th time"));
    }
    // The reader read the first byte and checked the time left for the
    // next: at the deadline now, with no time left, so the next byte ends
    // the handshake.
    clock.advance(DEADLINE);
    peer.write_all(&line[1..2]).unwrap();
    let got = Deadline::after(Duration::from_secs(5))
        .recv(&done_rx)
        .expect("the reader returns once the deadline has passed");
    let Err(Poll::Failed(error)) = got else {
        panic!("a trickle past the deadline fails");
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        read_rx.try_recv().is_err(),
        "no read after the deadline passed"
    );
}

#[test]
fn only_eagain_is_busy_and_only_einprogress_is_in_flight() {
    assert!(is_busy(&rustix::io::Errno::AGAIN));
    assert!(is_in_progress(&rustix::io::Errno::INPROGRESS));
    assert!(!is_busy(&rustix::io::Errno::INPROGRESS));
    assert!(!is_in_progress(&rustix::io::Errno::AGAIN));
    for errno in [
        rustix::io::Errno::NOENT,
        rustix::io::Errno::CONNREFUSED,
        rustix::io::Errno::TIMEDOUT,
    ] {
        assert!(!is_busy(&errno), "{errno:?}");
        assert!(!is_in_progress(&errno), "{errno:?}");
    }
}

/// A home with a `run/` directory and no hub yet.
fn unit_home() -> support::Temp {
    let temp = support::Temp::new();
    std::fs::create_dir_all(temp.0.join("run")).unwrap();
    temp
}

/// Runs the private probe on a worker thread, handing back its answer and
/// the thread's id, so the test steps the fake clock naming that thread.
fn spawn_probe(
    home: std::path::PathBuf,
    clock: Arc<fakes::clock::FakeClock>,
    deadline: std::time::Instant,
    mut accepted: impl FnMut() + Send + 'static,
    mut before_wait: impl FnMut() + Send + 'static,
) -> (
    mpsc::Receiver<Probe>,
    std::thread::ThreadId,
    thread::JoinHandle<()>,
) {
    let (id_tx, id_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("hub-test-probe".to_owned())
        .spawn(move || {
            id_tx.send(thread::current().id()).unwrap_or(());
            done_tx
                .send(probe_with(
                    &home,
                    &*clock,
                    deadline,
                    &mut accepted,
                    &mut before_wait,
                ))
                .unwrap_or(());
        })
        .unwrap();
    let id = Deadline::after(DEADLINE)
        .recv(&id_rx)
        .expect("the probe worker reports its thread");
    (done_rx, id, worker)
}

#[test]
fn a_probe_with_no_socket_reports_absence() {
    let temp = unit_home();
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    assert!(matches!(
        probe_with(&temp.0, &*clock, deadline, &mut || {}, &mut || {}),
        Probe::Absent
    ));
}

#[test]
fn a_silent_probe_waits_each_park_and_connects_err() {
    use std::os::unix::net::UnixListener;
    let temp = unit_home();
    let socket = temp.0.join("run").join("hub");
    let listener = UnixListener::bind(&socket).unwrap();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let (silent, _) = listener.accept().unwrap();
        release_rx.recv().unwrap_or(());
        drop(silent);
    });
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let deadline = origin + Duration::from_millis(50);
    let waits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (done_rx, id, worker) = spawn_probe(temp.0.clone(), Arc::clone(&clock), deadline, || {}, {
        let waits = Arc::clone(&waits);
        move || {
            waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    clock.advance_when_parked(
        Some(deadline),
        Some(id),
        Duration::from_millis(50),
        &Deadline::after(DEADLINE),
    );
    match Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the silent probe answers")
    {
        Probe::Connected(Err(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(
                error.to_string(),
                format!(
                    "the hub at {} accepted but did not say hub_hello in 0.05 s",
                    socket.display()
                )
            );
        }
        Probe::Connected(Ok(_)) => panic!("a silent hub has no hello"),
        Probe::Absent => panic!("an accepted hub is not absent"),
        Probe::Failed(error) => panic!("an accepted hub is not failed: {error}"),
    }
    assert_eq!(clock.now(), deadline);
    assert!(
        waits.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the silent wait parks at least once"
    );
    release_tx.send(()).unwrap_or(());
    worker.join().unwrap();
}

#[test]
fn acceptance_runs_at_connect_time_before_the_hello_check() {
    use std::os::unix::net::UnixListener;
    let temp = unit_home();
    let listener = UnixListener::bind(temp.0.join("run").join("hub")).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(&hello_line()).unwrap();
        stream.write_all(b"\n").unwrap();
    });
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (done_rx, _, worker) = spawn_probe(
        temp.0.clone(),
        Arc::clone(&clock),
        deadline,
        {
            let order = Arc::clone(&order);
            move || order.lock().unwrap().push("accepted")
        },
        || {},
    );
    match Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the hello probe answers")
    {
        Probe::Connected(Ok((_, hello))) => assert_eq!(hello.kind, "hub_hello"),
        Probe::Connected(Err(error)) => panic!("a hello connects: {error}"),
        Probe::Absent => panic!("a hello is not absent"),
        Probe::Failed(error) => panic!("a hello connects: {error}"),
    }
    assert_eq!(order.lock().unwrap().as_slice(), ["accepted"]);
    worker.join().unwrap();
}

#[test]
fn a_wrong_version_probe_connects_err_naming_both() {
    use std::os::unix::net::UnixListener;
    let temp = unit_home();
    let listener = UnixListener::bind(temp.0.join("run").join("hub")).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let line = format!(
            "{{\"kind\":\"hub_hello\",\"ts\":1,\"schema_version\":{},\"payload\":{{}}}}",
            contract::SCHEMA_VERSION + 1
        );
        stream.write_all(line.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
    });
    let clock = fakes::clock::FakeClock::new();
    let deadline = clock.now() + DEADLINE;
    let (done_rx, _, worker) =
        spawn_probe(temp.0.clone(), Arc::clone(&clock), deadline, || {}, || {});
    match Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the wrong-version probe answers")
    {
        Probe::Connected(Err(error)) => {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(
                error.to_string().contains("schema version"),
                "the refusal names both versions: {error}"
            );
        }
        Probe::Connected(Ok(_)) => panic!("another schema version is refused"),
        Probe::Absent => panic!("an accepted hub is not absent"),
        Probe::Failed(error) => panic!("an accepted hub is not failed: {error}"),
    }
    worker.join().unwrap();
}
