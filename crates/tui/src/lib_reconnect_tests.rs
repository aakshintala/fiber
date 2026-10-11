//! Tests for the dropped connection through `run` on a pseudo-terminal,
//! and for the hub thread ending with the loop (`docs/tui.md`, "A dropped
//! connection").

use std::collections::VecDeque;
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use contract::HubLine;
use contract::clock::Clock;
use fakes::clock::FakeClock;
use ratatui::backend::TestBackend;

use super::Input;
use super::tests::{DEADLINE, Pair, command, hello, launch, new_loop, open};
use crate::pty_watch::{watch, watched};
use crate::retry::Retry;
use crate::sources::spawn_hub;

/// The first delay after a failure.
const HALF: Duration = Duration::from_millis(500);

/// How long a test watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(200);

/// A connect for tests: hands out its streams in order and fails once
/// they are used, reports each call, and reports being dropped, which
/// happens when the hub thread that owns it returns.
struct Dial {
    streams: VecDeque<(UnixStream, HubLine)>,
    calls: Sender<()>,
    gone: Sender<()>,
}

impl Dial {
    fn next(&mut self) -> io::Result<(UnixStream, HubLine)> {
        self.calls.send(()).unwrap_or(());
        self.streams
            .pop_front()
            .ok_or_else(|| io::Error::other("refused"))
    }
}

impl Drop for Dial {
    fn drop(&mut self) {
        self.gone.send(()).unwrap_or(());
    }
}

/// The connect over `streams`, a receiver of its calls, and one of its
/// drop.
fn dial(streams: Vec<(UnixStream, HubLine)>) -> (crate::Connect, Receiver<()>, Receiver<()>) {
    let (calls, called) = mpsc::channel();
    let (gone, ended) = mpsc::channel();
    let mut dial = Dial {
        streams: streams.into(),
        calls,
        gone,
    };
    (Box::new(move || dial.next()), called, ended)
}

/// A socket pair: the terminal's end and the hub's.
fn pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"))
}

/// Waits for one signal on `rx` within [`DEADLINE`].
fn signalled(rx: &Receiver<()>, what: &str) {
    rx.recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for {what}: {err}"));
}

/// Runs the terminal on a pty with `connect` and `clock`: the pty pair,
/// and the exit code once it quits. The hub thread starts once the first
/// frame is read.
fn spawn_run(connect: crate::Connect, clock: &Arc<FakeClock>) -> (Pair, Receiver<i32>) {
    let pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let clock: Arc<dyn Clock> = clock.clone();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(slave, launch(), connect, Box::new(|_| {}), clock);
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (pair, finished)
}

/// Ctrl+C twice, then the exit code within [`DEADLINE`].
fn quit(pair: &mut Pair, finished: &Receiver<i32>) -> i32 {
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"))
}

#[test]
fn run_reconnects_after_a_drop_with_backoff_on_the_fake_clock() {
    let (ours, theirs) = pair();
    let (again, theirs_again) = pair();
    let clock = FakeClock::new();
    let (connect, called, _gone) = dial(vec![(ours, hello()), (again, hello())]);
    let (mut pair, finished) = spawn_run(connect, &clock);
    let frames = watch(&pair.main, vec![b"shortcuts" as &[u8], b"lost" as &[u8]]);
    watched(&frames, "the first frame");
    signalled(&called, "the first connect");
    let reader = BufReader::new(
        theirs
            .try_clone()
            .unwrap_or_else(|err| panic!("dup: {err}")),
    );
    let (reader, feed) = command(reader, "the first feed");
    assert_eq!(feed["command"], "feed");
    // The hub hangs up: home has no working line, so its notice says so.
    drop(reader);
    drop(theirs);
    watched(&frames, "the drop notice");
    assert!(
        clock.await_parked(clock.origin() + HALF, DEADLINE),
        "waited {DEADLINE:?} for the backoff to park on the clock"
    );
    assert!(called.try_recv().is_err());
    clock.advance(HALF);
    signalled(&called, "the reconnect");
    let (_, feed) = command(BufReader::new(theirs_again), "the feed after reconnecting");
    assert_eq!(feed["command"], "feed");
    assert_eq!(quit(&mut pair, &finished), 0);
}

