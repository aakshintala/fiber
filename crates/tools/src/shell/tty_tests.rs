//! The terminal's two sides, the receipt's output window, and the
//! first-output wait.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::{Read, Write};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Wake;
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};

use super::{open, output_so_far, wait_first_output};
use crate::shell::output::{Shared, lock, note_eof};

const DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn what_the_secondary_prints_is_read_from_the_primary_and_input_reaches_the_secondary() {
    let terminal = open().unwrap();
    let mut secondary = std::fs::File::from(terminal.secondary);
    secondary.write_all(b"out\n").unwrap();
    let mut reader = terminal.reader;
    let mut seen = [0_u8; 5];
    reader.read_exact(&mut seen).unwrap();
    // The line discipline turns the newline into a carriage return and a
    // newline.
    assert_eq!(&seen, b"out\r\n");
    (terminal.input.0)(b"typed\n").unwrap();
    let mut got = [0_u8; 6];
    secondary.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"typed\n");
}

#[test]
fn output_so_far_is_the_whole_small_file_and_empty_for_a_missing_one() {
    let dir = TempDir::new("fiber-tty-so-far");
    let path = dir.path().join("out.log");
    assert_eq!(output_so_far(&path), "");
    std::fs::write(&path, b"one\ntwo").unwrap();
    assert_eq!(output_so_far(&path), "one\ntwo");
}

#[test]
fn output_so_far_keeps_the_last_16_kib_and_not_half_a_character() {
    let dir = TempDir::new("fiber-tty-cut");
    let path = dir.path().join("out.log");
    // 16 KiB + 2 bytes, with an e-acute whose second byte is where the cut
    // starts.
    let mut bytes = vec![b'a'; 16 * 1024 + 2];
    bytes[1] = 0xC3;
    bytes[2] = 0xA9;
    std::fs::write(&path, &bytes).unwrap();
    let text = output_so_far(&path);
    assert_eq!(text.len(), 16 * 1024 - 1);
    assert!(
        text.bytes().all(|byte| byte == b'a'),
        "a half character was kept"
    );
    // At exactly the cut nothing is dropped.
    std::fs::write(&path, vec![b'b'; 16 * 1024]).unwrap();
    assert_eq!(output_so_far(&path).len(), 16 * 1024);
}

fn waiting() -> (
    Arc<FakeClock>,
    Arc<Shared>,
    mpsc::Receiver<()>,
    std::time::Instant,
) {
    let clock = FakeClock::new();
    let shared = Arc::new(Shared::default());
    let wake: Arc<dyn Wake> = shared.clone();
    contract::clock::Clock::subscribe(clock.as_ref(), Arc::downgrade(&wake));
    let due = clock.origin() + Duration::from_millis(250);
    let (tx, rx) = mpsc::channel();
    let (clock_in, shared_in) = (Arc::clone(&clock), Arc::clone(&shared));
    thread::spawn(move || {
        wait_first_output(&shared_in, clock_in.as_ref(), &CancelToken::new());
        let _sent = tx.send(());
    });
    (clock, shared, rx, due)
}

#[test]
fn the_first_output_wait_ends_at_250_ms_and_not_before() {
    let (clock, _shared, done, due) = waiting();
    assert!(
        clock.await_parked(due, DEADLINE),
        "the wait did not park at 250 ms"
    );
    assert!(done.try_recv().is_err());
    clock.advance(Duration::from_millis(249));
    assert!(clock.await_parked(due, DEADLINE));
    assert!(
        done.try_recv().is_err(),
        "the wait ended a millisecond early"
    );
    clock.advance(Duration::from_millis(1));
    done.recv_timeout(DEADLINE)
        .expect("the wait to end at 250 ms");
}

#[test]
fn the_first_output_wait_ends_at_once_on_end_of_file() {
    let (clock, shared, done, due) = waiting();
    assert!(clock.await_parked(due, DEADLINE));
    note_eof(&shared);
    done.recv_timeout(DEADLINE)
        .expect("the wait to end at end of file");
}

#[test]
fn the_first_output_wait_does_not_park_when_the_output_already_ended() {
    let clock = FakeClock::new();
    let shared = Shared::default();
    lock(&shared.inner).eof = true;
    // With no park this returns here; a park would wait for the clock.
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(shared);
    thread::spawn(move || {
        wait_first_output(&shared, clock.as_ref(), &CancelToken::new());
        let _sent = tx.send(());
    });
    rx.recv_timeout(DEADLINE)
        .expect("the wait to return at once");
}

#[test]
fn output_arriving_does_not_end_the_wait() {
    let (clock, shared, done, due) = waiting();
    assert!(clock.await_parked(due, DEADLINE));
    {
        let mut inner = lock(&shared.inner);
        inner.output.extend_from_slice(b"chunk");
        crate::shell::output::bump(&mut inner);
    }
    shared.wake();
    assert!(clock.await_parked(due, DEADLINE));
    assert!(done.try_recv().is_err());
    clock.advance(Duration::from_millis(250));
    done.recv_timeout(DEADLINE)
        .expect("the wait to end at 250 ms");
}
