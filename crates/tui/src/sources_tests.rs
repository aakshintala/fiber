//! Tests for the tty reader and its pause handshake.

use crate::Input;
use fakes::Deadline;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// Runs `work` on a thread and returns its result, failing after
/// [`DEADLINE`] with `what`: one deadline however many reads it makes.
#[track_caller]
fn within<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("sources-within".to_owned())
        .spawn(move || done.send(work()).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(result) => result,
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

/// A reader on a pipe standing in for the tty: the reader, the pipe's
/// write end and the loop's channel.
fn piped_reader() -> (super::Reader, io::PipeWriter, mpsc::Receiver<Input>) {
    let (read, write) = io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let tty = File::from(std::os::fd::OwnedFd::from(read));
    let (tx, rx) = mpsc::channel();
    let reader = super::Reader::spawn(&tty, tx).unwrap_or_else(|| panic!("the reader started"));
    (reader, write, rx)
}

/// The next bytes the reader sends, with one deadline.
#[track_caller]
fn next_bytes(rx: &mpsc::Receiver<Input>, what: &str, wait: &Deadline) -> Vec<u8> {
    match wait.recv(rx) {
        Ok(Input::Bytes(bytes)) => bytes,
        Ok(_) => panic!("{what}: not bytes"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

/// Pauses `reader` on a thread with one deadline, handing it back.
#[track_caller]
fn paused(reader: super::Reader) -> super::Reader {
    within("the pause to return", move || {
        let mut reader = reader;
        reader.pause();
        reader
    })
}

#[test]
fn a_paused_reader_holds_the_ttys_bytes_until_resumed() {
    let (reader, mut tty, rx) = piped_reader();
    tty.write_all(b"a")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(
        next_bytes(&rx, "the first byte", &Deadline::after(DEADLINE)),
        b"a"
    );
    let reader = paused(reader);
    // Pause returned only once the reader parked.
    assert!(reader.gate.lock().parked);
    tty.write_all(b"b")
        .unwrap_or_else(|err| panic!("write: {err}"));
    // Parked on the condition variable, it reads nothing.
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(reader.gate.lock().parked);
    reader.resume();
    assert_eq!(
        next_bytes(&rx, "the byte after resume", &Deadline::after(DEADLINE)),
        b"b"
    );
    assert!(!reader.gate.lock().parked);
    // A second pause and resume works the same.
    let reader = paused(reader);
    tty.write_all(b"c")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    reader.resume();
    assert_eq!(
        next_bytes(
            &rx,
            "the byte after the second resume",
            &Deadline::after(DEADLINE)
        ),
        b"c"
    );
}

#[test]
fn bytes_read_before_the_pause_are_sent_not_lost() {
    let (reader, mut tty, rx) = piped_reader();
    tty.write_all(b"xy")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let reader = paused(reader);
    reader.resume();
    let mut got = Vec::new();
    let wait = Deadline::after(DEADLINE);
    while got.len() < 2 {
        got.extend(next_bytes(&rx, "the bytes written before the pause", &wait));
    }
    assert_eq!(got, b"xy");
}

#[test]
fn pause_on_an_ended_reader_returns_at_once() {
    let (reader, tty, rx) = piped_reader();
    // The tty's end ends the reader: its sender drops.
    drop(tty);
    match Deadline::after(DEADLINE).recv(&rx) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Ok(_) => panic!("bytes instead of the reader's end"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("waited {DEADLINE:?} for the reader to end")
        }
    }
    let reader = paused(reader);
    assert!(reader.gate.lock().ended);
    assert!(!reader.gate.lock().parked);
}

#[test]
fn a_reader_whose_channel_closed_ends() {
    let (reader, mut tty, rx) = piped_reader();
    drop(rx);
    tty.write_all(b"a")
        .unwrap_or_else(|err| panic!("write: {err}"));
    // Its send fails, and it records its end.
    let state = reader.gate.lock();
    let (state, waited) = reader
        .gate
        .changed
        .wait_timeout_while(state, DEADLINE, |state| !state.ended)
        .unwrap_or_else(|err| panic!("lock: {err}"));
    assert!(
        !waited.timed_out(),
        "waited {DEADLINE:?} for the reader to end"
    );
    drop(state);
    // Pause on it returns at once.
    let reader = paused(reader);
    assert!(!reader.gate.lock().parked);
}

#[test]
fn pause_on_a_reader_already_parked_returns_at_once() {
    let (_woken, wake) = io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let gate = Arc::new(super::Gate::default());
    gate.lock().parked = true;
    let reader = paused(super::Reader { gate, wake });
    assert!(reader.gate.lock().paused);
}

/// A terminal thread's stack must be under a huge page, or the kernel can
/// back its first touch with 2 MiB (`docs/architecture.md`, "The threads").
#[test]
fn a_terminal_thread_stack_is_under_a_huge_page() {
    const HUGE_PAGE: usize = 2_097_152;
    let stack = std::hint::black_box(super::STACK);
    assert!(
        stack < HUGE_PAGE,
        "a {stack} byte stack can sit on a whole 2 MiB span"
    );
}
