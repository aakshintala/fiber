//! Tests for a connection's relay map: epochs, the entry a finished relay
//! drops, the subscription kept once the session accepts it, and the
//! acknowledgement a replay drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::io::BufRead;
use std::os::unix::net::UnixStream;

use serde_json::{Value, json};

use super::ack::Verdict;
use super::*;
use fakes::Deadline;

#[test]
fn a_stale_relay_never_overwrites_a_reconnects_subscription() {
    fn entry(session: &str, epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: session.to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
        }
    }
    let line = |id: &str, level: &str| {
        json!({"id": id, "command": "subscribe", "args": {"level": level}})
            .as_object()
            .unwrap()
            .clone()
    };
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = Relays::default();
    // The first relay accepts `full`; a failed write drops its entry and
    // a reconnect mints the next epoch.
    let stale = relays.mint();
    relays.entries.push(entry(sid, stale));
    relays.accepted(sid, stale, line("c_1", "full"));
    relays.entries.remove(0);
    let fresh = relays.mint();
    assert_ne!(fresh, stale);
    relays.entries.push(entry(sid, fresh));
    // The replacement accepts `summary` first; the stale relay's buffered
    // `full` acknowledgement arrives after. Forced order, no sleeps.
    relays.accepted(sid, fresh, line("c_2", "summary"));
    assert_eq!(relays.subscription(sid), Some(line("c_2", "summary")));
    relays.accepted(sid, stale, line("c_1", "full"));
    assert_eq!(
        relays.subscription(sid),
        Some(line("c_2", "summary")),
        "the stale acknowledgement keeps the replacement's level"
    );
    assert_eq!(relays.subscribed.len(), 1, "one entry per session");
}

#[test]
fn a_stale_relay_never_drops_a_reconnect_to_the_same_session() {
    fn entry(epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: "s_0123456789abcdef".to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
        }
    }
    let sid = "s_0123456789abcdef";
    let mut relays = Relays::default();
    let stale = relays.mint();
    relays.entries.push(entry(stale));
    // A failed write drops the entry, leaving the map empty; the
    // reconnect mints its epoch after that.
    relays.entries.remove(0);
    let fresh = relays.mint();
    assert_ne!(fresh, stale);
    relays.entries.push(entry(fresh));
    // The stale relay thread finishes after the reconnect.
    relays.finish(sid, stale);
    assert_eq!(relays.entries.len(), 1, "the reconnect's entry stays");
    assert_eq!(relays.entries[0].epoch, fresh);
    relays.finish(sid, fresh);
    assert!(relays.entries.is_empty(), "a relay drops its own entry");
}

#[test]
fn relay_slots_drop_only_their_own_entry() {
    fn entry(session: &str, epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: session.to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
        }
    }
    let entries = [
        entry("s_aaaaaaaaaaaaaaaa", 1),
        entry("s_bbbbbbbbbbbbbbbb", 2),
    ];
    assert_eq!(relay_slot(&entries, "s_aaaaaaaaaaaaaaaa", 1), Some(0));
    assert_eq!(relay_slot(&entries, "s_bbbbbbbbbbbbbbbb", 2), Some(1));
    assert_eq!(relay_slot(&entries, "s_aaaaaaaaaaaaaaaa", 2), None);
    assert_eq!(relay_slot(&entries, "s_bbbbbbbbbbbbbbbb", 1), None);
    assert_eq!(relay_slot(&entries, "s_cccccccccccccccc", 1), None);
    assert_eq!(relay_slot(&[], "s_aaaaaaaaaaaaaaaa", 1), None);
}

#[test]
fn an_accepted_subscribe_replaces_the_kept_one() {
    fn entry(session: &str, epoch: u64) -> Relay {
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: session.to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
        }
    }
    let line = |id: &str, command: &str| {
        json!({"id": id, "command": command, "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone()
    };
    let mut relays = Relays::default();
    let first = relays.mint();
    relays.entries.push(entry("s_aaaaaaaaaaaaaaaa", first));
    relays.accepted("s_aaaaaaaaaaaaaaaa", first, line("c_1", "subscribe"));
    relays.accepted("s_aaaaaaaaaaaaaaaa", first, line("c_2", "subscribe"));
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(line("c_2", "subscribe"))
    );
    assert_eq!(relays.subscribed.len(), 1, "one entry per session");
    // Another session's entry is added or replaced on its own.
    let second = relays.mint();
    relays.entries.push(entry("s_bbbbbbbbbbbbbbbb", second));
    relays.accepted("s_bbbbbbbbbbbbbbbb", second, line("c_3", "subscribe"));
    assert_eq!(
        relays.subscription("s_bbbbbbbbbbbbbbbb"),
        Some(line("c_3", "subscribe"))
    );
    assert_eq!(relays.subscribed.len(), 2);
    relays.accepted("s_bbbbbbbbbbbbbbbb", second, line("c_4", "subscribe"));
    assert_eq!(
        relays.subscription("s_bbbbbbbbbbbbbbbb"),
        Some(line("c_4", "subscribe"))
    );
    assert_eq!(relays.subscribed.len(), 2);
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(line("c_2", "subscribe"))
    );
}

fn entry_with_epoch(session: &str, epoch: u64) -> Relay {
    let (writer, _) = UnixStream::pair().unwrap();
    Relay {
        session: session.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    }
}

#[test]
fn an_accepted_subscribe_keeps_no_credential() {
    let line = |id: &str| {
        json!({"id": id, "command": "subscribe",
            "args": {"level": "full", "credential": "work"}})
        .as_object()
        .unwrap()
        .clone()
    };
    let mut relays = Relays::default();
    let epoch = relays.mint();
    relays
        .entries
        .push(entry_with_epoch("s_aaaaaaaaaaaaaaaa", epoch));
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_1"));
    // Level and id are kept; only the label is stripped.
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(
            json!({"id": "c_1", "command": "subscribe", "args": {"level": "full"}})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    // A later explicit subscribe replaces the kept one, label and all.
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_2"));
    assert_eq!(
        relays
            .subscription("s_aaaaaaaaaaaaaaaa")
            .as_ref()
            .and_then(|kept| kept.get("id"))
            .and_then(Value::as_str),
        Some("c_2")
    );
    assert!(
        relays
            .subscription("s_aaaaaaaaaaaaaaaa")
            .as_ref()
            .and_then(|kept| kept.get("args"))
            .and_then(|args| args.get("credential"))
            .is_none(),
        "the replacement keeps no label either"
    );
}

#[test]
fn an_accepted_prompt_leaves_the_kept_subscribe_alone() {
    let subscribe = json!({"id": "c_1", "command": "subscribe",
        "args": {"level": "full", "credential": "work"}})
    .as_object()
    .unwrap()
    .clone();
    let prompt = json!({"id": "c_2", "command": "prompt",
        "args": {"content": []}})
    .as_object()
    .unwrap()
    .clone();
    let mut relays = Relays::default();
    let epoch = relays.mint();
    relays
        .entries
        .push(entry_with_epoch("s_aaaaaaaaaaaaaaaa", epoch));
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, subscribe);
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, prompt);
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(
            json!({"id": "c_1", "command": "subscribe", "args": {"level": "full"}})
                .as_object()
                .unwrap()
                .clone()
        )
    );
}

#[test]
fn a_transferred_subscribe_keeps_no_credential() {
    let line = json!({"id": "c_1", "command": "subscribe",
        "args": {"level": "full", "credential": "work"}})
    .as_object()
    .unwrap()
    .clone();
    let mut relays = Relays::default();
    assert!(!relays.transfer("s_aaaaaaaaaaaaaaaa", &line));
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(
            json!({"id": "c_1", "command": "subscribe", "args": {"level": "full"}})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    // The caller's line keeps its label: only the stored copy is stripped.
    assert_eq!(line["args"]["credential"], "work");
}

#[test]
fn an_accepted_command_that_is_not_subscribe_changes_nothing() {
    let line = |id: &str, command: &str| {
        json!({"id": id, "command": command, "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone()
    };
    let mut relays = Relays::default();
    let epoch = relays.mint();
    relays.entries.push({
        let (writer, _) = UnixStream::pair().unwrap();
        Relay {
            session: "s_aaaaaaaaaaaaaaaa".to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
        }
    });
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_1", "prompt"));
    assert_eq!(relays.subscription("s_aaaaaaaaaaaaaaaa"), None);
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_2", "subscribe"));
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_3", "prompt"));
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(line("c_2", "subscribe"))
    );
}

#[test]
fn only_an_accepted_full_subscribe_returns_the_waiting_first_prompt() {
    let line = |id: &str, command: &str, level: &str| {
        json!({"id": id, "command": command, "args": {"level": level}})
            .as_object()
            .unwrap()
            .clone()
    };
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = Relays::default();
    let stale = relays.mint();
    let epoch = relays.mint();
    let (writer, _) = UnixStream::pair().unwrap();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    });
    let first = crate::first::First::new(std::sync::Arc::default());
    relays
        .awaiting
        .push((sid.to_owned(), std::sync::Arc::clone(&first)));
    assert!(
        relays
            .accepted(sid, epoch, line("c_1", "subscribe", "summary"))
            .is_none()
    );
    assert!(
        relays
            .accepted(sid, epoch, line("c_2", "prompt", "full"))
            .is_none()
    );
    assert!(
        relays
            .accepted(sid, stale, line("c_3", "subscribe", "full"))
            .is_none(),
        "a stale relay's acknowledgement releases nothing"
    );
    assert_eq!(relays.awaiting.len(), 1, "the entry is kept until full");
    let released = relays.accepted(sid, epoch, line("c_4", "subscribe", "full"));
    assert!(released.is_some_and(|released| std::sync::Arc::ptr_eq(&released, &first)));
    assert!(relays.awaiting.is_empty());
    assert!(
        relays
            .accepted(sid, epoch, line("c_5", "subscribe", "full"))
            .is_none(),
        "a second full subscribe releases nothing"
    );
}

#[test]
fn only_an_acknowledgement_of_the_replayed_id_is_dropped() {
    let line = |kind: &str, id: &str| {
        let mut bytes = serde_json::to_vec(&json!({
            "kind": kind, "ts": 1, "schema_version": 1, "payload": {"command_id": id},
        }))
        .unwrap();
        bytes.push(b'\n');
        bytes
    };
    assert!(acknowledges(&line("command_accepted", "c_hub"), "c_hub"));
    assert!(acknowledges(&line("command_rejected", "c_hub"), "c_hub"));
    assert!(!acknowledges(&line("command_accepted", "c_1"), "c_hub"));
    assert!(!acknowledges(&line("session_status", "c_hub"), "c_hub"));
    assert!(!acknowledges(b"not json\n", "c_hub"));
    assert!(!acknowledges(
        &serde_json::to_vec(&json!({"kind": "command_accepted", "payload": {}})).unwrap(),
        "c_hub"
    ));
}

