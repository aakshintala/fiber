//! The attach bound (#2232): the parsed live hub events alive at once,
//! from the socket read to the fold, stay within K.

use super::super::hub::Connection;
use super::super::pause;
use super::{
    DEADLINE, answers_all, counted, counting, line, opened, quit, run_queued, serve, session,
    turn_lines,
};
use crate::Input;
use crate::link::{Credits, Feed, Line, answers, read_lines};
use crate::retry::Retry;
use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
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
    let hub = Connection::new(super::super::tests::idle_retry());
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
            | Input::Feedback(_)
            | Input::Viewed { .. }
            | Input::Models(_)
            | Input::Install { .. }
            | Input::HooksOfferNames { .. }
            | Input::HooksInstalled { .. }
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
    let mut hub = Connection::new(super::super::tests::idle_retry());
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
    let hub = Connection::new(super::super::tests::idle_retry());
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
    let mut hub = Connection::new(super::super::tests::idle_retry());
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
        let hub = Connection::new(Arc::clone(&retry));
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
            let connected = d.recv_or_fail(&rx, "reconnected after lend overflow");
            assert!(matches!(connected, Input::Connected(..)));
            let peer = next_hub.try_clone().unwrap();
            let (sent, feed_sent) = mpsc::channel();
            std::thread::spawn(move || {
                let mut text = String::new();
                BufReader::new(peer).read_line(&mut text).unwrap();
                sent.send(serde_json::from_str::<serde_json::Value>(&text).unwrap())
                    .unwrap();
            });
            let (handled, handle_done) = mpsc::channel();
            std::thread::spawn(move || {
                let (mut terminal, old_peer, _, _) = opened();
                drop(old_peer);
                terminal.app = crate::app::App::new(std::path::PathBuf::from("/w"));
                terminal.app.set_home(crate::Launch::default());
                let initial = terminal.app.on_line(Line::Hub(hello()));
                let initial: serde_json::Value = serde_json::from_str(&initial[0]).unwrap();
                assert_eq!(initial["command"], "feed");
                assert_eq!(terminal.handle(Input::Disconnected), None);
                assert_eq!(terminal.handle(connected), None);
                handled.send(()).unwrap();
            });
            assert_eq!(
                d.recv_or_fail(&feed_sent, "feed resent after overflow reconnect")["command"],
                "feed"
            );
            d.recv_or_fail(&handle_done, "loop handled overflow reconnect");
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
    let mut hub = Connection::new(Arc::clone(&retry));
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
    let hub = Connection::new(Arc::clone(&retry));
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
    let mut hub = Connection::new(Arc::clone(&retry));
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
            let result = lp
                .hub
                .answer(
                    &rx,
                    "a_1",
                    &mut lp.app,
                    &lp.tick,
                    lp.clock.as_ref(),
                    lp.tick.armed(),
                )
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
            let result = lp
                .hub
                .answer(
                    &rx,
                    "a_1",
                    &mut lp.app,
                    &lp.tick,
                    lp.clock.as_ref(),
                    lp.tick.armed(),
                )
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
        let mut hub = Connection::new(super::super::tests::idle_retry());
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
    let mut hub = Connection::new(super::super::tests::idle_retry());
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
    let mut hub = Connection::new(super::super::tests::idle_retry());
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
    let mut hub = Connection::new(super::super::tests::idle_retry());
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
            let result = lp
                .hub
                .answer(
                    &rx,
                    "a_1",
                    &mut lp.app,
                    &lp.tick,
                    lp.clock.as_ref(),
                    lp.tick.armed(),
                )
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
    let answer = lp.hub.answer(
        &rx,
        "a_1",
        &mut lp.app,
        &lp.tick,
        lp.clock.as_ref(),
        lp.tick.armed(),
    );
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
        .filter(|line| needed[0].1.contains(&line.seq.unwrap()))
        .collect();
    let piece = first
        .first()
        .map(|line| line.session_id.clone())
        .expect("the log names its session");
    lp.app.load(&piece, first.clone());
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
    assert!(lp.app.needs().iter().any(|(_, range)| range.start().0 == 0));
    lp.app.load(&piece, first);
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

/// A backend sending each flush's buffer text to the test's channel, so
/// the test orders the first frame against the hub's reads. Like
/// `loop_tests` `CountingBackend`, but recording instead of counting.
/// While armed, the flush reports and then waits for the test's release:
/// the raise reaching the socket before the release proves it was
/// written before `draw` returned.
struct RecordBackend {
    inner: ratatui::backend::TestBackend,
    events: mpsc::Sender<String>,
    block: std::sync::Arc<std::sync::atomic::AtomicBool>,
    release: mpsc::Receiver<()>,
}

impl crate::screen::SyncEmit for RecordBackend {
    fn emit(&mut self, _begin: bool) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
}

impl ratatui::backend::Backend for RecordBackend {
    type Error = <ratatui::backend::TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::result::Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(
        &mut self,
    ) -> std::result::Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::result::Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(
        &mut self,
        clear_type: ratatui::backend::ClearType,
    ) -> std::result::Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> std::result::Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> std::result::Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> std::result::Result<(), Self::Error> {
        let text = crate::view::text(self.inner.buffer());
        self.events.send(format!("frame\n{text}")).unwrap_or(());
        if self.block.load(std::sync::atomic::Ordering::SeqCst) {
            self.events.send("frame-enter".to_owned()).unwrap_or(());
            fakes::Deadline::after(DEADLINE)
                .recv(&self.release)
                .unwrap_or_else(|err| panic!("the frame never released: {err}"));
        }
        self.inner.flush()
    }
}

/// The open's `full` raise is written after the first frame draws, never
/// before it. The recording backend's armed flush parks the frame's
/// worker mid-`draw`: a raise on the socket before the test's release
/// fails, and the raise after it passes. Moving the raise before `draw`
/// in a scratch copy fails this test at the parked read.
#[test]
fn the_raise_is_written_after_the_frame_draws() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    app.set_size(60, 12);
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "\u{2318}".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Session(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()), None),
        ..Default::default()
    });
    app.set_size(60, 12);
    let (events_tx, events) = mpsc::channel();
    let block = std::sync::Arc::new(AtomicBool::new(false));
    let (release_tx, release) = mpsc::channel();
    let backend = RecordBackend {
        inner: ratatui::backend::TestBackend::new(60, 12),
        events: events_tx,
        block: std::sync::Arc::clone(&block),
        release,
    };
    let screen = super::Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let lp = super::super::Loop::for_test(app, screen);
    let (ours, theirs) = pair();
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let mut reader = std::io::BufReader::new(theirs);
    let socket_line = |reader: &mut std::io::BufReader<UnixStream>| {
        let mut text = String::new();
        match std::io::BufRead::read_line(reader, &mut text) {
            Ok(0) => panic!("the hub hung up"),
            Ok(_) => Some(text),
            Err(_) => None,
        }
    };
    // The first park: the writer is slow from the start, so the first
    // frame parks in its flush; the launch open's lines still arrive.
    block.store(true, Ordering::SeqCst);
    let (tx1, rx1) = mpsc::channel();
    let mut connect = Some((ours, hello()));
    let first = std::thread::Builder::new()
        .name("first-frame".to_owned())
        .spawn(move || {
            let (ours, hello) = connect.take().unwrap_or_else(|| panic!("once"));
            let mut lp = lp;
            let out = lp.step(Input::Connected(ours, hello), &rx1);
            (lp, out)
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let wait = Deadline::after(DEADLINE);
    let frame = wait.recv_or_fail(&events, "the first frame");
    assert!(frame.starts_with("frame\n"), "{frame:?}");
    assert_eq!(
        wait.recv_or_fail(&events, "the parked first flush"),
        "frame-enter"
    );
    // Feed, recent and summary go out while the writer is slow;
    // commands waits for summary acceptance, and full for the frame.
    // The three reads share one deadline.
    let (open_tx, open_rx) = mpsc::channel();
    let mut seen = Vec::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..3 {
                let line = socket_line(&mut reader).unwrap_or_else(|| panic!("an open line"));
                open_tx.send(line).unwrap_or(());
            }
        });
        let open = Deadline::after(DEADLINE);
        for _ in 0..3 {
            seen.push(open.recv_or_fail(&open_rx, "an open line"));
        }
    });
    // One release, then disarmed: the first frame finishes.
    block.store(false, Ordering::SeqCst);
    release_tx
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    let (mut lp, out) = first.join().unwrap_or_else(|_| panic!("the worker"));
    assert_eq!(out, None);
    drop(tx1);
    let lines: Vec<serde_json::Value> = seen
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let subscribes: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|line| line["command"] == "subscribe")
        .collect();
    assert_eq!(subscribes.len(), 1, "{lines:?}");
    assert_eq!(subscribes[0]["args"]["level"], "summary");
    let summary = subscribes[0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a summary id"))
        .to_owned();
    // The next frame repaints whole: the resize drops `last`, so the
    // armed flush parks inside `draw` even when the screen did not change.
    lp.screen
        .resize(60, 12)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    while events.try_recv().is_ok() {}
    // Acking the summary makes the raise due; the worker parks in the
    // next frame's flush, before `draw` returns.
    block.store(true, Ordering::SeqCst);
    let ack = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), serde_json::Value::String(summary))]
            .into_iter()
            .collect(),
    });
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("raise-frame".to_owned())
        .spawn(move || lp.step(Input::Hub(ack), &rx))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The tail read's `history` goes out before `draw`: answer it with
    // no lines, as an empty log would, so the frame installs its
    // preview and parks. No reader thread bridges the socket here, so
    // the answer goes straight onto the worker's channel.
    let commands = socket_line(&mut reader).unwrap_or_else(|| panic!("commands after acceptance"));
    let commands: serde_json::Value = serde_json::from_str(&commands).unwrap_or_default();
    assert_eq!(commands["command"], "commands");
    let read = socket_line(&mut reader).unwrap_or_else(|| panic!("the tail read"));
    let read: serde_json::Value = serde_json::from_str(&read).unwrap_or_default();
    assert_eq!(read["command"], "history");
    let history = read["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a history id"))
        .to_owned();
    let mut answer = serde_json::to_value(super::line("command_accepted", None, None, json!({})))
        .unwrap_or_default();
    answer["payload"] = json!({"command_id": history, "result": {"lines": []}});
    let answer: contract::Envelope =
        serde_json::from_value(answer).unwrap_or_else(|err| panic!("an answer: {err}"));
    tx.send(Input::Hub(Line::Session(answer)))
        .unwrap_or_else(|err| panic!("answer: {err}"));
    let flushed = wait.recv_or_fail(&events, "the parked frame");
    assert!(flushed.starts_with("frame\n"), "{flushed:?}");
    assert_eq!(
        wait.recv_or_fail(&events, "the parked flush"),
        "frame-enter"
    );
    // The raise is not on the socket yet: it goes out after `draw`
    // returns, which waits on the release below. A raise here fails.
    // The parked flush already proves no raise was written yet, so the
    // absence check takes no wait: the socket reads non-blocking.
    reader
        .get_mut()
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("non-blocking: {err}"));
    assert!(
        socket_line(&mut reader).is_none(),
        "the raise was written before `draw` returned"
    );
    reader
        .get_mut()
        .set_nonblocking(false)
        .unwrap_or_else(|err| panic!("blocking: {err}"));
    // One release, then disarmed: later flushes in this step run
    // through, so the frame always finishes after exactly one park.
    block.store(false, Ordering::SeqCst);
    release_tx
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    reader
        .get_mut()
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let raise = socket_line(&mut reader).unwrap_or_else(|| panic!("the raise"));
    let raise: serde_json::Value = serde_json::from_str(&raise).unwrap_or_default();
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["args"], json!({"level": "full"}));
    assert_eq!(worker.join().unwrap_or_else(|_| panic!("the worker")), None);
}

