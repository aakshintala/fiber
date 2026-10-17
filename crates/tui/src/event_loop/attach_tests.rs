//! The attach bound (#2232): the parsed live hub events alive at once,
//! from the socket read to the fold, stay within K.

use super::super::hub::Connection;
use super::super::pause;
use super::{
    DEADLINE, answers_all, counted, counting, line, opened, quit, run_queued, serve, session,
    turn_lines,
};
use crate::Input;
use crate::link::{Credits, Feed, Line, read_lines};
use crate::retry::Retry;
use contract::clock::Clock;
use fakes::Deadline;
use serde_json::json;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The parsed live hub events allowed at once: the reader's pending
/// line, the channel and the batch, plus the one lookahead.
const K: usize = 1 + 64 + 64 + 1;

/// The replay's lines, far more than K.
const LINES: usize = 600;

/// A link handing one JSON line per `read` call and reporting each
/// hand-out's line number, then `Ok(0)` after [`LINES`], reporting
/// `LINES + 1` as EOF. `BufReader` never reads ahead of the line the
/// reader is parsing, so lines handed out are lines parsed.
struct FakeLink {
    lines: Vec<Vec<u8>>,
    at: usize,
    within: usize,
    handouts: mpsc::Sender<usize>,
    eof: bool,
}

impl FakeLink {
    fn new(handouts: mpsc::Sender<usize>, first_seq: u64) -> Self {
        Self::with_lines(
            handouts,
            (0..LINES)
                .map(|at| replay_line(first_seq + at as u64, at as u64))
                .collect(),
        )
    }

    fn with_lines(handouts: mpsc::Sender<usize>, lines: Vec<Vec<u8>>) -> Self {
        Self {
            lines,
            at: 0,
            within: 0,
            handouts,
            eof: false,
        }
    }
}

/// One replay line: session line `seq` carrying marker `at`.
fn replay_line(seq: u64, at: u64) -> Vec<u8> {
    let envelope = line(
        "text_completed",
        Some(seq),
        Some("a_m"),
        json!({"text": format!("replay {at}")}),
    );
    let mut bytes = serde_json::to_vec(&envelope).unwrap_or_else(|err| panic!("encode: {err}"));
    bytes.push(b'\n');
    bytes
}

impl Read for FakeLink {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(bytes) = self.lines.get(self.at) else {
            if !self.eof {
                self.eof = true;
                self.handouts.send(self.at + 1).unwrap_or(());
            }
            return Ok(0);
        };
        let start = self.within;
        let end = start + buf.len().min(bytes.len() - start);
        buf[..end - start].copy_from_slice(&bytes[start..end]);
        self.within = end;
        if self.within >= bytes.len() {
            self.at += 1;
            self.within = 0;
            self.handouts.send(self.at).unwrap_or(());
        }
        Ok(end - start)
    }
}

/// Waits until one lane parks in `take` at a full cap: every test fills
/// the cap on its own thread first, so a waiter seen here has reached
/// the full-capacity branch. A cap that never fills fails here, naming
/// the waiter, instead of passing on an unscheduled thread.
#[track_caller]
fn await_waiter(credits: &Credits, what: &str) {
    let wait = Deadline::after(DEADLINE);
    while credits.waiters() == 0 {
        if wait.left().is_zero() {
            panic!("waited {DEADLINE:?} for {what}");
        }
        std::thread::yield_now();
    }
}