/// Extracts the acknowledged command's id and verdict for assertions.
fn verdict(line: &[u8]) -> Option<(String, Verdict)> {
    acknowledgement(line).map(|ack| (ack.id, ack.verdict))
}

#[test]
fn an_acknowledgement_reads_the_opening_boundary_only_when_accepted() {
    let line = |kind: &str, payload: Value| {
        serde_json::to_vec(&json!({"kind": kind, "payload": payload})).unwrap()
    };
    for (kind, result, expected) in [
        ("command_accepted", json!({"last_seq": 41}), Some(41)),
        ("command_accepted", json!({}), None),
        ("command_rejected", json!({"last_seq": 41}), None),
    ] {
        let ack =
            acknowledgement(&line(kind, json!({"command_id": "c", "result": result}))).unwrap();
        assert_eq!(ack.last_seq, expected);
    }
}

#[test]
fn an_acknowledgement_names_its_command_and_whether_it_is_closing() {
    let line = |kind: &str, payload: Value| {
        serde_json::to_vec(&json!({"kind": kind, "ts": 1, "payload": payload})).unwrap()
    };
    let closing = json!({"command_id": "c_1", "code": "closing", "message": "m"});
    assert_eq!(
        verdict(&line("command_rejected", closing.clone())),
        Some(("c_1".to_owned(), Verdict::Closing))
    );
    assert_eq!(
        verdict(&line("command_accepted", closing.clone())),
        Some(("c_1".to_owned(), Verdict::Accepted))
    );
    let other = json!({"command_id": "c_1", "code": "busy", "message": "m"});
    assert_eq!(
        verdict(&line("command_rejected", other)),
        Some(("c_1".to_owned(), Verdict::Rejected))
    );
    assert_eq!(
        verdict(&line("command_rejected", json!({"command_id": "c_1"}))),
        Some(("c_1".to_owned(), Verdict::Rejected))
    );
    // No acknowledgement at all.
    assert_eq!(verdict(&line("session_status", closing)), None);
    assert_eq!(
        verdict(&line("command_rejected", json!({"code": "closing"}))),
        None
    );
    assert_eq!(verdict(&line("command_rejected", Value::Null)), None);
    assert_eq!(verdict(b"not json\n"), None);
    assert_eq!(
        verdict(&serde_json::to_vec(&json!({"payload": {"command_id": "c_1"}})).unwrap()),
        None
    );
    // `rewound` names the session an accepted `rewind` starts.
    let rewound = |next: Value| {
        acknowledgement(&line(
            "command_accepted",
            json!({"command_id": "c_rw", "result": {"new_session_id": next}}),
        ))
        .and_then(|ack| ack.rewound)
    };
    assert_eq!(
        rewound(json!("s_bbbbbbbbbbbbbbbb")),
        Some(contract::SessionId("s_bbbbbbbbbbbbbbbb".to_owned())),
        "an accepted acknowledgement naming a minted session"
    );
    // A rejection starts nothing, whatever it names.
    assert_eq!(
        acknowledgement(&line(
            "command_rejected",
            json!({"command_id": "c_rw", "code": "busy", "message": "m", "result": {"new_session_id": "s_bbbbbbbbbbbbbbbb"}}),
        ))
        .and_then(|ack| ack.rewound),
        None,
    );
    // A minted shape only: the wrong length, the wrong case, a non-string
    // and a missing `result` all start nothing.
    for next in [
        json!("s_x"),
        json!(format!("s_{}", "b".repeat(17))),
        json!(format!("s_{}", "B".repeat(16))),
        Value::Null,
        json!({"id": "x"}),
    ] {
        assert_eq!(
            rewound(next.clone()),
            None,
            "an accepted acknowledgement naming {next} starts nothing"
        );
    }
    assert_eq!(
        acknowledgement(&line("command_accepted", json!({"command_id": "c_rw"})))
            .and_then(|ack| ack.rewound),
        None,
        "an accepted acknowledgement with no result starts nothing"
    );
}

/// A hub at `h` under `held`, whose sessions never start.
fn hub(held: &fakes::TempDir) -> crate::connection::Hub {
    let dir = held.path().join("h");
    std::fs::create_dir_all(&dir).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let timed: std::sync::Arc<dyn contract::clock::Clock> = clock;
    crate::testkit::hub(
        &dir,
        std::sync::Arc::new(crate::fake::FakeStarter::hang(&dir)),
        timed,
    )
}

/// Joins a relay thread the test has just ended, failing at the caller's
/// line instead of hanging when the thread does not end (#1877).
#[track_caller]
fn join_within_deadline(thread: std::thread::JoinHandle<()>) {
    let (ended_tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || ended_tx.send(thread.join().is_ok()).unwrap_or(()));
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&ended).ok(),
        Some(true),
        "the relay thread to end after its session closed"
    );
}

fn start_relay_thread(
    session: &'static str,
    epoch: u64,
    reader: UnixStream,
    hub: std::sync::Arc<crate::connection::Hub>,
    writer: std::sync::Arc<std::sync::Mutex<UnixStream>>,
    relays: std::sync::Arc<std::sync::Mutex<Relays>>,
    kept: Kept,
) -> std::thread::JoinHandle<()> {
    let (order, gone) = {
        let held = crate::connection::lock(&relays);
        (held.order.clone(), held.rejoin.closed_flag())
    };
    std::thread::Builder::new()
        .name("hub-relay-test".to_owned())
        .spawn(move || {
            relay(
                RelayThread {
                    reopen: None,
                    epoch,
                    replayed: Replayed::default(),
                    kept,
                    order,
                    gone,
                },
                session,
                reader,
                &hub,
                &writer,
                &relays,
            );
        })
        .unwrap()
}

#[test]
fn an_accepted_prompt_ack_settles_to_nothing() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let command = |id: &str, command: &str| {
        json!({"id": id, "command": command, "args": {}})
            .as_object()
            .unwrap()
            .clone()
    };
    let ack = |id: &str| {
        serde_json::to_vec(&json!({
            "kind": "command_accepted",
            "payload": {"command_id": id},
        }))
        .unwrap()
    };
    // The kept subscription stays the one the session accepted: an
    // accepted prompt is not a level, so settling it changes nothing.
    let mut relays = Relays::default();
    let subscribed = command("c_0", "subscribe");
    relays.subscribed.push((sid.to_owned(), subscribed.clone()));
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_1".to_owned(),
        command("c_1", "prompt"),
        true,
    )]));
    assert!(
        settle(
            acknowledgement(&ack("c_1")).as_ref(),
            &kept,
            &hub,
            sid,
            false
        )
        .is_none()
    );
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
    assert_eq!(
        relays.subscription(sid),
        Some(subscribed),
        "no replacement to replay on a reconnect"
    );
    // The subscribe path still replaces: the positive fact this feature ran.
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_2".to_owned(),
        command("c_2", "subscribe"),
        true,
    )]));
    match settle(
        acknowledgement(&ack("c_2")).as_ref(),
        &kept,
        &hub,
        sid,
        false,
    ) {
        Some(Settled::Subscribed(line)) => assert_eq!(line, command("c_2", "subscribe")),
        settled => panic!("an accepted subscribe replaces, got {}", settled.is_some()),
    }
}

#[test]
fn a_rejected_subscribe_settles_to_nothing() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let subscribe = |id: &str| {
        json!({"id": id, "command": "subscribe", "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone()
    };
    let rejected = |id: &str, code: &str| {
        serde_json::to_vec(&json!({
            "kind": "command_rejected",
            "payload": {"command_id": id, "code": code, "message": "m"},
        }))
        .unwrap()
    };
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_1".to_owned(),
        subscribe("c_1"),
        true,
    )]));
    assert!(
        settle(
            acknowledgement(&rejected("c_1", "invalid_arguments")).as_ref(),
            &kept,
            &hub,
            sid,
            false
        )
        .is_none()
    );
    assert!(kept.lock().unwrap().is_empty(), "the rejection is consumed");
    // `closing` from a session whose log does not end in `fiber_exited` is
    // passed on, not kept and not routed again.
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_2".to_owned(),
        subscribe("c_2"),
        true,
    )]));
    assert!(
        settle(
            acknowledgement(&rejected("c_2", "closing")).as_ref(),
            &kept,
            &hub,
            sid,
            false
        )
        .is_none()
    );
    assert!(kept.lock().unwrap().is_empty(), "the rejection is consumed");
}

