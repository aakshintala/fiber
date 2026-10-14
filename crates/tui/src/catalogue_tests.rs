//! Tests for the loop's one model-list read at a time: a read asked while
//! one runs waits, and the widest waiting wins.

use std::io;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::{Catalogue, ReadModels, Reader, Refresh};
use crate::Input;
use fakes::Deadline;

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
        self.release.recv().expect("the test releases the read");
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
#[track_caller]
fn next(rx: &mpsc::Receiver<Input>, wait: &Deadline) -> Input {
    match wait.recv(rx) {
        Ok(answer) => answer,
        Err(_) => panic!("waited {DEADLINE:?} for the read's answer"),
    }
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
    assert!(matches!(
        next(&rx, &Deadline::after(DEADLINE)),
        Input::Models(Ok(_))
    ));
    // Without `done` the queued read never starts.
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&rx)
            .is_err()
    );
    reader.done(&tx);
    release.send(()).unwrap();
    assert!(matches!(
        next(&rx, &Deadline::after(DEADLINE)),
        Input::Models(Ok(_))
    ));
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
    assert!(matches!(
        next(&rx, &Deadline::after(DEADLINE)),
        Input::Models(Ok(_))
    ));
    reader.done(&tx);
    release.send(()).unwrap();
    assert!(matches!(
        next(&rx, &Deadline::after(DEADLINE)),
        Input::Models(Ok(_))
    ));
    // One waiting read ran, at the widest asked: `done` with nothing
    // waiting starts nothing.
    assert_eq!(*seen.lock().unwrap(), [Refresh::Cached, Refresh::Every]);
    reader.done(&tx);
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&rx)
            .is_err()
    );
}

#[test]
fn no_reader_asks_nothing() {
    let mut reader = Reader::new(None);
    let (tx, rx) = mpsc::channel();
    reader.ask(Refresh::Cached, &tx);
    reader.done(&tx);
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&rx)
            .is_err()
    );
}

/// A spawn that always fails, so the asked read never starts.
fn failing_spawn(
    _: std::thread::Builder,
    _: Box<dyn FnOnce() + Send + 'static>,
) -> io::Result<std::thread::JoinHandle<()>> {
    Err(io::Error::other("no thread"))
}

#[test]
fn spawn_failure_fails_the_waiting_and_queued_reads() {
    let read: ReadModels = Arc::new(|_| Ok(Catalogue::default()));
    let mut reader = Reader::new(Some(read));
    // A wider refresh waits behind the asked one.
    reader.queued = Some(Refresh::Stale);
    let (tx, rx) = mpsc::channel();
    reader.ask_with(Refresh::Cached, &tx, failing_spawn);
    // The failure frees the reader: nothing runs and nothing waits.
    assert!(!reader.running);
    assert_eq!(reader.queued, None);
    // One failure per owed read, the waiting one and the queued one,
    // carrying the same message.
    let wait = Deadline::after(DEADLINE);
    for what in ["the waiting read", "the queued read"] {
        match wait.recv(&rx) {
            Ok(Input::Models(Err(message))) => assert_eq!(
                message, "terminal: could not start the model-list reader: no thread",
                "{what}"
            ),
            Ok(_) => panic!("{what}: the failure, not an answer"),
            Err(error) => panic!("waited {DEADLINE:?} for {what}: {error}"),
        }
    }
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    // A later ask spawns again: the reader runs once more.
    reader.ask_with(Refresh::Cached, &tx, |builder, body| builder.spawn(body));
    assert!(reader.running);
    assert!(matches!(
        next(&rx, &Deadline::after(DEADLINE)),
        Input::Models(Ok(_))
    ));
}
