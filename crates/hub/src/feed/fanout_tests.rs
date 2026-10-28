//! Tests for the bounded fan-out: the queue holds exactly [`QUEUE`]
//! lines, the next send reports the stalled subscriber, a disconnected
//! receiver is not a cap violation, cleanup hands back every shutdown and
//! handle once, and [`close_connections`] shuts write before read.

use std::io::{BufRead, BufReader, Read};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use super::*;
use fakes::Deadline;

/// One named deadline per wait: every join and signal lands before it.
const DEADLINE: Duration = Duration::from_secs(10);

/// One queued line.
fn line() -> Line {
    Arc::from(&b"l\n"[..])
}

/// A shutdown over a held socket half: nothing ever reads the far end.
fn closer() -> Closer {
    let (owned, _far) = UnixStream::pair().unwrap();
    Closer {
        stream: owned,
        web: None,
    }
}

/// A thread that ends once released: a writer handle the test owns.
fn inert() -> (JoinHandle<()>, mpsc::Sender<()>) {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let handle = thread::Builder::new()
        .name("fanout-test-inert".to_owned())
        .spawn(move || {
            release_rx.recv().unwrap_or(());
        })
        .unwrap();
    (handle, release_tx)
}

/// A held receiver: nothing is ever dequeued.
fn held() -> (SyncSender<Line>, mpsc::Receiver<Line>) {
    mpsc::sync_channel(QUEUE)
}

/// Joins a test-owned handle under one deadline: a join that hangs fails
/// the test instead of hanging it.
#[track_caller]
fn join_within(handle: JoinHandle<()>, what: &str) {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx.send(handle.join()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| panic!("{what} before its deadline"))
        .unwrap_or(());
}

/// Reaps `fanout`'s ended writers on a worker until one lands, handing the
/// registry and the reap back under one deadline: only the registry sees
/// endedness, and every wait ends on a channel.
#[track_caller]
fn reap_within(fanout: Fanout, what: &str) -> (Fanout, Vec<JoinHandle<()>>) {
    let (done_tx, done_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut fanout = fanout;
        loop {
            let done = fanout.finished();
            if !done.is_empty() {
                done_tx.send((fanout, done)).unwrap_or(());
                return;
            }
            if cancel_rx.try_recv().is_ok() {
                // Drops the registry; its threads end on their own once
                // their release senders drop.
                return;
            }
            thread::yield_now();
        }
    });
    Deadline::after(DEADLINE)
        .recv(&done_rx)
        .unwrap_or_else(|_| {
            cancel_tx.send(()).unwrap_or(());
            panic!("{what} before its deadline")
        })
}

/// Shuts test closers with the production shutdown: socket shutdowns are
/// real, even in these unit tests.
fn shut(closers: Vec<Closer>) {
    close_connections(closers, UnixStream::shutdown);
}

/// Registered ids in registration order, read from the private subscriber
/// list this module's own child test sees: the tests assert on the
/// registry they drive, so the production type carries no test-only reader.
fn ids(fanout: &Fanout) -> Vec<u64> {
    fanout.list.iter().map(|sub| sub.id).collect()
}