#[test]
fn a_relay_without_a_thread_is_recovered_with_its_queue_first() {
    use std::io::{BufRead, BufReader};
    use std::net::Shutdown;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(10);
    const SID: &str = "s_0123456789abcdef";
    for panics in [false, true] {
        let held = fakes::TempDir::new("rp");
        let dir = held.path().join("h");
        std::fs::create_dir_all(&dir).unwrap();
        let workspace = dir.join("w");
        std::fs::create_dir_all(&workspace).unwrap();
        let log_dir = dir.join("projects").join("-p").join("sessions").join(SID);
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("events.jsonl"),
            format!(
                "{{\"kind\":\"session_started\",\"seq\":0,\"session_id\":\"{SID}\",\"payload\":{{\"workspace\":\"{}\"}}}}\n{{}}\n",
                workspace.display()
            ),
        )
        .unwrap();
        let clock = fakes::clock::FakeClock::new();
        let timed: Arc<dyn contract::clock::Clock> = clock;
        let starter = crate::fake::FakeStarter::bind_and_hold(&dir);
        let hub = Arc::new(crate::testkit::hub(&dir, Arc::new(starter.clone()), timed));
        let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
        let epoch = crate::connection::lock(&relays).mint();
        let (writer, _peer) = UnixStream::pair().unwrap();
        writer.shutdown(Shutdown::Both).unwrap_or(());
        let thread = if panics {
            let thread = std::thread::Builder::new()
                .spawn(|| panic!("the old relay thread panicked"))
                .unwrap();
            while !thread.is_finished() {
                std::thread::yield_now();
            }
            Some(thread)
        } else {
            None
        };
        let mut queued = serde_json::Map::new();
        queued.insert("id".to_owned(), Value::String("c_1".to_owned()));
        queued.insert("command".to_owned(), Value::String("reply".to_owned()));
        queued.insert("args".to_owned(), Value::Object(serde_json::Map::new()));
        let kept: Kept = Arc::new(Mutex::new(vec![("c_1".to_owned(), queued, false)]));
        crate::connection::lock(&relays).entries.push(Relay {
            session: SID.to_owned(),
            epoch,
            writer,
            kept,
            replayed: Replayed::default(),
            thread,
            retiring: Some(crate::retire::Retire::Dead),
        });
        let (client_write, client_read) = UnixStream::pair().unwrap();
        client_read.set_read_timeout(Some(DEADLINE)).unwrap();
        let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
        let mut stripped = serde_json::Map::new();
        stripped.insert("id".to_owned(), Value::String("c_2".to_owned()));
        stripped.insert("command".to_owned(), Value::String("reply".to_owned()));
        stripped.insert("args".to_owned(), Value::Object(serde_json::Map::new()));
        // The recovery opens the resumed session, so the route runs on a
        // thread and its completion is received with the deadline.
        let (done, finished) = std::sync::mpsc::channel();
        let routed_relays = Arc::clone(&relays);
        std::thread::spawn(move || {
            route(
                &contract::CommandId("c_2".to_owned()),
                SID,
                stripped,
                &hub,
                &client_writer,
                &routed_relays,
                None,
                false,
            );
            done.send(()).unwrap_or(());
        });
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the route returns before its deadline");
        let mut read = BufReader::new(client_read);
        for expected in ["c_1", "c_2"] {
            let mut text = String::new();
            read.read_line(&mut text)
                .unwrap_or_else(|_| panic!("the resumed session acknowledges {expected}"));
            let line: Value = serde_json::from_str(text.trim_end()).unwrap();
            assert_eq!(line["kind"], "command_accepted", "{line}");
            assert_eq!(line["payload"]["command_id"], expected, "{line}");
        }
        assert_eq!(
            crate::connection::lock(&relays).entries.len(),
            1,
            "the dead entry is gone; only the reconnect's remains"
        );
        let got: Vec<(String, String)> = starter
            .received()
            .iter()
            .map(|text| {
                let line: Value = serde_json::from_str(text).unwrap();
                (
                    line["id"].as_str().unwrap().to_owned(),
                    line["command"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("c_1".into(), "reply".into()),
                ("c_2".into(), "reply".into())
            ],
            "the queue goes first, on one connection"
        );
        assert_eq!(
            starter.received_by_connection().len(),
            1,
            "panics: {panics}"
        );
    }
}

#[test]
fn an_accepted_rewind_settles_to_the_session_it_names() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let next = "s_bbbbbbbbbbbbbbbb";
    let command = json!({"id": "c_rw1", "command": "rewind", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_rw1".to_owned(),
        command,
        true,
    )]));
    let ack = serde_json::to_vec(&json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_rw1", "result": {"new_session_id": next}},
    }))
    .unwrap();
    match settle(acknowledgement(&ack).as_ref(), &kept, &hub, sid, false) {
        Some(Settled::Rewound(started)) => assert_eq!(started.0, next),
        settled => panic!("an accepted rewind starts, got {}", settled.is_some()),
    }
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
}

#[test]
fn a_rewind_without_a_minted_session_settles_to_nothing() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let kept_for = |command: Value| {
        std::sync::Arc::new(std::sync::Mutex::new(vec![(
            "c_rw1".to_owned(),
            command.as_object().unwrap().clone(),
            true,
        )]))
    };
    let rewind = json!({"id": "c_rw1", "command": "rewind", "args": {}});
    // A rejection starts nothing.
    let rejected = serde_json::to_vec(&json!({
        "kind": "command_rejected", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_rw1", "code": "busy", "message": "m"},
    }))
    .unwrap();
    assert!(
        settle(
            acknowledgement(&rejected).as_ref(),
            &kept_for(rewind.clone()),
            &hub,
            sid,
            false
        )
        .is_none(),
        "a rejected rewind starts nothing"
    );
    // An acknowledgement naming no minted session starts nothing.
    for next in [Value::Null, json!("nope"), json!({"id": "x"})] {
        let ack = serde_json::to_vec(&json!({
            "kind": "command_accepted", "ts": 1, "schema_version": 1,
            "payload": {"command_id": "c_rw1", "result": {"new_session_id": next}},
        }))
        .unwrap();
        assert!(
            settle(
                acknowledgement(&ack).as_ref(),
                &kept_for(rewind.clone()),
                &hub,
                sid,
                false
            )
            .is_none(),
            "an acknowledgement naming {next} starts nothing"
        );
    }
    // An accepted rewind with no result starts nothing.
    let bare = serde_json::to_vec(&json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_rw1"},
    }))
    .unwrap();
    assert!(
        settle(
            acknowledgement(&bare).as_ref(),
            &kept_for(rewind),
            &hub,
            sid,
            false
        )
        .is_none(),
        "an acknowledgement with no result starts nothing"
    );
}

#[test]
fn an_accepted_prompt_naming_a_minted_session_starts_nothing() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let prompt = json!({"id": "c_p1", "command": "prompt", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_p1".to_owned(),
        prompt,
        true,
    )]));
    // Only a rewind starts a session, whatever result an acknowledgement carries.
    let ack = serde_json::to_vec(&json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_p1", "result": {"new_session_id": "s_bbbbbbbbbbbbbbbb"}},
    }))
    .unwrap();
    assert!(
        settle(acknowledgement(&ack).as_ref(), &kept, &hub, sid, false).is_none(),
        "an accepted prompt starts nothing, even with a minted-shape session"
    );
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
}

#[test]
fn muted_drops_each_replay_once() {
    let ack = |id: &str| {
        serde_json::to_vec(&json!({
            "kind": "command_accepted", "ts": 1, "schema_version": 1,
            "payload": {"command_id": id},
        }))
        .unwrap()
    };
    let replayed: Replayed = Replayed::default();
    replayed.lock().unwrap().push("c_hub".to_owned());
    assert!(muted(acknowledgement(&ack("c_hub")).as_ref(), &replayed));
    assert!(
        replayed.lock().unwrap().is_empty(),
        "a replayed acknowledgement is consumed"
    );
    assert!(
        !muted(acknowledgement(&ack("c_hub")).as_ref(), &replayed),
        "only once"
    );
    assert!(
        !muted(acknowledgement(&ack("c_1")).as_ref(), &replayed),
        "other ids pass through"
    );
    assert!(
        !muted(acknowledgement(b"not json\n").as_ref(), &replayed),
        "not an acknowledgement"
    );
}

