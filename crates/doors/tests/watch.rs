//! `doors::watch` against a real session socket: the fold, live lines,
//! `fiber_exited` and a closed connection (`docs/delegates.md`, "Streams").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, mpsc};
use std::thread;
use support::*;

use contract::emit::Emit;
use contract::events::EventKind;
use contract::events::{Clients, Empty, Event, FiberExited};
use contract::shapes::{Tokens, Usage};
use contract::{Envelope, SCHEMA_VERSION, SessionId};
use doors::{Watched, mint};
use fakes::Deadline;
use log::Log;
use serde_json::Value;

/// Appends one durable step to the session's log.
fn step(log: &Log) {
    log.append(&Event::StepStarted(Empty {}), None, None)
        .unwrap();
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        cost: Some(0.0),
        subscription_cost: 0.0,
    }
}

fn exited() -> FiberExited {
    FiberExited {
        exit_code: 0,
        usage: usage(),
        final_message: None,
        error: None,
        suspended_on: None,
        questions: None,
    }
}

/// The durable `seq`s of `lines`, in order.
fn envelope_seqs(lines: &[Envelope]) -> Vec<u64> {
    lines
        .iter()
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect()
}

#[test]
fn a_watch_folds_then_streams_then_returns_at_fiber_exited() {
    let opened = Opened::open(Vec::new());
    step(&opened.log);
    step(&opened.log);
    let home = opened.home.clone();
    let id = opened.id.clone();
    let log = Arc::clone(&opened.log);
    // The watch's own `clients` line opens the gate, so the live thread
    // reads its watcher only once the attach is already written: the
    // latest a thread scheduled behind the watch could start.
    let (attached_tx, attached_rx) = mpsc::channel::<()>();
    // Live lines go out only once the watch subscribed: its attach writes
    // the `clients` line this watcher waits for. The `clients` line is
    // ephemeral, so the watcher is registered here, before the watch can
    // attach, rather than in the thread. The lines go out either way, so
    // a missed attach fails the test instead of hanging the watch.
    let mut watcher = log.watch();
    let deadline = Deadline::after(MUST_SUCCEED_WITHIN);
    let live = thread::spawn(move || {
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&attached_rx)
            .expect("the watch attached");
        let saw_clients = loop {
            match watcher.recv_timeout(deadline.left()) {
                Some(Ok(Some(line))) if line.kind == "clients" => break true,
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("the watcher failed: {error}"),
                None => break false,
            }
        };
        log.append(&Event::StepStarted(Empty {}), None, None)
            .unwrap();
        log.append(&Event::FiberExited(exited()), None, None)
            .unwrap();
        saw_clients
    });
    let mut seen = Vec::new();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            let mut lines = Vec::new();
            let watched = doors::watch(&home, &id, &mut |line: &Envelope| {
                if line.kind == "clients" {
                    let _sent = attached_tx.send(());
                }
                lines.push(line.clone());
            })
            .expect("the watch held to fiber_exited");
            assert_eq!(watched, Watched::Exited);
            // The fold first, in seq order, then the live lines.
            assert_eq!(envelope_seqs(&lines), vec![0, 1, 2, 3]);
            assert_eq!(
                lines.last().map(|line| line.kind.as_str()),
                Some("fiber_exited")
            );
            seen.clone_from(&lines);
            Ok(())
        })
        .unwrap();
    let saw_clients = live.join().expect("the live lines went out");
    assert!(
        saw_clients,
        "the live watcher missed the `clients` line the watch's attach wrote"
    );
    // Besides the four durable lines, the watch also sees the `clients`
    // line its own attach wrote.
    for line in &seen {
        assert!(
            line.seq.is_some() || line.kind == "clients",
            "unexpected line: {}",
            line.kind
        );
    }
    opened.close();
}

#[test]
fn a_closed_connection_returns_the_last_seq() {
    let opened = Opened::open(Vec::new());
    let home = opened.home.clone();
    let id = opened.id.clone();
    let (saw_tx, saw_rx) = mpsc::channel::<()>();
    let (res_tx, res_rx) = mpsc::channel();
    // The watch runs beside the session: closing the session ends its
    // connection.
    thread::spawn(move || {
        let mut first = true;
        let mut seqs = Vec::new();
        let watched = doors::watch(&home, &id, &mut |line: &Envelope| {
            if let Some(seq) = line.seq {
                if first {
                    first = false;
                    let _sent = saw_tx.send(());
                }
                seqs.push(seq.0);
            }
        })
        .expect("a close is not an error");
        let _sent = res_tx.send((watched, seqs));
    });
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            // Fold or live, the line reaches the watch either way.
            step(&opened.log);
            Deadline::after(DEADLINE)
                .recv(&saw_rx)
                .expect("the watch read the line");
            // An ephemeral line after the last `seq`: the close still
            // reports that seq, not the absence of one.
            opened.log.emit(&Event::Clients(Clients { count: 1 }));
            Ok(())
        })
        .unwrap();
    // The run returned; closing ends the watch's connection.
    opened.close();
    let (watched, seqs) = Deadline::after(DEADLINE)
        .recv(&res_rx)
        .expect("the watch ended at the close");
    assert_eq!(watched, Watched::Closed { last_seq: Some(0) });
    assert_eq!(seqs, vec![0]);
}

#[test]
fn a_refused_connection_is_an_error() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let watched = doors::watch(&home, &id, &mut |_| {
        panic!("no line arrives on a refused connection");
    });
    assert!(watched.is_err(), "a refused connect returns Err");
}