// The open's tail read: the scripted hub, the loop it drives, and the
// cases below (`docs/tui.md`, "History and paging"). The hub answers
// feed, recent, the summary subscribe and the session's commands at
// once, the tail's history windows from the log, and the `full` raise
// per the script; every command it reads is recorded in order.

/// What the scripted hub answers to the `full` raise.
enum Full {
    /// Accepts with the log's last seq, streams the log, serves the
    /// replay's `commands` requests, then hangs up.
    Replay,
    /// Accepts with no result: no pass opens.
    Accepted,
    /// Reads the raise and hangs up without answering.
    Hung,
    /// Accepts with no result, then serves a replay the test feeds
    /// itself: every post-raise `commands` is read but not answered, so
    /// the pass the test choreographs gains no input, and the hub hangs
    /// up when the loop does.
    ManualReplay,
}

/// What the scripted hub answers to a `history` read.
enum History {
    /// The doors' window: the last 256 lines at or below `to_seq`.
    Windows,
    /// The chain's pools: the own log, then the ancestor log when the
    /// args name a session, each read through the doors' window.
    Chain { ancestor: Vec<contract::Envelope> },
    /// Rejects with the message.
    Reject(&'static str),
    /// Answers no lines.
    Empty,
    /// Drops the stream without answering.
    Drop,
    /// Answers the first read's window, rejects the rest with the message.
    FailSecond(&'static str),
}

/// The loop an open's tail read drives: the recording backend's frames,
/// the hub's answers, and the channels the test steps and drains. The
/// loop is stepped through the summary ack, so the frame ran the read,
/// installed the preview and sent the raise.
struct TailOpen {
    lp: super::super::Loop<RecordBackend>,
    events: mpsc::Receiver<String>,
    seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    tx: mpsc::Sender<Input>,
    rx: mpsc::Receiver<Input>,
    hub_done: mpsc::Receiver<()>,
}

/// The doors' history window over `log`: the last 256 lines at or below
/// the args' `to_seq`, oldest first.
fn doors_window(log: &[contract::Envelope], args: &serde_json::Value) -> Vec<contract::Envelope> {
    let end = args
        .get("to_seq")
        .and_then(|to| to.as_u64())
        .unwrap_or(u64::MAX);
    let mut out: Vec<contract::Envelope> = log
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| seq.0 <= end))
        .cloned()
        .collect();
    out.drain(..out.len().saturating_sub(256));
    out
}

/// One open through the scripted hub: feed, recent, the summary ack,
/// the tail's history windows from `log` with `extra` hub lines before
/// each answer, then the scripted `full` answer. Stepped through the
/// summary ack, so the frame ran the read and sent the raise.
fn tail_open(
    log: Vec<contract::Envelope>,
    full: Full,
    history: History,
    extra: Vec<serde_json::Value>,
) -> TailOpen {
    tail_open_prepared(log, full, history, extra, |_| {})
}