#[test]
fn a_failed_transfer_leaves_a_dead_relay_that_a_route_recovers() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(10);
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rd");
    let dir = held.path().join("h");
    std::fs::create_dir_all(&dir).unwrap();
    let workspace = dir.join("w");
    std::fs::create_dir_all(&workspace).unwrap();
    let log_dir = dir.join("projects").join("-p").join("sessions").join(SID);
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(
        log_dir.join("events.jsonl"),
        format!(
            "{{\"kind\":\"session_started\",\"seq\":0,\"session_id\":\"{SID}\",\"payload\":{{\"workspace\":\"{}\"}}}}\n{{}}\n",
            workspace.display()
        ),
    )
    .unwrap();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let starter = crate::fake::FakeStarter::bind_and_hold(&dir);
    let hub = Arc::new(crate::testkit::hub(&dir, Arc::new(starter.clone()), timed));
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let epoch = crate::connection::lock(&relays).mint();
    let (writer, peer) = UnixStream::pair().unwrap();
    writer.shutdown(std::net::Shutdown::Both).unwrap_or(());
    drop(peer);
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    });
    assert!(crate::connection::lock(&relays).transfer(SID, &line));
    {
        let held = crate::connection::lock(&relays);
        assert_eq!(held.entries.len(), 1, "the dead relay stays");
        assert!(
            held.entries[0].retiring.is_some(),
            "the failed write retires it"
        );
        assert!(held.entries[0].thread.is_none(), "no thread was taken");
        assert_eq!(held.subscription(SID), Some(line), "the level is kept");
    }
    // A route for the session recovers the dead relay: the kept level is
    // replayed before the command on the resumed session.
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
    let mut stripped = serde_json::Map::new();
    stripped.insert("id".to_owned(), Value::String("c_1".to_owned()));
    stripped.insert("command".to_owned(), Value::String("reply".to_owned()));
    stripped.insert("args".to_owned(), Value::Object(serde_json::Map::new()));
    route(
        &contract::CommandId("c_1".to_owned()),
        SID,
        stripped,
        &hub,
        &client_writer,
        &relays,
        None,
        false,
    );
    let mut read = BufReader::new(client_read);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the resumed full stream reopens");
    let reopened: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(reopened["kind"], "stream_reopened");
    assert_eq!(reopened["payload"]["session_id"], SID);
    assert!(reopened["payload"].get("last_seq").is_none());
    assert!(reopened.get("seq").is_none());
    text.clear();
    read.read_line(&mut text)
        .expect("the resumed session acknowledges the command");
    let ack: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_1", "{ack}");
    let got: Vec<(String, String)> = starter
        .received()
        .iter()
        .map(|text| {
            let line: Value = serde_json::from_str(text).unwrap();
            (
                line["id"].as_str().unwrap().to_owned(),
                line["command"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].1, "subscribe", "the replay goes first");
    assert!(got[0].0.starts_with("c_"), "{got:?}");
    assert_ne!(got[0].0, "c_1", "the replay carries an id of the hub's own");
    assert_eq!(got[1], ("c_1".into(), "reply".into()));
}

#[test]
fn a_recovered_command_never_queues_behind_an_older_relay() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(10);
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rc");
    let dir = held.path().join("h");
    std::fs::create_dir_all(&dir).unwrap();
    let workspace = dir.join("w");
    std::fs::create_dir_all(&workspace).unwrap();
    let log_dir = dir.join("projects").join("-p").join("sessions").join(SID);
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(
        log_dir.join("events.jsonl"),
        format!(
            "{{\"kind\":\"session_started\",\"seq\":0,\"session_id\":\"{SID}\",\"payload\":{{\"workspace\":\"{}\"}}}}\n{{}}\n",
            workspace.display()
        ),
    )
    .unwrap();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let starter = crate::fake::FakeStarter::bind_and_hold(&dir);
    let hub = Arc::new(crate::testkit::hub(&dir, Arc::new(starter.clone()), timed));
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let command = |id: &str, command: &str| {
        let mut stripped = serde_json::Map::new();
        stripped.insert("id".to_owned(), Value::String(id.to_owned()));
        stripped.insert("command".to_owned(), Value::String(command.to_owned()));
        stripped.insert("args".to_owned(), Value::Object(serde_json::Map::new()));
        stripped
    };
    // A retiring relay holding a queued command, with a live thread.
    let retired = crate::connection::lock(&relays).mint();
    let (park_tx, park_rx) = std::sync::mpsc::channel::<()>();
    let parked = std::thread::Builder::new()
        .name("parked-relay".to_owned())
        .spawn(move || {
            // Parked until the test drops its sender.
            let _released = park_rx.recv();
        })
        .unwrap();
    let (retired_write, _) = UnixStream::pair().unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch: retired,
        writer: retired_write,
        kept: Arc::new(Mutex::new(vec![(
            "c_4".to_owned(),
            command("c_4", "reply"),
            false,
        )])),
        replayed: Replayed::default(),
        thread: Some(parked),
        retiring: Some(crate::retire::Retire::Exited),
    });
    // A newer dead relay holding an unsent command, with no thread.
    let dead = crate::connection::lock(&relays).mint();
    assert!(dead > retired);
    let (dead_write, dead_peer) = UnixStream::pair().unwrap();
    dead_write.shutdown(std::net::Shutdown::Both).unwrap_or(());
    drop(dead_peer);
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch: dead,
        writer: dead_write,
        kept: Arc::new(Mutex::new(vec![(
            "c_2".to_owned(),
            command("c_2", "reply"),
            false,
        )])),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Dead),
    });
    // A pass-on from the retiring relay routes with its bound: the dead
    // relay recovers its queue with that bound, never from the top, so
    // nothing queues behind the older relay's command.
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
    let (done, finished) = std::sync::mpsc::channel();
    let routed = Arc::clone(&relays);
    let passing = command("c_3", "steer");
    std::thread::spawn(move || {
        route(
            &contract::CommandId("c_3".to_owned()),
            SID,
            passing,
            &hub,
            &client_writer,
            &routed,
            Some(retired),
            false,
        );
        done.send(()).unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the route returns before its deadline");
    let mut read = BufReader::new(client_read);
    for expected in ["c_2", "c_3"] {
        let mut text = String::new();
        read.read_line(&mut text)
            .unwrap_or_else(|_| panic!("the resumed session acknowledges {expected}"));
        let line: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(line["kind"], "command_accepted", "{line}");
        assert_eq!(line["payload"]["command_id"], expected, "{line}");
    }
    let got: Vec<(String, String)> = starter
        .received()
        .iter()
        .map(|text| {
            let line: Value = serde_json::from_str(text).unwrap();
            (
                line["id"].as_str().unwrap().to_owned(),
                line["command"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("c_2".into(), "reply".into()),
            ("c_3".into(), "steer".into())
        ],
        "the recovered queue goes first, on one connection"
    );
    assert_eq!(starter.received_by_connection().len(), 1);
    let held = crate::connection::lock(&relays);
    assert_eq!(
        held.entries.len(),
        2,
        "the older relay still holds its queue"
    );
    let older = held
        .entries
        .iter()
        .find(|entry| entry.epoch == retired)
        .unwrap();
    let queued: Vec<String> = crate::connection::lock(&older.kept)
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect();
    assert_eq!(queued, vec!["c_4".to_owned()]);
    drop(park_tx);
}

#[test]
fn a_failed_transfer_unmutes_only_its_own_minted_id() {
    let mut relays = Relays::default();
    let sid = "s_aaaaaaaaaaaaaaaa";
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let epoch = relays.mint();
    let (writer, peer) = UnixStream::pair().unwrap();
    writer.shutdown(std::net::Shutdown::Both).unwrap_or(());
    drop(peer);
    // A mute from an earlier replay is still waiting for its acknowledgement.
    let replayed: Replayed = Replayed::default();
    replayed.lock().unwrap().push("c_unrelated".to_owned());
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: std::sync::Arc::clone(&replayed),
        thread: None,
        retiring: None,
    });
    assert!(relays.transfer(sid, &line));
    assert_eq!(
        *replayed.lock().unwrap(),
        vec!["c_unrelated".to_owned()],
        "the failed write's minted mute is removed and the other stays"
    );
    assert_eq!(relays.entries.len(), 1, "the dead relay stays");
}

#[test]
fn an_exited_relay_never_keeps_an_accepted_level() {
    let line = |id: &str| {
        json!({"id": id, "command": "subscribe", "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone()
    };
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = Relays::default();
    let epoch = relays.mint();
    let (writer, _) = UnixStream::pair().unwrap();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
    });
    assert!(relays.accepted(sid, epoch, line("c_1")).is_none());
    assert_eq!(
        relays.subscription(sid),
        None,
        "an exited relay keeps no level"
    );
    // A relay a failed write retired still keeps, as the joined path did.
    let epoch = relays.mint();
    let (writer, _) = UnixStream::pair().unwrap();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Dead),
    });
    assert!(relays.accepted(sid, epoch, line("c_2")).is_none());
    assert_eq!(
        relays.subscription(sid),
        Some(line("c_2")),
        "a dead relay keeps the level"
    );
}

#[test]
fn a_transfer_with_only_retiring_relays_keeps_the_level_and_opens_nothing() {
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = Relays::default();
    let epoch = relays.mint();
    let (writer, _peer) = UnixStream::pair().unwrap();
    let replayed = Replayed::default();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: std::sync::Arc::clone(&replayed),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
    });
    assert!(
        relays.transfer(sid, &line),
        "nothing more is needed: the next open replays the level"
    );
    assert_eq!(relays.subscription(sid), Some(line));
    assert_eq!(relays.entries.len(), 1, "no relay is opened");
    assert!(
        replayed.lock().unwrap().is_empty(),
        "nothing is written to the retiring relay"
    );
}

#[test]
fn a_transfer_lands_on_the_live_relay() {
    use std::io::{BufRead, BufReader};
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let sid = "s_aaaaaaaaaaaaaaaa";
    let mut relays = Relays::default();
    // An older retiring relay never takes the transfer.
    let stale = relays.mint();
    let (stale_write, _) = UnixStream::pair().unwrap();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch: stale,
        writer: stale_write,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
    });
    let live = relays.mint();
    let (writer, peer) = UnixStream::pair().unwrap();
    let replayed: Replayed = Replayed::default();
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch: live,
        writer,
        kept: Kept::default(),
        replayed: std::sync::Arc::clone(&replayed),
        thread: None,
        retiring: None,
    });
    assert!(relays.transfer(sid, &line));
    assert_eq!(relays.subscription(sid), Some(line));
    let mut read = BufReader::new(peer);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the transfer writes the level");
    let sent: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(sent["command"], "subscribe");
    assert_eq!(sent["args"], json!({"level": "full"}));
    let id = sent["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("c_"), "{sent}");
    assert_ne!(id, "c_sub1", "the transfer carries an id of the hub's own");
    assert_eq!(*replayed.lock().unwrap(), vec![id]);
}

/// Takes the test's release with the deadline, failing at the caller's
/// line: the fake session below runs on its own thread, which
/// `#[track_caller]` cannot cross.
#[track_caller]
fn await_release(reply: &std::sync::mpsc::Receiver<()>, wait: &Deadline) {
    wait.recv(reply).expect("the test releases the fake reply");
}

#[test]
fn a_transfer_registers_before_the_session_can_answer() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc;
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(10);
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rt");
    let hub = std::sync::Arc::new(hub(&held));
    // The relay thread reads the session end; the fake bridges the entry
    // writer's end back to it. The reply stays gated until transfer pauses
    // after its write and before the old registration point.
    let (relay_end, fake_write_end) = UnixStream::pair().unwrap();
    let (entry_end, fake_read_end) = UnixStream::pair().unwrap();
    fake_read_end.set_read_timeout(Some(DEADLINE)).unwrap();
    let relays: std::sync::Arc<std::sync::Mutex<Relays>> =
        std::sync::Arc::new(std::sync::Mutex::new(Relays::default()));
    let (written_tx, written_rx) = mpsc::channel();
    let (release_transfer_tx, release_transfer_rx) = mpsc::channel();
    crate::connection::lock(&relays).after_transfer_write = Some(Box::new(move || {
        written_tx.send(()).unwrap_or(());
        Deadline::start()
            .recv_or_fail(&release_transfer_rx, "the test releases the transfer pause");
    }));
    let (filter_tx, filter_rx) = mpsc::channel();
    let (release_filter_tx, release_filter_rx) = mpsc::channel();
    *crate::connection::lock(&hub.after_replay_filter) = Some(Box::new(move |line, muted| {
        filter_tx
            .send((acknowledgement(line).is_some(), muted))
            .unwrap_or(());
        Deadline::start().recv_or_fail(&release_filter_rx, "the test releases the relay pause");
    }));
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client_writer = std::sync::Arc::new(std::sync::Mutex::new(client_write));
    let replayed: Replayed = Replayed::default();
    let kept: Kept = Kept::default();
    let epoch = crate::connection::lock(&relays).mint();
    let thread = std::thread::Builder::new()
        .name("hub-relay".to_owned())
        .spawn({
            let hub = std::sync::Arc::clone(&hub);
            let writer = std::sync::Arc::clone(&client_writer);
            let relays = std::sync::Arc::clone(&relays);
            let replayed = std::sync::Arc::clone(&replayed);
            let kept = std::sync::Arc::clone(&kept);
            let (order, gone) = {
                let held = crate::connection::lock(&relays);
                (held.order.clone(), held.rejoin.closed_flag())
            };
            move || {
                relay(
                    RelayThread {
                        reopen: None,
                        epoch,
                        replayed,
                        kept,
                        order,
                        gone,
                    },
                    SID,
                    relay_end,
                    &hub,
                    &writer,
                    &relays,
                )
            }
        })
        .unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch,
        writer: entry_end,
        kept,
        replayed,
        thread: None,
        retiring: None,
    });
    let (reply_tx, reply_rx) = mpsc::channel();
    let fake = std::thread::Builder::new()
        .name("gated-session".to_owned())
        .spawn(move || {
            let mut read = BufReader::new(fake_read_end);
            let mut buf = String::new();
            if read.read_line(&mut buf).unwrap() == 0 {
                return;
            }
            let command: Value = serde_json::from_str(buf.trim_end()).unwrap();
            let id = command.get("id").cloned().unwrap();
            await_release(&reply_rx, &Deadline::after(DEADLINE));
            let mut ack = serde_json::to_vec(&json!({
                "kind": "command_accepted", "ts": 1, "schema_version": 1,
                "payload": {"command_id": id},
            }))
            .unwrap();
            ack.push(b'\n');
            let mut write = fake_write_end.try_clone().unwrap();
            write.write_all(&ack).unwrap();
            write.flush().unwrap();
            // A live line behind the acknowledgement: the client must read
            // this first, never the hub-only acknowledgement.
            let status =
                b"{\"kind\":\"session_status\",\"ts\":1,\"schema_version\":1,\"payload\":{}}\n";
            write.write_all(status).unwrap();
            write.flush().unwrap();
            // Hold the session open until the entry goes away.
            buf.clear();
            match read.read_line(&mut buf) {
                Ok(_) | Err(_) => {}
            }
        })
        .unwrap();
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let transfer = std::thread::Builder::new()
        .name("relay-transfer".to_owned())
        .spawn({
            let relays = std::sync::Arc::clone(&relays);
            move || crate::connection::lock(&relays).transfer(SID, &line)
        })
        .unwrap();

    let write_reached = Deadline::after(DEADLINE).recv(&written_rx).is_ok();
    reply_tx.send(()).unwrap_or(());
    let filter_result = if write_reached {
        Deadline::after(DEADLINE).recv(&filter_rx).ok()
    } else {
        None
    };
    // The relay has made its filter decision while transfer is still paused.
    // This is the bad interleaving if registration follows the write.
    release_transfer_tx.send(()).unwrap_or(());
    let transferred = transfer.join().unwrap();
    release_filter_tx.send(()).unwrap_or(());
    let mut read = BufReader::new(client_read);
    let mut first = String::new();
    let first_read = read.read_line(&mut first);
    let first_kind = first_read.ok().and_then(|_| {
        serde_json::from_str::<Value>(first.trim_end())
            .ok()
            .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
    });

    // Tear down before asserting, so a failed interleaving still joins both
    // session-side threads.
    crate::connection::lock(&relays).entries.clear();
    fake.join().unwrap();
    thread.join().unwrap();

    assert!(write_reached, "transfer reached the post-write pause");
    assert_eq!(
        filter_result,
        Some((true, true)),
        "the acknowledgement is filtered before transfer resumes"
    );
    assert!(transferred, "the transfer completed");
    assert_eq!(
        first_kind.as_deref(),
        Some("session_status"),
        "the transferred acknowledgement never reaches the client"
    );
}

