//! Tests for a connection's relay map: epochs, the entry a finished relay
//! drops, the subscription kept once the session accepts it, and the
//! acknowledgement a replay drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::os::unix::net::UnixStream;

use serde_json::{Value, json};

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

#[test]
fn an_acknowledgement_names_its_command_and_whether_it_is_closing() {
    let line = |kind: &str, payload: Value| {
        serde_json::to_vec(&json!({"kind": kind, "ts": 1, "payload": payload})).unwrap()
    };
    let closing = json!({"command_id": "c_1", "code": "closing", "message": "m"});
    assert_eq!(
        acknowledgement(&line("command_rejected", closing.clone())),
        Some(("c_1".to_owned(), Verdict::Closing))
    );
    assert_eq!(
        acknowledgement(&line("command_accepted", closing.clone())),
        Some(("c_1".to_owned(), Verdict::Accepted))
    );
    let other = json!({"command_id": "c_1", "code": "busy", "message": "m"});
    assert_eq!(
        acknowledgement(&line("command_rejected", other)),
        Some(("c_1".to_owned(), Verdict::Rejected))
    );
    assert_eq!(
        acknowledgement(&line("command_rejected", json!({"command_id": "c_1"}))),
        Some(("c_1".to_owned(), Verdict::Rejected))
    );
    // No acknowledgement at all.
    assert_eq!(acknowledgement(&line("session_status", closing)), None);
    assert_eq!(
        acknowledgement(&line("command_rejected", json!({"code": "closing"}))),
        None
    );
    assert_eq!(
        acknowledgement(&line("command_rejected", Value::Null)),
        None
    );
    assert_eq!(acknowledgement(b"not json\n"), None);
    assert_eq!(
        acknowledgement(&serde_json::to_vec(&json!({"payload": {"command_id": "c_1"}})).unwrap()),
        None
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
        Deadline::after(std::time::Duration::from_secs(4))
            .recv(&ended)
            .ok(),
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
    assert!(settle(&ack("c_1"), &kept, &hub, sid, false).is_none());
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
    match settle(&ack("c_2"), &kept, &hub, sid, false) {
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
            &rejected("c_1", "invalid_arguments"),
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
    assert!(settle(&rejected("c_2", "closing"), &kept, &hub, sid, false).is_none());
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
    match settle(&ack, &kept, &hub, sid, false) {
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
        settle(&rejected, &kept_for(rewind.clone()), &hub, sid, false).is_none(),
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
            settle(&ack, &kept_for(rewind.clone()), &hub, sid, false).is_none(),
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
        settle(&bare, &kept_for(rewind), &hub, sid, false).is_none(),
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
        settle(&ack, &kept, &hub, sid, false).is_none(),
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
    assert!(muted(&ack("c_hub"), &replayed));
    assert!(
        replayed.lock().unwrap().is_empty(),
        "a replayed acknowledgement is consumed"
    );
    assert!(!muted(&ack("c_hub"), &replayed), "only once");
    assert!(!muted(&ack("c_1"), &replayed), "other ids pass through");
    assert!(!muted(b"not json\n", &replayed), "not an acknowledgement");
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
    Deadline::after(DEADLINE)
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
    const SID: &str = "s_0123456789abcdef";

    let held = fakes::TempDir::new("rn");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, "c_missing");
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
        let timely = Deadline::after(DEADLINE).recv(&returned_rx).is_ok();
        order.done(held_session, held_id);
        if !timely {
            Deadline::after(DEADLINE)
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
    peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
    let timely = Deadline::after(DEADLINE).recv(&noticed_rx).is_ok();
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
    const SID: &str = "s_0123456789abcdef";
    const EPOCH: u64 = 7;

    /// Receives with the deadline, failing at the caller's line.
    #[track_caller]
    fn run_case(entry_session: &str, entry_epoch: u64, expect_pop: bool) {
        let held = fakes::TempDir::new("ri");
        let hub = Arc::new(hub(&held));
        let relays = Arc::new(Mutex::new(Relays::default()));
        let kept: Kept = Arc::new(Mutex::new(vec![
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
        ]));
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
        order.enqueue(SID, "c_queued");
        let (passed_tx, passed_rx) = mpsc::channel();
        *crate::connection::lock(&hub.on_pass_on) = Some(Box::new(move |passed| {
            passed_tx.send(passed).unwrap_or(());
        }));
        let (client, client_peer) = UnixStream::pair().unwrap();
        client_peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
        let passed = Deadline::after(DEADLINE).recv(&passed_rx);
        let mut client_peer = client_peer;
        let mut text = String::new();
        std::io::BufReader::new(&mut client_peer)
            .read_line(&mut text)
            .unwrap();
        // Close, never shut down: on macOS a shutdown can leave the blocked read
        // waiting (#1877).
        drop(session_peer);
        join_within_deadline(thread);
        if expect_pop {
            assert!(matches!(passed, Ok(crate::retire::PassOn::Popped(id)) if id == "c_queued"));
        } else {
            assert!(
                passed.is_err(),
                "an unrelated retiring relay does not pass the queue"
            );
        }
    }

    run_case(SID, EPOCH, true);
    run_case(SID, EPOCH + 1, false);
}

#[test]
fn passing_an_empty_queue_does_not_finish_its_retiring_relay() {
    use std::io::{BufRead, Write};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
    client_peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
    let passed = Deadline::after(DEADLINE)
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
    use std::time::Duration;
    const DEADLINE: Duration = Duration::from_secs(4);
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
    client_peer.set_read_timeout(Some(DEADLINE)).unwrap();
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
    session_peer.flush().unwrap();
    let mut unknown = String::new();
    let suppressed = match read.read_line(&mut unknown) {
        Err(error) => matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ),
        Ok(_) => false,
    };
    session_peer
        .write_all(b"{\"kind\":\"command_accepted\",\"payload\":{\"command_id\":\"c_known\"}}\n")
        .unwrap();
    session_peer.flush().unwrap();
    let mut known = String::new();
    read.read_line(&mut known).unwrap();
    // Closing the session's end ends the relay's read, as a session's
    // exit does: a shutdown on macOS can leave the blocked read waiting.
    drop(session_peer);
    let (ended_tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || ended_tx.send(thread.join().is_ok()).unwrap_or(()));
    assert_eq!(
        Deadline::after(DEADLINE).recv(&ended).ok(),
        Some(true),
        "the relay thread ends once the session closes"
    );

    let known: Value = serde_json::from_str(known.trim_end()).unwrap();
    assert!(suppressed, "an unknown acknowledgement is not forwarded");
    assert_eq!(known["payload"]["command_id"], "c_known");
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