fn tail_open_prepared(
    log: Vec<contract::Envelope>,
    full: Full,
    history: History,
    extra: Vec<serde_json::Value>,
    prepare: impl FnOnce(&mut crate::app::App),
) -> TailOpen {
    use std::sync::Mutex;
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: false,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Session(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()), None),
        ..Default::default()
    });
    app.set_size(60, 12);
    prepare(&mut app);
    let (events_tx, events) = mpsc::channel();
    let backend = RecordBackend {
        inner: ratatui::backend::TestBackend::new(60, 12),
        events: events_tx,
        block: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        release: mpsc::channel().1,
    };
    let screen = super::Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = super::super::Loop::for_test(app, screen);
    lp.hover = false;
    let (ours, hub_end) = pair();
    let (tx, rx) = mpsc::channel();
    let read_end = ours
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let read_tx = tx.clone();
    let feed = Feed::new(read_tx, lp.hub.credits());
    let (reader_done_tx, reader_done) = mpsc::channel();
    std::thread::Builder::new()
        .name("tail-read".to_owned())
        .spawn(move || {
            read_lines(read_end, &feed);
            reader_done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let want_full = matches!(
        full,
        Full::Replay | Full::Accepted | Full::Hung | Full::ManualReplay
    );
    let want_drop = matches!(history, History::Drop);
    let (hub_done_tx, hub_done) = mpsc::channel();
    let (full_tx, full_seen) = mpsc::channel();
    let replay = matches!(full, Full::Replay);
    let manual = matches!(full, Full::ManualReplay);
    // Every replayed turn start sends `commands` again: the streamed
    // replay's turn starts, which the hub serves before hanging up.
    let replay_turns = log
        .iter()
        .filter(|envelope| envelope.kind == "turn_started")
        .count();
    std::thread::Builder::new()
        .name("tail-hub".to_owned())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(hub_end);
            let mut text = String::new();
            let mut writer = reader
                .get_mut()
                .try_clone()
                .unwrap_or_else(|err| panic!("clone: {err}"));
            let mut write = move |value: serde_json::Value| {
                use std::io::Write;
                let mut bytes = serde_json::to_string(&value).unwrap_or_default();
                bytes.push('\n');
                writer
                    .write_all(bytes.as_bytes())
                    .unwrap_or_else(|err| panic!("hub write: {err}"));
            };
            let hub_ack = |id: &serde_json::Value, result: serde_json::Value| {
                json!({"kind": "command_accepted", "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": {"command_id": id, "result": result}})
            };
            let session_ack = |id: &serde_json::Value, result: serde_json::Value| {
                let envelope = super::line("command_accepted", None, None, json!({}));
                let mut value = serde_json::to_value(&envelope).unwrap_or_default();
                value["payload"] = json!({"command_id": id, "result": result});
                value
            };
            let session_rejected = |id: &serde_json::Value, message: &str| {
                let envelope = super::line("command_rejected", None, None, json!({}));
                let mut value = serde_json::to_value(&envelope).unwrap_or_default();
                value["payload"] = json!({"command_id": id, "message": message});
                value
            };
            let mut reads = 0usize;
            let mut replaying = false;
            let mut replay_commands = 0usize;
            while std::io::BufRead::read_line(&mut reader, &mut text).is_ok_and(|read| read > 0) {
                let command: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if let Ok(mut held) = record.lock() {
                    held.push(command.clone());
                }
                let id = command["id"].clone();
                match command["command"].as_str() {
                    Some("feed") => write(hub_ack(&id, json!({}))),
                    Some("recent") => write(hub_ack(&id, json!({"sessions": []}))),
                    Some("commands") => {
                        if replaying && manual {
                            // The test feeds the replay itself: the request
                            // is recorded above, and no answer goes out, so
                            // the choreographed pass gains no input.
                        } else {
                            write(session_ack(&id, json!({"commands": []})));
                            if replaying {
                                replay_commands += 1;
                                if replay_commands >= replay_turns {
                                    break;
                                }
                            }
                        }
                    }
                    Some("subscribe") if command["args"]["level"] == "full" => {
                        full_tx.send(()).unwrap_or(());
                        match full {
                            Full::Replay => {
                                let last = log
                                    .iter()
                                    .filter_map(|envelope| envelope.seq.map(|seq| seq.0))
                                    .max()
                                    .unwrap_or(0);
                                write(session_ack(&id, json!({"last_seq": last})));
                                for envelope in &log {
                                    write(serde_json::to_value(envelope).unwrap_or_default());
                                }
                                // The streamed replay's turn starts send
                                // `commands` again: serve those, then hang
                                // up, so no send lands on a dead hub.
                                replaying = true;
                            }
                            Full::Accepted => {
                                write(session_ack(&id, json!({})));
                            }
                            Full::Hung => {}
                            Full::ManualReplay => {
                                write(session_ack(&id, json!({})));
                                // The test feeds the replay itself: read
                                // its `commands`, answer none, and hang up
                                // when the loop does.
                                replaying = true;
                            }
                        }
                        if !replaying {
                            break;
                        }
                    }
                    Some("subscribe") => write(session_ack(&id, json!({}))),
                    Some("history") => {
                        for line in &extra {
                            write(line.clone());
                        }
                        if replaying && manual {
                            // The test feeds the replay itself: an empty
                            // window keeps the choreographed pass
                            // single-sourced.
                            write(session_ack(&id, json!({"lines": []})));
                            continue;
                        }
                        reads += 1;
                        match &history {
                            History::Windows => write(session_ack(
                                &id,
                                json!({"lines": doors_window(&log, &command["args"])}),
                            )),
                            History::Chain { ancestor } => {
                                let pool = if command["args"].get("session_id").is_some() {
                                    ancestor
                                } else {
                                    &log
                                };
                                write(session_ack(
                                    &id,
                                    json!({"lines": doors_window(pool, &command["args"])}),
                                ))
                            }
                            History::Reject(message) => write(session_rejected(&id, message)),
                            History::Empty => write(session_ack(&id, json!({"lines": []}))),
                            History::Drop => break,
                            History::FailSecond(message) if reads > 1 => {
                                write(session_rejected(&id, message));
                            }
                            History::FailSecond(_) => write(session_ack(
                                &id,
                                json!({"lines": doors_window(&log, &command["args"])}),
                            )),
                        }
                    }
                    _ => write(hub_ack(&id, json!({}))),
                }
            }
            hub_done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let wait = Deadline::after(DEADLINE);
    assert_eq!(lp.step(Input::Connected(ours, hello()), &rx), None);
    // The hub answers feed and recent: neither frame reads yet.
    for what in ["the feed answer", "the recent answer"] {
        let input = wait.recv_or_fail(&rx, what);
        assert_eq!(lp.step(input, &rx), None);
    }
    // Summary acceptance releases commands and starts the tail read.
    // Its commands reply can be stashed while the read awaits history.
    let summary = wait.recv_or_fail(&rx, "the summary subscribe answer");
    assert_eq!(lp.step(summary, &rx), None);
    let commands = lp
        .hub
        .pop_stashed()
        .unwrap_or_else(|| panic!("the stashed commands reply"));
    assert!(matches!(&commands, Input::Hub(Line::Session(envelope))
        if envelope.payload.get("result").is_some_and(|result| result.get("commands").is_some())));
    assert_eq!(lp.step(commands, &rx), None);
    if want_full && !want_drop {
        wait.recv_or_fail(&full_seen, "the hub to read the raise");
    }
    if want_drop {
        wait.recv_or_fail(&hub_done, "the hub to drop the stream");
        wait.recv_or_fail(&reader_done, "the reader to reach EOF");
    } else if !replay && !manual {
        wait.recv_or_fail(&hub_done, "the hub to hang up");
        wait.recv_or_fail(&reader_done, "the reader to reach EOF");
    }
    TailOpen {
        lp,
        events,
        seen,
        tx,
        rx,
        hub_done,
    }
}

/// The hub's commands in order, as (command, level or empty) pairs.
fn seen_commands(open: &TailOpen) -> Vec<(String, String)> {
    open.seen
        .lock()
        .map(|held| {
            held.iter()
                .map(|command| {
                    (
                        command["command"].as_str().unwrap_or_default().to_owned(),
                        command["args"]["level"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The recording backend's frame texts, oldest first.
fn frames(open: &TailOpen) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(event) = open.events.try_recv() {
        if let Some(text) = event.strip_prefix("frame\n") {
            out.push(text.to_owned());
        }
    }
    out
}

/// The first frame draws the last page of the tail before the `full`
/// raise: the preview's last reply shows, and the raise goes out after
/// the frame's history read.
#[test]
fn startup_loader_completes_before_the_first_draw_observes_its_result() {
    struct TracedLoad(mpsc::Sender<&'static str>);
    impl crate::ext::Load for TracedLoad {
        fn load(&self) -> (std::rc::Rc<dyn crate::ext::Renderer>, Vec<(String, String)>) {
            let loaded: (std::rc::Rc<dyn crate::ext::Renderer>, Vec<(String, String)>) = (
                std::rc::Rc::new(crate::ext::tests::SessionRenderer::new("first-renderer")),
                vec![("extension_failed".into(), "FIRST-LOAD-RESULT".into())],
            );
            self.0.send("load completed").unwrap();
            loaded
        }
    }
    let pair = crate::event_loop::tests::open();
    let watched = crate::pty_watch::watch(&pair.main, vec![b"\x1b]2;fiber\x07"]);
    let (trace_tx, trace) = mpsc::channel();
    let observer = trace_tx.clone();
    let (done, finished) = mpsc::channel();
    let mut launch = crate::event_loop::tests::launch();
    launch.extensions = Some(Arc::new(TracedLoad(trace_tx)));
    std::thread::spawn(move || {
        let result = crate::event_loop::run_with(
            pair.slave,
            launch,
            Box::new(|| Err(std::io::Error::other("unused"))),
            Box::new(|_| {}),
            fakes::clock::FakeClock::new(),
            |tty, tx| {
                crate::event_loop::Reader::spawn_with(
                    tty,
                    tx,
                    |_| Err(std::io::Error::other("stop after first frame")),
                    std::io::pipe,
                    |builder, body| builder.spawn(body),
                )
            },
            |builder, body| builder.spawn(body),
            crate::timing::info_diag(),
        );
        done.send(result).unwrap();
    });
    let wait = Deadline::after(DEADLINE);
    let bytes = wait.recv_or_fail(&watched, "the first frame title");
    assert!(
        String::from_utf8_lossy(&bytes).contains("FIRST-LOAD-RESULT"),
        "first draw must observe the loader result"
    );
    observer.send("first draw").unwrap();
    // docs/tui.md:2659: the first frame waits for the loader, so load
    // completion precedes the first draw observation in this trace.
    assert_eq!(
        wait.recv_or_fail(&trace, "load completion"),
        "load completed"
    );
    assert_eq!(
        wait.recv_or_fail(&trace, "first draw observation"),
        "first draw"
    );
    assert!(wait.recv_or_fail(&finished, "terminal cleanup").is_err());
}

#[test]
fn tail_speculation_failure_survives_replay_and_reaches_actual_app_drain_once() {
    let mut log = super::handoff_log(3, "tail reply");
    let at = log
        .iter()
        .position(|line| line.kind == "assistant_message_completed")
        .unwrap();
    let mut tool = log[at].clone();
    tool.kind = "tool_call_requested".into();
    tool.action_id = Some(contract::ActionId("a_tail_tool".into()));
    tool.payload = json!({"name":"shell","arguments":{"command":"echo tail"}})
        .as_object()
        .unwrap()
        .clone();
    log.insert(at, tool);
    for (at, line) in log.iter_mut().enumerate() {
        line.seq = Some(contract::Seq(at as u64));
    }
    let mut fake = crate::ext::tests::SessionRenderer::new("tail-failure");
    fake.failure = Some(crate::ext::FailureKind::HookFailed);
    let fake = std::rc::Rc::new(fake);
    let renderer = fake.clone();
    let mut open = tail_open_prepared(
        log,
        Full::Replay,
        History::Windows,
        Vec::new(),
        move |app| {
            app.install_renderer(renderer);
            app.on_key(
                crate::keys::Key::CtrlO,
                contract::clock::Clock::now(&*fakes::clock::FakeClock::new()),
            );
        },
    );
    assert!(
        !fake.calls.borrow().is_empty(),
        "tail counted a rendered tool row"
    );
    open.lp.app.drawn(&[]);
    let notices = open.lp.app.notices();
    // docs/tui.md:2734: the failure keeps one hook_failed per extension and slot.
    assert_eq!(notices.iter().filter(|line|matches!(line,crate::app::NoticeLine::Notice{code,text,..} if code=="hook_failed"&&text.contains("session-test"))).count(),1);
    assert!(!open.lp.app.drawn(&[]), "a replay cannot report it twice");
}

#[test]
fn tail_speculation_notice_survives_replay_and_reaches_durable_drain_once() {
    // Two contenders claim every ledger row: selection stays inert and
    // queues one conflict notice through the shared sink, without halves.
    #[derive(Debug)]
    struct ConflictPair {
        pending: std::cell::RefCell<Vec<(String, String)>>,
        reported: std::cell::Cell<bool>,
    }
    impl crate::ext::Renderer for ConflictPair {
        fn name(&self) -> &str {
            "tail-a"
        }
        fn fills(&self, slot: &str) -> bool {
            slot.starts_with("ledger_row")
        }
        fn fills_for(
            &self,
            _ctx: &crate::ext::SessionCtx,
            slot: &str,
        ) -> crate::ext::SlotSelection {
            if !self.fills(slot) {
                return crate::ext::SlotSelection::Absent;
            }
            if !self.reported.replace(true) {
                self.pending.borrow_mut().push((
                    "command_conflict".into(),
                    format!("extensions tail-a, tail-b replace {slot}; the built-in stays"),
                ));
            }
            crate::ext::SlotSelection::Suppressed
        }
        fn take_pending(&self) -> Vec<(String, String)> {
            std::mem::take(&mut *self.pending.borrow_mut())
        }
        fn render(
            &self,
            _ctx: &crate::ext::SessionCtx,
            _slot: &str,
            _input: &crate::ext::Input<'_>,
            _width: u16,
            _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
        ) -> crate::ext::Decision {
            crate::ext::Decision::Draw {
                extension: "tail-a".into(),
                lines: Vec::new(),
            }
        }
    }
    let mut log = super::handoff_log(3, "tail reply");
    let at = log
        .iter()
        .position(|line| line.kind == "assistant_message_completed")
        .unwrap();
    let mut tool = log[at].clone();
    tool.kind = "tool_call_requested".into();
    tool.action_id = Some(contract::ActionId("a_tail_tool".into()));
    tool.payload = json!({"name":"shell","arguments":{"command":"echo tail"}})
        .as_object()
        .unwrap()
        .clone();
    log.insert(at, tool);
    for (at, line) in log.iter_mut().enumerate() {
        line.seq = Some(contract::Seq(at as u64));
    }
    let renderer = std::rc::Rc::new(ConflictPair {
        pending: std::cell::RefCell::new(Vec::new()),
        reported: std::cell::Cell::new(false),
    });
    let mut open = tail_open_prepared(
        log,
        Full::Replay,
        History::Windows,
        Vec::new(),
        move |app| {
            app.install_renderer(renderer);
            app.on_key(
                crate::keys::Key::CtrlO,
                contract::clock::Clock::now(&*fakes::clock::FakeClock::new()),
            );
        },
    );
    open.lp.app.drawn(&[]);
    let notices = open.lp.app.notices();
    // docs/tui.md:2757: one command_conflict per slot and set of contenders.
    assert_eq!(notices.iter().filter(|line|matches!(line,crate::app::NoticeLine::Notice{code,text,..} if code=="command_conflict"&&text.contains("tail-a, tail-b"))).count(),1);
    assert!(
        !open.lp.app.drawn(&[]),
        "a replay cannot report the notice twice"
    );
}

#[test]
fn the_first_frame_draws_the_last_page_before_the_full_subscribe() {
    let log = super::handoff_log(3, "the last reply holds quokkas");
    let open = tail_open(log, Full::Hung, History::Windows, Vec::new());
    assert_eq!(
        seen_commands(&open),
        [
            ("feed".to_owned(), String::new()),
            ("recent".to_owned(), String::new()),
            ("subscribe".to_owned(), "summary".to_owned()),
            ("commands".to_owned(), String::new()),
            ("history".to_owned(), String::new()),
            ("subscribe".to_owned(), "full".to_owned()),
        ]
    );
    let drawn = frames(&open);
    let frame = drawn.last().unwrap_or_else(|| panic!("a frame"));
    assert!(frame.contains("quokkas"), "{frame}");
}

/// Older reads ask `to_seq` just below the earliest held line: a
/// 304-line session reads the last 256, then the window below line 48.
#[test]
fn older_reads_use_to_seq_just_below_the_earliest_held_line() {
    let log = usage_log();
    assert!(log.len() > 256, "two windows");
    let open = tail_open(log.clone(), Full::Accepted, History::Windows, Vec::new());
    let histories: Vec<serde_json::Value> = open
        .seen
        .lock()
        .map(|held| {
            held.iter()
                .filter(|command| command["command"] == "history")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(histories.len(), 2, "{histories:?}");
    assert_eq!(histories[0]["args"], json!({}));
    let earliest = log.len() as u64 - 256;
    assert_eq!(histories[1]["args"], json!({"to_seq": earliest - 1}));
}

/// A rejected history read shows the notice and still raises: the tail
/// ends as failed with the answer's message, and the raise goes out.
#[test]
fn a_history_rejection_shows_the_notice_and_still_raises() {
    let log = super::handoff_log(3, "the last reply");
    let open = tail_open(log, Full::Accepted, History::Reject("boom"), Vec::new());
    assert_eq!(open.lp.app.notice(), Some("Could not load history: boom"));
    let levels: Vec<String> = seen_commands(&open)
        .into_iter()
        .filter(|(command, _)| command == "subscribe")
        .map(|(_, level)| level)
        .collect();
    assert_eq!(levels, ["summary".to_owned(), "full".to_owned()]);
}

/// A reopening during the read abandons it and still raises: the live
/// `stream_reopened` ends the tail without a notice, the pass stays
/// open, and the raise goes out.
#[test]
fn a_reopening_during_the_read_abandons_it_and_still_raises() {
    let log = super::handoff_log(3, "the last reply");
    let reopened = json!({"kind": "stream_reopened", "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"session_id": "s_aaaaaaaaaaaaaaaa"}});
    let open = tail_open(log, Full::Accepted, History::Windows, vec![reopened]);
    assert!(open.lp.app.pass_open(), "the reopening holds the pass");
    assert_eq!(open.lp.app.notice(), None);
    let levels: Vec<String> = seen_commands(&open)
        .into_iter()
        .filter(|(command, _)| command == "subscribe")
        .map(|(_, level)| level)
        .collect();
    assert_eq!(levels, ["summary".to_owned(), "full".to_owned()]);
}

/// A lost connection during the read ends everything: the link drops,
/// the tail never installs, and no raise goes out.
#[test]
fn a_lost_connection_during_the_read_ends_everything() {
    let log = super::handoff_log(3, "the last reply");
    let mut open = tail_open(log, Full::Hung, History::Drop, Vec::new());
    assert!(!open.lp.app.connected());
    assert!(!open.lp.hub.has_stream());
    assert_eq!(
        open.lp.app.notice(),
        Some("Connection lost."),
        "a lost read ends through disconnected, never as a failed tail"
    );
    assert_eq!(open.lp.app.take_raise(), None);
    assert!(
        !seen_commands(&open)
            .iter()
            .any(|(command, level)| command == "subscribe" && level == "full"),
        "no raise"
    );
}

/// An empty log draws no history and raises: the frame holds no rows,
/// the install holds nothing, and the ack without a result opens no
/// pass.
#[test]
fn an_empty_log_draws_no_history_and_raises() {
    let log = super::handoff_log(3, "the last reply holds quokkas");
    let mut open = tail_open(log, Full::Accepted, History::Empty, Vec::new());
    let drawn = frames(&open);
    let frame = drawn.last().unwrap_or_else(|| panic!("a frame"));
    assert!(!frame.contains("quokkas"), "{frame}");
    assert_eq!(open.lp.app.pages().index().total(), 0);
    assert!(
        seen_commands(&open)
            .iter()
            .any(|(command, level)| command == "subscribe" && level == "full"),
        "the raise"
    );
    let wait = Deadline::after(DEADLINE);
    let ack = wait.recv_or_fail(&open.rx, "the raise's ack");
    assert_eq!(open.lp.step(ack, &open.rx), None);
    assert!(!open.lp.app.pass_open());
    assert_eq!(open.lp.app.take_raise(), None);
}

/// The tail read stashes other inputs and replays them in order: two
/// notices arriving mid-read wait the answer out, then fold oldest
/// first with the answer wait holding the commands reply and two notices.
#[test]
fn the_tail_read_stashes_other_inputs_and_replays_them_in_order() {
    let log = super::handoff_log(3, "the last reply");
    let notice = |text: &str| {
        let envelope = super::line("notice", None, None, json!({"message": text}));
        let mut value = serde_json::to_value(&envelope).unwrap_or_default();
        value["session_id"] = json!("s_aaaaaaaaaaaaaaaa");
        value
    };
    let mut open = tail_open(
        log,
        Full::Accepted,
        History::Windows,
        vec![notice("first"), notice("second")],
    );
    assert!(open.lp.hub.answer_stash_peak() <= 3);
    drop(open.tx);
    assert_eq!(open.lp.run(&open.rx), 0);
    let texts: Vec<String> = open
        .lp
        .app
        .notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { text, .. } => Some(text),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect();
    // The run's EOF notice lands after the replayed pair.
    assert!(texts.len() >= 2, "{texts:?}");
    assert_eq!(&texts[..2], ["first".to_owned(), "second".to_owned()]);
}

/// No frame shows part of the replay through a slow channel: the test
/// is the slow producer, sending the replay in parts and each part only
/// after the loop reports the held batch before it ended. The loop
/// waits at each batch end until the next part is wholly queued, so
/// every batch takes exactly the part meant for it. The first
/// part fills one `HUB_BATCH` batch, the next empties the channel short
/// of the boundary, a scroll key goes in mid-pass, and the last part
/// holds the boundary with two live lines above it. Neither the full
/// batch, the emptied channel nor the key writes a frame; the landing
/// writes the one frame, the whole session with the live lines below.
#[test]
fn no_frame_shows_part_of_the_replay_through_a_slow_channel() {
    use crate::event_loop::batch::HUB_BATCH;
    const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
    // The lines the emptied channel takes behind the full batch.
    const SHORT: usize = 50;
    // Nine lines a turn bar the last: the replay overfills one batch,
    // with a tail behind the short part.
    let log = super::handoff_log(479, "the last reply holds quokkas");
    // The ack and `HUB_BATCH - 1` lines fill the first batch.
    let head = HUB_BATCH - 1;
    assert!(
        log.len() > head + SHORT + 1,
        "the boundary sits behind the emptied channel"
    );
    let last = last_seq(&log);
    let live = [
        super::line(
            "turn_started",
            Some(last + 1),
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "live otters"}]}]}),
        ),
        // A whole turn, so the quit below finds nothing running to
        // interrupt.
        super::line(
            "turn_completed",
            Some(last + 2),
            None,
            json!({"outcome": "completed"}),
        ),
    ];
    // The hub serves the replay the test feeds below: it reads every
    // `commands` a replayed turn start sends but answers none, so the
    // choreographed batches gain no input.
    let open = tail_open(
        log.clone(),
        Full::ManualReplay,
        History::Windows,
        Vec::new(),
    );
    let first = frames(&open)
        .pop()
        .unwrap_or_else(|| panic!("a preview frame"));
    assert!(first.contains("quokkas"), "{first}");
    // Drain first: the hub's own ack may still sit queued, and the
    // worker must see exactly one pass, this ack, then the replay.
    while open.rx.try_recv().is_ok() {}
    let raise_id = open
        .seen
        .lock()
        .map(|held| {
            held.iter()
                .find(|command| {
                    command["command"] == "subscribe" && command["args"]["level"] == "full"
                })
                .and_then(|command| command["id"].as_str().map(str::to_owned))
        })
        .unwrap_or_default()
        .unwrap_or_else(|| panic!("the raise"));
    let TailOpen {
        mut lp,
        events,
        seen,
        tx,
        rx,
        hub_done,
    } = open;
    let send = |input: Input| tx.send(input).unwrap_or_else(|error| panic!("{error}"));
    let send_lines = |lines: &[contract::Envelope]| {
        for envelope in lines {
            send(Input::Hub(Line::Session(envelope.clone())));
        }
    };
    // The first part: the ack carrying the boundary and enough replay
    // to fill a batch.
    send(Input::Hub(raise_accepted(SESSION, &raise_id, last)));
    send_lines(&log[..head]);
    let (ended_tx, ended) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        pause::report_ends(ended_tx, release);
        let code = lp.run(&rx);
        done_tx.send((code, lp)).unwrap_or(());
    });
    let wait = Deadline::after(DEADLINE);
    let mut taken = Vec::new();
    // Each batch end holds the loop until this send: the next part is
    // queued whole by then.
    let resume = || {
        release_tx
            .send(())
            .unwrap_or_else(|error| panic!("{error}"))
    };
    let mut batch_end = |what: &str| {
        let count = wait.recv_or_fail(&ended, what);
        taken.push(count);
        count
    };
    let drawn = || {
        let mut out = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Some(text) = event.strip_prefix("frame\n") {
                out.push(text.to_owned());
            }
        }
        out
    };
    assert_eq!(batch_end("the full batch"), HUB_BATCH);
    // The second part, sent after the full batch ended: it empties the
    // channel short of the boundary.
    send_lines(&log[head..head + SHORT]);
    resume();
    assert_eq!(batch_end("the emptied channel"), SHORT);
    // A scroll key mid-pass, then the last part: the rest of the replay
    // with the live lines pushed behind the boundary line.
    send(Input::Bytes(b"\x1b[5~".to_vec()));
    send_lines(&log[head + SHORT..]);
    send_lines(&live);
    resume();
    let landing = batch_end("the landing batch");
    // The landing batch began after the full batch's, the emptied
    // channel's and the key's frames were all written: none was.
    assert!(
        drawn().is_empty(),
        "no frame before the landing: neither the full batch, the emptied channel nor the key draws"
    );
    assert_eq!(landing, log.len() - head - SHORT + live.len());
    // Every later batch end runs through.
    drop(release_tx);
    send(quit());
    let (code, lp) =
        Deadline::after(DEADLINE).recv_or_fail(&done, "the loop to quit after the replay");
    assert_eq!(code, 0);
    assert!(
        taken.contains(&HUB_BATCH),
        "a full batch was taken: {taken:?}"
    );
    assert!(
        taken[..taken.len() - 1]
            .iter()
            .any(|count| *count < HUB_BATCH),
        "the channel emptied mid-pass: {taken:?}"
    );
    // Every frame written: the landing's alone, holding the session's
    // last reply with the live lines below it.
    let written = drawn();
    assert_eq!(written.len(), 1, "the landing frame alone: {written:?}");
    let shown = &written[0];
    assert_ne!(*shown, first, "the landing draws");
    let otters = shown
        .find("live otters")
        .unwrap_or_else(|| panic!("the live lines show: {shown}"));
    if let Some(quokkas) = shown.find("quokkas") {
        assert!(quokkas < otters, "the live lines sit below: {shown}");
    }
    assert!(!lp.app.pass_open());
    let mut whole = lp.app.pages().fresh();
    for envelope in log.iter().chain(&live) {
        whole.apply(envelope);
    }
    assert_eq!(lp.app.pages().index().total(), whole.index().total());
    // Every replayed turn start sends `commands` again: one per log
    // turn plus the live one, each read and none answered.
    drop(lp);
    Deadline::after(DEADLINE).recv_or_fail(&hub_done, "the hub to hang up");
    let replay_turns = log
        .iter()
        .filter(|envelope| envelope.kind == "turn_started")
        .count()
        + 1;
    let seen_commands: Vec<serde_json::Value> =
        seen.lock().map(|held| held.clone()).unwrap_or_default();
    let full_at = seen_commands
        .iter()
        .position(|command| command["command"] == "subscribe" && command["args"]["level"] == "full")
        .expect("the full subscribe");
    let replayed: Vec<&serde_json::Value> = seen_commands[full_at + 1..]
        .iter()
        .filter(|command| command["command"] == "commands")
        .collect();
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(replayed.len(), replay_turns, "{replayed:?}");
}

#[test]
fn a_preview_survives_the_pass_and_lands_with_the_whole_session() {
    let log = super::handoff_log(3, "the last reply holds quokkas");
    let mut open = tail_open(log.clone(), Full::Replay, History::Windows, Vec::new());
    let drawn = frames(&open);
    let frame = drawn.last().unwrap_or_else(|| panic!("a frame"));
    assert!(frame.contains("quokkas"), "{frame}");
    assert!(open.lp.app.pages().index().total() > 0);
    let mut whole = open.lp.app.pages().fresh();
    for envelope in &log {
        whole.apply(envelope);
    }
    drop(open.tx);
    assert_eq!(open.lp.run(&open.rx), 0);
    assert_eq!(open.lp.app.pages().index().total(), whole.index().total());
    assert!(!open.lp.app.pass_open());
    assert_eq!(open.lp.app.take_raise(), None);
    // Every replayed turn start sends `commands` again: the requests
    // after the `full` subscribe match the log's turn starts one for one.
    Deadline::after(DEADLINE).recv_or_fail(&open.hub_done, "the hub to hang up");
    let replay_turns = log
        .iter()
        .filter(|envelope| envelope.kind == "turn_started")
        .count();
    let seen_commands: Vec<serde_json::Value> = open
        .seen
        .lock()
        .map(|held| held.clone())
        .unwrap_or_default();
    let full_at = seen_commands
        .iter()
        .position(|command| command["command"] == "subscribe" && command["args"]["level"] == "full")
        .expect("the full subscribe");
    let replayed = &seen_commands[full_at + 1..];
    // docs/tui.md, "Slash commands": one `commands` request per `turn_started` (docs/testing.md, "Event streams").
    assert_eq!(replayed.len(), replay_turns, "{replayed:?}");
    assert!(
        replayed
            .iter()
            .all(|command| command["command"] == "commands"),
        "{replayed:?}"
    );
}

/// A log whose last 256 lines hold the second turn's start with about
/// one row: the first read installs them without filling the screen, so
/// the read continues to the second window.
fn push_line(
    log: &mut Vec<contract::Envelope>,
    seq: &mut u64,
    kind: &str,
    action: Option<&str>,
    payload: serde_json::Value,
) {
    log.push(super::line(kind, Some(*seq), action, payload));
    *seq += 1;
}

fn usage_payload() -> serde_json::Value {
    json!({"generation_id": "g1", "model": "fake/m",
        "tokens": {"input": 1, "cache_read": 0, "cache_write": {}, "output": 1},
        "input_bytes": 0, "cost": null})
}

fn usage_log() -> Vec<contract::Envelope> {
    let mut log = Vec::new();
    let mut seq = 0u64;
    // The first turn fills the lines above the window: its usage lines
    // fold to no rows.
    push_line(
        &mut log,
        &mut seq,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "first"}]}]}),
    );
    push_line(&mut log, &mut seq, "step_started", None, json!({}));
    push_line(
        &mut log,
        &mut seq,
        "text_completed",
        Some("a_m0"),
        json!({"text": "the first reply"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "assistant_message_completed",
        Some("a_m0"),
        json!({"outcome": "completed"}),
    );
    for _ in 0..39 {
        push_line(
            &mut log,
            &mut seq,
            "usage_recorded",
            Some("a_m0"),
            usage_payload(),
        );
    }
    push_line(
        &mut log,
        &mut seq,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    // The second turn's start sits at line 44 with usage behind it.
    push_line(
        &mut log,
        &mut seq,
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "second"}]}]}),
    );
    while log.len() < 300 {
        push_line(
            &mut log,
            &mut seq,
            "usage_recorded",
            Some("a_m1"),
            usage_payload(),
        );
    }
    log
}