#[test]
fn the_acknowledgement_queue_orders_a_retiring_relays_handovers() {
    use std::sync::{Arc, mpsc};
    const SID: &str = "s_0123456789abcdef";
    const OTHER: &str = "s_aaaaaaaaaaaaaaaa";
    const EPOCH: u64 = 7;
    const NEWER: u64 = 8;
    let stripped = |command: &str| {
        let mut stripped = serde_json::Map::new();
        stripped.insert("id".to_owned(), Value::String("x".to_owned()));
        stripped.insert("command".to_owned(), Value::String(command.to_owned()));
        stripped
    };
    let id = |id: &str| contract::CommandId(id.to_owned());
    // A command the session answers when it ends is never queued and never
    // recorded: it holds back nothing behind it.
    assert!(crate::retire::AckOrder::is_shell_ended(&stripped("shell")));
    assert!(!crate::retire::AckOrder::is_shell_ended(&stripped("reply")));
    assert!(!crate::retire::AckOrder::is_shell_ended(
        &serde_json::Map::new()
    ));
    let order = Arc::new(crate::retire::AckOrder::default());
    crate::retire::enqueue_new(&order, SID, &id("c_shell"), &stripped("shell"));
    assert_eq!(order.session_for("c_shell"), None);
    crate::retire::enqueue_new(&order, SID, &id("c_1"), &stripped("reply"));
    assert_eq!(order.session_for("c_1").as_deref(), Some(SID));
    // Read order, and a second enqueue of the same command changes nothing.
    order.enqueue(SID, "c_2");
    order.enqueue(SID, "c_1");
    order.enqueue(OTHER, "c_other");
    assert_eq!(order.session_for("c_2").as_deref(), Some(SID));
    // Nothing re-routed: every wait returns at once, on any epoch, even
    // for a command never queued.
    order.wait_rerouted(SID, EPOCH, "c_2");
    order.wait_rerouted(SID, NEWER, "c_2");
    order.wait_rerouted(SID, EPOCH, "c_missing");
    // A queued command that never reached a session is not recorded, so
    // waiting for it cannot hold back passing it on: only a handover waits.
    order.wait_rerouted(SID, EPOCH, "c_3");
    // The handover waits until the re-routed answer is forwarded.
    order.mark_rerouted("c_1", SID, EPOCH);
    let (done_tx, done_rx) = mpsc::channel();
    let waiting = Arc::clone(&order);
    std::thread::Builder::new()
        .name("ack-order-wait".to_owned())
        .spawn(move || {
            waiting.wait_rerouted(SID, EPOCH, "c_2");
            done_tx.send(()).unwrap_or(());
        })
        .unwrap();
    order.done(SID, "c_1");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done_rx)
        .expect("the handover follows the re-routed answer");
    // One entry, not two: the second enqueue changed nothing.
    assert_eq!(order.session_for("c_1"), None);
    order.wait_rerouted(SID, EPOCH, "c_2");
    // An empty drop changes nothing.
    order.drop_ids(&[]);
    order.wait_rerouted(SID, EPOCH, "c_2");
    // A dropped handover holds back nothing behind it.
    order.mark_rerouted("c_2", SID, EPOCH);
    order.enqueue(SID, "c_3");
    order.drop_ids(&[(SID.to_owned(), "c_2".to_owned())]);
    order.wait_rerouted(SID, EPOCH, "c_3");
    order.done(SID, "c_3");
    order.done(OTHER, "c_other");
    assert_eq!(order.session_for("c_2"), None);
    assert_eq!(order.session_for("c_3"), None);
    assert_eq!(order.session_for("c_other"), None);
}

#[test]
fn refusing_a_missing_relay_writes_the_rejection_and_releases_its_acknowledgement() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";

    let held = fakes::TempDir::new("rn");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, "c_missing");
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let (session, gone) = UnixStream::pair().unwrap();
    drop(gone);
    let replay = json!({"id": "c_sub", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let command = json!({"id": "c_missing", "command": "reply", "args": {}})
        .as_object()
        .unwrap()
        .clone();

    attach(
        SID,
        session,
        &hub,
        &writer,
        &relays,
        Some(replay),
        Some((
            contract::CommandId("c_missing".to_owned()),
            Vec::new(),
            command,
        )),
        false,
    );

    let mut read = BufReader::new(peer);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the missing relay is refused within the deadline");
    let answer: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["code"], "session_not_found");
    assert_eq!(answer["payload"]["command_id"], "c_missing");
    assert_eq!(order.session_for("c_missing"), None);
}

#[test]
fn acknowledgement_wait_ignores_other_sessions_epochs_and_itself() {
    use std::sync::{Arc, mpsc};
    const SID: &str = "s_0123456789abcdef";
    const OTHER: &str = "s_aaaaaaaaaaaaaaaa";
    const EPOCH: u64 = 7;
    const NEWER: u64 = 8;

    /// Receives with the deadline, failing at the caller's line.
    #[track_caller]
    fn returns_without_waiting(
        held_session: &str,
        held_epoch: u64,
        held_id: &str,
        session: &str,
        epoch: u64,
        id: &str,
    ) {
        let order = Arc::new(crate::retire::AckOrder::default());
        order.mark_rerouted(held_id, held_session, held_epoch);
        let waiting = Arc::clone(&order);
        let session = session.to_owned();
        let id = id.to_owned();
        let (returned_tx, returned_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ack-order-boundary".to_owned())
            .spawn(move || {
                waiting.wait_rerouted(&session, epoch, &id);
                returned_tx.send(()).unwrap_or(());
            })
            .unwrap();
        let timely = Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&returned_rx)
            .is_ok();
        order.done(held_session, held_id);
        if !timely {
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&returned_rx)
                .expect("releasing the handover wakes the bounded waiter");
        }
        thread.join().unwrap();
        assert!(timely, "an unrelated handover must not hold this wait");
    }

    returns_without_waiting(SID, EPOCH, "c_self", SID, EPOCH, "c_self");
    returns_without_waiting(OTHER, EPOCH, "c_other", SID, EPOCH, "c_wait");
    returns_without_waiting(SID, NEWER, "c_newer", SID, EPOCH, "c_wait");
}

#[test]
fn forwarding_does_not_wait_for_a_different_epoch_to_retire() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex, mpsc};
    const SID: &str = "s_0123456789abcdef";
    const EPOCH: u64 = 7;
    const NEWER: u64 = 8;

    let held = fakes::TempDir::new("rf");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let (other_writer, _other_peer) = UnixStream::pair().unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch: NEWER,
        writer: other_writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
    });
    let (client, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let client = Arc::new(Mutex::new(client));
    let order = crate::connection::lock(&relays).order.clone();
    let kept: Kept = Arc::new(Mutex::new(vec![(
        "c_later".to_owned(),
        json!({"id": "c_later", "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        true,
    )]));
    order.enqueue(SID, "c_later");
    order.mark_rerouted("c_earlier", SID, EPOCH);
    let (noticed_tx, noticed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.before_forward) = Some(Box::new(move |_, _| {
        noticed_tx.send(()).unwrap_or(());
    }));
    let (session, mut session_peer) = UnixStream::pair().unwrap();
    let thread = start_relay_thread(
        SID,
        EPOCH,
        session,
        Arc::clone(&hub),
        Arc::clone(&client),
        Arc::clone(&relays),
        kept,
    );
    session_peer
        .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_later\"}}\n")
        .unwrap();
    session_peer.flush().unwrap();
    let timely = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&noticed_rx)
        .is_ok();
    if !timely {
        order.done(SID, "c_earlier");
    }
    let mut read = BufReader::new(peer);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the later acknowledgement is forwarded");
    // Close, never shut down: on macOS a shutdown can leave the blocked read
    // waiting (#1877).
    drop(session_peer);
    join_within_deadline(thread);
    assert!(timely, "a different epoch must not hold back this forward");
}

