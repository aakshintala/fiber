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

use super::{Nudge, open, output_so_far, wait_first_output, write_chunks};
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
    let clock = FakeClock::new();
    let written = (terminal.input.0)(b"typed\n", clock.as_ref(), &CancelToken::new()).unwrap();
    assert_eq!(written, 6);
    let mut got = [0_u8; 6];
    secondary.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"typed\n");
}

#[test]
fn the_reader_waits_for_output_written_later() {
    let terminal = open().unwrap();
    let mut secondary = std::fs::File::from(terminal.secondary);
    let mut reader = terminal.reader;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut seen = [0_u8; 5];
        let _sent = tx.send(reader.read_exact(&mut seen).map(|()| seen));
    });
    secondary.write_all(b"late\n").unwrap();
    let seen = rx
        .recv_timeout(DEADLINE)
        .expect("the read to return")
        .unwrap();
    assert_eq!(&seen, b"late\r");
}

#[test]
fn output_so_far_is_the_whole_file_lossily_and_empty_for_a_missing_one() {
    let dir = TempDir::new("fiber-tty-so-far");
    let path = dir.path().join("out.log");
    assert_eq!(output_so_far(&path), "");
    std::fs::write(&path, b"one\ntwo").unwrap();
    assert_eq!(output_so_far(&path), "one\ntwo");
    // Nothing is cut here: the loop bounds the result.
    let mut big = vec![b'a'; 100 * 1024];
    big.push(0xFF);
    std::fs::write(&path, &big).unwrap();
    let text = output_so_far(&path);
    assert_eq!(text.len(), 100 * 1024 + '\u{fffd}'.len_utf8());
    assert!(text.ends_with('\u{fffd}'));
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
    let mark = clock.advance_marked(Duration::from_millis(249));
    assert!(
        clock.await_parked_since(&mark, Some(due), DEADLINE),
        "the wait parks again a millisecond short of 250 ms"
    );
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
    // The clock's subscription delivers the wake, so advancing by zero wakes
    // the wait without moving the deadline.
    let mark = clock.advance_marked(Duration::ZERO);
    assert!(
        clock.await_parked_since(&mark, Some(due), DEADLINE),
        "output arriving parks the wait again at 250 ms"
    );
    assert!(done.try_recv().is_err());
    clock.advance(Duration::from_millis(250));
    done.recv_timeout(DEADLINE)
        .expect("the wait to end at 250 ms");
}

#[test]
fn a_nudge_counts_its_wakes() {
    let nudge = Nudge::default();
    assert_eq!(nudge.seen(), 0);
    nudge.wake();
    assert_eq!(nudge.seen(), 1);
    nudge.wake();
    assert_eq!(nudge.seen(), 2);
}

/// A writer that answers each write from a script. Once the script is
/// spent it writes nothing, which ends `write_chunks` with an error, so a
/// mutant that loops on a step ends too.
struct Script(std::collections::VecDeque<std::io::Result<usize>>);

impl Write for Script {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        self.0.pop_front().unwrap_or(Ok(0))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn script(steps: Vec<std::io::Result<usize>>) -> Script {
    Script(steps.into())
}

/// Runs `write_chunks` on its own thread, so a step that parks for good
/// fails at `DEADLINE` and does not hang the run.
fn chunked(
    mut writer: Script,
    bytes: &'static [u8],
    clock: Arc<FakeClock>,
    cancel: CancelToken,
) -> mpsc::Receiver<std::io::Result<usize>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(write_chunks(&mut writer, bytes, clock.as_ref(), &cancel));
    });
    rx
}

#[test]
fn chunks_add_up_to_the_whole_input() {
    let rx = chunked(
        script(vec![Ok(2), Ok(3)]),
        b"hello",
        FakeClock::new(),
        CancelToken::new(),
    );
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap().unwrap(), 5);
}

#[test]
fn a_full_queue_parks_for_ten_ms_on_the_clock_and_tries_again() {
    let clock = FakeClock::new();
    let due = clock.origin() + Duration::from_millis(10);
    let rx = chunked(
        script(vec![Err(std::io::ErrorKind::WouldBlock.into()), Ok(5)]),
        b"hello",
        Arc::clone(&clock),
        CancelToken::new(),
    );
    assert!(
        clock.await_parked(due, DEADLINE),
        "the write did not wait for room"
    );
    assert!(rx.try_recv().is_err());
    clock.advance(Duration::from_millis(10));
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap().unwrap(), 5);
}

#[test]
fn an_interrupted_write_is_tried_again_at_once() {
    let rx = chunked(
        script(vec![Err(std::io::ErrorKind::Interrupted.into()), Ok(2)]),
        b"hi",
        FakeClock::new(),
        CancelToken::new(),
    );
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap().unwrap(), 2);
}

#[test]
fn any_other_error_ends_the_write_with_that_error() {
    let rx = chunked(
        script(vec![Err(std::io::ErrorKind::BrokenPipe.into()), Ok(2)]),
        b"hi",
        FakeClock::new(),
        CancelToken::new(),
    );
    let err = rx.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn a_write_of_nothing_is_an_error() {
    let rx = chunked(
        script(vec![Ok(0)]),
        b"hi",
        FakeClock::new(),
        CancelToken::new(),
    );
    let err = rx.recv_timeout(DEADLINE).unwrap().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::WriteZero);
}

#[test]
fn a_cancel_before_the_first_write_writes_nothing() {
    let cancel = CancelToken::new();
    cancel.cancel();
    let rx = chunked(script(vec![Ok(2)]), b"hi", FakeClock::new(), cancel);
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap().unwrap(), 0);
}

#[test]
fn a_cancel_while_waiting_for_room_returns_what_was_written() {
    let clock = FakeClock::new();
    let due = clock.origin() + Duration::from_millis(10);
    let cancel = CancelToken::new();
    let rx = chunked(
        script(vec![Ok(2), Err(std::io::ErrorKind::WouldBlock.into())]),
        b"hello",
        Arc::clone(&clock),
        cancel.clone(),
    );
    assert!(clock.await_parked(due, DEADLINE));
    cancel.cancel();
    assert_eq!(rx.recv_timeout(DEADLINE).unwrap().unwrap(), 2);
}