/// A failed read keeps the preview and drops the partial fold: the
/// second read rejected, the screen holds the first answer's lines with
/// the refusal, and the raise still goes out.
#[test]
fn a_failed_read_keeps_the_preview_and_drops_the_partial_fold() {
    let log = usage_log();
    assert!(log.len() > 256);
    let first: Vec<contract::Envelope> = log[log.len() - 256..].to_vec();
    let open = tail_open(log, Full::Accepted, History::FailSecond("boom"), Vec::new());
    assert_eq!(open.lp.app.notice(), Some("Could not load history: boom"));
    let mut installed = crate::window::Pages::new(60);
    for envelope in &first {
        installed.apply(envelope);
    }
    assert!(installed.index().total() > 0);
    assert_eq!(
        open.lp.app.pages().index().total(),
        installed.index().total()
    );
    assert!(
        seen_commands(&open)
            .iter()
            .any(|(command, level)| command == "subscribe" && level == "full"),
        "the raise"
    );
}

/// An attached app at 60x12 with the hub up and the session live: lines
/// fold straight in, with no open in flight.
fn live_app() -> crate::app::App {
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: false,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Home,
        ..Default::default()
    });
    app.set_size(60, 12);
    app.on_line(Line::Hub(hello()));
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    app
}

/// A session `command_accepted` carrying the command's id.
fn session_accepted(session: &str, command: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), json!(command))]
            .into_iter()
            .collect(),
    })
}