#[test]
fn retiring_relays_pass_queues_only_for_their_session_and_epoch() {
    use std::io::Write;
    use std::sync::{Arc, Mutex, mpsc};
    const SID: &str = "s_0123456789abcdef";
    const EPOCH: u64 = 7;

    /// Runs one pass-on case: the positive case receives the pop, the
    /// negative case proves the check ran through a probe forward.
    #[track_caller]
    fn run_case(entry_session: &str, entry_epoch: u64, expect_pop: bool) {
        let held = fakes::TempDir::new("ri");
        let hub = Arc::new(hub(&held));
        let relays = Arc::new(Mutex::new(Relays::default()));
        let mut entries = vec![
            (
                "c_ack".to_owned(),
                json!({"id": "c_ack", "command": "reply", "args": {}})
                    .as_object()
                    .unwrap()
                    .clone(),
                true,
            ),
            (
                "c_queued".to_owned(),
                json!({"id": "c_queued", "command": "reply", "args": {}})
                    .as_object()
                    .unwrap()
                    .clone(),
                false,
            ),
        ];
        if !expect_pop {
            // A sent probe ahead of the unsent queue, so the queue never
            // holds the probe behind it: its forward proves the relay
            // ran the pass-on check after the earlier forward.
            entries.insert(
                1,
                (
                    "c_probe".to_owned(),
                    json!({"id": "c_probe", "command": "reply", "args": {}})
                        .as_object()
                        .unwrap()
                        .clone(),
                    true,
                ),
            );
        }
        let kept: Kept = Arc::new(Mutex::new(entries));
        let (entry_writer, _entry_peer) = UnixStream::pair().unwrap();
        crate::connection::lock(&relays).entries.push(Relay {
            session: entry_session.to_owned(),
            epoch: entry_epoch,
            writer: entry_writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: Some(crate::retire::Retire::Exited),
        });
        let order = crate::connection::lock(&relays).order.clone();
        order.enqueue(SID, "c_ack");
        if !expect_pop {
            order.enqueue(SID, "c_probe");
        }
        order.enqueue(SID, "c_queued");
        let (passed_tx, passed_rx) = mpsc::channel();
        *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
            passed_tx.send(passed).unwrap_or(());
        }));
        let (client, client_peer) = UnixStream::pair().unwrap();
        client_peer
            .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
            .unwrap();
        let client = Arc::new(Mutex::new(client));
        let (session, mut session_peer) = UnixStream::pair().unwrap();
        let thread = start_relay_thread(
            SID,
            EPOCH,
            session,
            Arc::clone(&hub),
            client,
            Arc::clone(&relays),
            kept,
        );
        session_peer
            .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_ack\"}}\n")
            .unwrap();
        session_peer.flush().unwrap();
        let mut client_peer = client_peer;
        let mut read = std::io::BufReader::new(&mut client_peer);
        if expect_pop {
            let passed = Deadline::after(MUST_SUCCEED_WITHIN).recv(&passed_rx);
            let mut text = String::new();
            read.read_line(&mut text).unwrap();
            // Close, never shut down: on macOS a shutdown can leave the blocked read
            // waiting (#1877).
            drop(session_peer);
            join_within_deadline(thread);
            assert!(matches!(passed, Ok(crate::retire::PassOn::Popped(id)) if id == "c_queued"));
        } else {
            let mut first = String::new();
            read.read_line(&mut first).unwrap();
            let first: Value = serde_json::from_str(first.trim_end()).unwrap();
            assert_eq!(first["payload"]["command_id"], "c_ack");
            session_peer
                .write_all(
                    b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_probe\"}}\n",
                )
                .unwrap();
            session_peer.flush().unwrap();
            let mut second = String::new();
            read.read_line(&mut second).unwrap();
            let second: Value = serde_json::from_str(second.trim_end()).unwrap();
            assert_eq!(second["payload"]["command_id"], "c_probe");
            // The probe's forward proves the relay ran its pass-on
            // check after the earlier forward and passed nothing on.
            // Check before closing: the end (drain) calls the same hook.
            assert!(
                passed_rx.try_recv().is_err(),
                "an unrelated retiring relay does not pass the queue"
            );
            // Close, never shut down: on macOS a shutdown can leave the blocked read
            // waiting (#1877).
            drop(session_peer);
            join_within_deadline(thread);
        }
    }

    run_case(SID, EPOCH, true);
    run_case(SID, EPOCH + 1, false);
}

#[test]
fn passing_an_empty_queue_does_not_finish_its_retiring_relay() {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";
    const EPOCH: u64 = 7;
    let held = fakes::TempDir::new("rp");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let kept: Kept = Arc::new(Mutex::new(vec![(
        "c_ack".to_owned(),
        json!({"id": "c_ack", "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        true,
    )]));
    let (entry_writer, _entry_peer) = UnixStream::pair().unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch: EPOCH,
        writer: entry_writer,
        kept: Arc::clone(&kept),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Exited),
    });
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, "c_ack");
    let (client, client_peer) = UnixStream::pair().unwrap();
    client_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client = Arc::new(Mutex::new(client));
    let (session, mut session_peer) = UnixStream::pair().unwrap();
    let thread = start_relay_thread(
        SID,
        EPOCH,
        session,
        Arc::clone(&hub),
        client,
        Arc::clone(&relays),
        kept,
    );
    session_peer
        .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_ack\"}}\n")
        .unwrap();
    session_peer
        .write_all(b"{\"kind\":\"session_status\"}\n")
        .unwrap();
    session_peer.flush().unwrap();
    let mut read = std::io::BufReader::new(client_peer);
    let mut acknowledgement = String::new();
    read.read_line(&mut acknowledgement).unwrap();
    assert!(acknowledgement.contains("c_ack"));
    let mut status = String::new();
    read.read_line(&mut status).unwrap();
    assert!(status.contains("session_status"));
    let remains = crate::connection::lock(&relays)
        .entries
        .iter()
        .any(|entry| entry.session == SID && entry.epoch == EPOCH);
    // Close, never shut down: on macOS a shutdown can leave the blocked read
    // waiting (#1877).
    drop(session_peer);
    join_within_deadline(thread);
    assert!(remains, "pass_on leaves an empty retiring queue in the map");
}

#[test]
fn a_relay_end_drops_sent_commands_and_clears_only_unsent_commands() {
    use std::sync::{Arc, Mutex, mpsc};
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rd");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    let kept: Kept = Arc::new(Mutex::new(vec![
        ("c_sent".to_owned(), serde_json::Map::new(), true),
        ("c_unsent1".to_owned(), serde_json::Map::new(), false),
        ("c_unsent2".to_owned(), serde_json::Map::new(), false),
    ]));
    for id in ["c_sent", "c_unsent1", "c_unsent2"] {
        order.enqueue(SID, id);
    }
    let (passed_tx, passed_rx) = mpsc::channel();
    *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
        passed_tx.send(passed).unwrap_or(());
    }));
    let (client, _client_peer) = UnixStream::pair().unwrap();
    let client = Arc::new(Mutex::new(client));
    let (session, session_peer) = UnixStream::pair().unwrap();
    let thread = start_relay_thread(
        SID,
        7,
        session,
        Arc::clone(&hub),
        client,
        Arc::clone(&relays),
        Arc::clone(&kept),
    );
    // Close, never shut down: on macOS a shutdown can leave the blocked read
    // waiting (#1877).
    drop(session_peer);
    let passed = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&passed_rx)
        .expect("the relay end drains and clears its queue");
    join_within_deadline(thread);

    assert!(matches!(passed, crate::retire::PassOn::Dropped(2)));
    assert!(kept.lock().unwrap().is_empty());
    for id in ["c_sent", "c_unsent1", "c_unsent2"] {
        assert_eq!(order.session_for(id), None);
    }
}

#[test]
fn a_relay_drops_unknown_acknowledgements_but_forwards_its_own() {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rk");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let kept: Kept = Arc::new(Mutex::new(vec![(
        "c_known".to_owned(),
        json!({"id": "c_known", "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        true,
    )]));
    let (entry_writer, _entry_peer) = UnixStream::pair().unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch: 1,
        writer: entry_writer,
        kept: Arc::clone(&kept),
        replayed: Replayed::default(),
        thread: None,
        retiring: None,
    });
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, "c_known");
    let (client, client_peer) = UnixStream::pair().unwrap();
    client_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client = Arc::new(Mutex::new(client));
    let (session, mut session_peer) = UnixStream::pair().unwrap();
    let thread = start_relay_thread(
        SID,
        1,
        session,
        Arc::clone(&hub),
        client,
        Arc::clone(&relays),
        Arc::clone(&kept),
    );
    let mut read = std::io::BufReader::new(client_peer);
    session_peer
        .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_unknown\"}}\n")
        .unwrap();
    session_peer
        .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_known\"}}\n")
        .unwrap();
    session_peer.flush().unwrap();
    // The relay forwards in stream order, so a forwarded unknown
    // acknowledgement would be the first line: the first line is the
    // known one. The read keeps the 10 s socket deadline.
    let mut first = String::new();
    read.read_line(&mut first).unwrap();
    // Closing the session's end ends the relay's read, as a session's
    // exit does: a shutdown on macOS can leave the blocked read waiting.
    drop(session_peer);
    let (ended_tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || ended_tx.send(thread.join().is_ok()).unwrap_or(()));
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&ended).ok(),
        Some(true),
        "the relay thread ends once the session closes"
    );

    let first: Value = serde_json::from_str(first.trim_end()).unwrap();
    assert_eq!(
        first["payload"]["command_id"], "c_known",
        "an unknown acknowledgement is not forwarded"
    );
}

#[test]
fn a_transfer_does_not_use_another_sessions_live_relay() {
    let sid = "s_0123456789abcdef";
    let other = "s_aaaaaaaaaaaaaaaa";
    let line = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let mut relays = Relays::default();
    let (writer, _peer) = UnixStream::pair().unwrap();
    let replayed = Replayed::default();
    relays.entries.push(Relay {
        session: other.to_owned(),
        epoch: 1,
        writer,
        kept: Kept::default(),
        replayed: std::sync::Arc::clone(&replayed),
        thread: None,
        retiring: None,
    });

    assert!(!relays.transfer(sid, &line));
    assert_eq!(relays.subscription(sid), Some(line));
    assert!(replayed.lock().unwrap().is_empty());
}

