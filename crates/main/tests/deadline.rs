//! Tests of the binary-level tests' shared deadline (`docs/testing.md`,
//! "Waits and timeouts") through the socket and process helpers in
//! `support`: each wait gets its own bound, made when the wait starts,
//! capped by the test's guard. A first helper wait consuming 40 s of guard
//! leaves the second its full 40 s; refusal cases spend the guard, not a
//! single wait. The deadline's own arithmetic lives in `fakes::deadline`'s
//! tests, on a fake clock; the live-process probe below waits on real
//! processes, so it takes its deadline from the process clock, which is the
//! only clock those waits advance.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::io::{BufReader, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::thread;

use fakes::clock::FakeClock;
use support::*;

/// A fake clock that lives as long as the test process, as a deadline's
/// clock must.
fn fake_clock() -> &'static Arc<FakeClock> {
    Box::leak(Box::new(FakeClock::new()))
}

/// Spawns `sleep 60` in its own process group.
fn sleeper() -> (std::process::Child, u32, KillGroup) {
    let child = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    (child, group, KillGroup(group))
}

#[test]
fn a_first_helper_wait_leaves_the_second_its_full_bound() {
    // One wait's own bound carries one line: the rest of the stream waits
    // for the next wait, not for the remainder of this one.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"a\n").unwrap();
    let mut reader = BufReader::new(mine);
    assert_eq!(
        read_line(&mut reader, deadline, "the first line").unwrap(),
        Some("a\n".to_owned())
    );
    // 40 s of the guard pass with no wait open. Socket reads wait on the
    // wall clock, never on this clock, so time moves here by hand.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAIT);
    // The next wait still gets its full fresh bound, not the remainder.
    assert_eq!(deadline.left(), fakes::deadline::WAIT);
    assert_eq!(
        deadline.start_wait().left(),
        fakes::deadline::WAIT,
        "a wait started late keeps its full bound"
    );
    // ... through the helpers too: the second line arrives at once.
    peer.write_all(b"c\n").unwrap();
    assert_eq!(
        read_line(&mut reader, deadline, "the second line").unwrap(),
        Some("c\n".to_owned())
    );

    // Writes: delivered with time left.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mut mine, peer) = UnixStream::pair().unwrap();
    write_line(&mut mine, deadline, b"hi\n", "a line").unwrap();
    let mut got = [0; 3];
    (&peer).read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hi\n");
}

#[test]
fn a_spent_guard_refuses_without_reading_or_writing() {
    // Past the guard, a read fails naming its wait without reading: the
    // peer holds a partial line whose rest never arrives.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"b").unwrap();
    let mut reader = BufReader::new(mine);
    // Socket reads wait on the wall clock, never on this clock, so the
    // guard is spent here by hand.
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    let error = read_line(&mut reader, deadline, "the refused line").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error.to_string().contains("the refused line"), "{error}");

    // Past the guard, a write fails naming its wait without writing.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mut mine, peer) = UnixStream::pair().unwrap();
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    let error = write_line(&mut mine, deadline, b"no\n", "a late line").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error.to_string().contains("a late line"), "{error}");
    drop(mine);
    let mut rest = Vec::new();
    (&peer).read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "nothing was written at zero: {rest:?}");
}

#[test]
fn a_closed_peer_is_the_end_of_the_stream() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mine, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let mut reader = BufReader::new(mine);
    assert_eq!(read_line(&mut reader, deadline, "a line").unwrap(), None);
}

#[test]
fn bounded_returns_or_expires() {
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    assert_eq!(bounded(deadline, "a quick answer", || 5), 5);
    // Past the guard, the blocking call expires at once instead of taking
    // a fresh wait. No code under test waits on the clock here; the clock
    // only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(fakes::deadline::WAITS);
    let (_never, blocked) = mpsc::channel::<()>();
    let expired = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        bounded(deadline, "a reply nobody sends", move || blocked.recv())
    }))
    .unwrap_err();
    let message = expired
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(message.contains("a reply nobody sends"), "{message}");
}

#[test]
fn a_probe_on_a_live_group_is_bounded() {
    // A wait on a live process advances only the wall clock, so this probe
    // takes the test's process-clock deadline: each wait takes a fresh
    // bound under the guard, and the guard outlives the reaps below
    // (`docs/testing.md`, "Waits and timeouts").
    let deadline = Deadline::start();
    let (mut child, group, _guard) = sleeper();
    assert!(fakes::group_alive(group));
    assert!(fakes::kill_group(group, "KILL").unwrap());
    let (done, reaped) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    assert!(
        deadline.cleanup_phase().recv(&reaped).is_ok(),
        "waited until the deadline for the killed sleep to be reaped"
    );
    assert!(fakes::group_empties(group, deadline.cleanup_phase()));
    assert!(!fakes::group_alive(group));

    let (mut child, group, _guard) = sleeper();
    drop(fakes::kill_group(group, "KILL"));
    let (done, reaped) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    assert!(
        deadline.cleanup_phase().recv(&reaped).is_ok(),
        "waited until the deadline for the detached kill's sleep to be reaped"
    );
    assert!(fakes::group_empties(group, deadline.cleanup_phase()));
}
