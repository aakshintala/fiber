//! Tests of the binary-level tests' shared deadline (`docs/testing.md`,
//! "Waits and timeouts") through the socket and process helpers in
//! `support`: each wait takes what remains of the test's one deadline.
//! The deadline's own arithmetic lives in `fakes::deadline`'s tests, on a
//! fake clock; the live-process probe below waits on real processes, so it
//! takes its deadline from the process clock, which is the only clock those
//! waits advance.

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
use std::time::Duration;

use fakes::clock::FakeClock;
use support::*;

/// A fake clock that lives as long as the test process, as a deadline's
/// clock must.
fn fake_clock() -> &'static Arc<FakeClock> {
    Box::leak(Box::new(FakeClock::new()))
}

/// Kills process group `group` on drop, without waiting.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        kill_group_detached(self.0, "KILL");
    }
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
fn a_socket_line_takes_what_remains_and_no_more() {
    // A partial line: the rest never arrives, and no time is left for it.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"a\nb").unwrap();
    let mut reader = BufReader::new(mine);
    assert_eq!(
        read_line(&mut reader, deadline, "the first line").unwrap(),
        Some("a\n".to_owned())
    );
    clock.advance(Duration::from_secs(40));
    let error = read_line(&mut reader, deadline, "the second line").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error.to_string().contains("the second line"), "{error}");

    // A buffered line: it is taken at zero, without reading.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mine, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"a\nc\n").unwrap();
    let mut reader = BufReader::new(mine);
    assert_eq!(
        read_line(&mut reader, deadline, "the first line").unwrap(),
        Some("a\n".to_owned())
    );
    clock.advance(Duration::from_secs(40));
    assert_eq!(
        read_line(&mut reader, deadline, "the buffered line").unwrap(),
        Some("c\n".to_owned())
    );

    // Writes: delivered with time left, refused at zero.
    let clock = fake_clock();
    let deadline = Deadline::on(&**clock);
    let (mut mine, peer) = UnixStream::pair().unwrap();
    write_line(&mut mine, deadline, b"hi\n", "a line").unwrap();
    let mut got = [0; 3];
    (&peer).read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hi\n");
    clock.advance(Duration::from_secs(40));
    let error = write_line(&mut mine, deadline, b"no\n", "a late line").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error.to_string().contains("a late line"), "{error}");
    drop(mine);
    let mut rest = Vec::new();
    (&peer).read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "nothing was written at zero: {rest:?}");

    // The peer closing is the end of the stream.
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
    clock.advance(Duration::from_secs(40));
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
    // A wait on a live process advances only the wall clock: a fake clock
    // that never advances would renew the allowance at every wait, so this
    // probe takes the test's one process-clock deadline, and successive
    // waits share its budget (`docs/testing.md`, "Waits and timeouts").
    let deadline = Deadline::start();
    let (mut child, group, _guard) = sleeper();
    assert!(group_alive(deadline, group));
    assert!(kill_group(deadline, group, "KILL").unwrap());
    let (done, reaped) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    assert!(
        deadline.cleanup_phase().recv(&reaped).is_ok(),
        "waited until the deadline for the killed sleep to be reaped"
    );
    assert!(fakes::group_empties(group, deadline.cleanup()));
    assert!(!group_alive(deadline, group));

    let (mut child, group, _guard) = sleeper();
    kill_group_detached(group, "KILL");
    let (done, reaped) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    assert!(
        deadline.cleanup_phase().recv(&reaped).is_ok(),
        "waited until the deadline for the detached kill's sleep to be reaped"
    );
    assert!(fakes::group_empties(group, deadline.cleanup()));
}

#[test]
fn the_budget_is_half_of_nextests_kill() {
    assert!(WAITS < CLEANUP && CLEANUP < BUDGET);
    assert!(BUDGET * 2 <= Duration::from_secs(120));
    assert!(BUDGET.saturating_sub(CLEANUP) >= Duration::from_secs(10));
}
