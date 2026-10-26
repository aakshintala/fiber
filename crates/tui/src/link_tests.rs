//! Tests for hub lines out and in.

use super::{Credits, Feed, Line, QUEUE, answers, parse_line, read_lines, write_line};
use crate::Input;
use fakes::Deadline;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn lends_allow_exactly_one_extra_queue_and_restore_the_cap() {
    let credits = Credits::new(QUEUE);
    let epoch = credits.epoch();
    for _ in 0..QUEUE {
        assert!(credits.take(epoch));
    }
    let lend = credits.lend();
    for _ in 0..QUEUE {
        assert!(credits.take(epoch));
    }
    assert!(!credits.take(epoch));
    drop(lend);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = Arc::clone(&credits);
    std::thread::spawn(move || {
        tx.send(worker.take(epoch)).unwrap();
    });
    await_waiter(&credits, "restored cap");
    for _ in 0..=QUEUE {
        credits.received(&Input::Hub(Line::Hub(contract::HubLine {
            kind: "ignored".into(),
            ts: 0,
            schema_version: 1,
            payload: serde_json::Map::new(),
        })));
    }
    assert!(Deadline::start().recv_or_fail(&rx, "take below restored cap"));
}

#[test]
fn lend_wakes_a_reader_parked_at_the_cap() {
    let credits = Credits::new(1);
    let epoch = credits.epoch();
    assert!(credits.take(epoch));
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = Arc::clone(&credits);
    std::thread::spawn(move || {
        tx.send(worker.take(epoch)).unwrap();
    });
    await_waiter(&credits, "park before lend");
    let _lend = credits.lend();
    assert!(Deadline::start().recv_or_fail(&rx, "lend wakes reader"));
}

#[test]
fn routed_feed_installs_the_stream_and_bypasses_line_credits() {
    let deadline = Deadline::start();
    let router = crate::remote::Router::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let feed = Feed::new(tx, Credits::new(0)).routed(Arc::clone(&router));
    let (client, mut hub) = UnixStream::pair().unwrap();
    let reader = client.try_clone().unwrap();
    feed.send(Input::Connected(
        client,
        contract::HubLine {
            kind: "hub_hello".into(),
            ts: 0,
            schema_version: 1,
            payload: serde_json::Map::new(),
        },
    ))
    .unwrap();
    assert!(matches!(
        deadline.recv_or_fail(&rx, "connected"),
        Input::Connected(..)
    ));
    let pending = router
        .send("configure", serde_json::json!({"operation":"settings"}))
        .unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        let mut text = String::new();
        BufReader::new(hub.try_clone().unwrap())
            .read_line(&mut text)
            .unwrap();
        let line: serde_json::Value = serde_json::from_str(&text).unwrap();
        writeln!(hub, "{}", serde_json::json!({"kind":"command_accepted","ts":0,"schema_version":1,"payload":{"command_id":line["id"],"result":{}}})).unwrap();
        done.send(()).unwrap();
    });
    let (result, result_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        result.send(pending.wait()).unwrap();
    });
    std::thread::spawn(move || read_lines(reader, &feed));
    assert!(deadline.recv_or_fail(&result_rx, "routed answer").is_ok());
    deadline.recv_or_fail(&finished, "hub answer written");
    assert!(matches!(
        deadline.recv_or_fail(&rx, "disconnected"),
        Input::Disconnected
    ));
    assert!(matches!(
        router.send("configure", serde_json::json!({})),
        Err(crate::remote::AskError::Detached)
    ));
}

