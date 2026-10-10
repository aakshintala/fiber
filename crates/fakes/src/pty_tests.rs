//! Tests for [`open`](super::open) and [`read_to_eof`](super::read_to_eof):
//! the pair passes bytes from slave to master, every chunk in order to end
//! of file, reading on past a gone consumer, retrying `Interrupted`, and
//! ending on any other error.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{open, read_to_eof};
use crate::deadline::Deadline;
use crate::within::{MUST_SUCCEED_WITHIN, within};

/// How long a wait that must succeed may take. Every wait below ends as
/// soon as its signal arrives; the deadline only reports a missed one.
const WAIT: Duration = Duration::from_secs(3);

/// One scripted `Read` outcome.
enum Step {
    Data(&'static [u8]),
    Fail(ErrorKind),
    /// Fails with this kind on every read, never reaching end of file.
    Forever(ErrorKind),
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
            Some(Step::Forever(kind)) => {
                self.steps.push_front(Step::Forever(kind));
                Err(std::io::Error::new(kind, "scripted"))
            }
        }
    }
}

/// Every value sent, in order, up to the end of file: the collector ends when
/// the reader thread drops the callback. The whole wait has one deadline.
#[track_caller]
fn collect_to_eof<T: Send + 'static>(rx: mpsc::Receiver<T>, what: &str) -> Vec<T> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx.send(rx.iter().collect::<Vec<T>>()).unwrap_or(());
    });
    match Deadline::after(WAIT).recv(&done_rx) {
        Ok(values) => values,
        Err(err) => panic!("waited {WAIT:?} for {what}: {err}"),
    }
}

#[track_caller]
fn disconnected<T: std::fmt::Debug>(result: Result<T, mpsc::RecvTimeoutError>, what: &str) {
    match result {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        other => panic!("expected the reader thread to end at {what}, got {other:?}"),
    }
}

#[test]
fn open_passes_bytes_from_slave_to_master() {
    let (master, slave) = within("opening a pty pair", MUST_SUCCEED_WITHIN, || {
        open().expect("opening a pty pair")
    });
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave)
        .expect("opening the pty slave");
    within(
        "reading what the slave wrote",
        MUST_SUCCEED_WITHIN,
        move || {
            let (mut master, mut slave) = (master, slave);
            slave.write_all(b"hi").expect("writing to the pty slave");
            let mut byte = [0u8; 1];
            let mut got = Vec::new();
            while got.len() < 2 {
                match master.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got.extend_from_slice(&byte[..n]),
                }
            }
            assert_eq!(got, b"hi");
        },
    );
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
    let got = collect_to_eof(rx, "every chunk up to end of file");
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
    assert_eq!(
        collect_to_eof(read_rx, "both reads and end of file"),
        vec![3, 3]
    );
}

#[test]
fn retries_an_interrupted_read() {
    let script = Script::of(vec![Step::Fail(ErrorKind::Interrupted), Step::Data(b"hi")]);
    let (tx, rx) = mpsc::channel();
    read_to_eof(script, move |bytes: &[u8]| {
        tx.send(bytes.to_vec()).unwrap();
    });
    assert_eq!(
        collect_to_eof(rx, "the chunk and end of file"),
        vec![b"hi".to_vec()]
    );
}

#[test]
fn a_non_interrupted_error_ends_the_thread() {
    let (alive_tx, alive_rx) = mpsc::channel::<()>();
    read_to_eof(
        Script::of(vec![Step::Forever(ErrorKind::Other)]),
        move |_bytes: &[u8]| {
            alive_tx.send(()).unwrap_or(());
        },
    );
    disconnected(Deadline::after(WAIT).recv(&alive_rx), "the error");
}
