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
    crate::connection::Hub::new(
        &dir,
        "0.0.0",
        std::sync::Arc::new(crate::fake::FakeStarter::hang(&dir)),
        std::sync::Arc::clone(&timed),
        crate::diag::Diag::open(&dir, timed),
    )
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
    )]));
    assert!(settle(&rejected("c_2", "closing"), &kept, &hub, sid, false).is_none());
    assert!(kept.lock().unwrap().is_empty(), "the rejection is consumed");
}

#[test]
fn a_reconnect_proceeds_without_a_thread_or_after_a_panic() {
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
        let hub = Arc::new(crate::connection::Hub::new(
            &dir,
            "0.0.0",
            Arc::new(crate::fake::FakeStarter::bind_and_hold(&dir)),
            Arc::clone(&timed),
            crate::diag::Diag::open(&dir, timed),
        ));
        let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
        let epoch = crate::connection::lock(&relays).mint();
        let (writer, _peer) = UnixStream::pair().unwrap();
        writer.shutdown(Shutdown::Both).unwrap_or(());
        let thread = if panics {
            Some(
                std::thread::Builder::new()
                    .spawn(|| panic!("the old relay thread panicked"))
                    .unwrap(),
            )
        } else {
            None
        };
        crate::connection::lock(&relays).entries.push(Relay {
            session: SID.to_owned(),
            epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread,
        });
        let (client_write, client_read) = UnixStream::pair().unwrap();
        client_read.set_read_timeout(Some(DEADLINE)).unwrap();
        let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
        let mut stripped = serde_json::Map::new();
        stripped.insert("id".to_owned(), Value::String("c_1".to_owned()));
        stripped.insert("command".to_owned(), Value::String("reply".to_owned()));
        stripped.insert("args".to_owned(), Value::Object(serde_json::Map::new()));
        // The reconnect and the join block, so the route runs on a thread
        // and its completion is received with the deadline.
        let (done, finished) = std::sync::mpsc::channel();
        let routed_relays = Arc::clone(&relays);
        std::thread::spawn(move || {
            route(
                &contract::CommandId("c_1".to_owned()),
                SID,
                stripped,
                &hub,
                &client_writer,
                &routed_relays,
                false,
            );
            done.send(()).unwrap_or(());
        });
        finished
            .recv_timeout(DEADLINE)
            .expect("the route returns before its deadline");
        let mut read = BufReader::new(client_read);
        let mut text = String::new();
        read.read_line(&mut text)
            .expect("the resumed session acknowledges the command");
        let line: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(line["kind"], "command_accepted", "{line}");
        assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
        assert_eq!(
            crate::connection::lock(&relays).entries.len(),
            1,
            "the dead entry is gone; only the reconnect's remains"
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
    let kept: Kept =
        std::sync::Arc::new(std::sync::Mutex::new(vec![("c_rw1".to_owned(), command)]));
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
fn transfer_drops_a_dead_relay_and_reports_it() {
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
    relays.entries.push(Relay {
        session: sid.to_owned(),
        epoch,
        writer,
        kept: Kept::default(),
        replayed: Replayed::default(),
        thread: None,
    });
    assert!(!relays.transfer(sid, &line));
    assert!(relays.entries.is_empty(), "the dead relay is dropped");
    assert_eq!(
        relays.subscription(sid),
        Some(line),
        "the level is kept anyway"
    );
}
