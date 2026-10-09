//! Tests for [`read_to_eof`](super::read_to_eof): every chunk in order to
//! end of file, reading on past a gone consumer, retrying `Interrupted`,
//! and ending on any other error.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::read_to_eof;

/// How long a wait that must succeed may take. Every wait below ends as
/// soon as its signal arrives; the deadline only reports a missed one.
const WAIT: Duration = Duration::from_secs(3);

/// One scripted `Read` outcome.
enum Step {
    Data(&'static [u8]),
    Fail(ErrorKind),
}

/// A `Read` replaying `steps` in order, then `Ok(0)`.
struct Script {
    steps: VecDeque<Step>,
}

impl Script {
    fn of(steps: Vec<Step>) -> Self {
        Self {
            steps: steps.into(),
        }
    }
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.steps.pop_front() {
            None => Ok(0),
            Some(Step::Data(data)) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
            Some(Step::Fail(kind)) => Err(std::io::Error::new(kind, "scripted")),
        }
    }
}

fn disconnected<T: std::fmt::Debug>(result: Result<T, mpsc::RecvTimeoutError>, what: &str) {
    match result {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        other => panic!("expected the reader thread to end at {what}, got {other:?}"),
    }
}

#[test]
fn delivers_every_chunk_in_order_up_to_eof() {
    let script = Script::of(vec![
        Step::Data(b"foo"),
        Step::Data(b"bar"),
        Step::Data(b"baz"),
    ]);
    let (tx, rx) = mpsc::channel();
    read_to_eof(script, move |bytes: &[u8]| {
        tx.send(bytes.to_vec()).unwrap();
    });
    // The collector ends only when the reader thread drops the callback at
    // end of file, so the chunks and the end of file share one deadline.
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let chunks: Vec<Vec<u8>> = rx.iter().collect();
        done_tx.send(chunks).unwrap();
    });
    let got = done_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(got, vec![b"foo".to_vec(), b"bar".to_vec(), b"baz".to_vec()]);
}

#[test]
fn keeps_reading_after_the_consumer_is_gone() {
    let script = Script::of(vec![Step::Data(b"abc"), Step::Data(b"def")]);
    let (gone_tx, gone_rx) = mpsc::channel::<Vec<u8>>();
    drop(gone_rx);
    let (read_tx, read_rx) = mpsc::channel::<usize>();
    read_to_eof(script, move |bytes: &[u8]| {
        // The consumer is gone; the failed send is ignored and reading
        // goes on.
        gone_tx.send(bytes.to_vec()).unwrap_or(());
        read_tx.send(bytes.len()).unwrap_or(());
    });
    let first = read_rx.recv_timeout(WAIT).unwrap();
    let second = read_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(first + second, 6);
}

#[test]
fn retries_an_interrupted_read() {
    let script = Script::of(vec![Step::Fail(ErrorKind::Interrupted), Step::Data(b"hi")]);
    let (tx, rx) = mpsc::channel();
    read_to_eof(script, move |bytes: &[u8]| {
        tx.send(bytes.to_vec()).unwrap();
    });
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), b"hi");
    disconnected(rx.recv_timeout(WAIT), "end of file");
}

#[test]
fn a_non_interrupted_error_ends_the_thread() {
    struct Gone;

    impl Read for Gone {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("the slave closed"))
        }
    }

    let (alive_tx, alive_rx) = mpsc::channel::<()>();
    read_to_eof(Gone, move |_bytes: &[u8]| {
        alive_tx.send(()).unwrap_or(());
    });
    disconnected(alive_rx.recv_timeout(WAIT), "the error");
}