/// A hub `command_accepted` for `id` starting `session`, as a line.
fn started(id: &serde_json::Value, session: &str) -> String {
    let line = HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": id, "result": {"session_id": session}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    format!(
        "{}\n",
        serde_json::to_string(&line).unwrap_or_else(|err| panic!("line: {err}"))
    )
}

#[test]
fn a_prompt_in_flight_is_resent_after_reconnecting() {
    let (ours, mut theirs) = pair();
    let (again, theirs_again) = pair();
    let clock = FakeClock::new();
    let (connect, called, _gone) = dial(vec![(ours, hello()), (again, hello())]);
    let (mut pair, finished) = spawn_run(connect, &clock);
    let frames = watch(&pair.main, vec![b"shortcuts" as &[u8]]);
    watched(&frames, "the first frame");
    signalled(&called, "the first connect");
    let reader = BufReader::new(
        theirs
            .try_clone()
            .unwrap_or_else(|err| panic!("dup: {err}")),
    );
    let (reader, feed) = command(reader, "the first feed");
    assert_eq!(feed["command"], "feed");
    let (reader, _) = command(reader, "the first recent");
    pair.main
        .write_all(b"hi\r")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let (reader, start) = command(reader, "the start");
    assert_eq!(start["command"], "start");
    theirs
        .write_all(started(&start["id"], "s_aaaaaaaaaaaaaaaa").as_bytes())
        .unwrap_or_else(|err| panic!("write: {err}"));
    let (reader, _) = command(reader, "the subscribe");
    let (reader, _) = command(reader, "the commands");
    let (reader, prompt) = command(reader, "the prompt");
    assert_eq!(prompt["command"], "prompt");
    // The hub hangs up before it answers the prompt.
    drop(reader);
    drop(theirs);
    assert!(
        clock.await_parked(clock.origin() + HALF, DEADLINE),
        "waited {DEADLINE:?} for the backoff to park on the clock"
    );
    clock.advance(HALF);
    signalled(&called, "the reconnect");
    let reader = BufReader::new(theirs_again);
    let (reader, subscribe) = command(reader, "the subscribe after reconnecting");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(subscribe["args"]["level"], "full");
    let (reader, commands) = command(reader, "the commands after reconnecting");
    assert_eq!(commands["command"], "commands");
    let (_, resent) = command(reader, "the resent prompt");
    assert_eq!(resent, prompt);
    assert_eq!(quit(&mut pair, &finished), 0);
}

#[test]
fn run_stops_retrying_after_a_refused_schema() {
    let (ours, _theirs) = pair();
    let mut newer = hello();
    newer.schema_version = contract::SCHEMA_VERSION + 1;
    let clock = FakeClock::new();
    let (connect, called, gone) = dial(vec![(ours, newer)]);
    let (mut pair, finished) = spawn_run(connect, &clock);
    let frames = watch(&pair.main, vec![b"shortcuts" as &[u8], b"schema" as &[u8]]);
    watched(&frames, "the first frame");
    signalled(&called, "the first connect");
    watched(&frames, "the refusal notice");
    // No permit: the thread never waits on the clock or connects again.
    assert!(!clock.await_parked(clock.origin() + HALF, QUIET));
    assert!(clock.parked().is_empty());
    assert!(called.try_recv().is_err());
    assert_eq!(quit(&mut pair, &finished), 0);
    signalled(&gone, "the hub thread to end");
    assert!(called.try_recv().is_err());
}

#[test]
fn quitting_during_backoff_ends_the_hub_thread() {
    let clock = FakeClock::new();
    let (connect, called, gone) = dial(Vec::new());
    let (mut pair, finished) = spawn_run(connect, &clock);
    let frames = watch(&pair.main, vec![b"shortcuts" as &[u8]]);
    watched(&frames, "the first frame");
    signalled(&called, "the first connect");
    assert!(
        clock.await_parked(clock.origin() + HALF, DEADLINE),
        "waited {DEADLINE:?} for the backoff to park on the clock"
    );
    assert_eq!(quit(&mut pair, &finished), 0);
    signalled(&gone, "the hub thread to end");
    assert_eq!(clock.now(), clock.origin());
    assert!(called.try_recv().is_err());
}