/// The raise's acceptance with the pass ending at `last`.
fn raise_accepted(session: &str, command: &str, last: u64) -> Line {
    Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [
            ("command_id".to_owned(), json!(command)),
            ("result".to_owned(), json!({"last_seq": last})),
        ]
        .into_iter()
        .collect(),
    })
}

/// The launched open's summary id: the summary subscribe among the
/// lines the reconnect returns.
fn summary_id(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap_or_default())
        .find(|line| line["command"] == "subscribe" && line["args"]["level"] == "summary")
        .and_then(|line| line["id"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("a summary subscribe in {lines:?}"))
}

/// The whole 60x12 screen text.
fn rendered(app: &crate::app::App) -> String {
    let area = ratatui::layout::Rect::new(0, 0, 60, 12);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(app, area, &mut buf, None);
    crate::view::text(&buf)
}

/// Drops the disconnect notice: the place asserts compare the
/// conversation, and the disconnect notice is covered by the loop's
/// lost-connection case.
fn dismiss_notices(app: &mut crate::app::App) {
    let ids: Vec<usize> = app
        .notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { id, .. } => Some(id),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect();
    for id in ids {
        app.dismiss_notice(id);
    }
}

/// The last seq in `log`.
fn last_seq(log: &[contract::Envelope]) -> u64 {
    log.iter()
        .filter_map(|envelope| envelope.seq.map(|seq| seq.0))
        .max()
        .unwrap_or(0)
}

/// A reconnect to the session on screen reads nothing and keeps its
/// place: no `history` goes out, the rows and the jumped top stay, and
/// the replay from zero lands with the same rows on screen. Without
/// the anchor the landing follows the end instead, and a row open
/// still reads and follows.
#[test]
fn a_reconnect_to_the_session_on_screen_reads_nothing_and_keeps_its_place() {
    const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
    // The held rows stay the preview through the reconnect.
    let log = super::handoff_log(5, "the held reply holds quokkas");
    let last = last_seq(&log);
    let mut app = live_app();
    for envelope in &log {
        app.on_line(Line::Session(envelope.clone()));
    }
    let total = app.pages().index().total();
    assert!(total > 12, "taller than the screen");
    app.jump(2);
    let before = rendered(&app);
    app.disconnected();
    let lines = app.on_line(Line::Hub(hello()));
    dismiss_notices(&mut app);
    assert!(
        !lines.iter().any(|line| line.contains("\"history\"")),
        "reads nothing: {lines:?}"
    );
    assert_eq!(app.tail_due(), None);
    assert_eq!(app.pages().index().total(), total);
    assert_eq!(rendered(&app), before);
    let summary = summary_id(&lines);
    app.on_line(session_accepted(SESSION, &summary));
    let raise: serde_json::Value =
        serde_json::from_str(&app.take_raise().expect("a raise")).unwrap_or_default();
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(raise["args"]["level"], "full");
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(raise_accepted(SESSION, raise_id, last));
    assert!(app.pass_open());
    for envelope in &log {
        app.on_line(Line::Session(envelope.clone()));
    }
    assert!(!app.pass_open());
    assert_eq!(app.pages().index().total(), total);
    assert_eq!(rendered(&app), before);

    // Without the anchor the landing follows the end: the replay
    // lacks the top row's reply, so the top row's text is gone and
    // the last lines show.
    let log = super::handoff_log(2, "the first reply");
    let mut app = live_app();
    for envelope in &log {
        app.on_line(Line::Session(envelope.clone()));
    }
    app.jump(app.pages().index().total() - 1);
    app.disconnected();
    let lines = app.on_line(Line::Hub(hello()));
    assert!(
        !lines.iter().any(|line| line.contains("\"history\"")),
        "reads nothing: {lines:?}"
    );
    let other = super::handoff_log(1, "the other last reply");
    let summary = summary_id(&lines);
    app.on_line(session_accepted(SESSION, &summary));
    let raise: serde_json::Value =
        serde_json::from_str(&app.take_raise().expect("a raise")).unwrap_or_default();
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    app.on_line(raise_accepted(SESSION, raise_id, last_seq(&other)));
    for envelope in &other {
        app.on_line(Line::Session(envelope.clone()));
    }
    assert!(!app.pass_open());
    let mut whole = app.pages().fresh();
    for envelope in &other {
        whole.apply(envelope);
    }
    assert_eq!(app.pages().index().total(), whole.index().total());
    let screen = rendered(&app);
    assert!(screen.contains("the other last reply"), "{screen}");
    assert!(!screen.contains("the first reply"), "{screen}");

    // A row open still reads and follows: the tail read goes out,
    // and the landed screen shows the end of a session taller than
    // the screen.
    let log = super::handoff_log(5, "the last reply holds quokkas");
    let mut open = tail_open(log, Full::Replay, History::Windows, Vec::new());
    assert!(
        open.seen
            .lock()
            .map(|held| held.iter().any(|command| command["command"] == "history"))
            .unwrap_or_default(),
        "the tail read goes out"
    );
    drop(open.tx);
    assert_eq!(open.lp.run(&open.rx), 0);
    let screen = rendered(&open.lp.app);
    assert!(screen.contains("quokkas"), "{screen}");
    assert!(!screen.contains("turn 0"), "{screen}");
}

