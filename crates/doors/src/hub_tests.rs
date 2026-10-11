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
    for kind in [
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
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