#[test]
fn a_spawned_writer_writes_each_line_and_ends_with_its_senders() {
    let (writer, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let (tx, handle) = spawn_writer(Arc::new(Mutex::new(writer)), "fanout-test-out").unwrap();
    for _ in 0..3 {
        tx.send(line()).unwrap();
    }
    let mut read = BufReader::new(far);
    for _ in 0..3 {
        let mut text = String::new();
        read.read_line(&mut text).unwrap();
        assert_eq!(text, "l\n");
    }
    drop(tx);
    join_within(handle, "the writer to end");
}

#[test]
fn the_queue_reports_full_at_the_bound_and_keeps_a_healthy_subscriber() {
    let mut fanout = Fanout::default();
    let (stalled_tx, _stalled_rx) = held();
    let (stalled_writer, stalled_release) = inert();
    let stalled = fanout.push((stalled_tx, stalled_writer), closer(), closer(), line());
    let (healthy_tx, healthy_rx) = held();
    let (healthy_writer, healthy_release) = inert();
    let healthy = fanout.push((healthy_tx, healthy_writer), closer(), closer(), line());
    // The snapshot holds one slot: drain it first, then acknowledge each
    // healthy line before the next broadcast, so the healthy queue never
    // fills and the final broadcast cannot fill it too.
    let healthy_line = Deadline::after(DEADLINE);
    healthy_line.recv_or_fail(&healthy_rx, "the healthy snapshot to land");
    // QUEUE-1 more sends fit, and the QUEUEth
    // queued line still fits.
    for _ in 0..QUEUE - 1 {
        assert!(
            fanout.broadcast(&line()).is_empty(),
            "no overflow below the bound"
        );
        healthy_line.recv_or_fail(&healthy_rx, "each healthy line to land");
        assert_eq!(ids(&fanout), [stalled, healthy]);
    }
    let closers = fanout.broadcast(&line());
    assert_eq!(closers.len(), 1, "exactly the stalled subscriber reports");
    assert_eq!(ids(&fanout), [healthy], "the stalled id is gone");
    shut(closers);
    stalled_release.send(()).unwrap_or(());
    healthy_release.send(()).unwrap_or(());
    for end in fanout.close() {
        shut(vec![end.closer]);
        if let Some(writer) = end.writer {
            join_within(writer, "the retired writer to end");
        }
    }
}

#[test]
fn a_disconnected_receiver_is_not_a_cap_violation() {
    let (tx, rx) = held();
    // Dropped synchronously: no thread races the classification.
    drop(rx);
    let (writer, release) = inert();
    let mut fanout = Fanout::default();
    let id = fanout.push((tx, writer), closer(), closer(), line());
    assert_eq!(ids(&fanout), [id]);
    assert!(
        fanout.broadcast(&line()).is_empty(),
        "a dead writer reports no overflow"
    );
    assert!(ids(&fanout).is_empty(), "the dead id is gone");
    release.send(()).unwrap_or(());
    let (fanout, finished) = reap_within(fanout, "the dead writer to reap");
    assert_eq!(finished.len(), 1, "its handle is kept for joining");
    for writer in finished {
        join_within(writer, "the dead writer to join");
    }
    drop(fanout);
}

#[test]
fn remove_hands_back_shutdown_and_writer_once() {
    let (tx, _rx) = held();
    let (writer, release) = inert();
    let mut fanout = Fanout::default();
    let id = fanout.push((tx, writer), closer(), closer(), line());
    let end = fanout.remove(id).expect("the registered id removes");
    shut(vec![end.closer]);
    assert!(fanout.remove(id).is_none(), "removing twice is a no-op");
    assert!(fanout.remove(id + 1).is_none(), "an unknown id is a no-op");
    release.send(()).unwrap_or(());
    join_within(end.writer.expect("a live writer"), "the removed writer");
}

#[test]
fn finished_reaps_only_ended_writers_and_close_hands_back_the_rest() {
    let (live_tx, _live_rx) = held();
    let (live, live_release) = inert();
    let (gone_tx, gone_rx) = held();
    drop(gone_rx);
    let (gone, gone_release) = inert();
    let mut fanout = Fanout::default();
    let live_id = fanout.push((live_tx, live), closer(), closer(), line());
    fanout.push((gone_tx, gone), closer(), closer(), line());
    // The broadcast retires the dead channel without reporting it.
    assert!(fanout.broadcast(&line()).is_empty());
    assert_eq!(ids(&fanout), [live_id]);
    gone_release.send(()).unwrap_or(());
    let (fanout, done) = reap_within(fanout, "the ended writer to reap");
    assert_eq!(done.len(), 1, "only the ended writer reaps");
    for writer in done {
        join_within(writer, "the ended writer");
    }
    assert_eq!(ids(&fanout), [live_id], "the live writer stays");
    live_release.send(()).unwrap_or(());
    let mut ends = fanout.close();
    assert_eq!(ends.len(), 1, "close hands back the live writer");
    let end = ends.pop().unwrap();
    shut(vec![end.closer]);
    join_within(end.writer.expect("a live writer"), "the live writer");
}

#[test]
fn close_connections_shuts_write_then_read_and_never_both() {
    let (owned, far) = UnixStream::pair().unwrap();
    far.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut calls = Vec::new();
    let first = Closer {
        stream: owned.try_clone().unwrap(),
        web: None,
    };
    let second = Closer {
        stream: owned.try_clone().unwrap(),
        web: None,
    };
    close_connections(vec![first, second], |stream, how| {
        calls.push(how);
        match how {
            Shutdown::Write => UnixStream::shutdown(stream, how),
            // A failed read shutdown never skips the next closer.
            Shutdown::Read => Err(std::io::Error::other("the read half is gone")),
            Shutdown::Both => panic!("Shutdown::Both is never used for cleanup"),
        }
    });
    assert_eq!(
        calls,
        [
            Shutdown::Write,
            Shutdown::Read,
            Shutdown::Write,
            Shutdown::Read
        ],
        "write before read, per closer, in order"
    );
    // The write shutdown really reached the socket.
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(far).read(&mut buf).unwrap(),
        0,
        "the write shutdown closes the far end"
    );
}