#[test]
fn concurrent_large_writes_are_whole_lines() {
    use std::io::{BufRead, BufReader};
    let deadline = Deadline::start();
    let (client, hub) = UnixStream::pair().unwrap();
    let other = client.try_clone().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let lines: Vec<_> = BufReader::new(hub)
            .lines()
            .take(2)
            .map(Result::unwrap)
            .collect();
        tx.send(lines).unwrap();
    });
    let (done, finished) = std::sync::mpsc::channel();
    for (socket, byte) in [(client, 'a'), (other, 'b')] {
        let done = done.clone();
        std::thread::spawn(move || {
            write_line(&socket, &byte.to_string().repeat(65536)).unwrap();
            done.send(()).unwrap();
        });
    }
    let lines = deadline.recv_or_fail(&rx, "whole large lines");
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert_eq!(line.len(), 65536);
        assert!(line.bytes().all(|byte| byte == line.as_bytes()[0]));
    }
    assert_ne!(lines[0].as_bytes()[0], lines[1].as_bytes()[0]);
    deadline.recv_or_fail(&finished, "first writer");
    deadline.recv_or_fail(&finished, "second writer");
}

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn written_lines_are_exact_json() {
    let (send, mut recv) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let line =
        serde_json::json!({"id": "c_1", "command": "cancel", "session_id": "s_x"}).to_string();
    write_line(&send, &line).unwrap_or_else(|err| panic!("write: {err}"));
    let mut buf = vec![0u8; line.len() + 1];
    // One deadline for the whole read, however many reads it takes.
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-read-exact".to_owned())
        .spawn(move || {
            done.send(recv.read_exact(&mut buf).map(|()| buf))
                .unwrap_or(())
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let buf = Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the line: {err}"))
        .unwrap_or_else(|err| panic!("read: {err}"));
    assert_eq!(buf, format!("{line}\n").into_bytes());
}

#[test]
fn lines_read_split_into_hub_and_session() {
    let (client, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("link-read".to_owned())
        .spawn(move || {
            let feed = Feed::from(tx);
            read_lines(hub, &feed)
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let mut client = client;
    for line in [
        serde_json::json!({"kind": "command_accepted", "ts": 0, "schema_version": 1, "payload": {}}).to_string(),
        serde_json::json!({"kind": "turn_started", "session_id": "s_x", "ts": 0, "schema_version": 1, "payload": {"input": []}}).to_string(),
    ] {
        use std::io::Write;
        client
            .write_all(format!("{line}\n").as_bytes())
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
    let wait = Deadline::after(DEADLINE);
    let first = wait
        .recv(&rx)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the hub line: {err}"));
    let second = wait
        .recv(&rx)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the session line: {err}"));
    assert!(matches!(first, Input::Hub(Line::Hub(_))));
    assert!(matches!(second, Input::Hub(Line::Session(_))));
    drop(client);
    let end = Deadline::after(DEADLINE)
        .recv(&rx)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the disconnect: {err}"));
    assert!(matches!(end, Input::Disconnected));
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

#[test]
fn parse_line_splits_and_refuses_malformed() {
    assert!(matches!(
        parse_line(r#"{"kind":"hub_hello","ts":0,"schema_version":1,"payload":{}}"#),
        Some(Line::Hub(_))
    ));
    assert!(matches!(
        parse_line(
            r#"{"kind":"turn_started","session_id":"s_x","ts":0,"schema_version":1,"payload":{"input":[]}}"#
        ),
        Some(Line::Session(_))
    ));
    assert!(parse_line("not json").is_none());
    assert!(parse_line(r#"{"kind": 1}"#).is_none());
}

#[test]
fn answers_matches_only_its_command() {
    let line = |kind: &str, id: &str| contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_x".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    assert!(answers(&line("command_accepted", "c_1"), "c_1"));
    assert!(answers(&line("command_rejected", "c_1"), "c_1"));
    assert!(!answers(&line("command_accepted", "c_2"), "c_1"));
    assert!(!answers(&line("turn_started", "c_1"), "c_1"));
}

#[test]
fn history_answer_reads_lines_and_rejections() {
    use super::history_answer;
    let line = |kind: &str, payload: serde_json::Value| contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_x".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    };
    // Accepted with lines reads them.
    let lines = vec![
        line("turn_started", serde_json::json!({"input": []})),
        line(
            "turn_completed",
            serde_json::json!({"outcome": "completed"}),
        ),
    ];
    let accepted = line(
        "command_accepted",
        serde_json::json!({"command_id": "c_1", "result": {"lines": lines}}),
    );
    assert_eq!(history_answer(&accepted).expect("lines"), lines);
    // Accepted without readable lines is an error naming it.
    let unreadable = line(
        "command_accepted",
        serde_json::json!({"command_id": "c_1", "result": {}}),
    );
    assert_eq!(
        history_answer(&unreadable).unwrap_err(),
        "the answer could not be read"
    );
    // Rejected with a message carries it, else `rejected`.
    let rejected = line(
        "command_rejected",
        serde_json::json!({"command_id": "c_1", "message": "past the latest line"}),
    );
    assert_eq!(
        history_answer(&rejected).unwrap_err(),
        "past the latest line"
    );
    let bare = line("command_rejected", serde_json::json!({"command_id": "c_1"}));
    assert_eq!(history_answer(&bare).unwrap_err(), "rejected");
}

/// One valid session line with identity `seq`, as the hub relays it.
fn valid(seq: u64) -> String {
    serde_json::json!({"kind": format!("line-{seq}"), "session_id": "s_x", "ts": 0,
        "schema_version": contract::SCHEMA_VERSION, "payload": {}})
    .to_string()
}

/// The identity of a received hub line.
fn identity(input: &Input) -> String {
    match input {
        Input::Hub(Line::Session(line)) => line.kind.clone(),
        Input::Hub(Line::Hub(line)) => line.kind.clone(),
        Input::Bytes(_)
        | Input::Connected(..)
        | Input::ConnectFailed(_)
        | Input::Disconnected
        | Input::Resize
        | Input::Tick
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
        | Input::Login { .. } => panic!("a hub line, not another input"),
    }
}

/// Writes `lines` to the hub's end, one per line.
fn write_lines(client: &mut UnixStream, lines: &[String]) {
    use std::io::Write;
    for line in lines {
        client
            .write_all(format!("{line}\n").as_bytes())
            .unwrap_or_else(|err| panic!("write: {err}"));
    }
}

/// Reads the hub end on a thread through `feed`, signalling its return.
fn read(
    hub: impl Read + Send + 'static,
    feed: Feed,
) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<()>) {
    let (done_tx, done) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("link-read".to_owned())
        .spawn(move || {
            read_lines(hub, &feed);
            done_tx.send(()).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (reader, done)
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
fn lines_below_the_cap_end_with_one_disconnect() {
    for count in [63usize, 64] {
        let (mut client, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
        let (tx, rx) = std::sync::mpsc::channel();
        let feed = Feed::new(tx, Credits::new(QUEUE));
        let (reader, done) = read(hub, feed);
        write_lines(
            &mut client,
            &(0..count as u64).map(valid).collect::<Vec<_>>(),
        );
        drop(client);
        // The reader ends on EOF with nobody receiving: EOF needs no
        // unit.
        Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to reach EOF");
        reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
        // One drain for the whole wait: every line in order, then
        // exactly one disconnect, and nothing after it.
        let drained = Deadline::after(DEADLINE);
        for seq in 0..count as u64 {
            match drained.recv(&rx) {
                Ok(input) => assert_eq!(identity(&input), format!("line-{seq}")),
                Err(err) => panic!("waited {DEADLINE:?} for line {seq} of {count}: {err}"),
            }
        }
        match drained.recv(&rx) {
            Ok(Input::Disconnected) => {}
            Ok(_) => panic!("one disconnect after {count} lines"),
            Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
        }
        assert!(rx.try_recv().is_err(), "nothing follows the disconnect");
    }
}

#[test]
fn the_65th_line_waits_for_a_receive() {
    let (mut client, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (tx, rx) = std::sync::mpsc::channel();
    let credits = Credits::new(QUEUE);
    let feed = Feed::new(tx, Arc::clone(&credits));
    let (reader, done) = read(hub, feed);
    write_lines(&mut client, &(0..65u64).map(valid).collect::<Vec<_>>());
    // One drain for the queued 64, in order.
    let queued = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        match queued.recv(&rx) {
            Ok(input) => assert_eq!(identity(&input), format!("line-{seq}")),
            Err(err) => panic!("waited {DEADLINE:?} for line {seq}: {err}"),
        }
    }
    // The 65th line is parsed and pending: the reader parks at the
    // full-capacity branch, so nothing more arrives, and no disconnect
    // either.
    await_waiter(&credits, "the reader to wait at the queue cap");
    assert!(
        rx.try_recv().is_err(),
        "the 65th line arrived with none received"
    );
    // One receive releases the pending line, then EOF ends the reader.
    // The release is the loop's receive of a hub line, explicit here
    // with no loop running: a new wait after the release above.
    credits.received(&hub_line(0));
    let released = Deadline::after(DEADLINE);
    match released.recv(&rx) {
        Ok(input) => assert_eq!(identity(&input), "line-64"),
        Err(err) => panic!("waited {DEADLINE:?} for line 64: {err}"),
    }
    drop(client);
    match released.recv(&rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect after 65 lines"),
        Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
    }
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    assert!(rx.try_recv().is_err(), "nothing follows the disconnect");
}

#[test]
fn malformed_lines_take_no_unit() {
    let (mut client, hub) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (tx, rx) = std::sync::mpsc::channel();
    let credits = Credits::new(QUEUE);
    let feed = Feed::new(tx, Arc::clone(&credits));
    let (reader, done) = read(hub, feed);
    let mut lines = Vec::new();
    for seq in 0..64u64 {
        lines.push(valid(seq));
        lines.push("not json".to_owned());
    }
    lines.extend((100..136).map(|_| "not json".to_owned()));
    lines.push(valid(64));
    write_lines(&mut client, &lines);
    // All 100 malformed lines are read past while 64 valid lines sit
    // queued: the queue holds exactly the 64 valid lines, in order.
    let queued = Deadline::after(DEADLINE);
    for seq in 0..64u64 {
        match queued.recv(&rx) {
            Ok(input) => assert_eq!(identity(&input), format!("line-{seq}")),
            Err(err) => panic!("waited {DEADLINE:?} for line {seq}: {err}"),
        }
    }
    // The 65th valid line waits at the full-capacity branch: the
    // parked reader sends nothing more.
    await_waiter(&credits, "the reader to wait at the queue cap");
    assert!(rx.try_recv().is_err(), "line 64 arrived with none received");
    // One receive releases it: a new wait after the release.
    credits.received(&hub_line(0));
    let released = Deadline::after(DEADLINE);
    match released.recv(&rx) {
        Ok(input) => assert_eq!(identity(&input), "line-64"),
        Err(err) => panic!("waited {DEADLINE:?} for line 64: {err}"),
    }
    drop(client);
    match released.recv(&rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect"),
        Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
    }
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
}

/// A link yielding two lines, then a read error.
struct FailAfter {
    lines: Vec<Vec<u8>>,
}

impl std::io::Read for FailAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(next) = self.lines.first() else {
            return Err(std::io::Error::other("boom"));
        };
        let end = buf.len().min(next.len());
        buf[..end].copy_from_slice(&next[..end]);
        if end >= next.len() {
            self.lines.remove(0);
        } else {
            self.lines[0] = next[end..].to_vec();
        }
        Ok(end)
    }
}

#[test]
fn a_read_error_after_queued_lines_delivers_then_disconnects() {
    let (tx, rx) = std::sync::mpsc::channel();
    let feed = Feed::new(tx, Credits::new(QUEUE));
    let link = FailAfter {
        lines: [valid(0), valid(1)]
            .iter()
            .map(|line| format!("{line}\n").into_bytes())
            .collect(),
    };
    let (reader, done) = read(link, feed);
    // One drain: the queued lines first, in order, then one disconnect.
    let drained = Deadline::after(DEADLINE);
    for seq in 0..2u64 {
        match drained.recv(&rx) {
            Ok(input) => assert_eq!(identity(&input), format!("line-{seq}")),
            Err(err) => panic!("waited {DEADLINE:?} for line {seq}: {err}"),
        }
    }
    match drained.recv(&rx) {
        Ok(Input::Disconnected) => {}
        Ok(_) => panic!("one disconnect after the error"),
        Err(err) => panic!("waited {DEADLINE:?} for the disconnect: {err}"),
    }
    Deadline::after(DEADLINE).recv_or_fail(&done, "the reader to return");
    reader.join().unwrap_or_else(|err| panic!("join: {err:?}"));
    assert!(rx.try_recv().is_err(), "nothing follows the disconnect");
}

#[test]
fn takes_past_the_cap_wait_for_a_receive() {
    let credits = Credits::new(2);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    assert!(credits.take(lane.epoch));
    assert!(credits.take(lane.epoch));
    // The third take waits: a parked waiter at the full-capacity
    // branch. Each blocking wait has its deadline; the waiter's return
    // has one too.
    let waiter = Arc::clone(&credits);
    let epoch = lane.epoch;
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-take".to_owned())
        .spawn(move || {
            let took = waiter.take(epoch);
            done_tx.send(took).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    await_waiter(&credits, "the third take to wait at a cap of 2");
    assert!(done.try_recv().is_err(), "the third take passed a cap of 2");
    credits.received(&Input::Tick);
    // A tick holds no unit, so the waiter stays parked.
    assert_eq!(credits.waiters(), 1, "a tick releases nothing");
    assert!(done.try_recv().is_err(), "a tick released the waiter");
    credits.received(&hub_line(0));
    assert!(Deadline::after(DEADLINE).recv_or_fail(&done, "the take to return"),);
}

/// One parsed hub line: released when received.
fn hub_line(seq: u64) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: format!("line-{seq}"),
        session_id: contract::SessionId("s_x".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::Map::new(),
    }))
}

#[test]
fn received_ignores_non_hub_and_empty() {
    let credits = Credits::new(1);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    // Nothing held: releases saturate at zero instead of underflowing.
    credits.received(&Input::Tick);
    credits.received(&hub_line(0));
    credits.received(&Input::Bytes(vec![0x03]));
    assert!(credits.take(lane.epoch));
    // One held: a non-hub receive changes nothing, a hub one releases.
    credits.received(&Input::Resize);
    // Still held: the next take parks at the full-capacity branch.
    let waiter = Arc::clone(&credits);
    let epoch = lane.epoch;
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-take".to_owned())
        .spawn(move || {
            let took = waiter.take(epoch);
            done_tx.send(took).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    await_waiter(&credits, "the take to wait at a held unit");
    assert!(done.try_recv().is_err(), "a resize released the held unit");
    credits.received(&hub_line(1));
    assert!(Deadline::after(DEADLINE).recv_or_fail(&done, "the take to return"),);
}

#[test]
fn cancel_ends_every_wait_without_taking() {
    // Before: a take at a moved epoch fails even with room.
    let credits = Credits::new(1);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    credits.cancel();
    assert!(!credits.take(lane.epoch));
    // During: the waiter returns false.
    let credits = Credits::new(1);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    assert!(credits.take(lane.epoch));
    let waiter = Arc::clone(&credits);
    let epoch = lane.epoch;
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-take".to_owned())
        .spawn(move || {
            let took = waiter.take(epoch);
            done_tx.send(took).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    await_waiter(&credits, "the take to wait at a full cap");
    assert!(done.try_recv().is_err(), "the take passed a full cap");
    credits.cancel();
    assert!(!Deadline::after(DEADLINE).recv_or_fail(&done, "the waiter to end"),);
    // After: the old lane stays cancelled, and a lane made after the
    // cancel takes normally. Fresh credits: the takes above still hold
    // their unit.
    assert!(!credits.take(lane.epoch));
    let (after_tx, _after_rx) = std::sync::mpsc::channel();
    let after_credits = Credits::new(1);
    let after_feed = Feed::new(after_tx, Arc::clone(&after_credits));
    after_credits.cancel();
    assert!(!after_credits.take(after_feed.epoch));
    let after = after_feed.lane();
    assert!(after_credits.take(after.epoch));
}

#[test]
fn a_zero_cap_waits_until_cancelled() {
    let credits = Credits::new(0);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    let waiter = Arc::clone(&credits);
    let epoch = lane.epoch;
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-take".to_owned())
        .spawn(move || {
            let took = waiter.take(epoch);
            done_tx.send(took).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    await_waiter(&credits, "the take to wait at a cap of 0");
    assert!(done.try_recv().is_err(), "a take passed a cap of 0");
    credits.cancel();
    assert!(!Deadline::after(DEADLINE).recv_or_fail(&done, "the waiter to end"),);
}

#[test]
fn the_queue_cap_holds_64_then_waits() {
    // The ordinary maximum: 64 takes pass, the 65th waits for a
    // receive — never 65 held. A `>=` mutated to `>` lets the 65th
    // through, and the bounded receive below catches it.
    let credits = Credits::new(QUEUE);
    let lane = Feed::new(std::sync::mpsc::channel().0, Arc::clone(&credits));
    for _ in 0..QUEUE {
        assert!(credits.take(lane.epoch));
    }
    let waiter = Arc::clone(&credits);
    let epoch = lane.epoch;
    let (done_tx, done) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("link-take".to_owned())
        .spawn(move || {
            let took = waiter.take(epoch);
            done_tx.send(took).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    await_waiter(&credits, "the 65th take to wait at the queue cap");
    assert!(
        done.try_recv().is_err(),
        "the 65th take passed the queue cap"
    );
    credits.received(&hub_line(0));
    assert!(Deadline::after(DEADLINE).recv_or_fail(&done, "the take to return"),);
}