#[test]
fn dropping_the_loop_ends_a_hub_thread_that_waits_for_a_permit() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let retry = Retry::new(&lp.clock);
    lp.retry = Some(Arc::clone(&retry));
    let (tx, rx) = mpsc::channel();
    let (connect, _called, gone) = dial(Vec::new());
    spawn_hub(connect, tx, retry, Arc::clone(&lp.clock));
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::ConnectFailed(_)) => {}
        Ok(_) => panic!("the first input is the failed connect"),
        Err(err) => panic!("waited {DEADLINE:?} for the failed connect: {err}"),
    }
    drop(lp);
    signalled(&gone, "the hub thread to end");
}

#[test]
fn dropping_the_loop_ends_a_hub_thread_that_reads() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let retry = Retry::new(&lp.clock);
    lp.retry = Some(Arc::clone(&retry));
    let (ours, _theirs) = pair();
    let (tx, rx) = mpsc::channel();
    let (connect, _called, gone) = dial(vec![(ours, hello())]);
    spawn_hub(connect, tx, retry, Arc::clone(&lp.clock));
    let connected = match rx.recv_timeout(DEADLINE) {
        Ok(input @ Input::Connected(..)) => input,
        Ok(_) => panic!("the first input is the connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the connection: {err}"),
    };
    assert_eq!(lp.step(connected, &rx), None);
    assert!(lp.app.connected());
    // The hub's end stays open: only the loop's hang-up ends the read.
    drop(lp);
    signalled(&gone, "the hub thread to end");
}

#[test]
fn dropping_the_loop_before_the_connect_is_stepped_ends_the_read() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let retry = Retry::new(&lp.clock);
    lp.retry = Some(Arc::clone(&retry));
    let (ours, _theirs) = pair();
    let (tx, rx) = mpsc::channel();
    let (connect, _called, gone) = dial(vec![(ours, hello())]);
    spawn_hub(connect, tx, retry, Arc::clone(&lp.clock));
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the first input is the connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the connection: {err}"),
    }
    // The queued connection is never stepped, so the loop holds no
    // stream: only the permit's stop of the watched read ends it.
    // The hub's end stays open throughout.
    drop(lp);
    signalled(&gone, "the hub thread to end");
}

#[test]
fn watch_matches_a_marker_only_at_its_full_length() {
    let mut pty = open();
    let frames = watch(&pty.main, vec![b"lost" as &[u8]]);
    pty.slave
        .write_all(b"los")
        .unwrap_or_else(|err| panic!("write: {err}"));
    match frames.recv_timeout(QUIET) {
        Ok(chunk) => panic!("a marker one byte short matched: {chunk:?}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Err(err) => panic!("the watcher ended before the marker: {err}"),
    }
    pty.slave
        .write_all(b"t")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(
        watched(&frames, "the marker at its full length"),
        b"lost",
        "the chunk ends with the marker and starts at the first byte"
    );
}

#[test]
fn watch_cuts_a_chunk_at_each_marker_in_one_write() {
    let mut pty = open();
    let frames = watch(&pty.main, vec![b"one" as &[u8], b"two" as &[u8]]);
    pty.slave
        .write_all(b"one two")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(
        watched(&frames, "the first marker"),
        b"one",
        "the first chunk ends with the first marker"
    );
    assert_eq!(
        watched(&frames, "the second marker"),
        b" two",
        "the second chunk starts after the first marker"
    );
}

#[test]
fn watch_keeps_draining_after_its_last_marker() {
    let pair = open();
    let frames = watch(&pair.main, vec![b"mark" as &[u8]]);
    let mut slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, written) = mpsc::channel();
    std::thread::Builder::new()
        .name("reconnect-write-past-marker".to_owned())
        .spawn(move || {
            slave
                .write_all(b"mark")
                .unwrap_or_else(|err| panic!("write: {err}"));
            slave
                .write_all(&vec![b'x'; 256 * 1024])
                .unwrap_or_else(|err| panic!("write: {err}"));
            done.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    assert_eq!(
        watched(&frames, "the marker"),
        b"mark",
        "the chunk ends with the marker and starts at the first byte"
    );
    match written.recv_timeout(DEADLINE) {
        Ok(()) => {}
        Err(err) => panic!("waited {DEADLINE:?} for the writer past the last marker: {err}"),
    }
}

#[test]
#[should_panic(expected = "an empty marker")]
fn watch_refuses_an_empty_marker() {
    let pair = open();
    let _ = watch(&pair.main, vec![b"ok" as &[u8], b"" as &[u8]]);
}
