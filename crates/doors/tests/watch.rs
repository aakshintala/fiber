//! `doors::watch` against a real session socket: the fold, live lines,
//! `fiber_exited` and a closed connection (`docs/delegates.md`, "Streams").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;
use support::*;

use contract::emit::Emit;
use contract::events::{Clients, Empty, Event, FiberExited};
use contract::shapes::{Tokens, Usage};
use contract::{Envelope, SessionId};
use doors::{Watched, mint};
use fakes::Deadline;
use log::Log;

/// How long the live thread waits for the watch to attach, and then for
/// its watcher to read the `clients` line that attach wrote.
const ATTACH_DEADLINE: Duration = Duration::from_secs(5);

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
    let deadline = Deadline::after(ATTACH_DEADLINE);
    let live = thread::spawn(move || {
        Deadline::after(ATTACH_DEADLINE)
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