#[test]
fn the_relay_parses_each_session_line_once() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(10);
    const SID: &str = "s_0123456789abcdef";
    const NEXT: &str = "s_bbbbbbbbbbbbbbbb";
    let held = fakes::TempDir::new("rp");
    let hub = Arc::new(hub(&held));
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    // `kept` holds `c_1` (a `prompt`) and `c_rw` (a `rewind`, sent).
    let kept: Kept = Arc::new(Mutex::new(vec![
        (
            "c_1".to_owned(),
            json!({"id": "c_1", "command": "prompt", "args": {}})
                .as_object()
                .unwrap()
                .clone(),
            true,
        ),
        (
            "c_rw".to_owned(),
            json!({"id": "c_rw", "command": "rewind", "args": {}})
                .as_object()
                .unwrap()
                .clone(),
            true,
        ),
    ]));
    // `replayed` holds `c_hub`.
    let replayed: Replayed = Arc::new(Mutex::new(vec!["c_hub".to_owned()]));
    let (order, gone) = {
        let held = crate::connection::lock(&relays);
        (held.order.clone(), held.rejoin.closed_flag())
    };
    order.enqueue(SID, "c_1");
    order.enqueue(SID, "c_rw");
    let line = |text: &str| format!("{text}\n");
    let session_lines = [
        line(r#"{"kind":"session_status","ts":1,"schema_version":1,"payload":{}}"#),
        line("not json"),
        line(
            r#"{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_hub"}}"#,
        ),
        line(
            r#"{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{"command_id":"c_1"}}"#,
        ),
        line(
            r#"{"kind":"command_rejected","ts":1,"schema_version":1,"payload":{"command_id":"c_9","code":"busy","message":"m"}}"#,
        ),
        line(&format!(
            r#"{{"kind":"command_accepted","ts":1,"schema_version":1,"payload":{{"command_id":"c_rw","result":{{"new_session_id":"{NEXT}"}}}}}}"#
        )),
    ];
    // Every line is written, and the session half dropped, before the
    // relay starts reading, so the count never depends on its timing.
    let (session, mut session_peer) = UnixStream::pair().unwrap();
    for text in &session_lines {
        session_peer.write_all(text.as_bytes()).unwrap();
    }
    session_peer.flush().unwrap();
    drop(session_peer);
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read.set_read_timeout(Some(DEADLINE)).unwrap();
    let client = Arc::new(Mutex::new(client_write));
    let epoch = crate::connection::lock(&relays).mint();
    let (count_tx, count_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("hub-relay-count".to_owned())
        .spawn({
            let hub = Arc::clone(&hub);
            let relays = Arc::clone(&relays);
            move || {
                relay(
                    RelayThread {
                        reopen: None,
                        epoch,
                        replayed,
                        kept,
                        order,
                        gone,
                    },
                    SID,
                    session,
                    &hub,
                    &client,
                    &relays,
                );
                count_tx.send(ack_parses()).unwrap_or(());
            }
        })
        .unwrap();
    // The client reads what the relay passes back: every line but the
    // muted replay and the unknown acknowledgement, in order. One
    // deadline for the whole wait.
    let (lines_tx, lines_rx) = mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("hub-relay-client".to_owned())
        .spawn(move || {
            let mut read = BufReader::new(client_read);
            let mut got = Vec::new();
            for _ in 0..4 {
                let mut text = String::new();
                match read.read_line(&mut text) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => got.push(text),
                }
            }
            lines_tx.send(got).unwrap_or(());
        })
        .unwrap();
    let got = Deadline::after(DEADLINE)
        .recv(&lines_rx)
        .expect("the client reads the four forwarded lines before its deadline");
    reader.join().unwrap();
    assert_eq!(got.len(), 4, "the relay forwards four lines: {got:?}");
    assert_eq!(got[0], session_lines[0].clone());
    assert_eq!(got[1], session_lines[1].clone());
    assert_eq!(got[2], session_lines[3].clone());
    // The rewind's acknowledgement is refused: the hub has no log for
    // the new session, so its start fails `io_failed`. Only kind,
    // `command_id` and code compare: the hub stamps its own `ts`.
    let refused: Value = serde_json::from_str(got[3].trim_end()).unwrap();
    assert_eq!(refused["kind"], "command_rejected", "{refused}");
    assert_eq!(refused["payload"]["command_id"], "c_rw", "{refused}");
    assert_eq!(refused["payload"]["code"], "io_failed", "{refused}");
    join_within_deadline(thread);
    assert_eq!(
        Deadline::after(DEADLINE).recv(&count_rx).ok(),
        Some(6),
        "the relay parses each session line once"
    );
}

#[test]
fn a_non_subscribe_command_naming_a_credential_is_left_for_the_session() {
    // Only a subscribe whose args name `credential` is read as the
    // closed command line: a prompt carrying a `credential` key is not
    // the hub's to refuse, so the startup label is `None` and the
    // command is relayed for the session to read, as today.
    let stripped = json!({"id": "c_1", "command": "prompt",
        "args": {"content": [], "credential": "work"}})
    .as_object()
    .unwrap()
    .clone();
    assert_eq!(
        startup_credential(&stripped),
        Ok(None),
        "a non-subscribe command is never read for a startup label"
    );
}

#[test]
fn a_label_bearing_subscribe_the_line_rejects_is_refused_before_any_start() {
    // A subscribe whose args name `credential` is read as the closed
    // command line before any process starts, so a wrong-typed level is
    // refused `invalid_arguments` here: nothing is relayed or started.
    let stripped = json!({"id": "c_1", "command": "subscribe",
        "args": {"level": 1, "credential": "work"}})
    .as_object()
    .unwrap()
    .clone();
    assert_eq!(
        startup_credential(&stripped),
        Err(Refused {
            code: contract::ErrorCode::InvalidArguments,
            message: "The arguments do not fit this command.".to_owned(),
        }),
        "a malformed label-bearing subscribe never reaches a session"
    );
}

#[test]
fn a_subscribe_without_a_credential_is_left_for_the_session() {
    // Without a `credential` key the hub reads nothing, so a
    // wrong-typed level is left for the session to refuse (the session
    // starts first); the hub itself refuses nothing.
    let stripped = json!({"id": "c_1", "command": "subscribe",
        "args": {"level": 1}})
    .as_object()
    .unwrap()
    .clone();
    assert_eq!(
        startup_credential(&stripped),
        Ok(None),
        "a subscribe naming no label is never read for one"
    );
}

#[test]
fn a_replay_that_cannot_start_is_refused_io_failed() {
    use std::io::{BufRead, BufReader};
    use std::time::Duration;
    // The socket-pair, clone and spawn failures `could_not_replay` answers
    // cannot be forced from outside, so a mutant emptying its body, or
    // changing its code or message, would survive unnoticed and hang the
    // client on its enqueued command: this test calls it alone and pins
    // the refusal.
    const DEADLINE: Duration = Duration::from_secs(10);
    let held = fakes::TempDir::new("rs");
    let hub = Arc::new(hub(&held));
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let (writer, reader) = UnixStream::pair().unwrap();
    let writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(writer));
    super::could_not_replay(
        &writer,
        &hub,
        &relays,
        "s_aaaaaaaaaaaaaaaa",
        &CommandId("c_1".to_owned()),
    );
    reader.set_read_timeout(Some(DEADLINE)).unwrap();
    let mut text = String::new();
    BufReader::new(reader)
        .read_line(&mut text)
        .expect("the refusal arrives");
    let line: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "io_failed", "{line}");
    assert!(
        line["payload"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("s_aaaaaaaaaaaaaaaa"),
        "{line}"
    );
}

/// `docs/invocation.md`, "Lifecycle": a command is delivered once; a second unanswered end, or a `closing` answer to it, is rejected `closing`.
#[test]
fn hand_once_takes_an_id_once_until_it_is_forgotten() {
    let order = crate::retire::AckOrder::default();
    assert!(order.hand_once("c_1"));
    assert!(!order.hand_once("c_1"), "the second handover is refused");
    assert!(order.hand_once("c_2"), "another id has its own");
    order.forget_handed("c_1");
    assert!(order.hand_once("c_1"), "a forgotten id may be handed again");
    order.drop_ids(&[("s".to_owned(), "c_2".to_owned())]);
    assert!(order.hand_once("c_2"), "a dropped id is forgotten");
}

/// One relay thread at its session's end, over a hub with a fake starter
/// that resumes the session and acknowledges what it receives: the log's
/// last line, the relay's entry, its sent and unsent commands, and the
/// client's reads are the test's to set.
struct Ended {
    _held: fakes::TempDir,
    dir: std::path::PathBuf,
    hub: std::sync::Arc<crate::connection::Hub>,
    starter: crate::fake::FakeStarter,
    order: std::sync::Arc<crate::retire::AckOrder>,
    kept: Kept,
    client: std::io::BufReader<UnixStream>,
    client_writer: std::sync::Arc<std::sync::Mutex<UnixStream>>,
    /// The entry's session end, held so the entry's writes do not fail.
    _entry_peer: Option<UnixStream>,
    session_peer: Option<UnixStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

const END_SID: &str = "s_0123456789abcdef";

impl Ended {
    /// `last` is the kind of the log's last line. `entry` says whether the
    /// relay's entry is in the map. `sent` and `unsent` are command ids.
    fn start(last: &str, entry: bool, sent: &[&str], unsent: &[&str]) -> Self {
        use std::sync::{Arc, Mutex};
        let held = fakes::TempDir::new("re");
        let dir = held.path().join("h");
        std::fs::create_dir_all(&dir).unwrap();
        let workspace = dir.join("w");
        std::fs::create_dir_all(&workspace).unwrap();
        let log_dir = dir
            .join("projects")
            .join("-p")
            .join("sessions")
            .join(END_SID);
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(
            log_dir.join("events.jsonl"),
            format!(
                "{{\"kind\":\"session_started\",\"seq\":0,\"session_id\":\"{END_SID}\",\"payload\":{{\"workspace\":\"{}\"}}}}\n{{\"kind\":\"{last}\",\"seq\":1,\"session_id\":\"{END_SID}\",\"payload\":{{}}}}\n",
                workspace.display()
            ),
        )
        .unwrap();
        let clock = fakes::clock::FakeClock::new();
        let timed: Arc<dyn contract::clock::Clock> = clock;
        let starter = crate::fake::FakeStarter::bind_and_hold(&dir);
        let hub = Arc::new(crate::testkit::hub(&dir, Arc::new(starter.clone()), timed));
        let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
        let order = crate::connection::lock(&relays).order.clone();
        let epoch = crate::connection::lock(&relays).mint();
        let command = |id: &str| {
            json!({"id": id, "command": "reply", "args": {}})
                .as_object()
                .unwrap()
                .clone()
        };
        let mut queued = Vec::new();
        for id in sent {
            queued.push(((*id).to_owned(), command(id), true));
        }
        for id in unsent {
            queued.push(((*id).to_owned(), command(id), false));
        }
        for (id, _, _) in &queued {
            order.enqueue(END_SID, id);
        }
        let kept: Kept = Arc::new(Mutex::new(queued));
        let mut entry_peer = None;
        if entry {
            let (writer, peer) = UnixStream::pair().unwrap();
            entry_peer = Some(peer);
            crate::connection::lock(&relays).entries.push(Relay {
                session: END_SID.to_owned(),
                epoch,
                writer,
                kept: Arc::clone(&kept),
                replayed: Replayed::default(),
                thread: None,
                retiring: None,
            });
        }
        let (client_write, client_read) = UnixStream::pair().unwrap();
        client_read
            .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
            .unwrap();
        let client_writer = Arc::new(Mutex::new(client_write));
        let (session, session_peer) = UnixStream::pair().unwrap();
        let thread = start_relay_thread(
            END_SID,
            epoch,
            session,
            Arc::clone(&hub),
            Arc::clone(&client_writer),
            Arc::clone(&relays),
            Arc::clone(&kept),
        );
        Self {
            _held: held,
            dir,
            hub,
            starter,
            order,
            kept,
            client: std::io::BufReader::new(client_read),
            client_writer,
            _entry_peer: entry_peer,
            session_peer: Some(session_peer),
            thread: Some(thread),
        }
    }

