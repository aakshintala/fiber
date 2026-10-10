//! Tests for [`super`]: the stoppable read ends a reader whose peer stays
//! open and writes nothing, with a bounded join. Every result receive goes
//! through `fakes::Deadline::after(..).recv(..)`, and no test sleeps. No
//! case calls `shutdown`, which proves the end does not depend on its
//! wakeup: each passes with `shutdown` removed from the stop path.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

/// A stopped or ended read answers before this.
const DEADLINE: Duration = Duration::from_secs(5);
/// A blocked read is still blocked after this: long enough that a read that
/// would answer at once has answered, short enough to keep the suite fast.
const STILL_BLOCKED: Duration = Duration::from_millis(50);

/// The stop error a case waited for.
#[track_caller]
fn assert_stopped(got: std::io::Result<usize>) {
    match got {
        Err(got) => assert_eq!(got.kind(), std::io::ErrorKind::ConnectionAborted),
        Ok(_) => panic!("a stopped read reports the stop"),
    }
}

#[test]
fn a_stop_ends_a_read_on_a_silent_open_peer() {
    let (peer, stream) = UnixStream::pair().unwrap();
    let (reader, stop) = super::reader(stream).unwrap();
    let (tx, rx) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut read = BufReader::new(reader);
        let mut buf = Vec::new();
        let got = read.read_until(b'\n', &mut buf);
        tx.send(got).unwrap_or(());
    });
    // The read is blocked: nothing answers within the bound.
    assert!(
        matches!(
            Deadline::after(STILL_BLOCKED).recv(&rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the read blocks on a silent peer"
    );
    stop.stop();
    let got = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the stop ends the read");
    assert_stopped(got);
    // Bounded through the same channel: the send above is the thread's last
    // act, so the join returns at once.
    reading.join().unwrap();
    drop(peer);
}

#[test]
fn bytes_before_a_stop_are_delivered() {
    let (mut peer, stream) = UnixStream::pair().unwrap();
    let (reader, stop) = super::reader(stream).unwrap();
    peer.write_all(b"a\n").unwrap();
    let mut read = BufReader::new(reader);
    let mut buf = Vec::new();
    let got = read.read_until(b'\n', &mut buf).unwrap();
    assert_eq!((got, buf), (2, b"a\n".to_vec()));
    drop(peer);
    drop(stop);
}

#[test]
fn a_stop_before_the_first_read_answers_at_once() {
    let (_peer, stream) = UnixStream::pair().unwrap();
    let (mut reader, stop) = super::reader(stream).unwrap();
    stop.stop();
    let mut buf = [0_u8; 8];
    assert_stopped(reader.read(&mut buf));
}

#[test]
fn a_stop_wins_over_pending_data() {
    let (mut peer, stream) = UnixStream::pair().unwrap();
    let (mut reader, stop) = super::reader(stream).unwrap();
    peer.write_all(b"b\n").unwrap();
    stop.stop();
    let mut buf = [0_u8; 8];
    assert_stopped(reader.read(&mut buf));
    drop(peer);
}

#[test]
fn a_stop_keeps_bytes_already_read() {
    let (mut peer, stream) = UnixStream::pair().unwrap();
    let (mut reader, stop) = super::reader(stream).unwrap();
    // Written before the thread starts, so the first read finds every byte.
    peer.write_all(b"par").unwrap();
    let (tx, rx) = mpsc::channel();
    let reading = thread::spawn(move || {
        for _ in 0..2 {
            let mut buf = [0_u8; 8];
            match reader.read(&mut buf) {
                Ok(read) => tx.send(Ok((read, buf[..read].to_vec()))).unwrap_or(()),
                Err(read) => {
                    tx.send(Err(read.kind())).unwrap_or(());
                    return;
                }
            }
        }
    });
    let first = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the first read answers");
    match first {
        Ok((read, bytes)) => assert_eq!((read, bytes), (3, b"par".to_vec())),
        Err(_) => panic!("the first read delivers what the peer wrote"),
    }
    // The thread is blocked in its second read.
    assert!(
        matches!(
            Deadline::after(STILL_BLOCKED).recv(&rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the second read blocks"
    );
    stop.stop();
    let second = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the stop ends the second read");
    match second {
        Err(kind) => assert_eq!(kind, std::io::ErrorKind::ConnectionAborted),
        Ok(_) => panic!("a stopped read reports the stop"),
    }
    reading.join().unwrap();
    drop(peer);
}

#[test]
fn a_second_stop_does_nothing() {
    let (_peer, stream) = UnixStream::pair().unwrap();
    let (mut reader, stop) = super::reader(stream).unwrap();
    stop.stop();
    stop.stop();
    let mut buf = [0_u8; 8];
    assert_stopped(reader.read(&mut buf));
}

#[test]
fn dropping_the_stop_ends_a_blocked_read() {
    let (peer, stream) = UnixStream::pair().unwrap();
    let (reader, stop) = super::reader(stream).unwrap();
    let (tx, rx) = mpsc::channel();
    let reading = thread::spawn(move || {
        let mut read = BufReader::new(reader);
        let mut buf = Vec::new();
        let got = read.read_until(b'\n', &mut buf);
        tx.send(got.map_err(|got| got.kind())).unwrap_or(());
    });
    assert!(
        matches!(
            Deadline::after(STILL_BLOCKED).recv(&rx),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the read blocks on a silent peer"
    );
    drop(stop);
    let got = Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("dropping the stop ends the read");
    match got {
        Err(kind) => assert_eq!(kind, std::io::ErrorKind::ConnectionAborted),
        Ok(_) => panic!("a dropped stop ends the read"),
    }
    reading.join().unwrap();
    drop(peer);
}

#[test]
fn the_peer_closing_ends_the_read_with_no_stop() {
    let (peer, stream) = UnixStream::pair().unwrap();
    let (reader, stop) = super::reader(stream).unwrap();
    drop(peer);
    let mut read = BufReader::new(reader);
    let mut buf = Vec::new();
    let got = read.read_until(b'\n', &mut buf).unwrap();
    assert_eq!((got, buf), (0, Vec::new()));
    drop(stop);
}

#[test]
fn get_ref_returns_the_wrapped_stream() {
    let (mut peer, stream) = UnixStream::pair().unwrap();
    let (reader, stop) = super::reader(stream).unwrap();
    // A write through the wrapped stream reaches the peer.
    let mut through = reader.get_ref().try_clone().unwrap();
    through.write_all(b"ping").unwrap();
    let mut buf = [0_u8; 4];
    peer.read_exact(&mut buf).unwrap();
    assert_eq!(buf, *b"ping");
    drop(peer);
    drop(stop);
}