/// A log with two runs in it: steps, an earlier `fiber_exited` at some
/// `seq`, more steps, and a later `fiber_exited`. Returns that earlier
/// `fiber_exited`'s `seq`.
fn two_runs(opened: &Opened) -> u64 {
    step(&opened.log);
    opened
        .log
        .append(&Event::FiberExited(exited()), None, None)
        .unwrap();
    step(&opened.log);
    opened
        .log
        .append(&Event::FiberExited(exited()), None, None)
        .unwrap();
    let text =
        std::fs::read_to_string(opened.dir.join("events.jsonl")).expect("the log reads back");
    text.lines()
        .map(|line| serde_json::from_str::<Envelope>(line).expect("a log line parses"))
        .find_map(|line| {
            (line.kind == "fiber_exited").then(|| line.seq.expect("a durable line carries seq").0)
        })
        .expect("the earlier fiber_exited")
}

#[test]
fn watch_after_skips_every_line_at_or_before_its_boundary() {
    let opened = Opened::open(Vec::new());
    let home = opened.home.clone();
    let id = opened.id.clone();
    let boundary = two_runs(&opened);
    let mut seen = Vec::new();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            // The earlier `fiber_exited` is at the boundary: neither
            // delivered nor ending the watch. Only lines past it arrive,
            // and the later `fiber_exited` ends it.
            let watched = doors::watch_after(&home, &id, Some(boundary), &mut |line: &Envelope| {
                seen.push(line.clone());
            })
            .expect("the watch held to the later fiber_exited");
            assert_eq!(watched, Watched::Exited);
            Ok(())
        })
        .unwrap();
    let durable: Vec<u64> = seen
        .iter()
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect();
    assert!(
        !durable.is_empty() && durable.iter().all(|seq| *seq > boundary),
        "only lines past the boundary arrive: {durable:?}"
    );
    assert_eq!(
        seen.last()
            .map(|line| (line.kind.as_str(), line.seq.map(|seq| seq.0))),
        Some(("fiber_exited", Some(*durable.last().unwrap()))),
        "the later fiber_exited ends the watch"
    );
    opened.close();
}

#[test]
fn watch_after_one_before_the_boundary_returns_at_the_old_fiber_exited() {
    let opened = Opened::open(Vec::new());
    let home = opened.home.clone();
    let id = opened.id.clone();
    let boundary = two_runs(&opened);
    let mut seen = Vec::new();
    opened
        .session
        .run(Vec::new(), Arc::new(|| false), |_inbox| {
            // One before the boundary, the old `fiber_exited` still ends
            // the watch: the `<=` is exact.
            let watched =
                doors::watch_after(&home, &id, Some(boundary - 1), &mut |line: &Envelope| {
                    seen.push(line.clone());
                })
                .expect("the watch held to fiber_exited");
            assert_eq!(watched, Watched::Exited);
            Ok(())
        })
        .unwrap();
    let at: Vec<u64> = seen
        .iter()
        .filter_map(|line| line.seq.map(|seq| seq.0))
        .collect();
    assert_eq!(
        seen.last()
            .map(|line| (line.kind.as_str(), line.seq.map(|seq| seq.0))),
        Some(("fiber_exited", Some(boundary))),
        "the old fiber_exited ends the watch: {at:?}"
    );
    opened.close();
}

/// A full envelope line of `kind` with `payload`, as a session writes it.
fn enveloped(kind: &str, payload: Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "kind": kind,
        "session_id": "s_stand",
        "ts": 0,
        "schema_version": SCHEMA_VERSION,
        "payload": payload,
    }))
    .unwrap()
}

fn write_line(writer: &mut UnixStream, line: &str) {
    writer.write_all(line.as_bytes()).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
}

#[test]
fn a_kind_from_a_newer_build_neither_ends_the_watch_nor_loses_its_name() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let id = SessionId(mint("s_"));
    let socket = run.join(&id.0);
    // Its own listener, not a session: the subscribe is answered, then a
    // line whose kind no released build knows arrives ahead of the real
    // `fiber_exited`. Unknown is never the exit, so the watch holds past
    // it and delivers it with its name intact.
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("watch connected");
        stream
            .set_read_timeout(Some(DEADLINE))
            .expect("a read timeout is set");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let mut writer = stream;
        let mut line = String::new();
        reader.read_line(&mut line).expect("the subscribe arrives");
        let sub: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(sub["command"], "subscribe");
        write_line(
            &mut writer,
            &serde_json::to_string(&serde_json::json!({
                "kind": "command_accepted",
                "session_id": "s_stand",
                "ts": 0,
                "schema_version": SCHEMA_VERSION,
                "payload": {"command_id": sub["id"]},
            }))
            .unwrap(),
        );
        write_line(
            &mut writer,
            &enveloped("kind_from_a_newer_build", serde_json::json!({})),
        );
        write_line(
            &mut writer,
            &enveloped("fiber_exited", serde_json::json!({})),
        );
    });

    let mut seen = Vec::new();
    let watched = doors::watch(&home, &id, &mut |line: &Envelope| {
        seen.push(line.clone());
    })
    .expect("the watch held past the unknown kind to fiber_exited");
    assert_eq!(watched, Watched::Exited);
    assert_eq!(
        seen.iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["kind_from_a_newer_build", "fiber_exited"]
    );
    assert_eq!(
        EventKind::from_name("kind_from_a_newer_build"),
        EventKind::Unknown("kind_from_a_newer_build".to_owned()),
        "the unknown name parses to its own case, keeping its text"
    );
    server.join().expect("the listener served the watch");
}