#[test]
fn attach_bounded_reader_stops_at_the_queue_cap() {
    let (tx, rx) = mpsc::channel();
    // The connection owns the credits the reader takes at, so the
    // plateau below is exactly the queue's: dropping it ends the
    // reader with one `Disconnected`.
    let hub = Connection::new(None);
    let feed = Feed::new(tx.clone(), hub.credits());
    let (hand_tx, hand_rx) = mpsc::channel();
    let first_seq = session().len() as u64;
    let link = FakeLink::new(hand_tx, first_seq);
    // One deadline for the whole drain to the plateau, made before the
    // reader starts.
    let handouts = Deadline::after(DEADLINE);
    let reader = std::thread::Builder::new()
        .name("attach-read".to_owned())
        .spawn(move || read_lines(link, &feed))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    for want in 1..=65usize {
        match handouts.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // The 65th line waits for a unit the loop's first receive releases:
    // the reader parks at the full-capacity branch, so no 66th line is
    // parsed.
    await_waiter(&hub.credits(), "the reader to wait at the queue cap");
    assert!(
        hand_rx.try_recv().is_err(),
        "hand-out 66 arrived with 65 lines unreceived"
    );
    // One receive releases one unit: hand-out 66 follows, and 67 does
    // not — the 64/65/66 edges. The release is the loop's receive of
    // a hub line, explicit here with no loop running.
    let released = Deadline::after(DEADLINE);
    let first = match released.recv(&rx) {
        Ok(input @ crate::Input::Hub(_)) => input,
        Ok(_) => panic!("the first queued input is a hub line"),
        Err(err) => panic!("waited {DEADLINE:?} for the first queued line: {err}"),
    };
    hub.credits().received(&first);
    match released.recv(&hand_rx) {
        Ok(got) => assert_eq!(got, 66, "one receive releases one line"),
        Err(err) => panic!("waited {DEADLINE:?} for hand-out 66: {err}"),
    }
    await_waiter(&hub.credits(), "the reader to wait after one receive");
    assert!(
        hand_rx.try_recv().is_err(),
        "hand-out 67 arrived after one receive"
    );
    drop(hub);
    drop(rx);
    drop(tx);
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

#[test]
fn attach_bounded_live_events_stay_within_k_while_the_fold_is_held() {
    let (mut lp, theirs, tx, rx) = opened();
    let _served = serve(theirs, tx.clone(), answers_all());
    let (hand_tx, hand_rx) = mpsc::channel();
    let first_seq = session().len() as u64;
    let link = FakeLink::new(hand_tx, first_seq);
    // The reader runs first, with no consumer, taking at the loop's
    // credits.
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    std::thread::Builder::new()
        .name("attach-read".to_owned())
        .spawn(move || read_lines(link, &feed))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let handouts = Deadline::after(DEADLINE);
    for want in 1..=65usize {
        match handouts.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // Then the loop runs on a worker with the fold held: every wait
    // after this action is a new wait with its own deadline. The credits
    // stay here, so the test still sees the reader park.
    let credits = lp.hub.credits();
    let (reached_tx, reached) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-run".to_owned())
        .spawn(move || {
            pause::install(reached_tx, release);
            let code = lp.run(&rx);
            done_tx.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    Deadline::after(DEADLINE).recv_or_fail(&reached, "the loop to hold before the fold");
    let refill = Deadline::after(DEADLINE);
    for want in 66..=129usize {
        match refill.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // The fold is held before folding any of the 129 parsed lines, so
    // the 130th line waits at the full-capacity branch: the parked
    // reader parses nothing more.
    await_waiter(&credits, "the reader to wait with the fold held");
    assert!(
        hand_rx.try_recv().is_err(),
        "hand-out 130 arrived with the fold held"
    );
    let handed = 129;
    let folded = 0;
    assert!(
        handed - folded <= K,
        "live parsed events {handed} exceed K = {K} with {folded} folded"
    );
    // Dropping the release sender lets every later fold through, and a
    // panicking test releases the loop the same way.
    drop(release_tx);
    // Draining hand-outs to the EOF report on one deadline: the loop
    // folds every chunk once released.
    let eof = Deadline::after(DEADLINE);
    loop {
        match eof.recv(&hand_rx) {
            Ok(got) if got == LINES + 1 => break,
            Ok(_) => {}
            Err(err) => panic!("waited {DEADLINE:?} for the EOF report: {err}"),
        }
    }
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    let returned = Deadline::after(DEADLINE);
    let (code, _) = returned.recv_or_fail(&done, "the loop to quit");
    assert_eq!(code, 0);
}

/// One hub line with identity `seq`.
fn hub_line(seq: u64) -> Input {
    Input::Hub(Line::Session(line(
        &format!("line-{seq}"),
        Some(seq),
        Some("a_m"),
        json!({"text": format!("line {seq}")}),
    )))
}

/// What each input is, in order: hub identities, or the input's shape.
fn kinds(inputs: &[Input]) -> Vec<String> {
    inputs
        .iter()
        .map(|input| match input {
            Input::Hub(Line::Session(line)) => line.kind.clone(),
            Input::Hub(Line::Hub(line)) => line.kind.clone(),
            Input::Bytes(_) => "bytes".to_owned(),
            Input::Tick => "tick".to_owned(),
            Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Signal(_)
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Viewed { .. }
            | Input::Models(_)
            | Input::Login { .. } => "other".to_owned(),
        })
        .collect()
}

/// One wire line with identity `name`.
fn wire(name: &str) -> String {
    serde_json::json!({"kind": name, "session_id": "s_x", "ts": 0,
        "schema_version": contract::SCHEMA_VERSION, "payload": {}})
    .to_string()
}

/// Writes `lines` to the hub's end, one per line.
fn write_lines(client: &mut UnixStream, lines: &[String]) {
    for line in lines {
        client
            .write_all(format!("{line}\n").as_bytes())
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
}

/// Reads `end` on a thread through `feed`, signalling its return.
fn spawn_reader(
    end: impl Read + Send + 'static,
    feed: Feed,
) -> (std::thread::JoinHandle<()>, mpsc::Receiver<()>) {
    let (done_tx, done) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("attach-read".to_owned())
        .spawn(move || {
            read_lines(end, &feed);
            done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (reader, done)
}

/// The hub's greeting.
fn hello() -> contract::HubLine {
    contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }
}

/// A socket pair: the terminal's end and the hub's.
fn pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"))
}

/// A connect handing out `streams` in order and failing after, reporting
/// each call and its drop, which happens when the hub thread that owns
/// it returns.
fn dial(
    streams: Vec<(UnixStream, contract::HubLine)>,
) -> (crate::Connect, mpsc::Receiver<()>, mpsc::Receiver<()>) {
    struct Dial {
        streams: VecDeque<(UnixStream, contract::HubLine)>,
        calls: mpsc::Sender<()>,
        gone: mpsc::Sender<()>,
    }
    impl Dial {
        fn next(&mut self) -> std::io::Result<(UnixStream, contract::HubLine)> {
            self.calls.send(()).unwrap_or(());
            self.streams
                .pop_front()
                .ok_or_else(|| std::io::Error::other("refused"))
        }
    }
    impl Drop for Dial {
        fn drop(&mut self) {
            self.gone.send(()).unwrap_or(());
        }
    }
    let (calls, called) = mpsc::channel();
    let (gone, ended) = mpsc::channel();
    let mut dial = Dial {
        streams: streams.into(),
        calls,
        gone,
    };
    (Box::new(move || dial.next()), called, ended)
}

#[test]
fn a_key_behind_a_full_queue_keeps_its_own_frame() {
    key_behind_a_full_queue(Input::Bytes(b"a".to_vec()));
}

#[test]
fn a_resize_behind_a_full_queue_keeps_its_own_frame() {
    key_behind_a_full_queue(Input::Resize);
}

/// 128 hub inputs ready; `take_batch` returns chunk 1 (64); the probe
/// sent next; `next_chunk` returns chunk 2 (the 64 hub inputs that
/// arrived before it); the next `next_chunk` returns `None` with the
/// probe at the stash's front; `take_batch` returns the probe alone; a
/// hub input sent after it runs in the batch after it, never before.
#[track_caller]
fn key_behind_a_full_queue(probe: Input) {
    let mut hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    for seq in 0..128u64 {
        tx.send(hub_line(seq))
            .unwrap_or_else(|err| panic!("send: {err}"));
    }
    let chunk1: Vec<_> = hub.take_batch(&rx).expect("chunk 1").into_iter().collect();
    assert_eq!(chunk1.len(), 64);
    let probe_kind = if matches!(&probe, Input::Bytes(_)) {
        "bytes"
    } else {
        "other"
    };
    tx.send(probe).unwrap_or_else(|err| panic!("send: {err}"));
    let chunk2: Vec<_> = hub
        .next_chunk(&rx, 64)
        .expect("chunk 2")
        .into_iter()
        .collect();
    assert_eq!(chunk2.len(), 64);
    assert!(hub.next_chunk(&rx, 128).is_none());
    assert_eq!(
        hub.stashed().len(),
        1,
        "the probe waits at the stash's front"
    );
    // The chunks hold the hub inputs that arrived before the probe, in
    // order, none dropped or duplicated.
    let held: Vec<String> = [chunk1, chunk2]
        .iter()
        .flat_map(|chunk| kinds(chunk))
        .collect();
    let want: Vec<String> = (0..128).map(|seq| format!("line-{seq}")).collect();
    assert_eq!(held, want);
    let alone: Vec<_> = hub
        .take_batch(&rx)
        .expect("the probe")
        .into_iter()
        .collect();
    assert_eq!(alone.len(), 1);
    assert_eq!(kinds(&alone), [probe_kind]);
    tx.send(hub_line(128))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let after: Vec<_> = hub.take_batch(&rx).expect("after").into_iter().collect();
    assert_eq!(kinds(&after), ["line-128"]);
}

#[test]
fn a_key_among_128_plus_64_lines_draws_its_own_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut turn = 0;
    let mut inputs = Vec::new();
    while inputs.len() < 128 {
        let mut lines = turn_lines(&format!("marker {turn}"), turn, &mut seq);
        turn += 1;
        lines.truncate(128 - inputs.len());
        inputs.extend(lines);
    }
    inputs.push(Input::Bytes(b"a".to_vec()));
    while inputs.len() < 128 + 1 + 64 {
        let mut lines = turn_lines(&format!("marker {turn}"), turn, &mut seq);
        turn += 1;
        lines.truncate(128 + 1 + 64 - inputs.len());
        inputs.extend(lines);
    }
    // The last turn may truncate to its `turn_started` alone, which is
    // always pushed: its prompt proves the last line folded.
    let last = format!("prompt {}", turn - 1);
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The 128 as one logical batch, the key's own, the last 64.
    assert_eq!(flushes.get(), 3);
    assert_eq!(lp.app.draft(), "a");
    let screen = counted(&lp);
    assert!(screen.contains(&last), "{screen}");
}

#[test]
fn quit_while_full_ends_the_reader() {
    let hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    let feed = Feed::new(tx.clone(), hub.credits());
    let (mut client, end) = pair();
    write_lines(
        &mut client,
        &(0..65u64)
            .map(|seq| wire(&format!("s1-{seq}")))
            .collect::<Vec<_>>(),
    );
    let (reader, done) = spawn_reader(end, feed);
    // The reader sends 64 lines, then waits for a unit with the 65th
    // parsed and pending: drain the 64 first, in order, so the hang-up
    // below lands while it waits.
    let waiting = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        match waiting.recv(&rx) {
            Ok(input) => assert_eq!(kinds(std::slice::from_ref(&input)), [format!("s1-{seq}")]),
            Err(err) => panic!("waited {DEADLINE:?} for line {seq}: {err}"),
        }
    }
    drop(hub);
    // The reader ends: its disconnect is the last input.
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    let queued = Deadline::after(DEADLINE);
    match queued.recv(&rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect after the queued lines"),
        Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
    }
    assert!(rx.try_recv().is_err(), "nothing follows the disconnect");
    drop(tx);
}

#[test]
fn hang_up_while_full_then_reconnect() {
    let mut hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    let feed = Feed::new(tx.clone(), hub.credits());
    // The first stream hands out far more lines than the queue holds.
    // The plateau, then one chunk through the connection, leaves the
    // reader waiting with the 129th line parsed and pending: a take
    // after the cancel fails even with room, and the link holds a
    // parsed line for a reader still in its read.
    let (hand_tx, hand_rx) = mpsc::channel();
    let first_seq = session().len() as u64;
    let link1 = FakeLink::with_lines(hand_tx, stream_lines("s1", 200, first_seq));
    let lane1 = feed.lane();
    let (reader1, done1) = spawn_reader(link1, lane1);
    let plateau = Deadline::after(DEADLINE);
    for want in 1..=65usize {
        match plateau.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // One chunk through the connection releases 64 units, so the
    // reader refills the queue and waits again.
    let chunk1: Vec<_> = hub.take_batch(&rx).expect("chunk 1").into_iter().collect();
    assert_eq!(chunk1.len(), 64);
    assert_eq!(
        kinds(&chunk1)[0],
        "s1-0",
        "the first chunk holds the first stream's lines",
    );
    let refill = Deadline::after(DEADLINE);
    for want in 66..=129usize {
        match refill.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    hub.hang_up();
    // The cancelled reader sends only its disconnect: chunk 2 through
    // `take_batch`, the disconnect at the stash's front, then the
    // disconnect alone.
    let chunk2: Vec<_> = hub.take_batch(&rx).expect("chunk 2").into_iter().collect();
    assert_eq!(chunk2.len(), 64);
    let held: Vec<String> = [chunk1, chunk2]
        .iter()
        .flat_map(|chunk| kinds(chunk))
        .collect();
    let want: Vec<String> = (0..128).map(|seq| format!("s1-{seq}")).collect();
    assert_eq!(held, want);
    assert!(hub.next_chunk(&rx, 128).is_none());
    let end: Vec<_> = hub
        .take_batch(&rx)
        .expect("the disconnect")
        .into_iter()
        .collect();
    assert_eq!(end.len(), 1);
    assert!(matches!(end.first(), Some(Input::Disconnected)));
    Deadline::after(DEADLINE).recv_or_fail(&done1, "the first reader to return");
    reader1.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    // A lane made after the cancel is not cancelled: the second stream
    // reaches its own plateau of 65, with only its own identities. No
    // line of the first stream follows its disconnect.
    let (hand2_tx, hand2_rx) = mpsc::channel();
    let link2 = FakeLink::with_lines(hand2_tx, stream_lines("s2", 65, first_seq));
    let lane2 = feed.lane();
    let (reader2, done2) = spawn_reader(link2, lane2);
    let handouts = Deadline::after(DEADLINE);
    for want in 1..=65usize {
        match handouts.recv(&hand2_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    let second = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        match second.recv(&rx) {
            Ok(input) => assert_eq!(kinds(std::slice::from_ref(&input)), [format!("s2-{seq}")]),
            Err(err) => panic!("waited {DEADLINE:?} for line {seq} of the second stream: {err}"),
        }
    }
    // The second reader parks at its own plateau, so nothing more
    // arrives.
    await_waiter(&hub.credits(), "the second reader to wait at its plateau");
    assert!(
        rx.try_recv().is_err(),
        "the second stream passed its plateau"
    );
    drop(hub);
    let last = Deadline::after(DEADLINE);
    match last.recv(&rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect for the second stream"),
        Err(err) => panic!("waited {DEADLINE:?} for the second disconnect: {err}"),
    }
    Deadline::after(DEADLINE).recv_or_fail(&done2, "the second reader to return");
    reader2.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    drop(tx);
}

/// Wire lines with identities `{prefix}-{i}`, numbered from `first_seq`.
fn stream_lines(prefix: &str, count: usize, first_seq: u64) -> Vec<Vec<u8>> {
    (0..count)
        .map(|at| {
            let envelope = line(
                &format!("{prefix}-{at}"),
                Some(first_seq + at as u64),
                Some("a_m"),
                json!({"text": format!("{prefix} {at}")}),
            );
            let mut bytes =
                serde_json::to_vec(&envelope).unwrap_or_else(|err| panic!("encode: {err}"));
            bytes.push(b'\n');
            bytes
        })
        .collect()
}

/// Receives one hub line with identity `want` on `wait`.
#[track_caller]
fn recv_line(rx: &mpsc::Receiver<Input>, wait: &Deadline, want: &str) {
    match wait.recv(rx) {
        Ok(input) => assert_eq!(kinds(std::slice::from_ref(&input)), [want]),
        Err(err) => panic!("waited {DEADLINE:?} for {want}: {err}"),
    }
}

/// Receives the disconnect ending one stream.
#[track_caller]
fn recv_disconnect(rx: &mpsc::Receiver<Input>, wait: &Deadline) {
    match wait.recv(rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect"),
        Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
    }
}

#[test]
fn routed_loop_wait_lends_one_queue_then_parks_or_reconnects_at_the_bound() {
    use std::io::{BufRead, BufReader};
    for extra in [64, 65] {
        let d = Deadline::start();
        let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        let retry = Retry::new(&clock);
        let hub = Connection::new(Some(Arc::clone(&retry)));
        let router = crate::remote::Router::new();
        let (tx, rx) = mpsc::channel();
        let feed = Feed::new(tx, hub.credits()).routed(Arc::clone(&router));
        let (ours, theirs) = pair();
        let (next, next_hub) = pair();
        let (release, resume) = mpsc::channel();
        let (served, server_done) = mpsc::channel();
        std::thread::spawn(move || {
            for index in 0..64 {
                crate::link::write_line(&theirs, &wire(&format!("before-{index}"))).unwrap();
            }
            let mut request = String::new();
            BufReader::new(theirs.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            for index in 0..extra {
                crate::link::write_line(&theirs, &wire(&format!("extra-{index}"))).unwrap();
            }
            crate::link::write_line(&theirs,&json!({"kind":"command_accepted","ts":0,"schema_version":1,"payload":{"command_id":request["id"],"result":{"ok":true}}}).to_string()).unwrap_or(());
            resume.recv().unwrap_or(());
            if extra == 64 {
                crate::link::write_line(&theirs, &wire("after-lend")).unwrap();
            }
            served.send(()).unwrap();
        });
        let (connect, _called, gone) = dial(vec![(ours, hello()), (next, hello())]);
        crate::sources::spawn_hub(connect, feed, Arc::clone(&retry), clock);
        assert!(matches!(
            d.recv_or_fail(&rx, "connected before lend"),
            Input::Connected(..)
        ));
        let mut held = Vec::new();
        for index in 0..64 {
            let input = d.recv_or_fail(&rx, "initial full queue");
            assert_eq!(
                kinds(std::slice::from_ref(&input)),
                [format!("before-{index}")]
            );
            held.push(input);
        }
        let worker = Arc::clone(&router);
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            done.send(worker.ask(
                "configure",
                json!({"operation":"settings"}),
                crate::remote::Caller::Loop,
            ))
            .unwrap();
        });
        let answer = d.recv_or_fail(&result, "routed call past full queue");
        for index in 0..64 {
            let input = d.recv_or_fail(&rx, "extra bounded queue");
            assert_eq!(
                kinds(std::slice::from_ref(&input)),
                [format!("extra-{index}")]
            );
            held.push(input);
        }
        if extra == 64 {
            assert_eq!(
                answer.unwrap().payload.get("result"),
                Some(&json!({"ok":true}))
            );
            release.send(()).unwrap();
            await_waiter(&hub.credits(), "reader after lend drops above cap");
            assert!(rx.try_recv().is_err());
            for input in held.iter().take(65) {
                hub.credits().received(input);
            }
            recv_line(&rx, &d, "after-lend");
            recv_disconnect(&rx, &d);
        } else {
            assert_eq!(answer.unwrap_err(), crate::remote::AskError::Lost);
            recv_disconnect(&rx, &d);
            release.send(()).unwrap();
            for input in &held {
                hub.credits().received(input);
            }
            retry.give(Duration::ZERO);
            assert!(matches!(
                d.recv_or_fail(&rx, "reconnected after lend overflow"),
                Input::Connected(..)
            ));
        }
        d.recv_or_fail(&server_done, "flood server done");
        drop(next_hub);
        drop(hub);
        d.recv_or_fail(&gone, "hub thread stopped");
    }
}

#[test]
fn spawn_hub_full_queue_hang_up_then_reconnect() {
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let retry = Retry::new(&clock);
    let mut hub = Connection::new(Some(Arc::clone(&retry)));
    let (tx, rx) = mpsc::channel();
    let feed = Feed::new(tx.clone(), hub.credits());
    let (ours1, theirs1) = pair();
    let (ours2, theirs2) = pair();
    let mut theirs1 = theirs1;
    let mut theirs2 = theirs2;
    write_lines(
        &mut theirs1,
        &(0..66u64)
            .map(|seq| wire(&format!("s1-{seq}")))
            .collect::<Vec<_>>(),
    );
    write_lines(
        &mut theirs2,
        &(0..66u64)
            .map(|seq| wire(&format!("s2-{seq}")))
            .collect::<Vec<_>>(),
    );
    let (connect, _called, gone) = dial(vec![(ours1, hello()), (ours2, hello())]);
    crate::sources::spawn_hub(connect, feed, Arc::clone(&retry), Arc::clone(&clock));
    // The connection, then exactly 64 hub inputs with raw receives:
    // raw receives release nothing.
    let connected = Deadline::after(DEADLINE);
    match connected.recv(&rx) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the first input is the connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the connection: {err}"),
    }
    let queued = Deadline::after(DEADLINE);
    let mut drained = Vec::new();
    for seq in 0..64u64 {
        let input = match queued.recv(&rx) {
            Ok(input) => input,
            Err(err) => panic!("waited {DEADLINE:?} for s1-{seq}: {err}"),
        };
        assert_eq!(kinds(std::slice::from_ref(&input)), [format!("s1-{seq}")]);
        drained.push(input);
    }
    // The 65th waits for a unit: the reader parks at the
    // full-capacity branch, so nothing more arrives.
    await_waiter(&hub.credits(), "the reader to wait at the queue cap");
    assert!(
        rx.try_recv().is_err(),
        "the 65th line arrived with none released"
    );
    // Releasing one unit lets the 65th through: no more, so the 66th
    // stays parsed-but-waiting and the hang-up below lands while the
    // reader waits.
    hub.credits().received(&drained[0]);
    let arrived = Deadline::after(DEADLINE);
    let last = match arrived.recv(&rx) {
        Ok(input) => input,
        Err(err) => panic!("waited {DEADLINE:?} for s1-64: {err}"),
    };
    assert_eq!(kinds(std::slice::from_ref(&last)), ["s1-64"]);
    drained.push(last);
    hub.hang_up();
    // The reader ends: one disconnect follows the queued lines.
    let ended = Deadline::after(DEADLINE);
    recv_disconnect(&rx, &ended);
    // Fill again: release every drained line, so the second stream
    // starts with its whole queue of credit. Releases saturate at
    // zero.
    for input in &drained {
        hub.credits().received(input);
    }
    // The permit lets the second stream through: its lines are the only
    // ones after that disconnect, up to its own plateau of 64.
    retry.give(Duration::ZERO);
    let connected2 = Deadline::after(DEADLINE);
    match connected2.recv(&rx) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the second input is the second connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the second connection: {err}"),
    }
    let second = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        recv_line(&rx, &second, &format!("s2-{seq}"));
    }
    // The second reader parks at its own plateau, so nothing more
    // arrives.
    await_waiter(&hub.credits(), "the second reader to wait at its plateau");
    assert!(
        rx.try_recv().is_err(),
        "the second stream passed its plateau"
    );
    drop(hub);
    let last = Deadline::after(DEADLINE);
    recv_disconnect(&rx, &last);
    match Deadline::after(DEADLINE).recv(&gone) {
        Ok(()) => {}
        Err(err) => panic!("waited {DEADLINE:?} for the hub thread to end: {err}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "nothing follows the last disconnect"
    );
    drop(tx);
}

#[test]
fn dropping_the_connection_ends_a_full_reader() {
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let retry = Retry::new(&clock);
    let hub = Connection::new(Some(Arc::clone(&retry)));
    let (tx, rx) = mpsc::channel();
    let feed = Feed::new(tx.clone(), hub.credits());
    let (ours1, theirs1) = pair();
    let mut theirs1 = theirs1;
    write_lines(
        &mut theirs1,
        &(0..66u64)
            .map(|seq| wire(&format!("s1-{seq}")))
            .collect::<Vec<_>>(),
    );
    let (connect, _called, gone) = dial(vec![(ours1, hello())]);
    crate::sources::spawn_hub(connect, feed, Arc::clone(&retry), Arc::clone(&clock));
    let connected = Deadline::after(DEADLINE);
    match connected.recv(&rx) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the first input is the connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the connection: {err}"),
    }
    let queued = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        recv_line(&rx, &queued, &format!("s1-{seq}"));
    }
    // Dropping the connection ends the waiting reader the same way a
    // hang-up does: it quits the permit and cancels the capacity wait.
    drop(hub);
    let ended = Deadline::after(DEADLINE);
    recv_disconnect(&rx, &ended);
    match Deadline::after(DEADLINE).recv(&gone) {
        Ok(()) => {}
        Err(err) => panic!("waited {DEADLINE:?} for the hub thread to end: {err}"),
    }
    assert!(rx.try_recv().is_err(), "nothing follows the disconnect");
    drop(tx);
}

#[test]
fn spawn_hub_overlapping_reconnect_while_full() {
    // The permit is already given, as at `event_loop.rs:230` before
    // `hub.rs:276` quits the permit: the hub thread may connect reader
    // 2 while reader 1 still waits.
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let retry = Retry::new(&clock);
    retry.give(Duration::ZERO);
    let mut hub = Connection::new(Some(Arc::clone(&retry)));
    let (tx, rx) = mpsc::channel();
    let feed = Feed::new(tx.clone(), hub.credits());
    let (ours1, theirs1) = pair();
    let (ours2, theirs2) = pair();
    let mut theirs1 = theirs1;
    let mut theirs2 = theirs2;
    write_lines(
        &mut theirs1,
        &(0..66u64)
            .map(|seq| wire(&format!("s1-{seq}")))
            .collect::<Vec<_>>(),
    );
    write_lines(
        &mut theirs2,
        &(0..66u64)
            .map(|seq| wire(&format!("s2-{seq}")))
            .collect::<Vec<_>>(),
    );
    let (connect, _called, gone) = dial(vec![(ours1, hello()), (ours2, hello())]);
    crate::sources::spawn_hub(connect, feed, Arc::clone(&retry), Arc::clone(&clock));
    let connected = Deadline::after(DEADLINE);
    match connected.recv(&rx) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the first input is the connection"),
        Err(err) => panic!("waited {DEADLINE:?} for the connection: {err}"),
    }
    let queued = Deadline::after(DEADLINE);
    let mut drained = Vec::new();
    for seq in 0..64u64 {
        let input = match queued.recv(&rx) {
            Ok(input) => input,
            Err(err) => panic!("waited {DEADLINE:?} for s1-{seq}: {err}"),
        };
        assert_eq!(kinds(std::slice::from_ref(&input)), [format!("s1-{seq}")]);
        drained.push(input);
    }
    // Reader 1 waits with the 65th parsed and pending: no release yet,
    // so the hang-up lands while it waits. Then release every drained
    // line, so the second stream starts with its whole queue of
    // credit; the permit already given connects reader 2, which fills
    // its own queue and waits.
    hub.hang_up();
    let ended1 = Deadline::after(DEADLINE);
    recv_disconnect(&rx, &ended1);
    for input in &drained {
        hub.credits().received(input);
    }
    let connected2 = Deadline::after(DEADLINE);
    match connected2.recv(&rx) {
        Ok(Input::Connected(..)) => {}
        Ok(_) => panic!("the next connection is the second stream's"),
        Err(err) => panic!("waited {DEADLINE:?} for the second connection: {err}"),
    }
    let second = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        recv_line(&rx, &second, &format!("s2-{seq}"));
    }
    // No input of the first stream follows its disconnect: every input
    // after it is the second stream's.
    drop(hub);
    let ended2 = Deadline::after(DEADLINE);
    recv_disconnect(&rx, &ended2);
    match Deadline::after(DEADLINE).recv(&gone) {
        Ok(()) => {}
        Err(err) => panic!("waited {DEADLINE:?} for the hub thread to end: {err}"),
    }
    // The second disconnect is the last input.
    assert!(
        rx.try_recv().is_err(),
        "nothing follows the last disconnect"
    );
    drop(tx);
}

/// One replay envelope with identity `seq`.
fn replay(seq: u64) -> contract::Envelope {
    line(
        &format!("replay-{seq}"),
        Some(seq),
        Some("a_m"),
        json!({"text": format!("replay {seq}")}),
    )
}

/// The answer to command `id` carrying `lines`.
fn command_answer(id: &str, lines: Vec<contract::Envelope>) -> contract::Envelope {
    let mut answer = line("command_accepted", None, None, json!({}));
    answer.payload = json!({"command_id": id, "result": {"lines": lines}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    answer
}

/// The socket lines `replay` then `answer`, each with its newline.
fn socket_lines(replay: &[contract::Envelope], answer: &contract::Envelope) -> Vec<String> {
    replay
        .iter()
        .chain(std::iter::once(answer))
        .map(|envelope| {
            serde_json::to_string(envelope).unwrap_or_else(|err| panic!("encode: {err}"))
        })
        .collect()
}

/// A history wait behind replay: the answer's result, the stash's peak
/// and length, and the loop, channel, reader done and join handle back
/// for draining and ending the reader.
type WaitedHistory = (
    Result<Vec<contract::Envelope>, String>,
    usize,
    usize,
    super::super::Loop<ratatui::backend::TestBackend>,
    mpsc::Receiver<Input>,
    mpsc::Receiver<()>,
    std::thread::JoinHandle<()>,
);

/// Runs a history wait behind `replay` lines on its own thread: the
/// answer's result, the stash's peak and length, and the loop back for
/// draining.
#[track_caller]
fn wait_behind_replay(
    replay_lines: Vec<contract::Envelope>,
    answer: contract::Envelope,
) -> WaitedHistory {
    let (mut lp, _theirs, tx, rx) = opened();
    let (write_end, read_end) = pair();
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    let (reader, done) = spawn_reader(read_end, feed);
    let (answered_tx, answered) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-answer".to_owned())
        .spawn(move || {
            let result =
                lp.hub
                    .answer(&rx, "a_1", &mut lp.app, &lp.tick)
                    .map(|answer| match answer {
                        super::super::hub::Answer::Lines(lines) => lines,
                        super::super::hub::Answer::Reopened => {
                            panic!("no reopening in this script")
                        }
                    });
            let peak = lp.hub.answer_stash_peak();
            let stashed = lp.hub.stashed().len();
            answered_tx
                .send((result, peak, stashed, lp, rx))
                .unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Written only once the reader and the wait run: a full socket
    // buffer never parks the writer before the reader starts.
    let mut write_end = write_end;
    write_lines(&mut write_end, &socket_lines(&replay_lines, &answer));
    drop(write_end);
    let answered_wait = Deadline::after(DEADLINE);
    let (result, peak, stashed, lp, rx) = answered_wait.recv_or_fail(&answered, "the history wait");
    (result, peak, stashed, lp, rx, done, reader)
}

#[test]
fn a_history_wait_behind_replay_stashes_and_returns() {
    // 300 session lines, then the matching answer: progress, not a
    // bound — the wait completes with the stash growing.
    let replay_lines: Vec<_> = (0..300u64).map(replay).collect();
    let want = vec![replay(1000), replay(1001)];
    let (result, peak, stashed, mut lp, rx, done, reader) =
        wait_behind_replay(replay_lines, command_answer("a_1", want.clone()));
    assert_eq!(result.unwrap_or_else(|err| panic!("answer: {err}")), want);
    assert_eq!(peak, 300);
    assert_eq!(stashed, 300);
    // Draining yields the 300 identities in order, the first 64 as one
    // chunk.
    let mut drained = Vec::new();
    let Some(first) = lp.hub.take_batch(&rx) else {
        panic!("the first chunk");
    };
    let mut taken = 0usize;
    let mut chunk = Some(first);
    while let Some(batch) = chunk {
        let inputs: Vec<_> = batch.into_iter().collect();
        taken += inputs.len();
        drained.extend(kinds(&inputs));
        chunk = lp.hub.next_chunk(&rx, taken);
    }
    assert_eq!(taken, 300);
    let first_chunk: Vec<String> = drained.iter().take(64).cloned().collect();
    assert_eq!(first_chunk.len(), 64);
    let want_kinds: Vec<String> = (0..300).map(|seq| format!("replay-{seq}")).collect();
    assert_eq!(drained, want_kinds);
    // The reader ends on EOF behind the stash.
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

#[test]
fn an_answer_first_leaves_no_stash() {
    // The answer arrives before any replay: nothing is stashed. The
    // link hands the answer first, so the wait ends at once; the
    // replay behind it fills the queue, and dropping the loop ends
    // the reader waiting for a unit.
    let (mut lp, _theirs, tx, rx) = opened();
    let first_seq = session().len() as u64;
    let want = vec![replay(first_seq + 1000)];
    let mut lines = vec![{
        let answer = command_answer("a_1", want.clone());
        let mut bytes = serde_json::to_vec(&answer).unwrap_or_else(|err| panic!("encode: {err}"));
        bytes.push(b'\n');
        bytes
    }];
    lines.extend((0..64u64).map(|at| replay_line(first_seq + at, at)));
    let (hand_tx, hand_rx) = mpsc::channel();
    let link = FakeLink::with_lines(hand_tx, lines);
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    let (reader, done) = spawn_reader(link, feed);
    let (answered_tx, answered) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-answer".to_owned())
        .spawn(move || {
            let result =
                lp.hub
                    .answer(&rx, "a_1", &mut lp.app, &lp.tick)
                    .map(|answer| match answer {
                        super::super::hub::Answer::Lines(lines) => lines,
                        super::super::hub::Answer::Reopened => {
                            panic!("no reopening in this script")
                        }
                    });
            let peak = lp.hub.answer_stash_peak();
            let stashed = lp.hub.stashed().len();
            answered_tx.send((result, peak, stashed, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The link hands the answer first: one drain for its hand-out.
    let leading = Deadline::after(DEADLINE);
    match leading.recv(&hand_rx) {
        Ok(got) => assert_eq!(got, 1, "the answer is handed out first"),
        Err(err) => panic!("waited {DEADLINE:?} for the answer's hand-out: {err}"),
    }
    let answered_wait = Deadline::after(DEADLINE);
    let (result, peak, stashed, lp) = answered_wait.recv_or_fail(&answered, "the history wait");
    assert_eq!(result.unwrap_or_else(|err| panic!("answer: {err}")), want);
    assert_eq!(peak, 0);
    assert_eq!(stashed, 0);
    // The replay behind it fills the queue; dropping the loop ends the
    // reader waiting for a unit.
    drop(lp);
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

#[test]
fn bad_answers_behind_replay_keep_the_stash() {
    // Rejected, unreadable and empty answers behind replay return their
    // existing results with the same stash.
    let mut rejected = line("command_rejected", None, None, json!({}));
    rejected.payload = json!({"command_id": "a_1", "message": "gone"})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut unreadable = line("command_accepted", None, None, json!({}));
    unreadable.payload = json!({"command_id": "a_1", "result": {}})
        .as_object()
        .cloned()
        .unwrap_or_default();
    let empty = command_answer("a_1", Vec::new());
    for (name, answer, check) in [
        ("rejected", rejected, "Err(gone)"),
        (
            "unreadable",
            unreadable,
            "Err(the answer could not be read)",
        ),
        ("empty", empty, "Ok(vec![])"),
    ] {
        let replay_lines: Vec<_> = (0..30u64).map(replay).collect();
        let (result, peak, stashed, lp, _rx, done, reader) =
            wait_behind_replay(replay_lines, answer);
        match (name, result) {
            ("rejected", Err(message)) => assert_eq!(message, "gone"),
            ("unreadable", Err(message)) => assert_eq!(message, "the answer could not be read"),
            ("empty", Ok(lines)) => assert!(lines.is_empty()),
            (name, result) => panic!("{name} holds its result, not {check}: {result:?}"),
        }
        assert_eq!(peak, 30, "{name} records its stash");
        assert_eq!(stashed, 30, "{name} keeps its stash");
        drop(lp);
        Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
        reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    }
}

/// Drains one logical batch through `take_batch` then `next_chunk`
/// until `None`: the chunk sizes, and the inputs folded.
#[track_caller]
fn drain_logical(hub: &mut Connection, rx: &mpsc::Receiver<Input>) -> Vec<Vec<Input>> {
    let mut chunks = Vec::new();
    let Some(first) = hub.take_batch(rx) else {
        return chunks;
    };
    let mut taken = 0usize;
    let mut chunk = Some(first);
    while let Some(batch) = chunk {
        let inputs: Vec<_> = batch.into_iter().collect();
        taken += inputs.len();
        chunks.push(inputs);
        chunk = hub.next_chunk(rx, taken);
    }
    chunks
}

#[test]
fn logical_batch_4095_4096_4097() {
    // 4095, 4096 and 4097 ready hub inputs give logical batches of
    // 4095; 4096; and 4096 then 1.
    for (count, sizes) in [
        (
            4095usize,
            vec![64; 63].into_iter().chain([63]).collect::<Vec<_>>(),
        ),
        (4096, vec![64; 64]),
        (4097, vec![64; 64]),
    ] {
        let mut hub = Connection::new(None);
        let (tx, rx) = mpsc::channel();
        for seq in 0..count as u64 {
            tx.send(hub_line(seq))
                .unwrap_or_else(|err| panic!("send: {err}"));
        }
        // The first logical batch: every chunk but the last holds 64.
        let chunks = drain_logical(&mut hub, &rx);
        let got: Vec<usize> = chunks.iter().map(Vec::len).collect();
        assert_eq!(got, sizes, "{count} inputs");
        let kinds_got: Vec<String> = chunks.iter().flat_map(|chunk| kinds(chunk)).collect();
        let want: Vec<String> = (0..got.iter().sum::<usize>() as u64)
            .map(|seq| format!("line-{seq}"))
            .collect();
        assert_eq!(kinds_got, want, "ordered identities for {count}");
        if count == 4097 {
            // `taken` 4096 ends the batch receiving nothing: the last
            // line waits in the channel for the next batch.
            assert!(hub.next_chunk(&rx, 4096).is_none());
            let rest: Vec<_> = hub
                .take_batch(&rx)
                .expect("the last line")
                .into_iter()
                .collect();
            assert_eq!(kinds(&rest), ["line-4096"]);
        }
    }
}

#[test]
fn logical_batch_with_a_tick_at_4096() {
    // The 4096th input a tick: it joins the hub batch, and the logical
    // batch still holds 4096.
    let mut hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    for seq in 0..4095u64 {
        tx.send(hub_line(seq))
            .unwrap_or_else(|err| panic!("send: {err}"));
    }
    tx.send(Input::Tick)
        .unwrap_or_else(|err| panic!("send: {err}"));
    let chunks = drain_logical(&mut hub, &rx);
    let sizes: Vec<usize> = chunks.iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![64; 64]);
    let kinds_got: Vec<String> = chunks.iter().flat_map(|chunk| kinds(chunk)).collect();
    assert_eq!(kinds_got.len(), 4096);
    assert_eq!(kinds_got[4095], "tick");
    assert_eq!(kinds_got[4094], "line-4094");
}

#[test]
fn logical_batch_with_a_key_inside() {
    // A key at position 64 ends the logical hub batch at the chunk
    // boundary and draws on its own frame: the hub inputs that arrived
    // before it fold first, in order.
    let mut hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    for seq in 0..64u64 {
        tx.send(hub_line(seq))
            .unwrap_or_else(|err| panic!("send: {err}"));
    }
    tx.send(Input::Bytes(b"a".to_vec()))
        .unwrap_or_else(|err| panic!("send: {err}"));
    for seq in 64..128u64 {
        tx.send(hub_line(seq))
            .unwrap_or_else(|err| panic!("send: {err}"));
    }
    let chunks = drain_logical(&mut hub, &rx);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 64);
    let key: Vec<_> = hub.take_batch(&rx).expect("the key").into_iter().collect();
    assert_eq!(kinds(&key), ["bytes"]);
    let rest = drain_logical(&mut hub, &rx);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].len(), 64);
    let kinds_got: Vec<String> = rest.iter().flat_map(|chunk| kinds(chunk)).collect();
    let want: Vec<String> = (64..128).map(|seq| format!("line-{seq}")).collect();
    assert_eq!(kinds_got, want);
    // A key after a full 4096 ends the batch receiving nothing: the key
    // waits in the channel for its own batch.
    let mut hub = Connection::new(None);
    let (tx, rx) = mpsc::channel();
    for seq in 0..4096u64 {
        tx.send(hub_line(seq))
            .unwrap_or_else(|err| panic!("send: {err}"));
    }
    tx.send(Input::Bytes(b"a".to_vec()))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let chunks = drain_logical(&mut hub, &rx);
    assert_eq!(chunks.len(), 64);
    assert!(hub.next_chunk(&rx, 4096).is_none());
    let key: Vec<_> = hub.take_batch(&rx).expect("the key").into_iter().collect();
    assert_eq!(kinds(&key), ["bytes"]);
}

#[test]
fn a_history_wait_at_the_plateau_stashes_queue_plus_one() {
    // The structural minimum of the attach shape: 65 replay lines with
    // the answer next in socket order. The reader parses only what flow
    // control lets it, so it holds at the plateau of 65 before the wait
    // starts; the wait then stashes exactly those 65.
    let first_seq = session().len() as u64;
    let mut lines: Vec<Vec<u8>> = (0..65u64)
        .map(|at| replay_line(first_seq + at, at))
        .collect();
    let want = vec![replay(first_seq + 1000), replay(first_seq + 1001)];
    let answer = command_answer("a_1", want.clone());
    lines.push(serde_json::to_vec(&answer).unwrap_or_else(|err| panic!("encode: {err}")));
    if let Some(last) = lines.last_mut() {
        last.push(b'\n');
    }
    let (hand_tx, hand_rx) = mpsc::channel();
    let link = FakeLink::with_lines(hand_tx, lines);
    let (mut lp, _theirs, tx, rx) = opened();
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    let (reader, done) = spawn_reader(link, feed);
    // The plateau of 65 with nothing received: one drain.
    let plateau = Deadline::after(DEADLINE);
    for want_hand in 1..=65usize {
        match plateau.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want_hand, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want_hand}: {err}"),
        }
    }
    let (answered_tx, answered) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-answer".to_owned())
        .spawn(move || {
            let result =
                lp.hub
                    .answer(&rx, "a_1", &mut lp.app, &lp.tick)
                    .map(|answer| match answer {
                        super::super::hub::Answer::Lines(lines) => lines,
                        super::super::hub::Answer::Reopened => {
                            panic!("no reopening in this script")
                        }
                    });
            let peak = lp.hub.answer_stash_peak();
            let stashed = lp.hub.stashed().len();
            answered_tx.send((result, peak, stashed, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let answered_wait = Deadline::after(DEADLINE);
    let (result, peak, stashed, lp) = answered_wait.recv_or_fail(&answered, "the history wait");
    assert_eq!(result.unwrap_or_else(|err| panic!("answer: {err}")), want);
    assert_eq!(peak, 65);
    assert_eq!(stashed, 65);
    drop(lp);
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

/// The fake hub's socket buffer: a bounded queue of serialized lines.
/// The writer pushes replay lines, the link pops one line per read,
/// and the command thread appends the history answer behind them.
/// Workers retain their own `Arc`, so the test's [`CloseOnDrop`] guard
/// closes it on unwind, releasing every waiter.
struct Wire {
    state: std::sync::Mutex<WireState>,
    changed: std::sync::Condvar,
    cap: usize,
}

/// What the wire holds, and whether it is closed.
struct WireState {
    lines: VecDeque<Vec<u8>>,
    closed: bool,
}

impl Wire {
    /// Holds at most `cap` lines.
    fn new(cap: usize) -> Arc<Wire> {
        Arc::new(Self {
            state: std::sync::Mutex::new(WireState {
                lines: VecDeque::new(),
                closed: false,
            }),
            changed: std::sync::Condvar::new(),
            cap,
        })
    }

    /// The state, even after a thread panicked holding it.
    fn lock(&self) -> std::sync::MutexGuard<'_, WireState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Pushes a line, waiting for room with the test's deadline. False
    /// when closed, or when the wait expires: expiry ends the writer
    /// like a close, and the test's own deadline then names the
    /// missing hand-out.
    fn push(&self, line: Vec<u8>) -> bool {
        let state = self.lock();
        let (mut state, waited) = self
            .changed
            .wait_timeout_while(state, DEADLINE, |state| {
                state.lines.len() >= self.cap && !state.closed
            })
            .unwrap_or_else(|poison| poison.into_inner());
        if state.closed || (waited.timed_out() && state.lines.len() >= self.cap) {
            return false;
        }
        state.lines.push_back(line);
        self.changed.notify_one();
        true
    }

    /// Pops the oldest line. None when closed and empty, or when the
    /// wait expires: expiry ends the reader like a close, and the
    /// test's own deadline then names the missing hand-out.
    fn pop(&self) -> Option<Vec<u8>> {
        let state = self.lock();
        let (mut state, waited) = self
            .changed
            .wait_timeout_while(state, DEADLINE, |state| {
                state.lines.is_empty() && !state.closed
            })
            .unwrap_or_else(|poison| poison.into_inner());
        if waited.timed_out() && state.lines.is_empty() && !state.closed {
            return None;
        }
        let line = state.lines.pop_front();
        if line.is_some() {
            self.changed.notify_one();
        }
        line
    }

    /// Closes the wire: every waiter wakes and ends.
    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.close();
    }
}

/// Closes the wire when the test unwinds: it holds its own `Arc`, apart
/// from the ones the workers retain, so dropping the test's `Arc`
/// releases nothing but dropping this guard still closes the wire.
struct CloseOnDrop(Arc<Wire>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The link reading the wire: one line per read, reporting each
/// hand-out's count, blocking when empty until the wire closes. A short
/// read finishes the same line on the next read, reported once.
struct WireLink {
    wire: Arc<Wire>,
    handouts: mpsc::Sender<usize>,
    current: Vec<u8>,
    at: usize,
    handed: usize,
    count: Arc<AtomicUsize>,
    eof: bool,
}

impl WireLink {
    fn new(wire: Arc<Wire>, handouts: mpsc::Sender<usize>) -> Self {
        Self {
            wire,
            handouts,
            current: Vec::new(),
            at: 0,
            handed: 0,
            count: Arc::new(AtomicUsize::new(0)),
            eof: false,
        }
    }
}

impl Read for WireLink {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.current.len() {
            match self.wire.pop() {
                None => {
                    if !self.eof {
                        self.eof = true;
                        self.handouts.send(self.handed + 1).unwrap_or(());
                    }
                    return Ok(0);
                }
                Some(line) => {
                    self.current = line;
                    self.at = 0;
                }
            }
        }
        let start = self.at;
        let end = start + buf.len().min(self.current.len() - start);
        buf[..end - start].copy_from_slice(&self.current[start..end]);
        self.at = end;
        if self.at >= self.current.len() {
            self.current.clear();
            self.at = 0;
            self.handed += 1;
            self.count.store(self.handed, Ordering::SeqCst);
            self.handouts.send(self.handed).unwrap_or(());
        }
        Ok(end - start)
    }
}

/// The attach row's 10 MiB band: the fixture pool's serialized bytes
/// reach 10,485,760 and stay under 11,534,336, the band `size_note`
/// states in `crates/bench/examples/bench/attach.rs`.
const TEN_MIB_LOW: usize = 10_485_760;
const TEN_MIB_HIGH: usize = 11_534_336;

/// One 4,096-byte reply carrying marker `turn`.
fn big_reply(turn: usize) -> String {
    let mut text = format!("fixture {turn} ");
    while text.len() < 4096 {
        text.push_str(&format!("fixture {turn} "));
    }
    text.truncate(4096);
    text
}

/// Wire lines numbered from the session's last line, as serialized lines
/// with their newlines, with the pool's total bytes: whole turns until at
/// least `min_lines` lines and `min_bytes` bytes are held, so asking for a
/// prefix of the 10 MiB pool gives lines byte-identical to its start.
fn fixture_lines(min_lines: usize, min_bytes: usize) -> (Vec<Vec<u8>>, usize) {
    let first_seq = session().len() as u64;
    let mut lines = Vec::new();
    let mut bytes = 0usize;
    let mut seq = first_seq;
    let mut turn = 0usize;
    while bytes < min_bytes || lines.len() < min_lines {
        let prompt = json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": format!("prompt {turn}")}]}]});
        for (kind, action, payload) in [
            ("turn_started", None, prompt),
            (
                "text_completed",
                Some(format!("a_m{turn}")),
                json!({"text": big_reply(turn)}),
            ),
            ("turn_completed", None, json!({"outcome": "completed"})),
        ] {
            let envelope = line(kind, Some(seq), action.as_deref(), payload);
            seq += 1;
            let mut wire =
                serde_json::to_vec(&envelope).unwrap_or_else(|err| panic!("encode: {err}"));
            wire.push(b'\n');
            bytes += wire.len();
            lines.push(wire);
        }
        turn += 1;
    }
    (lines, bytes)
}

/// The 10 MiB fixture pool: turns with 4,096-byte replies, as
/// `fixture_lines` builds them, up to the attach row's 10 MiB band.
fn ten_mib_fixture() -> (Vec<Vec<u8>>, usize) {
    fixture_lines(0, TEN_MIB_LOW)
}

/// Serves `history` on the hub's end from the session's lines: the
/// first command waits for the test's go-ahead, later ones answer at
/// once, and the count of history commands seen returns with the
/// thread. Ends at EOF, when the loop hangs up.
fn serve_history(
    theirs: UnixStream,
    wire: Arc<Wire>,
    go: mpsc::Receiver<()>,
    lines: Vec<contract::Envelope>,
    observed: Option<(Arc<AtomicUsize>, mpsc::Sender<usize>)>,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<(usize, Vec<contract::Envelope>)>,
) {
    let (done_tx, done) = mpsc::channel();
    let hub = std::thread::Builder::new()
        .name("attach-commands".to_owned())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(theirs);
            let mut text = String::new();
            let mut seen = 0usize;
            let mut answered = Vec::new();
            let mut go = Some(go);
            loop {
                text.clear();
                match std::io::BufRead::read_line(&mut reader, &mut text) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let command: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                if command["command"] != "history" {
                    continue;
                }
                seen += 1;
                if let Some((count, report)) = &observed {
                    report.send(count.load(Ordering::SeqCst)).unwrap_or(());
                }
                if let Some(go) = go.take() {
                    // The test's go-ahead, sent once the writer is
                    // joined; a dropped sender releases a panicking
                    // test's thread.
                    if go.recv().is_err() {
                        break;
                    }
                }
                let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
                let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
                let id = command["id"].as_str().unwrap_or_default().to_owned();
                let result = super::history(&lines, from, to);
                if answered.is_empty() {
                    answered = result.clone();
                }
                let mut reply = serde_json::to_vec(&command_answer(&id, result))
                    .unwrap_or_else(|err| panic!("encode: {err}"));
                reply.push(b'\n');
                if !wire.push(reply) {
                    break;
                }
            }
            done_tx.send((seen, answered)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (hub, done)
}

/// A full subscription's opening boundary, without adding a durable line.
fn opening(app: &mut crate::app::App, last: u64) {
    app.on_line(Line::Session(line(
        "command_accepted",
        None,
        None,
        json!({"command_id": "opening", "result": {"last_seq": last}}),
    )));
}

/// A hub event naming one session, with or without a fold boundary.
fn stream_event(kind: &str, session: &contract::SessionId, last: Option<u64>) -> Line {
    let mut payload = json!({"session_id": session.0, "how": "crashed"});
    if let Some(last) = last {
        payload["last_seq"] = json!(last);
    }
    Line::Hub(contract::HubLine {
        kind: kind.into(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().unwrap().clone(),
    })
}

/// Scrolls to the dropped first page using the app's injected clock.
fn demand_top(lp: &mut super::super::Loop<ratatui::backend::TestBackend>) {
    for _ in 0..100 {
        lp.app.on_key(crate::keys::Key::PageUp, lp.clock.now());
    }
}

#[test]
fn a_history_wait_does_not_end_on_an_unrelated_hub_line() {
    let (mut lp, _theirs, tx, rx) = opened();
    let session = lp.app.session().unwrap().clone();
    let want = vec![replay(1000)];
    tx.send(Input::Hub(stream_event("attention", &session, None)))
        .unwrap();
    tx.send(Input::Hub(Line::Session(command_answer(
        "a_1",
        want.clone(),
    ))))
    .unwrap();
    // Every input is ready and EOF bounds the wait without a timer.
    drop(tx);
    let answer = lp.hub.answer(&rx, "a_1", &mut lp.app, &lp.tick);
    assert_eq!(answer, Ok(super::super::hub::Answer::Lines(want)));
    assert!(!lp.app.pass_open());
    let stashed = lp.hub.stashed();
    assert_eq!(stashed.len(), 1);
    assert!(matches!(&stashed[0], Input::Hub(Line::Hub(line)) if line.kind == "attention"));
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
}

#[test]
fn a_full_ack_replaces_its_own_boundary_and_keeps_other_sessions_passes() {
    let (mut lp, _, _, _) = opened();
    let last = session().len() as u64 + 10;
    opening(&mut lp.app, last + 10);
    opening(&mut lp.app, last);
    lp.app.on_line(Line::Session(replay(last - 1)));
    assert!(
        lp.app.pass_open(),
        "the replacement boundary is not folded yet"
    );
    lp.app.on_line(Line::Session(replay(last)));
    assert!(
        !lp.app.pass_open(),
        "the superseded boundary must not remain"
    );

    let other = contract::SessionId("s_bbbbbbbbbbbbbbbb".into());
    lp.app
        .on_line(stream_event("stream_reopened", &other, Some(last + 20)));
    opening(&mut lp.app, last);
    lp.app.on_line(Line::Session(replay(last)));
    assert!(
        lp.app.pass_open(),
        "the acknowledgement preserves the other session's pass"
    );
    lp.app.on_line(stream_event("stream_closed", &other, None));
    assert!(!lp.app.pass_open());
}

#[test]
fn marking_a_reopen_pending_replaces_its_session_and_keeps_other_pending_passes() {
    let (mut lp, _, _, _) = opened();
    let session = lp.app.session().unwrap().clone();
    let other = contract::SessionId("s_bbbbbbbbbbbbbbbb".into());
    let last = super::session().len() as u64 + 10;
    opening(&mut lp.app, last);
    lp.app.reopen_pending(session.clone());
    lp.app.on_line(Line::Session(replay(last)));
    assert!(
        lp.app.pass_open(),
        "a pending reopening supersedes the old boundary"
    );
    lp.app.reopen_pending(session.clone());
    assert!(
        lp.app.pass_open(),
        "marking the same session pending again keeps it open"
    );
    lp.app.reopen_pending(other.clone());
    lp.app
        .on_line(stream_event("stream_reopened", &other, None));
    assert!(
        lp.app.pass_open(),
        "marking another session pending must preserve the first"
    );
    lp.app
        .on_line(stream_event("stream_reopened", &session, None));
    assert!(!lp.app.pass_open(), "both pending passes have now ended");
}

#[test]
fn opening_passes_gate_pages_until_the_boundary_is_folded() {
    let (mut lp, _, _, _) = opened();
    demand_top(&mut lp);
    let needs = lp.app.needs();
    assert!(!needs.is_empty());
    let last = session().len() as u64 + 10;
    opening(&mut lp.app, last);
    assert!(lp.app.pass_open());
    assert!(lp.app.needs().is_empty());
    lp.app.on_line(Line::Session(replay(last - 1)));
    assert!(lp.app.pass_open());
    assert!(lp.app.needs().is_empty());
    lp.app.on_line(Line::Session(replay(last)));
    assert!(!lp.app.pass_open());
    assert_eq!(lp.app.needs(), needs);
    opening(&mut lp.app, last);
    lp.app.on_line(Line::Session(replay(last + 1)));
    assert!(!lp.app.pass_open());
    let empty = line(
        "command_accepted",
        None,
        None,
        json!({"command_id": "empty"}),
    );
    lp.app.on_line(Line::Session(empty));
    assert!(!lp.app.pass_open());
}

#[test]
fn stream_endings_close_only_the_named_pass_and_disconnect_clears_all() {
    let (mut lp, _, _, _) = opened();
    let session = lp.app.session().unwrap().clone();
    let other = contract::SessionId("s_bbbbbbbbbbbbbbbb".into());
    for kind in ["stream_closed", "session_left"] {
        opening(&mut lp.app, 9000);
        lp.app.on_line(stream_event(kind, &other, None));
        assert!(lp.app.pass_open());
        lp.app.on_line(stream_event(kind, &session, None));
        assert!(!lp.app.pass_open());
    }
    lp.app
        .on_line(stream_event("stream_reopened", &session, Some(9000)));
    assert!(lp.app.pass_open());
    lp.app
        .on_line(stream_event("stream_reopened", &other, Some(9001)));
    lp.app
        .on_line(stream_event("stream_closed", &session, None));
    assert!(lp.app.pass_open(), "the delegate session's pass remains");
    lp.app.on_line(stream_event("session_left", &other, None));
    assert!(!lp.app.pass_open(), "a delegate crash ends its pass too");
    lp.app
        .on_line(stream_event("stream_reopened", &session, Some(9002)));
    lp.app
        .on_line(stream_event("stream_reopened", &other, None));
    assert!(lp.app.pass_open());
    lp.app
        .on_line(stream_event("stream_reopened", &session, None));
    assert!(!lp.app.pass_open(), "an empty reopening has no pass");
    opening(&mut lp.app, 9000);
    lp.app.disconnected();
    assert!(!lp.app.pass_open());
}

#[test]
fn pending_reopen_survives_older_end_events_and_seq_lines() {
    let (mut lp, _, _, _) = opened();
    let session = lp.app.session().unwrap().clone();
    lp.app.reopen_pending(session.clone());
    for kind in ["session_left", "stream_closed"] {
        lp.app.on_line(stream_event(kind, &session, None));
        assert!(lp.app.pass_open(), "{kind} predates the pending reopening");
    }
    lp.app.on_line(Line::Session(replay(9999)));
    assert!(lp.app.pass_open(), "only the reopening resolves Pending");
    lp.app
        .on_line(stream_event("stream_reopened", &session, Some(10000)));
    lp.app.on_line(Line::Session(replay(9999)));
    assert!(lp.app.pass_open());
    lp.app.on_line(Line::Session(replay(10000)));
    assert!(!lp.app.pass_open());
    lp.app.reopen_pending(session);
    lp.app.disconnected();
    assert!(!lp.app.pass_open());
}

#[test]
fn abandoned_history_answers_are_discarded_once_by_id() {
    for kind in ["command_accepted", "command_rejected"] {
        let (mut lp, _, _, _) = opened();
        lp.app.abandon_history("late".into());
        let mut answer = line(
            kind,
            None,
            None,
            json!({"command_id": "late", "result": {"last_seq": 9000}}),
        );
        lp.app.on_line(Line::Session(answer.clone()));
        assert!(!lp.app.pass_open());
        answer.kind = "command_accepted".into();
        lp.app.on_line(Line::Session(answer));
        assert!(
            lp.app.pass_open(),
            "the abandoned id leaves the set on its first answer"
        );
    }
    let (mut lp, _, _, _) = opened();
    lp.app.abandon_history("late".into());
    lp.app.disconnected();
    lp.app.on_line(Line::Session(line(
        "command_accepted",
        None,
        None,
        json!({"command_id": "late", "result": {"last_seq": 9000}}),
    )));
    assert!(lp.app.pass_open(), "disconnect clears abandoned ids");
}

#[test]
fn the_boundary_line_sends_a_deferred_search_without_another_hub_line() {
    let (mut lp, _, _, _) = opened();
    let last = session().len() as u64 + 10;
    opening(&mut lp.app, last);
    lp.app.on_key(crate::keys::Key::CtrlF, lp.clock.now());
    lp.app.on_key(crate::keys::Key::Char('z'), lp.clock.now());
    assert!(lp.app.find_due(1).is_empty());
    let outgoing = lp.app.on_line(Line::Session(replay(last)));
    assert!(outgoing.iter().any(
        |text| serde_json::from_str::<serde_json::Value>(text).unwrap()["command"] == "history"
    ));
}

#[test]
fn a_session_left_before_the_boundary_allows_history_on_demand() {
    let (mut lp, theirs, tx, rx) = opened();
    let session = lp.app.session().unwrap().clone();
    let last = super::session().len() as u64 + 10;
    opening(&mut lp.app, last);
    lp.app.on_line(Line::Session(replay(last - 1)));
    lp.app.on_line(stream_event("session_left", &session, None));
    assert!(!lp.app.pass_open());
    let seen = serve(theirs, tx.clone(), answers_all());
    let _lp = super::run(lp, rx, &tx, super::to_the_top());
    assert!(!super::ranges(&seen).is_empty());
}

/// A mouse report against the targets drawn for the app's current view.
fn select_report(app: &mut crate::app::App, kind: crate::keys::MouseKind, col: u16, row: u16) {
    let area = ratatui::layout::Rect::new(0, 0, 60, 12);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buffer, None);
    app.on_select(&crate::keys::Mouse { kind, col, row }, &targets);
}

#[test]
fn an_open_delegate_item_pass_ends_on_the_delegates_session_left() {
    use crate::app::items::testkit;
    let mut app = testkit::home();
    testkit::opened(&mut app);
    testkit::start_delegate(&mut app, "j_pass", testkit::DELEGATE_A);
    let outgoing = testkit::open(&mut app, "j_pass");
    assert!(app.item_open());
    let mut accepted = line(
        "command_accepted",
        None,
        None,
        json!({"command_id": outgoing[0]["id"], "result": {"last_seq": 9000}}),
    );
    accepted.session_id = contract::SessionId(testkit::DELEGATE_A.into());
    app.on_line(Line::Session(accepted));
    assert!(
        app.pass_open(),
        "the item subscribe is consumed only after opening its pass"
    );
    app.on_line(stream_event(
        "session_left",
        &contract::SessionId(testkit::DELEGATE_A.into()),
        None,
    ));
    assert!(!app.pass_open());
}

#[test]
fn a_copy_pins_its_dropped_page_without_loading_during_a_pass() {
    use crate::keys::{Button, Key, MouseKind};
    let (mut lp, _, _, _) = opened();
    demand_top(&mut lp);
    let needed = lp.app.needs();
    assert!(!needed.is_empty());
    // A whole page can span several wire answers; the app loads it whole.
    let first: Vec<_> = session()
        .into_iter()
        .filter(|line| needed[0].contains(&line.seq.unwrap()))
        .collect();
    lp.app.load(first.clone());
    assert!(lp.app.pages().part(0).is_some());
    select_report(&mut lp.app, MouseKind::Press(Button::Left), 0, 0);
    select_report(&mut lp.app, MouseKind::Drag(Button::Left), 59, 2);
    lp.app.on_key(Key::End, lp.clock.now());
    assert!(lp.app.pages().part(0).is_none());
    let last = session().len() as u64 + 10;
    opening(&mut lp.app, last);
    select_report(&mut lp.app, MouseKind::Release, 59, 2);
    assert!(lp.app.pages().pinned() > 0);
    assert_eq!(lp.app.take_copy(), None);
    assert!(lp.app.needs().is_empty());
    lp.app.on_line(Line::Session(replay(last)));
    assert!(lp.app.needs().iter().any(|range| range.start().0 == 0));
    lp.app.load(first);
    assert!(
        lp.app
            .take_copy()
            .expect("copy after its page loads")
            .contains("first")
    );
}

/// Runs a history wait whose answer follows a reopening and a finite fold.
/// The older ending case also queues a key ahead of the reopening.
fn reopened_history(older_ending: bool) {
    let r = 367usize;
    let (pool, _) = fixture_lines(r, 0);
    let wire = Wire::new(174);
    let _guard = CloseOnDrop(Arc::clone(&wire));
    let (hand_tx, hand_rx) = mpsc::channel();
    let link = WireLink::new(Arc::clone(&wire), hand_tx);
    let count = Arc::clone(&link.count);
    let (mut lp, theirs, tx, rx) = opened();
    let session_id = lp.app.session().unwrap().clone();
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    let (reader, reader_done) = spawn_reader(link, feed);
    demand_top(&mut lp);
    let (history_tx, histories) = mpsc::channel();
    let (server_done_tx, server_done) = mpsc::channel();
    let server_wire = Arc::clone(&wire);
    let input = tx.clone();
    let prefix = if older_ending { 3 } else { 1 };
    let script = pool[..r].to_vec();
    let server = std::thread::spawn(move || {
        let mut commands = std::io::BufReader::new(theirs);
        let mut text = String::new();
        let mut seen = 0;
        loop {
            text.clear();
            match std::io::BufRead::read_line(&mut commands, &mut text) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let command: serde_json::Value = serde_json::from_str(&text).unwrap();
            if command["command"] != "history" {
                continue;
            }
            seen += 1;
            history_tx.send(count.load(Ordering::SeqCst)).unwrap_or(());
            if seen == 1 {
                let encode = |line: Line| {
                    let mut bytes = match line {
                        Line::Hub(line) => serde_json::to_vec(&line).unwrap(),
                        Line::Session(line) => serde_json::to_vec(&line).unwrap(),
                    };
                    bytes.push(b'\n');
                    bytes
                };
                if older_ending {
                    assert!(server_wire.push(encode(stream_event(
                        "session_left",
                        &session_id,
                        None
                    ))));
                    // A second read proves the previous line was sent to
                    // the input channel before the key is sent there.
                    assert!(server_wire.push(encode(Line::Session(line(
                        "read_marker",
                        None,
                        None,
                        json!({})
                    )))));
                    let wait = Deadline::after(DEADLINE);
                    for want in 1..=2 {
                        assert_eq!(
                            wait.recv_or_fail(&hand_rx, "the older ending to reach the reader"),
                            want
                        );
                    }
                    let keys = super::to_the_top()
                        .into_iter()
                        .flat_map(|input| {
                            let Input::Bytes(bytes) = input else {
                                panic!("PageUp bytes");
                            };
                            bytes
                        })
                        .collect();
                    input.send(Input::Bytes(keys)).unwrap();
                }
                let last = session().len() + r - 1;
                assert!(server_wire.push(encode(stream_event(
                    "stream_reopened",
                    &session_id,
                    Some(last as u64)
                ))));
                for line in &script {
                    if !server_wire.push(line.clone()) {
                        break;
                    }
                }
            }
            let from = command["args"]["from_seq"].as_u64().unwrap();
            let to = command["args"]["to_seq"].as_u64().unwrap();
            let id = command["id"].as_str().unwrap();
            // The abandoned first answer must never populate page zero.
            let lines = if seen == 1 {
                Vec::new()
            } else {
                super::history(&session(), from, to)
            };
            let mut answer = serde_json::to_vec(&command_answer(id, lines)).unwrap();
            answer.push(b'\n');
            if !server_wire.push(answer) {
                break;
            }
        }
        server_done_tx.send(seen).unwrap_or(());
    });
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let code = lp.run(&rx);
        done_tx.send((code, lp)).unwrap_or(());
    });
    for input in super::to_the_top() {
        tx.send(input).unwrap();
    }
    let wait = Deadline::after(DEADLINE);
    assert_eq!(
        wait.recv_or_fail(&histories, "the initial history request"),
        0
    );
    let second = wait.recv_or_fail(&histories, "history after the reopened fold");
    tx.send(quit()).unwrap();
    let (code, lp) = Deadline::after(DEADLINE).recv_or_fail(&done, "the loop to quit after paging");
    wire.close();
    assert!(
        second >= prefix + r,
        "history before the last fold line: {second}, prefix {prefix}, replay {r}"
    );
    assert!(lp.hub.answer_stash_peak() <= K);
    assert!(!lp.app.pass_open());
    assert!(
        lp.app.pages().part(0).is_some(),
        "the abandoned empty answer did not mark the page loaded"
    );
    assert_eq!(code, 0);
    drop(lp);
    Deadline::after(DEADLINE).recv_or_fail(&reader_done, "the reader to end");
    reader.join().unwrap();
    let seen = Deadline::after(DEADLINE).recv_or_fail(&server_done, "the command server to end");
    server.join().unwrap();
    assert!(seen > 1, "pages load after the pass ends");
}

#[test]
fn a_reopen_ends_the_history_wait_at_the_opening_window() {
    reopened_history(false);
}

#[test]
fn a_stashed_session_left_and_page_up_cannot_clear_a_pending_reopen() {
    reopened_history(true);
}

#[test]
fn a_history_waits_for_the_opening_pass_at_the_opening_window() {
    const LARGEST_REPLAY: usize = 193 + 174;
    let (pool, _) = fixture_lines(LARGEST_REPLAY, 0);
    for w in [64usize, 65, 66, 174] {
        let r = 193 + w;
        let wire = Wire::new(w);
        let _guard = CloseOnDrop(Arc::clone(&wire));
        let (hand_tx, hand_rx) = mpsc::channel();
        let link = WireLink::new(Arc::clone(&wire), hand_tx);
        let count = Arc::clone(&link.count);
        let (mut lp, theirs, tx, rx) = opened();
        let last = session().len() + r - 1;
        lp.app.on_line(Line::Session(line(
            "command_accepted",
            None,
            None,
            json!({"command_id": "opening", "result": {"last_seq": last}}),
        )));
        lp.app.set_size(70, 12);
        let feed = Feed::new(tx.clone(), lp.hub.credits());
        let (reader, reader_done) = spawn_reader(link, feed);
        let writer_lines = pool[..r].to_vec();
        let wire_writer = Arc::clone(&wire);
        let (writer_done_tx, writer_done) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            for line in writer_lines {
                if !wire_writer.push(line) {
                    break;
                }
            }
            writer_done_tx.send(()).unwrap_or(());
        });
        let (observed_tx, observed) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel();
        go_tx.send(()).unwrap();
        let (commands, commands_done) = serve_history(
            theirs,
            Arc::clone(&wire),
            go_rx,
            session(),
            Some((count, observed_tx)),
        );
        let plateau = Deadline::after(DEADLINE);
        for want in 1..=65usize {
            assert_eq!(
                plateau.recv_or_fail(&hand_rx, "the initial reader plateau"),
                want
            );
        }
        let (reached_tx, reached) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        std::thread::spawn(move || {
            pause::install(reached_tx, release);
            let code = lp.run(&rx);
            done_tx.send((code, lp)).unwrap_or(());
        });
        Deadline::after(DEADLINE).recv_or_fail(&reached, "chunk one to wait");
        let mut keys = Vec::new();
        for _ in 0..10 {
            for input in super::to_the_top() {
                let Input::Bytes(bytes) = input else {
                    panic!("PageUp bytes");
                };
                keys.extend(bytes);
            }
        }
        keys.extend_from_slice(b"\x06fixture");
        tx.send(Input::Bytes(keys)).unwrap();
        tx.send(Input::FindDue(7)).unwrap();
        drop(release_tx);
        let first = Deadline::after(DEADLINE).recv_or_fail(&observed, "history after opening");
        // The answer follows the finite replay on the same wire.
        let reads = Deadline::after(DEADLINE);
        loop {
            if reads.recv_or_fail(&hand_rx, "the first history answer") == r + 1 {
                break;
            }
        }
        tx.send(quit()).unwrap();
        let (code, lp) = Deadline::after(DEADLINE).recv_or_fail(&done, "the loop to quit");
        wire.close();
        let peak = lp.hub.answer_stash_peak();
        drop(lp);
        Deadline::after(DEADLINE).recv_or_fail(&writer_done, "the finite writer");
        writer.join().unwrap();
        Deadline::after(DEADLINE).recv_or_fail(&reader_done, "the reader");
        reader.join().unwrap();
        let (seen, _) =
            Deadline::after(DEADLINE).recv_or_fail(&commands_done, "the command thread");
        commands.join().unwrap();
        assert_eq!(first, r, "no history before the last opening line, W = {w}");
        assert!(peak <= K, "answer stash {peak} exceeds K = {K}, W = {w}");
        assert!(seen > 0, "pages load after the pass");
        assert_eq!(code, 0);
    }
}

#[test]
#[allow(clippy::print_stdout, reason = "the evidence run reads the peak line")]
fn attach_stash_peak_at_the_ten_mib_fixture() {
    let (pool, pool_bytes) = ten_mib_fixture();
    assert!(
        (TEN_MIB_LOW..TEN_MIB_HIGH).contains(&pool_bytes),
        "the pool holds the attach row's 10 MiB: {pool_bytes} bytes",
    );
    // The reader parses only what flow control lets it, so far fewer
    // lines than the pool are read before the stop.
    for w in [1usize, 64, 65, 66, 174] {
        // R is the most lines the writer can send and still be joined
        // before the answer: the pipeline holds 128 lines in the two
        // held chunks, the channel's 64, the reader's 1 and the wire's
        // W; one line more and the writer blocks on a full wire.
        let r = 193 + w;
        let wire = Wire::new(w);
        // Apart from the workers' `Arc`s: unwinding closes the wire
        // during the unwind, releasing every waiter.
        let _guard = CloseOnDrop(Arc::clone(&wire));
        let (hand_tx, hand_rx) = mpsc::channel();
        let link = WireLink::new(Arc::clone(&wire), hand_tx);
        let (mut lp, theirs, tx, rx) = opened();
        let feed = Feed::new(tx.clone(), lp.hub.credits());
        let (reader, reader_done) = spawn_reader(link, feed);
        // The finite writer sends exactly R pool lines, then ends. The
        // order on the wire is writer-before-answer by construction.
        let writer_lines = pool[..r].to_vec();
        let wire_writer = Arc::clone(&wire);
        let (writer_done_tx, writer_done) = mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("attach-writer".to_owned())
            .spawn(move || {
                for line in writer_lines {
                    if !wire_writer.push(line) {
                        break;
                    }
                }
                writer_done_tx.send(()).unwrap_or(());
            })
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        // The command thread serves history from the session's lines.
        let (go_tx, go_rx) = mpsc::channel();
        let (commands, commands_done) =
            serve_history(theirs, Arc::clone(&wire), go_rx, session(), None);
        // The loop runs on a worker with the fold held. It starts
        // only once the reader has sent its pre-loop 64: the 65
        // hand-outs force 64 takes, which need no release, so the
        // first chunk is deterministically full and releases 64.
        // The receivers above all predate their threads.
        let plateau = Deadline::after(DEADLINE);
        for want in 1..=65usize {
            match plateau.recv(&hand_rx) {
                Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
                Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
            }
        }
        let (reached_tx, reached) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        std::thread::Builder::new()
            .name("attach-run".to_owned())
            .spawn(move || {
                pause::install(reached_tx, release);
                let code = lp.run(&rx);
                done_tx.send((code, lp)).unwrap_or(());
            })
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        // Hold chunk 1; drain hand-outs to the plateau of 129. One
        // deadline for the drain.
        Deadline::after(DEADLINE).recv_or_fail(&reached, "the loop to hold chunk 1");
        for want in 66..=129usize {
            match plateau.recv(&hand_rx) {
                Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
                Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
            }
        }
        // The live parsed events at the first hold: 129 handed out,
        // none folded, the fold being held before it folds chunk 1.
        let handed = 129;
        let folded = 0;
        assert!(handed - folded <= K, "the first hold stays within K = {K}");
        // A frame must load pages while the replay streams: far more
        // than enough PageUps to reach the top, clamping there, in one
        // terminal read, so one key batch draws one frame. One read can
        // carry many escape sequences, as a tty read does.
        let mut top = Vec::new();
        for _ in 0..10 {
            for input in super::to_the_top() {
                let Input::Bytes(bytes) = input else {
                    panic!("to_the_top sends bytes");
                };
                top.extend(bytes);
            }
        }
        tx.send(Input::Bytes(top))
            .unwrap_or_else(|err| panic!("send: {err}"));
        release_tx
            .send(())
            .unwrap_or_else(|err| panic!("release: {err}"));
        // Hold chunk 2, where the reader's hand-outs reach 193.
        Deadline::after(DEADLINE).recv_or_fail(&reached, "the loop to hold chunk 2");
        let refill = Deadline::after(DEADLINE);
        for want in 130..=193usize {
            match refill.recv(&hand_rx) {
                Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
                Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
            }
        }
        // The live parsed events at the second hold: 193 handed out,
        // 64 folded. The second hold runs after chunk 1's fold, so
        // chunk 1's 64 lines are folded.
        let handed = 193;
        let folded = 64;
        assert!(handed - folded <= K, "the second hold stays within K = {K}");
        // Join the writer: it ends only once its R lines are all on
        // the wire or past it.
        let joined = Deadline::after(DEADLINE);
        joined.recv_or_fail(&writer_done, "the writer to end");
        writer.join().unwrap_or_else(|err| panic!("join: {err:?}"));
        // Only then is the answer appended: the go-ahead lets the
        // command thread append it behind the wire's W lines.
        go_tx.send(()).unwrap_or_else(|err| panic!("go: {err}"));
        release_tx
            .send(())
            .unwrap_or_else(|err| panic!("release: {err}"));
        // The loop folds chunk 2, ends the batch at the key, draws,
        // handles the key, and its frame waits in `answer` with the
        // channel at 64 hub lines, the reader at 1 and the wire at W.
        // The third hold is the receipt signal: it runs in the next
        // `take_batch` after the key's frame, whose `page_in` returns
        // only once `answer` has returned with the answer line.
        let resumed = Deadline::after(DEADLINE);
        match resumed.recv(&reached) {
            Ok(()) => {}
            Err(err) => panic!("waited for the loop to resume folding after the answer: {err}"),
        }
        // Quit is ordered after the answer is received by the loop, so
        // it cannot meet the stash arm in `answer`.
        tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
        release_tx
            .send(())
            .unwrap_or_else(|err| panic!("release: {err}"));
        drop(release_tx);
        let returned = Deadline::after(DEADLINE);
        let (code, lp) = returned.recv_or_fail(&done, "the loop to quit");
        assert_eq!(code, 0);
        let peak = lp.hub.answer_stash_peak();
        assert_eq!(peak, 65 + w, "the stash peaks at 65 + W");
        // The evidence run reads this line with `--no-capture`.
        println!("answer_stash_peak = {peak}, K = {K}, W = {w}");
        // Release every waiter, then read the evidence.
        wire.close();
        drop(lp);
        Deadline::after(DEADLINE).recv_or_fail(&reader_done, "the reader to return");
        reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
        let commands_wait = Deadline::after(DEADLINE);
        let (history_seen, first_answer) =
            commands_wait.recv_or_fail(&commands_done, "the command thread to end");
        commands
            .join()
            .unwrap_or_else(|err| panic!("join: {err:?}"));
        // The top page spans 513 lines, fetched in three `history`
        // commands, plus the one stale page the live folds mark next.
        // The later waits stash nothing more, the hub being exhausted,
        // so the peak still stands: no second wait adds stashed inputs.
        assert_eq!(
            history_seen, 4,
            "the top page's commands plus one stale refetch"
        );
        assert!(!first_answer.is_empty(), "the answer is returned");
        // Every line the writer sent was read exactly once: the R
        // replay lines and the answer. The EOF report follows.
        let reads = Deadline::after(DEADLINE);
        loop {
            match reads.recv(&hand_rx) {
                Ok(got) if got == r + 1 => break,
                Ok(_) => {}
                Err(err) => panic!("waited {DEADLINE:?} for hand-out {}: {err}", r + 1),
            }
        }
        let mut extra = 0usize;
        while hand_rx.try_recv().is_ok() {
            extra += 1;
        }
        assert!(
            r + 1 + extra <= 129 + w + 200,
            "far fewer lines than the pool are read before the stop"
        );
        drop(tx);
        drop(wire);
    }
}
