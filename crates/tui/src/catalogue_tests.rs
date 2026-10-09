//! Tests for the loop's one model-list read at a time: a read asked while
//! one runs waits, and the widest waiting wins.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::{Catalogue, ReadModels, Reader, Refresh};
use crate::Input;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// A read that blocks until released, recording every refresh it runs.
/// Behind a mutex: the loop runs it off its thread.
struct Fake {
    /// Every refresh run, in run order.
    seen: Arc<Mutex<Vec<Refresh>>>,
    /// Released once per read run.
    release: mpsc::Receiver<()>,
}

impl Fake {
    fn read(&self) -> Result<Catalogue, String> {
        self.release
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the read's release"));
        Ok(Catalogue::default())
    }
}

/// A reader over a gated read, with the loop's channel, the gate, and
/// every refresh run.
struct Setup {
    reader: Reader,
    tx: mpsc::Sender<Input>,
    rx: mpsc::Receiver<Input>,
    release: mpsc::Sender<()>,
    seen: Arc<Mutex<Vec<Refresh>>>,
}

fn setup() -> Setup {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (release_tx, release) = mpsc::channel();
    let fake = Arc::new(Mutex::new(Fake {
        seen: Arc::clone(&seen),
        release,
    }));
    let read: ReadModels = Arc::new(move |refresh| {
        let fake = fake.lock().unwrap();
        fake.seen.lock().unwrap().push(refresh);
        fake.read()
    });
    let (tx, rx) = mpsc::channel();
    Setup {
        reader: Reader::new(Some(read)),
        tx,
        rx,
        release: release_tx,
        seen,
    }
}

/// The next answer on `rx`, or the test's failure naming the wait.
fn next(rx: &mpsc::Receiver<Input>) -> Input {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("waited {DEADLINE:?} for the read's answer"))
}

#[test]
fn a_read_while_one_runs_waits_and_runs_after() {
    let Setup {
        mut reader,
        tx,
        rx,
        release,
        seen,
    } = setup();
    reader.ask(Refresh::Cached, &tx);
    reader.ask(Refresh::Stale, &tx);
    // The second read waits: releasing once answers only the first.
    release.send(()).unwrap();
    assert!(matches!(next(&rx), Input::Models(Ok(_))));
    // Without `done` the queued read never starts.
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    reader.done(&tx);
    release.send(()).unwrap();
    assert!(matches!(next(&rx), Input::Models(Ok(_))));
    assert_eq!(*seen.lock().unwrap(), [Refresh::Cached, Refresh::Stale]);
}

#[test]
fn the_queue_keeps_the_widest_refresh() {
    let Setup {
        mut reader,
        tx,
        rx,
        release,
        seen,
    } = setup();
    reader.ask(Refresh::Cached, &tx);
    reader.ask(Refresh::Stale, &tx);
    reader.ask(Refresh::Cached, &tx);
    reader.ask(Refresh::Every, &tx);
    release.send(()).unwrap();
    assert!(matches!(next(&rx), Input::Models(Ok(_))));
    reader.done(&tx);
    release.send(()).unwrap();
    assert!(matches!(next(&rx), Input::Models(Ok(_))));
    // One waiting read ran, at the widest asked: `done` with nothing
    // waiting starts nothing.
    assert_eq!(*seen.lock().unwrap(), [Refresh::Cached, Refresh::Every]);
    reader.done(&tx);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
}

#[test]
fn no_reader_asks_nothing() {
    let mut reader = Reader::new(None);
    let (tx, rx) = mpsc::channel();
    reader.ask(Refresh::Cached, &tx);
    reader.done(&tx);
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
}