/// A log with one delegate job between two turns.
fn job_log() -> Vec<contract::Envelope> {
    let mut log = Vec::new();
    let mut seq = 0u64;
    let prompt = |text: &str| {
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]})
    };
    push_line(&mut log, &mut seq, "turn_started", None, prompt("first"));
    push_line(&mut log, &mut seq, "step_started", None, json!({}));
    push_line(
        &mut log,
        &mut seq,
        "text_completed",
        Some("a_m0"),
        json!({"text": "the first reply"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "assistant_message_completed",
        Some("a_m0"),
        json!({"outcome": "completed"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "job_started",
        Some("a_1"),
        json!({"job_id": "j_1", "description": "task 1", "output_path": "/tmp/out"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "delegate_started",
        Some("a_1"),
        json!({"job_id": "j_1", "delegate_session_id": "s_j_00000000000001",
            "harness": "fiber", "model": "test/model", "workspace": "/w"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    push_line(&mut log, &mut seq, "turn_started", None, prompt("second"));
    push_line(&mut log, &mut seq, "step_started", None, json!({}));
    push_line(
        &mut log,
        &mut seq,
        "text_completed",
        Some("a_m1"),
        json!({"text": "the last reply"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "assistant_message_completed",
        Some("a_m1"),
        json!({"outcome": "completed"}),
    );
    push_line(
        &mut log,
        &mut seq,
        "turn_completed",
        None,
        json!({"outcome": "completed"}),
    );
    log
}

/// A held reconnect replay leaves one job card: the same log replayed
/// through the held pass and through the loop's read shows one card
/// and the same panel.
#[test]
fn a_held_reconnect_replay_leaves_one_job_card() {
    const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
    let log = job_log();
    let last = last_seq(&log);
    let mut held = live_app();
    for envelope in &log {
        held.on_line(Line::Session(envelope.clone()));
    }
    held.disconnected();
    let lines = held.on_line(Line::Hub(hello()));
    dismiss_notices(&mut held);
    let summary = summary_id(&lines);
    held.on_line(session_accepted(SESSION, &summary));
    let raise: serde_json::Value =
        serde_json::from_str(&held.take_raise().expect("a raise")).unwrap_or_default();
    let raise_id = raise["id"].as_str().unwrap_or_else(|| panic!("an id"));
    held.on_line(raise_accepted(SESSION, raise_id, last));
    for envelope in &log {
        held.on_line(Line::Session(envelope.clone()));
    }
    assert!(!held.pass_open());
    let mut open = tail_open(log.clone(), Full::Replay, History::Windows, Vec::new());
    let wait = Deadline::after(DEADLINE);
    for _ in 0..log.len() + 1 {
        let input = wait.recv_or_fail(&open.rx, "the replay");
        assert_eq!(open.lp.step(input, &open.rx), None);
    }
    assert!(!open.lp.app.pass_open());
    assert_eq!(rendered(&held).matches("task 1").count(), 1);
    assert_eq!(rendered(&open.lp.app).matches("task 1").count(), 1);
    assert_eq!(held.panel(), open.lp.app.panel());
    // The running delegate's spinner animates, so the screens can show
    // different frames of the same card: the counts above pin the one
    // card, the panel pins its state.
}

/// The ancestor's session: the chain's first segment.
const ANCESTOR: &str = "s_bbbbbbbbbbbbbbbb";

/// Ancestor lines, `turns` turns numbered from `seq 0` under [`ANCESTOR`].
fn ancestor_lines(turns: usize) -> Vec<contract::Envelope> {
    let mut seq = 0u64;
    let mut lines = Vec::new();
    for turn in 0..turns {
        for input in super::turn_lines(&format!("ancestor reply {turn}"), turn + 100, &mut seq) {
            if let Input::Hub(Line::Session(mut envelope)) = input {
                envelope.session_id = contract::SessionId(ANCESTOR.to_owned());
                lines.push(envelope);
            }
        }
    }
    lines
}

/// Answers every `history` window from the pool its args name: the
/// ancestor's lines when the args carry [`ANCESTOR`], the own log's
/// otherwise.
fn chain_answers(
    owned: Vec<contract::Envelope>,
    ancestor: Vec<contract::Envelope>,
) -> super::Script {
    Box::new(move |command: &serde_json::Value| {
        if command["command"] != "history" {
            return Vec::new();
        }
        let id = command["id"].as_str().unwrap_or_default().to_owned();
        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
        let pool = match command["args"].get("session_id").and_then(|id| id.as_str()) {
            Some(ANCESTOR) => &ancestor,
            _ => &owned,
        };
        let lines: Vec<contract::Envelope> = pool
            .iter()
            .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
            .cloned()
            .collect();
        vec![Input::Hub(Line::Session(command_answer(&id, lines)))]
    })
}

#[test]
fn history_fetch_names_each_window_with_its_segment_session() {
    use contract::Seq;
    use std::ops::RangeInclusive;
    let (mut lp, theirs, tx, rx) = opened();
    let owned = session();
    let ancestor = ancestor_lines(8);
    let serving = contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let seen = serve(
        theirs,
        tx.clone(),
        chain_answers(owned.clone(), ancestor.clone()),
    );
    // The own run's windows carry no session: absent means the serving
    // session, exactly as today.
    let range: RangeInclusive<Seq> = Seq(0)..=Seq(300);
    let lines = lp
        .hub
        .fetch(
            &rx,
            &serving,
            &range,
            &mut lp.app,
            &lp.tick,
            lp.clock.as_ref(),
            lp.tick.armed(),
        )
        .unwrap_or_else(|err| panic!("own fetch: {err}"));
    assert_eq!(lines.len(), 301, "both own windows answer");
    // The ancestor's window rides the serving session's chain read: the
    // top-level session stays the serving one, and the args name the
    // ancestor.
    let last = u64::try_from(ancestor.len())
        .expect("ancestor lines")
        .saturating_sub(1);
    let arange: RangeInclusive<Seq> = Seq(0)..=Seq(last);
    let alines = lp
        .hub
        .fetch(
            &rx,
            &contract::SessionId(ANCESTOR.to_owned()),
            &arange,
            &mut lp.app,
            &lp.tick,
            lp.clock.as_ref(),
            lp.tick.armed(),
        )
        .unwrap_or_else(|err| panic!("ancestor fetch: {err}"));
    assert_eq!(alines.len(), ancestor.len(), "the ancestor window answers");
    assert!(
        alines
            .iter()
            .all(|line| line.session_id == contract::SessionId(ANCESTOR.to_owned())),
        "the ancestor's lines come back under its session"
    );
    let seen = seen.lock().unwrap_or_else(|err| panic!("seen: {err}"));
    assert_eq!(seen.len(), 3, "two own windows and one ancestor window");
    for command in &seen[..2] {
        assert_eq!(command["command"], "history");
        assert_eq!(command["session_id"], "s_aaaaaaaaaaaaaaaa");
        assert!(
            command["args"].get("session_id").is_none(),
            "an own window names no session: {command}"
        );
    }
    assert_eq!(seen[0]["args"]["from_seq"], 0);
    assert_eq!(seen[0]["args"]["to_seq"], 255);
    assert_eq!(seen[1]["args"]["from_seq"], 256);
    assert_eq!(seen[1]["args"]["to_seq"], 300);
    assert_eq!(seen[2]["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(seen[2]["args"]["session_id"], ANCESTOR);
    assert_eq!(seen[2]["args"]["from_seq"], 0);
    assert_eq!(seen[2]["args"]["to_seq"], last);
}

#[test]
fn history_fetch_past_the_point_fails_with_its_message() {
    use contract::Seq;
    use std::ops::RangeInclusive;
    let (mut lp, theirs, tx, rx) = opened();
    let seen = serve(
        theirs,
        tx.clone(),
        Box::new(|command: &serde_json::Value| {
            if command["command"] != "history" {
                return Vec::new();
            }
            let id = command["id"].as_str().unwrap_or_default().to_owned();
            if command["args"].get("session_id").and_then(|id| id.as_str()) == Some(ANCESTOR) {
                let mut rejected = line("command_rejected", None, None, json!({}));
                rejected.payload =
                    json!({"command_id": id, "message": "from_seq is past the point"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default();
                return vec![Input::Hub(Line::Session(rejected))];
            }
            vec![Input::Hub(Line::Session(command_answer(&id, Vec::new())))]
        }),
    );
    let range: RangeInclusive<Seq> = Seq(0)..=Seq(10);
    let err = lp
        .hub
        .fetch(
            &rx,
            &contract::SessionId(ANCESTOR.to_owned()),
            &range,
            &mut lp.app,
            &lp.tick,
            lp.clock.as_ref(),
            lp.tick.armed(),
        )
        .expect_err("past the point fails");
    assert_eq!(err, "from_seq is past the point");
    let seen = seen.lock().unwrap_or_else(|err| panic!("seen: {err}"));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["args"]["session_id"], ANCESTOR);
}

/// A `session_started` opening `session` at seq zero, continuing from
/// `parent` when rewound.
fn started(session: &str, parent: Option<(&str, u64)>) -> contract::Envelope {
    let mut payload = json!({});
    if let Some((parent_session, point)) = parent {
        payload = json!({"forked_from": {"session_id": parent_session, "seq": point}});
    }
    contract::Envelope {
        kind: "session_started".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(0)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// `turns` full turns under `session` from `seq`, each replying
/// `marker N`, rehomed from the loop session.
fn segment_turns(
    session: &str,
    marker: &str,
    turn_at: usize,
    turns: usize,
    seq: &mut u64,
) -> Vec<contract::Envelope> {
    let mut lines = Vec::new();
    for turn in 0..turns {
        let message = format!("a_m{turn_at}_{turn}");
        push_line(
            &mut lines,
            seq,
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "prompt"}]}]}),
        );
        push_line(&mut lines, seq, "step_started", None, json!({}));
        push_line(
            &mut lines,
            seq,
            "assistant_message_started",
            Some(&message),
            json!({}),
        );
        push_line(
            &mut lines,
            seq,
            "text_completed",
            Some(&message),
            json!({"text": format!("{marker} {turn}")}),
        );
        push_line(
            &mut lines,
            seq,
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        push_line(
            &mut lines,
            seq,
            "turn_completed",
            None,
            json!({"outcome": "completed"}),
        );
    }
    for envelope in &mut lines {
        envelope.session_id = contract::SessionId(session.to_owned());
    }
    lines
}

/// An open across a continuation point walks the chain: the own
/// windows, then the ancestor's under its session, and the joint
/// total matches the joint fold.
#[test]
fn the_open_walks_the_chain_and_shows_the_joint_total() {
    const ROOT: &str = "s_arrrrrrrrrrrrrr";
    const CHILD: &str = "s_aaaaaaaaaaaaaaaa";
    let mut root_seq = 1u64;
    let mut ancestor = vec![started(ROOT, None)];
    ancestor.extend(segment_turns(ROOT, "root reply", 100, 3, &mut root_seq));
    let point = root_seq - 1;
    let mut child = vec![started(CHILD, Some((ROOT, point)))];
    let mut child_seq = 1u64;
    child.extend(segment_turns(CHILD, "child reply", 200, 2, &mut child_seq));
    let open = tail_open(
        child.clone(),
        Full::Hung,
        History::Chain {
            ancestor: ancestor.clone(),
        },
        Vec::new(),
    );
    let histories: Vec<serde_json::Value> = open
        .seen
        .lock()
        .map(|held| {
            held.iter()
                .filter(|command| command["command"] == "history")
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        histories
            .iter()
            .map(|command| &command["args"])
            .collect::<Vec<_>>(),
        vec![&json!({}), &json!({"session_id": ROOT, "to_seq": point}),],
        "the own window, then the ancestor's under its session: {histories:?}"
    );
    let mut preview = crate::window::Pages::new(60);
    for line in ancestor.iter().chain(&child) {
        preview.apply(line);
    }
    assert_eq!(
        open.lp.app.scroll().1,
        preview.index().total(),
        "the joint total spans the chain"
    );
    // The preview follows to the bottom: the last frame shows the
    // child, the joint total counts the chain, and the trimmed
    // ancestor page stays paged under its own session for a scroll up.
    let drawn = frames(&open).join("\n");
    assert!(drawn.contains("child reply 1"), "the child shows: {drawn}");
    let pages = open.lp.app.pages();
    let total = pages.index().total();
    let top_needs = pages.needs(0, total);
    assert!(
        top_needs.iter().any(|(session, range)| session.0 == ROOT
            && *range == (contract::Seq(0)..=contract::Seq(point))),
        "the ancestor stays paged under its session: {top_needs:?}"
    );
}

/// The first frame draws the own-log screen before any ancestor count
/// request and before the `full` raise: the own window fills the
/// display, so the frame shows the child's last page without waiting
/// for the ancestor; the ancestor count follows the frame, and the
/// raise goes last with the joint total exact.
#[test]
fn the_first_frame_draws_before_the_ancestor_count_and_the_raise() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const ROOT: &str = "s_arrrrrrrrrrrrrr";
    const CHILD: &str = "s_aaaaaaaaaaaaaaaa";
    let mut root_seq = 1u64;
    let mut ancestor = vec![started(ROOT, None)];
    ancestor.extend(segment_turns(ROOT, "root reply", 100, 3, &mut root_seq));
    let point = root_seq - 1;
    let mut child_seq = 1u64;
    let mut child = vec![started(CHILD, Some((ROOT, point)))];
    child.extend(segment_turns(CHILD, "child reply", 200, 30, &mut child_seq));
    assert!(child.len() <= 256, "the own run fits one window");
    let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        location: crate::LaunchLocation::Local {
            workspace: std::path::PathBuf::from("/w"),
            project: "-w".to_owned(),
            git: false,
        },
        hover: false,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Session(contract::SessionId(CHILD.to_owned()), None),
        ..Default::default()
    });
    app.set_size(60, 12);
    let (events_tx, events) = mpsc::channel();
    let block = std::sync::Arc::new(AtomicBool::new(false));
    let (release_tx, release) = mpsc::channel();
    let backend = RecordBackend {
        inner: ratatui::backend::TestBackend::new(60, 12),
        events: events_tx,
        block: std::sync::Arc::clone(&block),
        release,
    };
    let screen = super::Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = super::super::Loop::for_test(app, screen);
    lp.hover = false;
    let (ours, theirs) = pair();
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let mut reader = std::io::BufReader::new(theirs);
    let socket_line = |reader: &mut std::io::BufReader<UnixStream>| {
        let mut text = String::new();
        match std::io::BufRead::read_line(reader, &mut text) {
            Ok(0) => panic!("the hub hung up"),
            Ok(_) => Some(text),
            Err(_) => None,
        }
    };
    let (tx, rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Connected(ours, hello()), &rx), None);
    // The launch open's lines: feed, recent and the summary subscribe.
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(socket_line(&mut reader).unwrap_or_else(|| panic!("an open line")));
    }
    let lines: Vec<serde_json::Value> = seen
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let summary = lines
        .iter()
        .find(|line| line["command"] == "subscribe")
        .and_then(|line| line["id"].as_str())
        .unwrap_or_else(|| panic!("a summary subscribe: {lines:?}"))
        .to_owned();
    while events.try_recv().is_ok() {}
    // The own window fills the display on its own segment.
    let height = lp.app.conversation_height();
    let mut filled = crate::window::Pages::new(60);
    for line in doors_window(&child, &json!({})) {
        filled.apply(&line);
    }
    assert!(
        filled.index().total() >= height,
        "the own window fills the screen"
    );
    // Acking the summary starts the tail read on a worker; the first
    // frame parks in its flush once the preview installs.
    block.store(true, Ordering::SeqCst);
    let ack = Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId(CHILD.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: [("command_id".to_owned(), serde_json::Value::String(summary))]
            .into_iter()
            .collect(),
    });
    let worker = std::thread::Builder::new()
        .name("first-frame".to_owned())
        .spawn(move || {
            let mut lp = lp;
            let out = lp.step(Input::Hub(ack), &rx);
            (lp, out)
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let commands = socket_line(&mut reader).unwrap_or_else(|| panic!("commands after acceptance"));
    let commands: serde_json::Value = serde_json::from_str(&commands).unwrap_or_default();
    assert_eq!(commands["command"], "commands");
    let read = socket_line(&mut reader).unwrap_or_else(|| panic!("the tail read"));
    let read: serde_json::Value = serde_json::from_str(&read).unwrap_or_default();
    assert_eq!(read["command"], "history");
    assert_eq!(read["args"], json!({}), "the newest own window first");
    let history = read["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a history id"))
        .to_owned();
    let window = doors_window(&child, &json!({}));
    let mut answer = serde_json::to_value(super::line("command_accepted", None, None, json!({})))
        .unwrap_or_default();
    answer["payload"] = json!({"command_id": history, "result": {"lines": window}});
    let answer: contract::Envelope =
        serde_json::from_value(answer).unwrap_or_else(|err| panic!("an answer: {err}"));
    tx.send(Input::Hub(Line::Session(answer)))
        .unwrap_or_else(|err| panic!("answer: {err}"));
    // The first frame draws before any ancestor read: the parked flush
    // reports, while any hub command on the socket fails: the ancestor
    // count and the raise both follow the frame.
    reader
        .get_mut()
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("non-blocking: {err}"));
    let wait = Deadline::after(DEADLINE);
    let mut drawn = Vec::new();
    loop {
        while let Ok(event) = events.try_recv() {
            drawn.push(event);
        }
        if drawn.iter().any(|event| event == "frame-enter") {
            break;
        }
        if let Some(text) = socket_line(&mut reader) {
            let command: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            panic!("a hub command went out before the first frame drew: {command:?}");
        }
        if wait.left().is_zero() {
            panic!("waited {DEADLINE:?} for the first frame");
        }
        std::thread::yield_now();
    }
    reader
        .get_mut()
        .set_nonblocking(false)
        .unwrap_or_else(|err| panic!("blocking: {err}"));
    let first = drawn
        .iter()
        .find(|event| event.starts_with("frame\n"))
        .unwrap_or_else(|| panic!("a first frame"));
    assert!(
        first.contains("child reply 29"),
        "the first frame shows the child's last page: {first}"
    );
    // One release: the frame finishes, the ancestor count follows it,
    // and the raise goes last.
    block.store(false, Ordering::SeqCst);
    release_tx
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    let count = socket_line(&mut reader).unwrap_or_else(|| panic!("the ancestor count"));
    let count: serde_json::Value = serde_json::from_str(&count).unwrap_or_default();
    assert_eq!(count["command"], "history");
    assert_eq!(
        count["args"],
        json!({"session_id": ROOT, "to_seq": point}),
        "the ancestor count follows the frame"
    );
    let count_id = count["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a count id"))
        .to_owned();
    let window = doors_window(&ancestor, &count["args"]);
    let mut answer = serde_json::to_value(super::line("command_accepted", None, None, json!({})))
        .unwrap_or_default();
    answer["payload"] = json!({"command_id": count_id, "result": {"lines": window}});
    let answer: contract::Envelope =
        serde_json::from_value(answer).unwrap_or_else(|err| panic!("an answer: {err}"));
    tx.send(Input::Hub(Line::Session(answer)))
        .unwrap_or_else(|err| panic!("answer: {err}"));
    let raise = socket_line(&mut reader).unwrap_or_else(|| panic!("the raise"));
    let raise: serde_json::Value = serde_json::from_str(&raise).unwrap_or_default();
    assert_eq!(raise["command"], "subscribe");
    assert_eq!(
        raise["args"],
        json!({"level": "full"}),
        "the raise follows the count"
    );
    let (lp, out) = worker.join().unwrap_or_else(|_| panic!("the worker"));
    assert_eq!(out, None);
    // The ancestor count is included: the joint total spans the chain,
    // and the ancestor stays paged under its own session.
    let mut joint = crate::window::Pages::new(60);
    for line in ancestor.iter().chain(&child) {
        joint.apply(line);
    }
    assert_eq!(
        lp.app.scroll().1,
        joint.index().total(),
        "the joint total spans the chain"
    );
    let pages = lp.app.pages();
    let total = pages.index().total();
    let top_needs = pages.needs(0, total);
    assert!(
        top_needs.iter().any(|(session, range)| session.0 == ROOT
            && *range == (contract::Seq(0)..=contract::Seq(point))),
        "the ancestor stays paged under its session: {top_needs:?}"
    );
}

/// The history wait's bound in milliseconds, beside the production
/// constant it pins.
const WAIT_MS: u64 = 10_000;

/// The wait's deadline from the fake clock's origin: the clock moves
/// only when the test moves it, so a wait that starts before the first
/// move starts at the origin.
fn wait_deadline(clock: &FakeClock) -> std::time::Instant {
    clock
        .origin()
        .checked_add(super::super::hub::HISTORY_WAIT)
        .expect("10 s past the origin")
}

/// Reads one JSON command the loop wrote to the hub's end, within
/// [`DEADLINE`]: the write precedes the wait, so the line is already
/// buffered once the writer runs.
fn read_command(mut theirs: &UnixStream) -> serde_json::Value {
    let wait = Deadline::after(DEADLINE);
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match theirs.read(&mut byte) {
            Ok(0) => panic!("the hub's end closed before the command arrived"),
            Ok(_) => {
                bytes.extend_from_slice(&byte);
                if byte == *b"\n" {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if wait.left().is_zero() {
                    panic!("waited {DEADLINE:?} for the history command");
                }
                std::thread::yield_now();
            }
            Err(error) => panic!("the hub's read failed: {error}"),
        }
    }
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("decode: {err}"))
}

/// Handles stashed terminal bytes the way the loop does short of the
/// frame: parsed with the real parser, each event applied to the app,
/// and a prompt the app sends dropped on the hung-up stream, as the
/// loop's event handling does with no connection.
fn press(lp: &mut super::super::Loop<ratatui::backend::TestBackend>, bytes: &[u8]) {
    use crate::app::Effect;
    use crate::keys::{Event, Parser};
    let now = lp.clock.now();
    for event in Parser::default().feed(bytes) {
        let effect = match event {
            Event::Stroke(stroke) => lp.app.on_press(stroke, now),
            Event::Key(key) => lp.app.on_key(key, now),
            Event::Edit(edit) => lp.app.on_edit(edit),
            Event::Mouse(_) | Event::Reply(_) | Event::Focus(_) => {
                panic!("a typed key parses to a stroke, key or edit")
            }
        };
        if let Effect::Send(lines) = effect {
            lp.hub.send(&lines, &mut lp.app);
        } else if !matches!(effect, Effect::None) {
            panic!("a typed key sends at most its prompt");
        }
    }
}

/// Runs the wait for command `a_1` on a thread of its own and hands the
/// loop back with it, so a wait that never ends fails on the
/// [`DEADLINE`] below instead of hanging the suite.
fn wait_on(
    mut lp: super::super::Loop<ratatui::backend::TestBackend>,
    rx: mpsc::Receiver<Input>,
    frame_deadline: Option<std::time::Instant>,
) -> mpsc::Receiver<Finished> {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-history-wait".to_owned())
        .spawn(move || {
            let result = lp.hub.answer(
                &rx,
                "a_1",
                &mut lp.app,
                &lp.tick,
                lp.clock.as_ref(),
                frame_deadline,
            );
            done.send((result, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

/// The wait's result, the loop back, and its channel back.
type Finished = (
    Result<super::super::hub::Answer, String>,
    super::super::Loop<ratatui::backend::TestBackend>,
    mpsc::Receiver<Input>,
);

/// A hub that holds the connection and drops the `history` command: once
/// the injected clock passes 10 seconds the wait ends as a lost
/// connection, and a key pressed during the wait is handled after it.
#[test]
fn a_silent_hub_ends_the_history_wait_as_a_lost_connection() {
    use contract::Seq;
    let (mut lp, mut theirs, tx, rx) = opened();
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    let shared: Arc<dyn Clock> = clock.clone();
    // Our reader turns the wait's hang-up into `Disconnected`, the way
    // the loop's reader does.
    let end = lp
        .hub
        .stream()
        .and_then(|stream| stream.try_clone().ok())
        .expect("the loop holds the stream");
    let feed = Feed::new(tx.clone(), lp.hub.credits());
    let (_reader, reader_done) = spawn_reader(end, feed);
    // The ticker runs on the fake clock, and the fetch waits on a
    // thread of its own, handing the loop back.
    lp.tick
        .start(shared, tx.clone())
        .unwrap_or_else(|err| panic!("tick start: {err}"));
    let deadline = wait_deadline(&clock);
    let session = lp.app.session().cloned().expect("attached");
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-silent-hub".to_owned())
        .spawn(move || {
            let result = lp.hub.fetch(
                &rx,
                &session,
                &(Seq(0)..=Seq(5)),
                &mut lp.app,
                &lp.tick,
                lp.clock.as_ref(),
                None,
            );
            done.send((result, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The write precedes the wait: the command is already buffered, and
    // the parked ticker proves the wait armed its deadline.
    let command = read_command(&theirs);
    assert_eq!(command["command"], "history");
    assert!(
        clock.await_parked(deadline, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the wait's deadline"
    );
    // Before the deadline nothing ends: the wait holds past 9.999 s.
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    tx.send(Input::Bytes(b"x".to_vec()))
        .unwrap_or_else(|err| panic!("send: {err}"));
    clock.advance(Duration::from_millis(WAIT_MS - 1));
    assert!(matches!(
        finished.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    clock.advance(Duration::from_millis(1));
    let (result, mut lp, rx) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(super::super::hub::LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the expired wait hangs up");
    // The hang-up ends our reader, whose `Disconnected` reconnects like
    // any other lost connection.
    Deadline::after(DEADLINE)
        .recv(&reader_done)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the reader to end: {err}"));
    // The key may have reached the wait before the clock moved, ending
    // the wait on the tick, or after, ending it on the key with the tick
    // still queued: either way the stash and the loss match, so drain to
    // the disconnect, acking a stray tick. One deadline for the whole
    // drain (docs/testing.md, "Waits and timeouts"): a fresh deadline
    // per receive would keep resetting the wait.
    let drain = Deadline::after(DEADLINE);
    loop {
        match drain.recv(&rx) {
            Ok(Input::Disconnected) => {
                lp.hub.on_disconnected(&mut lp.app);
                break;
            }
            Ok(Input::Tick) => lp.tick.ack(),
            Ok(_) => panic!("the hang-up reads as a disconnect"),
            Err(error) => panic!("waited {DEADLINE:?} for the disconnect: {error}"),
        }
    }
    // The loss reconnects as after a disconnect (docs/tui.md, "History
    // and paging", and "A dropped connection" for the banner and the
    // draft staying).
    assert_eq!(
        lp.app.banner().as_deref(),
        Some("Connection lost · reconnecting (attempt 1)…")
    );
    // The key pressed during the wait waits stashed (docs/tui.md,
    // "History and paging": stashed lines fold in order): every stashed
    // line is that key, however many the wait held — no count, just
    // what the wait held.
    assert!(
        lp.hub
            .stashed()
            .iter()
            .all(|input| matches!(input, Input::Bytes(bytes) if bytes.as_slice() == b"x")),
        "the wait stashed only the key pressed during it"
    );
    let Some(first) = lp.hub.take_batch(&rx) else {
        panic!("the stashed key");
    };
    for input in first {
        if let Input::Bytes(bytes) = input {
            press(&mut lp, &bytes);
        } else {
            panic!("the stash holds only the key");
        }
    }
    assert_eq!(lp.app.draft(), "x");
    // Enter while disconnected sends no prompt on the old connection:
    // the hub's end stays silent, or reads its end when every end but
    // it has closed.
    press(&mut lp, b"\r");
    let mut byte = [0u8; 1];
    match theirs.read(&mut byte) {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(0) => {}
        Ok(_) => panic!("Enter wrote on the hung-up stream"),
        Err(error) => panic!("the hub's read failed: {error}"),
    }
}

/// Starts the loop's ticker on the fake clock: the thread sending its
/// ticks to the wait's channel.
fn start_tick(
    lp: &mut super::super::Loop<ratatui::backend::TestBackend>,
    clock: Arc<FakeClock>,
    tx: &mpsc::Sender<Input>,
) {
    let shared: Arc<dyn Clock> = clock;
    lp.tick
        .start(shared, tx.clone())
        .unwrap_or_else(|err| panic!("tick start: {err}"));
}

/// An answer handled just before the deadline draws: 9.999 s into the
/// wait the ticker stays quiet and the lines return with no hang-up.
#[test]
fn an_answer_just_before_the_deadline_draws() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    start_tick(&mut lp, clock.clone(), &tx);
    let deadline = wait_deadline(&clock);
    let want = vec![replay(1000), replay(1001)];
    let finished = wait_on(lp, rx, None);
    assert!(
        clock.await_parked(deadline, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the wait's deadline"
    );
    clock.advance(Duration::from_millis(WAIT_MS - 1));
    tx.send(Input::Hub(Line::Session(command_answer(
        "a_1",
        want.clone(),
    ))))
    .unwrap_or_else(|err| panic!("send: {err}"));
    let (result, lp, rx) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Ok(super::super::hub::Answer::Lines(want)));
    assert!(lp.hub.has_stream(), "no hang-up before the deadline");
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

/// An answer handled exactly at the deadline ends the wait as lost: the
/// deadline wins, and the matching answer waits stashed.
#[test]
fn an_answer_at_the_deadline_is_stashed_and_lost() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    // The ticker stays idle: its arm only records, and no tick ever
    // arrives, so the answer below is what the wait receives.
    let ticker = lp.tick.ticker();
    let deadline = wait_deadline(&clock);
    let want = vec![replay(1000)];
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-late-answer".to_owned())
        .spawn(move || {
            let mut lp = lp;
            let result = lp
                .hub
                .answer(&rx, "a_1", &mut lp.app, &lp.tick, lp.clock.as_ref(), None);
            done.send((result, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The recorded arm proves the wait started at the origin: nothing
    // moved the clock, so its deadline is the expected one.
    let wait = Deadline::after(DEADLINE);
    while ticker.armed() != Some(deadline) {
        if wait.left().is_zero() {
            panic!("waited {DEADLINE:?} for the wait to arm its deadline");
        }
        std::thread::yield_now();
    }
    clock.advance(super::super::hub::HISTORY_WAIT);
    // The channel is empty, so the receive below unblocks only after
    // this send, which is after the clock moves.
    tx.send(Input::Hub(Line::Session(command_answer("a_1", want))))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let (result, lp) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(super::super::hub::LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the expired wait hangs up");
    // The input at the deadline waits for after the loss (docs/tui.md,
    // "History and paging": stashed lines fold in order): the matching
    // answer is among what the wait held, however many lines that is.
    assert!(
        lp.hub
            .stashed()
            .iter()
            .any(|input| matches!(input, Input::Hub(Line::Session(line)) if answers(line, "a_1"))),
        "the matching answer waits stashed"
    );
}

/// At the deadline the tick ends the wait: a matching answer sent after
/// stays queued, never stashed, and the stream hangs up.
#[test]
fn a_tick_at_the_deadline_ends_the_wait_before_a_late_answer() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    start_tick(&mut lp, clock.clone(), &tx);
    let deadline = wait_deadline(&clock);
    let finished = wait_on(lp, rx, None);
    assert!(
        clock.await_parked(deadline, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the wait's deadline"
    );
    clock.advance(super::super::hub::HISTORY_WAIT);
    let (result, lp, rx) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(super::super::hub::LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the expired wait hangs up");
    // The wait already ended on the tick: the late answer stays queued.
    tx.send(Input::Hub(Line::Session(command_answer(
        "a_1",
        vec![replay(1000)],
    ))))
    .unwrap_or_else(|err| panic!("send: {err}"));
    assert!(matches!(rx.try_recv(), Ok(Input::Hub(_))));
}

/// A frame deadline earlier than the wait's wakes the wait first: its
/// tick is acked, the wait holds, and the answer draws.
#[test]
fn a_frame_tick_during_the_wait_is_acked_and_the_answer_draws() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    start_tick(&mut lp, clock.clone(), &tx);
    let frame = clock
        .origin()
        .checked_add(Duration::from_millis(120))
        .expect("a frame past the origin");
    let want = vec![replay(1000)];
    let finished = wait_on(lp, rx, Some(frame));
    // The wait arms the earlier deadline: the ticker parks at the
    // frame's, not the wait's.
    assert!(
        clock.await_parked(frame, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the frame's deadline"
    );
    let mark = clock.advance_marked(Duration::from_millis(120));
    // The re-arm proves the tick was received and acked before the
    // answer is sent: the wait holds at its own deadline again, so the
    // answer cannot win the race.
    let deadline = wait_deadline(&clock);
    assert!(
        clock.await_parked_since(&mark, Some(deadline), Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to re-park at the wait's deadline after the tick"
    );
    tx.send(Input::Hub(Line::Session(command_answer(
        "a_1",
        want.clone(),
    ))))
    .unwrap_or_else(|err| panic!("send: {err}"));
    let (result, lp, rx) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Ok(super::super::hub::Answer::Lines(want)));
    assert!(lp.hub.has_stream(), "no hang-up before the deadline");
    // Acked means acked: with no tick out, a deadline already past
    // sends at once.
    lp.tick.arm(Some(clock.origin()));
    assert!(matches!(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the next tick: {err}")),
        Input::Tick
    ));
}

/// After a frame tick the wait still ends at its own deadline: the ack
/// re-arms the wait, and silence past 10 s loses the connection.
#[test]
fn after_a_frame_tick_the_wait_still_ends_at_its_deadline() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    start_tick(&mut lp, clock.clone(), &tx);
    let frame = clock
        .origin()
        .checked_add(Duration::from_millis(120))
        .expect("a frame past the origin");
    let finished = wait_on(lp, rx, Some(frame));
    assert!(
        clock.await_parked(frame, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the frame's deadline"
    );
    let mark = clock.advance_marked(Duration::from_millis(120));
    // The ack re-arms the wait: the ticker parks at its deadline again
    // before the clock moves on.
    let deadline = wait_deadline(&clock);
    assert!(
        clock.await_parked_since(&mark, Some(deadline), Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the wait's deadline again"
    );
    clock.advance(
        super::super::hub::HISTORY_WAIT
            .checked_sub(Duration::from_millis(120))
            .expect("within the wait"),
    );
    let (result, lp, _) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(super::super::hub::LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the expired wait hangs up");
}

/// A blocked hub write ends in its configured timeout, not forever: the
/// shared helper with a short limit fails a write on a full stream
/// within [`DEADLINE`]. The write runs on a thread of its own, so a
/// timeout that never comes fails here instead of hanging the suite.
#[test]
fn a_blocked_hub_write_ends_in_its_timeout() {
    let (mut ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    // Fill the stream's buffer with the reader holding its end and
    // reading nothing: every further write blocks.
    ours.set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let chunk = vec![b'x'; 65536];
    loop {
        match ours.write_all(&chunk) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("the fill failed: {error}"),
        }
    }
    // Blocking again: without the timeout below this write never ends.
    ours.set_nonblocking(false)
        .unwrap_or_else(|err| panic!("blocking: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("attach-write-timeout".to_owned())
        .spawn(move || {
            let result = ours
                .set_write_timeout(Some(Duration::from_millis(100)))
                .and_then(|()| crate::link::write_line(&ours, &"x".repeat(65536)));
            done.send(result.map_err(|error| error.kind()))
                .unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // An expired send timeout reads back as a blocked write would: the
    // kernel reports no buffer, never a success.
    match Deadline::after(DEADLINE).recv(&finished) {
        Ok(Err(kind))
            if kind == std::io::ErrorKind::WouldBlock || kind == std::io::ErrorKind::TimedOut => {}
        Ok(Err(kind)) => panic!("the blocked write failed, but not with its timeout: {kind:?}"),
        Ok(Ok(())) => panic!("the blocked write succeeded on a full stream"),
        Err(error) => panic!("waited {DEADLINE:?} for the blocked write to end: {error}"),
    }
    drop(theirs);
}

/// Production adopts the stream with the wait's own bound: `store` sets
/// the shared 10 s timeout.
#[test]
fn store_sets_the_history_waits_write_timeout() {
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let mut hub = Connection::new(super::super::tests::idle_retry());
    hub.store(ours);
    assert!(hub.has_stream(), "the stream is adopted");
    let timeout = hub
        .stream()
        .expect("adopted")
        .write_timeout()
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    assert_eq!(timeout, Some(super::super::hub::HISTORY_WAIT));
}

/// A write-timeout setup that fails closes the stream without adopting
/// it: the peer reads the end, and nothing is held.
#[test]
fn a_failed_write_timeout_setup_closes_without_adopting() {
    let (ours, mut theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    theirs
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut hub = Connection::new(super::super::tests::idle_retry());
    let adopted = hub.store_with(ours, |_| Err(std::io::Error::other("no timeout")));
    assert!(!adopted, "the failed stream is not adopted");
    assert!(!hub.has_stream(), "nothing is held");
    // The failed stream is closed: the peer reads its end at once.
    let mut byte = [0u8; 1];
    match theirs.read(&mut byte) {
        Ok(0) => {}
        Ok(_) => panic!("nothing was written on the failed stream"),
        Err(error) => panic!("the peer's read failed: {error}"),
    }
}

/// A frame deadline later than the wait's leaves the wait's first: the
/// ticker parks at 10 s, and silence past it loses the connection.
#[test]
fn a_later_frame_deadline_leaves_the_wait_deadline_first() {
    let (mut lp, _theirs, tx, rx) = opened();
    let clock = FakeClock::new();
    lp.clock = clock.clone();
    start_tick(&mut lp, clock.clone(), &tx);
    let deadline = wait_deadline(&clock);
    let frame = clock
        .origin()
        .checked_add(Duration::from_secs(30))
        .expect("a frame past the wait");
    let finished = wait_on(lp, rx, Some(frame));
    assert!(
        clock.await_parked(deadline, Deadline::after(DEADLINE)),
        "waited {DEADLINE:?} for the ticker to park at the wait's deadline"
    );
    clock.advance(super::super::hub::HISTORY_WAIT);
    let (result, lp, _) = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history wait to end: {err}"));
    assert_eq!(result, Err(super::super::hub::LOST.to_owned()));
    assert!(!lp.hub.has_stream(), "the expired wait hangs up");
}
