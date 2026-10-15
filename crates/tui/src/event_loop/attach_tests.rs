//! The attach bound (#2232): the parsed live hub events alive at once,
//! from the socket read to the fold, stay within K.

use super::super::pause;
use super::{DEADLINE, answers_all, line, opened, quit, serve, session};
use crate::link::read_lines;
use fakes::Deadline;
use serde_json::json;
use std::io::Read;
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
        let lines = (0..LINES)
            .map(|at| {
                let envelope = line(
                    "text_completed",
                    Some(first_seq + at as u64),
                    Some("a_m"),
                    json!({"text": format!("replay {at}")}),
                );
                let mut bytes =
                    serde_json::to_vec(&envelope).unwrap_or_else(|err| panic!("encode: {err}"));
                bytes.push(b'\n');
                bytes
            })
            .collect();
        Self {
            lines,
            at: 0,
            within: 0,
            handouts,
            eof: false,
        }
    }
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

#[test]
fn attach_bounded_reader_stops_at_the_queue_cap() {
    let (tx, rx) = mpsc::channel();
    let (hand_tx, hand_rx) = mpsc::channel();
    let first_seq = session().len() as u64;
    let link = FakeLink::new(hand_tx, first_seq);
    // One deadline for the whole drain to the plateau, made before the
    // reader starts.
    let handouts = Deadline::after(DEADLINE);
    let reader_tx = tx.clone();
    std::thread::Builder::new()
        .name("attach-read".to_owned())
        .spawn(move || read_lines(link, &reader_tx))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    for want in 1..=65usize {
        match handouts.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // The 65th line waits for a unit the loop's first receive releases,
    // so with no consumer no 66th line is parsed: a bounded receive of
    // its own that must time out.
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&hand_rx)
            .is_err(),
        "hand-out 66 arrived with 65 lines unreceived"
    );
    drop(rx);
    drop(tx);
}

#[test]
fn attach_bounded_live_events_stay_within_k_while_the_fold_is_held() {
    let (mut lp, theirs, tx, rx) = opened();
    let _served = serve(theirs, tx.clone(), answers_all());
    let (hand_tx, hand_rx) = mpsc::channel();
    let first_seq = session().len() as u64;
    let link = FakeLink::new(hand_tx, first_seq);
    // The reader runs first, with no consumer.
    let reader_tx = tx.clone();
    std::thread::Builder::new()
        .name("attach-read".to_owned())
        .spawn(move || read_lines(link, &reader_tx))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let handouts = Deadline::after(DEADLINE);
    for want in 1..=65usize {
        match handouts.recv(&hand_rx) {
            Ok(got) => assert_eq!(got, want, "hand-outs arrive in order"),
            Err(err) => panic!("waited {DEADLINE:?} for hand-out {want}: {err}"),
        }
    }
    // Then the loop runs on a worker with the fold held: every wait
    // after this action is a new wait with its own deadline.
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
    // the 130th line waits: a bounded receive of its own that must time
    // out.
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&hand_rx)
            .is_err(),
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
    let eof = Deadline::after(DEADLINE);
    match eof.recv(&hand_rx) {
        Ok(got) => assert_eq!(got, LINES + 1, "the reader reaches EOF"),
        Err(err) => panic!("waited {DEADLINE:?} for the EOF report: {err}"),
    }
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    let returned = Deadline::after(DEADLINE);
    let (code, _) = returned.recv_or_fail(&done, "the loop to quit");
    assert_eq!(code, 0);
}