    /// The session closes the relayed connection, and the relay ends.
    fn close_session(&mut self) {
        // Close, never shut down: on macOS a shutdown can leave the blocked
        // read waiting (#1877).
        drop(self.session_peer.take());
        self.join();
    }

    /// Waits for the relay thread to end.
    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            join_within_deadline(thread);
        }
    }

    /// The next line the client reads.
    fn line(&mut self) -> Value {
        let mut text = String::new();
        self.client
            .read_line(&mut text)
            .expect("the client reads a line");
        serde_json::from_str(text.trim_end()).unwrap()
    }

    /// The ids the fake session received, in order.
    fn received_ids(&self) -> Vec<String> {
        self.starter
            .received()
            .iter()
            .map(|text| {
                let line: Value = serde_json::from_str(text).unwrap();
                line["id"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    fn say(&mut self, line: Value) {
        use std::io::Write;
        let mut text = line.to_string();
        text.push('\n');
        let peer = self.session_peer.as_mut().expect("the session is open");
        peer.write_all(text.as_bytes()).unwrap();
        peer.flush().unwrap();
    }
}

fn rejection(id: &str, code: &str) -> Value {
    json!({"kind": "command_rejected", "payload": {"command_id": id, "code": code, "message": "m"}})
}

/// `docs/invocation.md`, "Lifecycle": after a clean exit the hub treats a sent, unanswered command as a `closing` answer and delivers it to the resumed session, once, in read order.
#[test]
fn a_clean_exit_re_routes_a_sent_unanswered_command_once() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended.close_session();
    let ack = ended.line();
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_1", "{ack}");
    assert_eq!(ended.received_ids(), ["c_1"], "delivered exactly once");
    assert_eq!(ended.starter.resumed().len(), 1);
}

/// `docs/invocation.md`, "Lifecycle": a command is delivered once; a second unanswered end, or a `closing` answer to it, is rejected `closing`.
#[test]
fn a_command_already_handed_once_is_rejected_closing_by_the_drain() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    assert!(ended.order.hand_once("c_1"), "the first handover happened");
    ended.close_session();
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    assert_eq!(
        line["payload"]["message"], "The session ended before answering.",
        "{line}"
    );
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "nothing is delivered");
    assert_eq!(ended.order.session_for("c_1"), None);
    assert!(ended.order.hand_once("c_1"), "the answered id is forgotten");
}

/// `docs/invocation.md`, "Lifecycle": a command is delivered once; a second unanswered end, or a `closing` answer to it, is rejected `closing`.
#[test]
fn a_closing_answer_for_a_command_handed_once_is_forwarded_not_routed() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    assert!(ended.order.hand_once("c_1"), "the first handover happened");
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "nothing is delivered");
}

/// `docs/invocation.md`, "Lifecycle": after a clean exit the hub treats a sent, unanswered command as a `closing` answer and delivers it to the resumed session, once, in read order.
#[test]
fn a_closing_answer_the_first_time_is_routed_to_the_resumed_session() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_accepted", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(ended.received_ids(), ["c_1"]);
    ended.close_session();
}

/// `docs/invocation.md`, "Lifecycle": after a crash the hub never delivers the command again; it rejects it `indeterminate`, and the client decides whether to send it again.
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
#[test]
fn a_crash_rejects_each_sent_command_indeterminate_and_passes_the_unsent_on() {
    let (popped_tx, popped_rx) = std::sync::mpsc::channel();
    let mut ended = Ended::start("turn_completed", true, &["c_1", "c_2"], &["c_3"]);
    *crate::connection::lock(&ended.hub.on_pass_on) = Some(Box::new(move |passed| {
        popped_tx.send(passed).unwrap_or(());
    }));
    ended.close_session();
    let first = ended.line();
    let second = ended.line();
    for (line, id) in [(&first, "c_1"), (&second, "c_2")] {
        assert_eq!(line["kind"], "command_rejected", "{line}");
        assert_eq!(line["payload"]["command_id"], id, "kept order: {line}");
        assert_eq!(line["payload"]["code"], "indeterminate", "{line}");
        assert_eq!(
            line["payload"]["message"],
            "The session ended before answering; the command may have run."
        );
    }
    let passed = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&popped_rx)
        .expect("the unsent command passes on after the rejections");
    assert!(matches!(passed, crate::retire::PassOn::Popped(id) if id == "c_3"));
    assert!(
        ended.starter.await_received(1, MUST_SUCCEED_WITHIN),
        "the unsent command reaches the resumed session"
    );
    assert_eq!(
        ended.received_ids(),
        ["c_3"],
        "a sent command never runs again"
    );
}

/// `docs/invocation.md`, "What each command does", "`rewind` starts a new session process": the old session closes with `rewound` and the hub starts the new one; a command for it resumes as for any session ("Lifecycle").
/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
#[test]
fn after_a_rewind_a_sent_command_gets_no_line_and_only_unsent_ones_pass_on() {
    let mut ended = Ended::start("rewound", true, &["c_1"], &["c_2"]);
    ended.close_session();
    assert!(
        ended.starter.await_received(1, MUST_SUCCEED_WITHIN),
        "the unsent command passes on"
    );
    assert_eq!(ended.received_ids(), ["c_2"]);
    let line = ended.line();
    assert_eq!(
        line["payload"]["command_id"], "c_2",
        "the first line is c_2's: {line}"
    );
    assert_eq!(ended.order.session_for("c_1"), None, "c_1 left the queue");
}

/// `docs/invocation.md`, "Lifecycle": a client that disconnected has its commands dropped, never delivered.
#[test]
fn a_gone_entry_with_an_exited_log_drops_everything_and_routes_nothing() {
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let mut ended = Ended::start("fiber_exited", false, &["c_1"], &["c_2"]);
    *crate::connection::lock(&ended.hub.on_pass_on) = Some(Box::new(move |passed| {
        dropped_tx.send(passed).unwrap_or(());
    }));
    ended.close_session();
    let passed = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&dropped_rx)
        .expect("the queue is cleared");
    assert!(matches!(passed, crate::retire::PassOn::Dropped(1)));
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty());
    for id in ["c_1", "c_2"] {
        assert_eq!(ended.order.session_for(id), None);
    }
    assert!(ended.kept.lock().unwrap().is_empty());
}

/// `docs/invocation.md`, "Lifecycle": a client that disconnected has its commands dropped, never delivered.
#[test]
fn a_client_that_is_gone_while_its_entry_remains_gets_nothing_re_routed() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    // The client's end closes: the relay's next forward fails.
    ended.client = std::io::BufReader::new(UnixStream::pair().unwrap().0);
    let (shut, _peer) = UnixStream::pair().unwrap();
    shut.shutdown(std::net::Shutdown::Both).unwrap_or(());
    *crate::connection::lock(&ended.client_writer) = shut;
    ended.say(json!({"kind": "session_status", "payload": {}}));
    // The failed forward ends the relay's loop; the session is not read to
    // its end.
    ended.join();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "the command is dropped");
    assert_eq!(ended.order.session_for("c_1"), None);
}

/// `docs/invocation.md`, "Lifecycle": after a clean exit the hub treats a sent, unanswered command as a `closing` answer and delivers it to the resumed session, once, in read order.
#[test]
fn a_relay_that_saw_the_exited_window_is_clean_even_when_the_log_moved_on() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1", "c_2"], &[]);
    ended.say(rejection("c_1", "closing"));
    let first = ended.line();
    assert_eq!(first["payload"]["command_id"], "c_1", "{first}");
    // The resumed run wrote its start: the log no longer ends in
    // `fiber_exited`, but the relay saw the exited window.
    let events = ended
        .dir
        .join("projects")
        .join("-p")
        .join("sessions")
        .join(END_SID)
        .join("events.jsonl");
    let mut text = std::fs::read_to_string(&events).unwrap();
    text.push_str(&format!(
        "{{\"kind\":\"fiber_started\",\"seq\":2,\"session_id\":\"{END_SID}\",\"payload\":{{}}}}\n"
    ));
    std::fs::write(&events, text).unwrap();
    ended.close_session();
    let second = ended.line();
    assert_eq!(second["kind"], "command_accepted", "{second}");
    assert_eq!(second["payload"]["command_id"], "c_2", "{second}");
    assert_eq!(ended.received_ids(), ["c_1", "c_2"]);
}

/// `docs/invocation.md`, "What the hub speaks", "Commands keep their order": the close waits up to 2 seconds, then answers a command still running `indeterminate`, never replayed.
#[test]
fn an_indeterminate_answer_from_the_session_is_forwarded_and_never_routed() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended.say(rejection("c_1", "indeterminate"));
    let line = ended.line();
    assert_eq!(line["payload"]["code"], "indeterminate", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "nothing is delivered");
}
