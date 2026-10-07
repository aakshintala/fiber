//! Tests for a connection's relay map: epochs, the entry a finished relay
//! drops, the kept subscription, and the acknowledgement a replay drops.

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
    relays.keep(sid, &line("c_1", "full"));
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
fn only_the_first_subscribe_for_a_session_is_kept() {
    let line = |id: &str, command: &str| {
        json!({"id": id, "command": command, "args": {"level": "full"}})
            .as_object()
            .unwrap()
            .clone()
    };
    let mut relays = Relays::default();
    relays.keep("s_aaaaaaaaaaaaaaaa", &line("c_1", "prompt"));
    assert_eq!(relays.subscription("s_aaaaaaaaaaaaaaaa"), None);
    relays.keep("s_aaaaaaaaaaaaaaaa", &line("c_2", "subscribe"));
    relays.keep("s_aaaaaaaaaaaaaaaa", &line("c_3", "subscribe"));
    relays.keep("s_bbbbbbbbbbbbbbbb", &line("c_4", "subscribe"));
    assert_eq!(
        relays.subscription("s_aaaaaaaaaaaaaaaa"),
        Some(line("c_2", "subscribe"))
    );
    assert_eq!(
        relays.subscription("s_bbbbbbbbbbbbbbbb"),
        Some(line("c_4", "subscribe"))
    );
    assert_eq!(relays.subscription("s_cccccccccccccccc"), None);
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
    relays.keep("s_aaaaaaaaaaaaaaaa", &line("c_1", "subscribe"));
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
        }
    });
    relays.accepted("s_aaaaaaaaaaaaaaaa", epoch, line("c_1", "prompt"));
    assert_eq!(relays.subscription("s_aaaaaaaaaaaaaaaa"), None);
    relays.keep("s_aaaaaaaaaaaaaaaa", &line("c_2", "subscribe"));
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