#[test]
fn send_to_reaches_exactly_one_subscriber() {
    let mut fanout = Fanout::default();
    let (first_tx, _first_rx) = held();
    let (first, first_release) = inert();
    let first_id = fanout.push((first_tx, first), closer(), closer(), line());
    let (second_tx, _second_rx) = held();
    let (second, second_release) = inert();
    let second_id = fanout.push((second_tx, second), closer(), closer(), line());
    assert!(matches!(
        fanout.send_to(first_id + 100, &line()),
        Targeted::Gone
    ));
    // The snapshot holds one slot: QUEUE-1 targeted sends fit.
    for _ in 0..QUEUE - 1 {
        assert!(matches!(fanout.send_to(first_id, &line()), Targeted::Sent));
    }
    assert_eq!(ids(&fanout), [first_id, second_id]);
    match fanout.send_to(first_id, &line()) {
        Targeted::Full(closer) => shut(vec![closer]),
        Targeted::Sent => panic!("the send past the bound is not Sent"),
        Targeted::Gone => panic!("the registered id is not Gone"),
        Targeted::DeadWriter(_) => panic!("the live receiver is not dead"),
    }
    assert_eq!(ids(&fanout), [second_id], "only the full id is gone");
    assert!(matches!(fanout.send_to(first_id, &line()), Targeted::Gone));
    assert_eq!(ids(&fanout), [second_id], "a removed id never resurrects");
    // A disconnected receiver hands its handle back for joining.
    let (dead_tx, dead_rx) = held();
    drop(dead_rx);
    let (dead, dead_release) = inert();
    let dead_id = fanout.push((dead_tx, dead), closer(), closer(), line());
    match fanout.send_to(dead_id, &line()) {
        Targeted::DeadWriter(writer) => {
            dead_release.send(()).unwrap_or(());
            join_within(writer, "the dead writer");
        }
        Targeted::Sent => panic!("the dead receiver is not Sent"),
        Targeted::Full(_) => panic!("the dead receiver is not Full"),
        Targeted::Gone => panic!("the registered id is not Gone"),
    }
    assert_eq!(ids(&fanout), [second_id], "the dead id is gone");
    first_release.send(()).unwrap_or(());
    second_release.send(()).unwrap_or(());
    for end in fanout.close() {
        shut(vec![end.closer]);
        if let Some(writer) = end.writer {
            join_within(writer, "a remaining writer");
        }
    }
}

#[test]
fn pending_registrations_take_distinct_ids_and_remove_keeps_the_first() {
    let mut fanout = Fanout::default();
    // Pending registrations spawn no thread: these channels never leave
    // this test.
    let (first, _first_far) = UnixStream::pair().unwrap();
    let first_id = fanout.listen(Arc::new(Mutex::new(first)), closer(), closer());
    let (second, _second_far) = UnixStream::pair().unwrap();
    let second_id = fanout.listen(Arc::new(Mutex::new(second)), closer(), closer());
    // Exact ids: a constant return aliases the two registrations, and a
    // broken counter never counts past the first.
    assert_eq!(first_id, 1, "the first registration takes id 1");
    assert_eq!(second_id, 2, "the second registration takes id 2");
    assert_eq!(ids(&fanout), [first_id, second_id]);
    let end = fanout
        .remove(second_id)
        .expect("the second registration removes");
    assert!(
        end.writer.is_none(),
        "a pending registration owns no writer"
    );
    shut(vec![end.closer]);
    assert_eq!(ids(&fanout), [first_id], "the first registration stays");
    // Pending registrations own no thread and shut nothing: closing drops
    // them with nothing to join.
    assert!(fanout.close().is_empty(), "no writer to hand back");
}

#[test]
fn a_broadcast_whose_pending_writer_fails_to_spawn_unregisters_it_and_hands_back_its_shutdown() {
    let mut fanout = Fanout::default();
    // Two pending registrations, each closer tied to its own socket so
    // shutting the handed-back closer is observed on its far end.
    let (failed, failed_far) = UnixStream::pair().unwrap();
    failed_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let failed_stream = failed.try_clone().unwrap();
    let failed_id = fanout.listen(
        Arc::new(Mutex::new(failed)),
        Closer {
            stream: failed_stream.try_clone().unwrap(),
            web: None,
        },
        Closer {
            stream: failed_stream,
            web: None,
        },
    );
    let (healthy, healthy_far) = UnixStream::pair().unwrap();
    healthy_far.set_read_timeout(Some(DEADLINE)).unwrap();
    let healthy_stream = healthy.try_clone().unwrap();
    let healthy_id = fanout.listen(
        Arc::new(Mutex::new(healthy)),
        Closer {
            stream: healthy_stream.try_clone().unwrap(),
            web: None,
        },
        Closer {
            stream: healthy_stream,
            web: None,
        },
    );
    // Only the first spawn fails: the failure is ordered, not racy.
    fanout.fail_next_spawn();
    let closers = fanout.broadcast(&line());
    assert_eq!(closers.len(), 1, "exactly the failed registration reports");
    assert_eq!(ids(&fanout), [healthy_id], "the failed id is gone");
    assert_ne!(failed_id, healthy_id, "the two registrations differ");
    shut(closers);
    // Exactly the failed connection's shutdown came back: its far end sees
    // EOF, while the healthy registration's writer delivers the line.
    let mut buf = [0; 1];
    assert_eq!(
        BufReader::new(failed_far).read(&mut buf).unwrap(),
        0,
        "the failed side sees EOF"
    );
    let mut text = String::new();
    BufReader::new(healthy_far)
        .read_line(&mut text)
        .expect("the healthy registration delivers");
    assert_eq!(text, "l\n");
    for end in fanout.close() {
        shut(vec![end.closer]);
        if let Some(writer) = end.writer {
            join_within(writer, "the healthy writer to end");
        }
    }
}
