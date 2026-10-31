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
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use serde_json::{Value, json};

use super::ack::Verdict;
use super::*;
use fakes::Deadline;

struct TargetRead;

impl support::peer::ProcessRead for TargetRead {
    fn current_euid(&self) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
        Ok(support::peer::EffectiveUid(1000))
    }

    fn euid(
        &self,
        pid: support::peer::Pid,
    ) -> Result<support::peer::EffectiveUid, support::peer::ReadError> {
        match pid {
            support::peer::Pid(30) => Ok(support::peer::EffectiveUid(1000)),
            support::peer::Pid(1) => Ok(support::peer::EffectiveUid(0)),
            _ => Err(support::peer::ReadError::Unreadable { pid, what: "euid" }),
        }
    }

    fn parent(
        &self,
        pid: support::peer::Pid,
    ) -> Result<support::peer::Pid, support::peer::ReadError> {
        match pid {
            support::peer::Pid(41) | support::peer::Pid(42) => Ok(support::peer::Pid(30)),
            support::peer::Pid(30) => Ok(support::peer::Pid(1)),
            support::peer::Pid(1) => Ok(support::peer::Pid(0)),
            _ => Err(support::peer::ReadError::Unreadable {
                pid,
                what: "parent",
            }),
        }
    }

    fn argv(&self, pid: support::peer::Pid) -> Result<Vec<Vec<u8>>, support::peer::ReadError> {
        (pid == support::peer::Pid(30))
            .then(|| vec![b"sh".to_vec()])
            .ok_or(support::peer::ReadError::Unreadable { pid, what: "argv" })
    }
}

#[test]
fn a_reroute_uses_a_live_relay_with_a_strictly_newer_epoch() {
    use std::sync::{Arc, Mutex};

    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("epoch");
    let hub = Arc::new(hub(&held));
    *crate::connection::lock(&hub.test_read) = Some(Arc::new(TargetRead));
    let attribution = crate::connection::Attribution::Local {
        peer_pid: support::peer::Pid(10),
        session_process_pids: vec![support::peer::Pid(41)],
    };
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let (old_epoch, new_epoch) = {
        let mut held = crate::connection::lock(&relays);
        (held.mint(), held.mint())
    };
    let (stale_writer, mut stale_peer) = UnixStream::pair().unwrap();
    stale_peer.set_nonblocking(true).unwrap();
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let live_thread = std::thread::spawn(move || stop_rx.recv().unwrap());
    crate::connection::lock(&relays).entries.extend([
        Relay {
            session: SID.to_owned(),
            epoch: old_epoch,
            writer: stale_writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
            target: RelayTarget::Live {
                pid: support::peer::Pid(41),
            },
        },
        Relay {
            session: SID.to_owned(),
            epoch: new_epoch,
            writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: Some(live_thread),
            retiring: None,
            target: RelayTarget::Live {
                pid: support::peer::Pid(42),
            },
        },
    ]);
    let (client_writer, _client_reader) = UnixStream::pair().unwrap();
    let client_writer = Arc::new(Mutex::new(client_writer));
    let line = json!({"id": "c_1", "command": "steer", "args": {}})
        .as_object()
        .unwrap()
        .clone();

    route(
        &contract::CommandId("c_1".to_owned()),
        SID,
        line,
        &hub,
        &client_writer,
        &relays,
        Some(old_epoch),
        false,
        Some(&attribution),
    );

    let mut text = String::new();
    BufReader::new(peer).read_line(&mut text).unwrap();
    let routed: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(routed["id"], "c_1", "the replacement epoch receives it");
    assert_eq!(routed["command"], "steer");
    let mut stale_byte = [0u8; 1];
    assert!(
        matches!(std::io::Read::read(&mut stale_peer, &mut stale_byte), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    stop_tx.send(()).unwrap();
    crate::connection::lock(&relays).entries[1]
        .thread
        .take()
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn authorization_uses_the_target_session_relay_not_another_session() {
    use std::sync::{Arc, Mutex};

    const A: &str = "s_0123456789abcdef";
    const B: &str = "s_fedcba9876543210";
    let held = fakes::TempDir::new("relay-session-selection");
    let hub = Arc::new(hub(&held));
    *crate::connection::lock(&hub.test_read) = Some(Arc::new(TargetRead));
    let attribution = crate::connection::Attribution::Local {
        peer_pid: support::peer::Pid(10),
        session_process_pids: vec![support::peer::Pid(41)],
    };
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let (blocked_writer, _blocked_peer) = UnixStream::pair().unwrap();
    let (allowed_writer, allowed_peer) = UnixStream::pair().unwrap();
    allowed_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let live_thread = std::thread::spawn(move || {
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&stop_rx)
            .unwrap_or(());
    });
    {
        let mut held = crate::connection::lock(&relays);
        let first = held.mint();
        let second = held.mint();
        held.entries.extend([
            Relay {
                session: A.to_owned(),
                epoch: first,
                writer: blocked_writer,
                kept: Kept::default(),
                replayed: Replayed::default(),
                thread: None,
                retiring: None,
                target: RelayTarget::Live {
                    pid: support::peer::Pid(41),
                },
            },
            Relay {
                session: B.to_owned(),
                epoch: second,
                writer: allowed_writer,
                kept: Kept::default(),
                replayed: Replayed::default(),
                thread: Some(live_thread),
                retiring: None,
                target: RelayTarget::Live {
                    pid: support::peer::Pid(42),
                },
            },
        ]);
    }
    let (client_writer, client_reader) = UnixStream::pair().unwrap();
    client_reader
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client_writer = Arc::new(Mutex::new(client_writer));
    let line = json!({"id": "c_1", "command": "steer", "args": {}})
        .as_object()
        .unwrap()
        .clone();

    route(
        &contract::CommandId("c_1".to_owned()),
        B,
        line,
        &hub,
        &client_writer,
        &relays,
        None,
        false,
        Some(&attribution),
    );

    let mut text = String::new();
    BufReader::new(allowed_peer).read_line(&mut text).unwrap();
    let routed: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(routed["id"], "c_1", "session B receives its command");
    assert_eq!(routed["command"], "steer");
    stop_tx.send(()).unwrap();
    crate::connection::lock(&relays).entries[1]
        .thread
        .take()
        .unwrap()
        .join()
        .unwrap();
}

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
            target: RelayTarget::LiveUnreadable,
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
            target: RelayTarget::LiveUnreadable,
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
            target: RelayTarget::LiveUnreadable,
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
            target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
            target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
    assert!(
        acknowledgement(&line("command_accepted", "c_hub")).is_some_and(|ack| ack.id == "c_hub")
    );
    assert!(
        acknowledgement(&line("command_rejected", "c_hub")).is_some_and(|ack| ack.id == "c_hub")
    );
    assert!(
        !acknowledgement(&line("command_accepted", "c_1")).is_some_and(|ack| ack.id == "c_hub")
    );
    assert!(
        !acknowledgement(&line("session_status", "c_hub")).is_some_and(|ack| ack.id == "c_hub")
    );
    assert!(!acknowledgement(b"not json\n").is_some_and(|ack| ack.id == "c_hub"));
    assert!(
        !acknowledgement(
            &serde_json::to_vec(&json!({"kind": "command_accepted", "payload": {}})).unwrap()
        )
        .is_some_and(|ack| ack.id == "c_hub")
    );
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
        CommandState::Written,
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
        CommandState::Written,
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
        CommandState::Written,
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
        CommandState::Written,
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

/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
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
        let kept: Kept = Arc::new(Mutex::new(vec![(
            "c_1".to_owned(),
            queued,
            CommandState::Queued,
        )]));
        crate::connection::lock(&relays).entries.push(Relay {
            session: SID.to_owned(),
            epoch,
            writer,
            kept,
            replayed: Replayed::default(),
            thread,
            retiring: Some(crate::retire::Retire::Dead),
            target: RelayTarget::LiveUnreadable,
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
                None,
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
        CommandState::Written,
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
            CommandState::Written,
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
        CommandState::Written,
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
        target: RelayTarget::LiveUnreadable,
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
        None,
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

/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
/// `docs/invocation.md:1157`, "What the hub speaks": the retiring relay passes a queued command on after the handover ahead is answered when exiting, at its end when dead.
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
            CommandState::Queued,
        )])),
        replayed: Replayed::default(),
        thread: Some(parked),
        retiring: Some(crate::retire::Retire::Exited),
        target: RelayTarget::LiveUnreadable,
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
            CommandState::Queued,
        )])),
        replayed: Replayed::default(),
        thread: None,
        retiring: Some(crate::retire::Retire::Dead),
        target: RelayTarget::LiveUnreadable,
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
            None,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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
        target: RelayTarget::LiveUnreadable,
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

/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
/// `docs/invocation.md:1157`, "What the hub speaks": the retiring relay passes a queued command on after the handover ahead is answered when exiting, at its end when dead.
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
    order.enqueue(SID, &CommandId("c_2".to_owned()));
    order.enqueue(SID, &CommandId("c_1".to_owned()));
    order.enqueue(OTHER, &CommandId("c_other".to_owned()));
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
    order.enqueue(SID, &CommandId("c_3".to_owned()));
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
    order.enqueue(SID, &CommandId("c_missing".to_owned()));
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
        target: RelayTarget::LiveUnreadable,
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
        CommandState::Written,
    )]));
    order.enqueue(SID, &CommandId("c_later".to_owned()));
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

/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
/// `docs/invocation.md:1157`, "What the hub speaks": the retiring relay passes a queued command on after the handover ahead is answered when exiting, at its end when dead.
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
                CommandState::Written,
            ),
            (
                "c_queued".to_owned(),
                json!({"id": "c_queued", "command": "reply", "args": {}})
                    .as_object()
                    .unwrap()
                    .clone(),
                CommandState::Queued,
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
                    CommandState::Written,
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
            target: RelayTarget::LiveUnreadable,
        });
        let order = crate::connection::lock(&relays).order.clone();
        order.enqueue(SID, &CommandId("c_ack".to_owned()));
        if !expect_pop {
            order.enqueue(SID, &CommandId("c_probe".to_owned()));
        }
        order.enqueue(SID, &CommandId("c_queued".to_owned()));
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
        CommandState::Written,
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
        target: RelayTarget::LiveUnreadable,
    });
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, &CommandId("c_ack".to_owned()));
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
    // Both lines arrive; their relative order is stream order, which no
    // contract row promises (the order promise in
    // `docs/invocation.md:1106` covers acknowledgements), so the test
    // accepts either order.
    let mut first = String::new();
    read.read_line(&mut first).unwrap();
    let mut second = String::new();
    read.read_line(&mut second).unwrap();
    let both = format!("{first}{second}");
    assert!(
        both.contains("c_ack"),
        "the acknowledgement arrives: {both:?}"
    );
    assert!(
        both.contains("session_status"),
        "the status line arrives: {both:?}"
    );
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
        (
            "c_sent".to_owned(),
            serde_json::Map::new(),
            CommandState::Written,
        ),
        (
            "c_unsent1".to_owned(),
            serde_json::Map::new(),
            CommandState::Queued,
        ),
        (
            "c_unsent2".to_owned(),
            serde_json::Map::new(),
            CommandState::Queued,
        ),
    ]));
    for id in ["c_sent", "c_unsent1", "c_unsent2"] {
        order.enqueue(SID, &CommandId(id.to_owned()));
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

    // `docs/invocation.md:1158`, "What the hub speaks": the relay ends with
    // no entry left, so its two queued commands drop without an answer.
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
        CommandState::Written,
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
        target: RelayTarget::LiveUnreadable,
    });
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, &CommandId("c_known".to_owned()));
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
    // `docs/invocation.md:1106`, "Commands keep their order": the
    // session's acknowledgements come back in the order it sends them,
    // so a forwarded unknown acknowledgement would be the first line:
    // the first line is the known one. The read keeps the 10 s socket
    // deadline.
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
        target: RelayTarget::LiveUnreadable,
    });

    assert!(!relays.transfer(sid, &line));
    assert_eq!(relays.subscription(sid), Some(line));
    assert!(replayed.lock().unwrap().is_empty());
}

/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
/// `docs/invocation.md:1155`, "What the hub speaks": the relay's thread never starts, or its admission is refused, after the write — one `command_rejected` with the refusal's code.
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
            CommandState::Written,
        ),
        (
            "c_rw".to_owned(),
            json!({"id": "c_rw", "command": "rewind", "args": {}})
                .as_object()
                .unwrap()
                .clone(),
            CommandState::Written,
        ),
    ]));
    // `replayed` holds `c_hub`.
    let replayed: Replayed = Arc::new(Mutex::new(vec!["c_hub".to_owned()]));
    let (order, gone) = {
        let held = crate::connection::lock(&relays);
        (held.order.clone(), held.rejoin.closed_flag())
    };
    order.enqueue(SID, &CommandId("c_1".to_owned()));
    order.enqueue(SID, &CommandId("c_rw".to_owned()));
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
        // A line the typed pass rejects still counts its one pass.
        line(r#"{"kind":"command_accepted","ts":1,"payload":{"command_id":"#),
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
        Some(7),
        "the relay parses each session line once, failed parses included"
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

/// Stages `c_1` to `from` for the transition-table tests, shared by
/// `transition_allows_each_documented_move_and_refuses_the_rest` and
/// `the_command_state_table_matches_the_transition_api`: one helper so the
/// two tests cannot drift apart.
fn stage_command(order: &crate::retire::AckOrder, from: crate::relay::CommandState) {
    use crate::relay::CommandState;
    order.enqueue("s", &contract::CommandId("c_1".to_owned()));
    match from {
        CommandState::Read => {}
        CommandState::Queued => {
            order
                .transition(&contract::CommandId("c_1".to_owned()), CommandState::Queued)
                .unwrap();
        }
        CommandState::Writing => {
            order
                .transition(
                    &contract::CommandId("c_1".to_owned()),
                    CommandState::Writing,
                )
                .unwrap();
        }
        CommandState::Written | CommandState::Rerouted => {
            order
                .transition(
                    &contract::CommandId("c_1".to_owned()),
                    CommandState::Written,
                )
                .unwrap();
            if from == CommandState::Rerouted {
                order
                    .transition(
                        &contract::CommandId("c_1".to_owned()),
                        CommandState::Rerouted,
                    )
                    .unwrap();
            }
        }
        CommandState::Partial => {
            order
                .transition(
                    &contract::CommandId("c_1".to_owned()),
                    CommandState::Partial,
                )
                .unwrap();
        }
        CommandState::Answered => {
            order
                .transition(
                    &contract::CommandId("c_1".to_owned()),
                    CommandState::Answered,
                )
                .unwrap();
        }
        CommandState::Dropped => {
            order
                .transition(&contract::CommandId("c_1".to_owned()), CommandState::Queued)
                .unwrap();
            order
                .transition(
                    &contract::CommandId("c_1".to_owned()),
                    CommandState::Dropped,
                )
                .unwrap();
        }
    }
    assert_eq!(
        order.state(&contract::CommandId("c_1".to_owned())),
        Some(from),
        "staging {from:?}"
    );
}

/// `docs/invocation.md:1136-1161`, "What the hub speaks": only the move
/// into `Rerouted` claims a command's one permitted re-route, and only
/// from `Written`. Every other second claim, every move out of a terminal
/// state, and every move for an unrecorded command is refused, and a
/// refused move leaves the recorded state alone.
#[test]
fn transition_allows_each_documented_move_and_refuses_the_rest() {
    use crate::relay::CommandState;
    use crate::retire::{AckOrder, IllegalTransition};
    use CommandState::{Answered, Dropped, Partial, Queued, Read, Rerouted, Writing, Written};
    let states = [
        Read, Queued, Writing, Written, Partial, Rerouted, Answered, Dropped,
    ];
    fn allowed(from: CommandState, to: CommandState) -> bool {
        matches!(
            (from, to),
            (Read, Queued | Writing | Written | Partial | Answered)
                | (
                    Queued,
                    Queued | Writing | Written | Partial | Answered | Dropped
                )
                | (Writing, Written | Queued | Partial | Answered)
                | (Written, Written | Rerouted | Answered | Dropped)
                | (Partial, Answered | Dropped)
                | (Rerouted, Answered | Dropped)
        )
    }
    for from in states {
        for to in states {
            let order = AckOrder::default();
            stage_command(&order, from);
            match order.transition(&CommandId("c_1".to_owned()), to) {
                Ok(previous) => {
                    assert!(
                        allowed(from, to),
                        "the move from {from:?} to {to:?} is refused"
                    );
                    assert_eq!(previous, from, "the move reports what it moved from");
                    assert_eq!(
                        order.state(&CommandId("c_1".to_owned())),
                        Some(to),
                        "the move lands"
                    );
                }
                Err(IllegalTransition::Forbidden {
                    id,
                    from: at,
                    to: want,
                }) => {
                    assert!(
                        !allowed(from, to),
                        "the move from {from:?} to {to:?} is allowed"
                    );
                    assert_eq!(id, CommandId("c_1".to_owned()));
                    assert_eq!(at, from);
                    assert_eq!(want, to);
                    assert_eq!(
                        order.state(&CommandId("c_1".to_owned())),
                        Some(from),
                        "a refused move changes nothing"
                    );
                }
                Err(IllegalTransition::Unknown(_)) => {
                    panic!("c_1 is recorded in {from:?}: no move is unknown");
                }
            }
        }
    }
    let order = AckOrder::default();
    for to in states {
        assert_eq!(
            order.transition(&CommandId("c_unrecorded".to_owned()), to),
            Err(IllegalTransition::Unknown(CommandId(
                "c_unrecorded".to_owned()
            ))),
            "an unrecorded command never moves to {to:?}",
        );
    }
}

/// `docs/invocation.md:1134-1161`, "What the hub speaks" (`docs/code-quality/lints.md:20-22`):
/// the relayed-command state table mirrors `CommandState` and the transition
/// API, so a test fails when the two differ. Every state is named in the
/// table, every bare move the table documents is allowed by
/// [`crate::retire::AckOrder::transition`], every contract move the API
/// allows is documented, and no row moves out of `Rerouted` except to its
/// ending answer — the one-delivery promise the table states and the API
/// keeps. The two self-reports (`Queued -> Queued`, `Written -> Written`)
/// are internal no-ops, not contract moves: a write that changes nothing
/// reports its outcome anyway, and the client learns nothing new.
#[test]
fn the_command_state_table_matches_the_transition_api() {
    use crate::relay::CommandState;
    use crate::retire::AckOrder;
    fn names(text: &str, state: &str) -> bool {
        let mut rest = text;
        while let Some(at) = rest.find(state) {
            let before = rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let after = rest[at + state.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !before && !after {
                return true;
            }
            rest = &rest[at + state.len()..];
        }
        false
    }
    fn allows(from: CommandState, to: CommandState) -> bool {
        let order = AckOrder::default();
        stage_command(&order, from);
        order.transition(&CommandId("c_1".to_owned()), to).is_ok()
    }
    let text = include_str!("../../../docs/invocation.md");
    let start = text
        .find("**A relayed command passes through these states.**")
        .expect("the relayed-command table intro is present");
    let end = text
        .find("**A session behind a relayed command passes through these states.**")
        .expect("the session table intro ends the relayed-command table");
    assert!(start < end, "the relayed-command table comes first");
    let table = &text[start..end];
    let states = [
        CommandState::Read,
        CommandState::Queued,
        CommandState::Writing,
        CommandState::Written,
        CommandState::Partial,
        CommandState::Rerouted,
        CommandState::Answered,
        CommandState::Dropped,
    ];
    for state in states {
        assert!(
            names(table, &format!("{state:?}")),
            "the table names every command state: {state:?}",
        );
    }
    let mut documented = Vec::new();
    for line in table.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').collect();
        if cells.len() < 5 {
            continue;
        }
        let (before, after) = (cells[1].trim(), cells[3].trim());
        if before.contains("State before") || before.starts_with("---") {
            continue;
        }
        let froms: Vec<CommandState> = states
            .iter()
            .copied()
            .filter(|state| names(before, &format!("{state:?}")))
            .collect();
        let tos: Vec<CommandState> = states
            .iter()
            .copied()
            .filter(|state| names(after, &format!("{state:?}")))
            .collect();
        if froms.is_empty() {
            continue;
        }
        assert_eq!(
            tos.len(),
            1,
            "each table row ends in one command state: {line}",
        );
        for from in froms {
            documented.push((from, tos[0]));
        }
    }
    assert!(!documented.is_empty(), "the table documents moves");
    for (from, to) in &documented {
        assert!(
            allows(*from, *to),
            "the table documents {from:?} -> {to:?}, so the transition API allows it",
        );
        if *from == CommandState::Rerouted {
            assert!(
                matches!(*to, CommandState::Answered | CommandState::Dropped),
                "no table row spends the reroute claim again: {from:?} -> {to:?}",
            );
        }
    }
    // Every contract move the API allows is documented: the two
    // self-reports are internal no-ops, not contract moves.
    let noops = [
        (CommandState::Queued, CommandState::Queued),
        (CommandState::Written, CommandState::Written),
    ];
    for from in states {
        for to in states {
            if !allows(from, to) {
                continue;
            }
            if noops.contains(&(from, to)) {
                continue;
            }
            assert!(
                documented.contains(&(from, to)),
                "the API allows {from:?} -> {to:?}, so the table documents it",
            );
        }
    }
    assert!(
        !allows(CommandState::Rerouted, CommandState::Written),
        "a resumed write never reclaims the reroute the table promises once",
    );
}

/// One relay thread at its session's end, over a hub with a fake starter
/// that resumes the session and acknowledges what it receives: the log's
/// last line, the relay's entry, its sent and unsent commands, and the
/// client's reads are the test's to set.
struct Ended {
    _held: fakes::TempDir,
    dir: std::path::PathBuf,
    hub: std::sync::Arc<crate::connection::Hub>,
    relays: std::sync::Arc<std::sync::Mutex<Relays>>,
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
            queued.push(((*id).to_owned(), command(id), CommandState::Written));
        }
        for id in unsent {
            queued.push(((*id).to_owned(), command(id), CommandState::Queued));
        }
        for (id, _, _) in &queued {
            order.enqueue(END_SID, &CommandId(id.clone()));
        }
        // The fixture stands in for `route`'s write: the relay wrote every
        // staged command, so the canonical state matches the kept one.
        for (id, _, state) in &queued {
            order.transition(&CommandId(id.clone()), *state).unwrap();
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
                target: RelayTarget::LiveUnreadable,
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
            relays,
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

    fn mark_session_owned_caller(&self) {
        crate::connection::lock(&self.relays).attribution =
            Some(crate::connection::Attribution::Local {
                peer_pid: support::peer::Pid(u32::MAX),
                session_process_pids: vec![support::peer::Pid(std::process::id())],
            });
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

/// `docs/invocation.md:1152`, "What the hub speaks" (#2054): after a clean exit the hub treats a sent, unanswered command as a `closing` answer and delivers it to the resumed session, once, in read order.
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
fn a_relay_thread_reroute_rechecks_the_connection_attribution() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended.mark_session_owned_caller();
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["code"], "from_session", "{line}");
    assert!(
        ended.starter.received().is_empty(),
        "replacement gets no command"
    );
}

#[test]
fn a_queue_pump_reroute_rechecks_the_connection_attribution() {
    let mut ended = Ended::start("fiber_exited", true, &[], &["c_1"]);
    ended.mark_session_owned_caller();
    ended.close_session();
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["code"], "from_session", "{line}");
    assert!(
        ended.starter.received().is_empty(),
        "replacement gets no command"
    );
}

/// `docs/invocation.md:1152`, "What the hub speaks" (#2054): a command is delivered once; a second unanswered end is rejected `closing` instead of routed again.
#[test]
fn a_command_already_handed_once_is_rejected_closing_by_the_drain() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended
        .order
        .transition(&CommandId("c_1".to_owned()), CommandState::Rerouted)
        .expect("the first handover claims the re-route");
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
    assert_eq!(
        ended.order.state(&CommandId("c_1".to_owned())),
        None,
        "the answered id is forgotten"
    );
}

/// `docs/invocation.md:1152`, "What the hub speaks": a command already
/// rerouted once keeps its `Rerouted` claim through the resumed session's
/// write, so a second clean exit without an answer is rejected `closing`
/// instead of delivered again (`docs/invocation.md:1161`).
#[test]
fn a_second_exit_after_a_resumed_write_is_rejected_closing() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended
        .order
        .transition(&CommandId("c_1".to_owned()), CommandState::Rerouted)
        .expect("the first handover claims the re-route");
    // The resumed session's write keeps its local attempt in `Writing`
    // while the canonical claim holds: the `Writing` report is refused and
    // the state stays `Rerouted`, and so is the write's `Written` outcome.
    // A second clean exit then answers the command `closing`.
    report(
        &ended.order,
        &CommandId("c_1".to_owned()),
        CommandState::Writing,
    );
    assert_eq!(
        ended.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Rerouted),
        "the resumed write runs while the reroute claim holds"
    );
    report(
        &ended.order,
        &CommandId("c_1".to_owned()),
        CommandState::Written,
    );
    assert_eq!(
        ended.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Rerouted),
        "the resumed write keeps the reroute claim"
    );
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
    assert_eq!(
        ended.order.state(&CommandId("c_1".to_owned())),
        None,
        "the answered id is forgotten"
    );
}

/// `docs/invocation.md:1161`, "What the hub speaks": a `Partial` candidate
/// reported after the `Written -> Rerouted` claim is refused, so the
/// canonical state stays `Rerouted`; the wire answer is the same `closing`
/// rejection a second clean exit earns.
#[test]
fn a_partial_report_after_reroute_keeps_the_canonical_claim() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended
        .order
        .transition(&CommandId("c_1".to_owned()), CommandState::Rerouted)
        .expect("the first handover claims the re-route");
    // The failed write's candidate outcome: the session may have read a
    // byte, but the command already moved past it, so the canonical
    // report is refused and the state stays `Rerouted`.
    report(
        &ended.order,
        &CommandId("c_1".to_owned()),
        CommandState::Partial,
    );
    assert_eq!(
        ended.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Rerouted),
        "the partial candidate keeps the reroute claim"
    );
    ended.close_session();
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "nothing is delivered");
}

/// `docs/invocation.md:1145`, "What the hub speaks": a `Written` command the
/// session answers accepted finishes `Answered` with the acknowledgement;
/// `docs/invocation.md:1160`: a `Rerouted` command the resumed session
/// answers finishes `Answered` with the acknowledgement. An accepted
/// acknowledgement that consumes the kept attempt while it is still
/// `Written` is forwarded even when a later flush-failure candidate reports
/// `Partial`: the `Partial` report is refused and the canonical state stays
/// `Rerouted` until the forwarded acknowledgement reports `Answered`. A
/// full write followed by a failed flush is the candidate
/// `the_command_writer_counts_accepted_bytes_before_its_failure` proves; an
/// unanswered kept `Partial` settling `indeterminate` once is what
/// `a_reply_cut_before_its_line_feed_is_answered_indeterminate_once` proves.
#[test]
fn a_written_ack_before_partial_report_finishes_answered() {
    let held = fakes::TempDir::new("ra");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let command = json!({"id": "c_1", "command": "prompt", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_1".to_owned(),
        command,
        CommandState::Written,
    )]));
    let order = crate::retire::AckOrder::default();
    order.enqueue(sid, &CommandId("c_1".to_owned()));
    order
        .transition(&CommandId("c_1".to_owned()), CommandState::Written)
        .expect("the write records the sent command");
    order
        .transition(&CommandId("c_1".to_owned()), CommandState::Rerouted)
        .expect("the handover claims the re-route");
    assert_eq!(
        order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Rerouted),
        "the canonical claim is rerouted before the acknowledgement"
    );
    let ack = serde_json::to_vec(&json!({
        "kind": "command_accepted",
        "payload": {"command_id": "c_1"},
    }))
    .unwrap();
    assert!(
        settle(acknowledgement(&ack).as_ref(), &kept, &hub, sid, false).is_none(),
        "an accepted answer to a still-written attempt settles to nothing to forward"
    );
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
    report(&order, &CommandId("c_1".to_owned()), CommandState::Partial);
    assert_eq!(
        order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Rerouted),
        "the partial candidate keeps the reroute claim"
    );
    report(&order, &CommandId("c_1".to_owned()), CommandState::Answered);
    assert_eq!(
        order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Answered),
        "the forwarded acknowledgement finishes answered"
    );
}

/// A live relay `route` can write to, with its acknowledgement reader
/// under the test's control: the log ends in `fiber_exited`, the entry is
/// published with a parked stand-in thread for liveness, and the kept
/// commands start empty. The test reads the command bytes itself, then
/// hands the reader to [`start_relay_thread`], so the thread never mistakes
/// the command for a session line.
struct LiveWrite {
    _held: fakes::TempDir,
    starter: crate::fake::FakeStarter,
    hub: std::sync::Arc<crate::connection::Hub>,
    relays: std::sync::Arc<std::sync::Mutex<Relays>>,
    order: std::sync::Arc<crate::retire::AckOrder>,
    epoch: u64,
    kept: Kept,
    client: std::io::BufReader<UnixStream>,
    client_writer: std::sync::Arc<std::sync::Mutex<UnixStream>>,
    /// The session side of the pair: the test reads the command bytes and
    /// writes acknowledgements here, as the session would.
    session: Option<UnixStream>,
    /// A hub-side clone of the entry's writer: the relay thread reads here,
    /// as it reads the connected stream in production. Reads and writes on
    /// opposite ends never meet, so the thread only ever sees
    /// acknowledgements.
    hub_reader: Option<UnixStream>,
    stop_dummy: Option<std::sync::mpsc::Sender<()>>,
}

impl LiveWrite {
    /// Stages `c_1` as `Read`, or as `Queued` when `queued` holds: the two
    /// canonical schedules a live write starts from.
    fn start(tag: &str, queued: bool) -> Self {
        use std::sync::{Arc, Mutex};
        let held = fakes::TempDir::new(tag);
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
                "{{\"kind\":\"session_started\",\"seq\":0,\"session_id\":\"{END_SID}\",\"payload\":{{\"workspace\":\"{}\"}}}}\n{{\"kind\":\"fiber_exited\",\"seq\":1,\"session_id\":\"{END_SID}\",\"payload\":{{}}}}\n",
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
        let kept: Kept = Arc::new(Mutex::new(Vec::new()));
        let (entry_writer, session) = UnixStream::pair().unwrap();
        // No read timeout anywhere on this pair: the relay thread blocks
        // until the test's acknowledgement arrives, and a timeout would
        // surface as a `WouldBlock` instead of waiting. Every read here
        // follows a signal proving its bytes are on the way, and every join
        // has a deadline.
        let hub_reader = entry_writer.try_clone().unwrap();
        let (stop_dummy, held_stop) = std::sync::mpsc::channel();
        let dummy = std::thread::spawn(move || {
            held_stop.recv().unwrap_or(());
        });
        crate::connection::lock(&relays).entries.push(Relay {
            session: END_SID.to_owned(),
            epoch,
            writer: entry_writer,
            kept: Arc::clone(&kept),
            replayed: Replayed::default(),
            thread: Some(dummy),
            retiring: None,
            target: RelayTarget::LiveUnreadable,
        });
        let c_1 = CommandId("c_1".to_owned());
        order.enqueue(END_SID, &c_1);
        if queued {
            report(&order, &c_1, CommandState::Queued);
        }
        let (client_write, client_read) = UnixStream::pair().unwrap();
        client_read
            .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
            .unwrap();
        Self {
            _held: held,
            starter,
            hub,
            relays,
            order,
            epoch,
            kept,
            client: std::io::BufReader::new(client_read),
            client_writer: Arc::new(Mutex::new(client_write)),
            session: Some(session),
            hub_reader: Some(hub_reader),
            stop_dummy: Some(stop_dummy),
        }
    }

    /// The `reply` line `route` writes for `c_1`.
    fn stripped() -> Map<String, Value> {
        json!({"id": "c_1", "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone()
    }

    /// Runs `route` for `c_1` on a thread: the caller releases the write
    /// through the armed pause hook and joins the thread.
    fn spawn_route(&self) -> std::thread::JoinHandle<()> {
        let hub = std::sync::Arc::clone(&self.hub);
        let writer = std::sync::Arc::clone(&self.client_writer);
        let relays = std::sync::Arc::clone(&self.relays);
        std::thread::spawn(move || {
            route(
                &CommandId("c_1".to_owned()),
                END_SID,
                Self::stripped(),
                &hub,
                &writer,
                &relays,
                None,
                false,
                None,
            );
        })
    }

    /// Reads the command bytes the live write delivered: proof the session
    /// received the line before any acknowledgement runs. Reads through
    /// the acknowledgement handle, after the reader itself moved to the
    /// relay thread.
    fn read_command(&self, ack: &UnixStream) {
        let mut probe = std::io::BufReader::new(ack.try_clone().unwrap());
        let mut line = String::new();
        probe.read_line(&mut line).unwrap();
        let command: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(command["id"], "c_1", "the live write delivers the command");
    }

    /// Starts the acknowledgement reader on the hub side, after
    /// [`LiveWrite::read_command`]: returns a handle the test writes acks
    /// with and the thread the test joins. Opposite ends carry opposite
    /// directions, so the thread only ever sees acknowledgements.
    fn start_reader(&mut self) -> (UnixStream, std::thread::JoinHandle<()>) {
        let session = self.session.as_ref().expect("the session side is held");
        let ack = session.try_clone().unwrap();
        let reader = self.hub_reader.take().expect("the hub side is held");
        let thread = start_relay_thread(
            END_SID,
            self.epoch,
            reader,
            std::sync::Arc::clone(&self.hub),
            std::sync::Arc::clone(&self.client_writer),
            std::sync::Arc::clone(&self.relays),
            std::sync::Arc::clone(&self.kept),
        );
        (ack, thread)
    }

    /// Writes one session line for the relay thread to read.
    fn say(&self, ack: &UnixStream, line: Value) {
        use std::io::Write;
        let mut text = line.to_string();
        text.push('\n');
        let mut ack = ack.try_clone().unwrap();
        ack.write_all(text.as_bytes()).unwrap();
        ack.flush().unwrap();
    }

    /// The next client line.
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

    /// Ends the relay thread: shuts the entry's writer so its read ends,
    /// joins it past the quiet drain, and stops the stand-in thread.
    fn end_reader(&mut self, ack: UnixStream, thread: std::thread::JoinHandle<()>) {
        drop(ack);
        {
            let held = crate::connection::lock(&self.relays);
            let at = crate::relay::relay_slot(&held.entries, END_SID, self.epoch)
                .expect("the live entry is still published");
            held.entries[at]
                .writer
                .shutdown(std::net::Shutdown::Both)
                .unwrap_or(());
        }
        join_within_deadline(thread);
        if let Some(stop) = self.stop_dummy.take() {
            stop.send(()).unwrap_or(());
        }
        let dummy = crate::connection::lock(&self.relays)
            .entries
            .iter_mut()
            .find(|entry| entry.session == END_SID && entry.epoch == self.epoch)
            .and_then(|entry| entry.thread.take());
        if let Some(dummy) = dummy {
            join_route(dummy, "the stand-in relay thread ends");
        }
    }
}

/// Joins a `route` or stand-in thread, failing at the caller's line
/// instead of hanging when it does not end.
#[track_caller]
fn join_route(thread: std::thread::JoinHandle<()>, what: &str) {
    let (ended_tx, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || ended_tx.send(thread.join().is_ok()).unwrap_or(()));
    assert_eq!(
        Deadline::after(MUST_SUCCEED_WITHIN).recv(&ended).ok(),
        Some(true),
        "{what}"
    );
}

/// `docs/invocation.md:1148`, "What the hub speaks": a `closing` answer
/// `docs/invocation.md:1147`, "What the hub speaks": a post-exit `closing`
/// confirms `Written` from `Read`, `Queued` or `Writing`; `docs/invocation.md:1148`
/// then claims the one `Rerouted`, suppressing the old answer and delivering
/// once to the resumed session (`docs/invocation.md:1106`, "Commands keep
/// their order"). `docs/invocation.md:1142` answers a write cut after the
/// session may have read a byte (`#3619`, `#3616`) with one `indeterminate`,
/// never delivered again. `docs/invocation.md:1151` answers a session-read
/// command in the exit window (`#3279`, `#3326`) exactly once before shutdown.
/// `docs/invocation.md:1145` answers a command the session read while closing
/// (`#3491`, `#3534`) with its own `closing`, never stranded. `docs/invocation.md:1183`
/// completes no command when the connection ends before `hub_hello`
/// (`#1662`, `#2094`). `docs/invocation.md:1152` reroutes a sent, unanswered
/// command after a clean exit (`#2054`) once, in read order.
/// Ticket #3665 Fix step 3: one crate-level test enumerates pause points
/// across write, line-feed, exit, resume and the close grace through existing
/// test-only seams, asserting exactly one answer per command read on every
/// path. `#3565`'s interleaving stays in `rewind_tests.rs`
/// (`a_failed_old_exit_retry_is_never_repeated`, `docs/invocation.md:1179`).
/// No production hook is added.
#[test]
fn ticket_3665_enumerates_pause_points_with_exactly_one_answer() {
    in_flight_closing_from_read_delivers_once();
    in_flight_closing_from_queued_delivers_once();
    cut_before_line_feed_answers_indeterminate_once();
    exit_window_accept_is_forwarded_once();
    closing_while_closing_is_forwarded_once();
    exiting_hub_completes_no_command();
    clean_exit_reroutes_sent_command_once();
}

/// The `Read` closing schedule (`#2054`): a post-exit `closing` consumes the
/// in-flight `Writing` attempt first, so the claim confirms `Written` and
/// spends the one re-route; the late write result is skipped and the old
/// `closing` is never forwarded.
fn in_flight_closing_from_read_delivers_once() {
    in_flight_closing_delivers_once("e1r", false);
}

/// The `Queued` closing schedule (`#2054`): the same race from `Queued`, for a
/// command the retiring relay passed on.
fn in_flight_closing_from_queued_delivers_once() {
    in_flight_closing_delivers_once("e1q", true);
}

fn in_flight_closing_delivers_once(tag: &str, queued: bool) {
    let mut harness = LiveWrite::start(tag, queued);
    if queued {
        assert_eq!(
            harness.order.state(&CommandId("c_1".to_owned())),
            Some(CommandState::Queued),
            "the schedule starts queued"
        );
    }
    let (written_tx, written_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    crate::connection::lock(&harness.relays).after_command_write = Some(Box::new(move || {
        written_tx.send(()).unwrap_or(());
        release_rx.recv().unwrap_or(());
    }));
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
    *crate::connection::lock(&harness.hub.after_reroute_claim) = Some(Box::new(move || {
        claimed_tx.send(()).unwrap_or(());
    }));
    // Opposite ends carry opposite directions: the thread only ever sees
    // acknowledgements, so it can start before the command bytes are
    // drained, and starting it here keeps it off the relays lock the
    // paused write holds.
    let (ack, thread) = harness.start_reader();
    let route = harness.spawn_route();
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&written_rx)
        .expect("the live write pauses before it publishes its outcome");
    {
        let kept = crate::connection::lock(&harness.kept);
        assert_eq!(kept.len(), 1, "the in-flight attempt is kept");
        assert_eq!(
            kept[0].2,
            CommandState::Writing,
            "the live write owns the attempt"
        );
    }
    assert_eq!(
        harness.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Writing),
        "the canonical state is the in-flight write"
    );
    // The command travels hub to session and the acknowledgement travels
    // back, so draining the command first keeps the thread's first read the
    // acknowledgement.
    harness.read_command(&ack);
    harness.say(&ack, rejection("c_1", "closing"));
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&claimed_rx)
        .expect("the closing acknowledgement claims the re-route first");
    release_tx.send(()).unwrap();
    join_route(route, "the paused write returns once released");
    assert!(
        harness.starter.await_received(1, MUST_SUCCEED_WITHIN),
        "the command reaches the resumed session"
    );
    assert_eq!(
        harness.received_ids(),
        ["c_1"],
        "the command is delivered exactly once"
    );
    let line = harness.line();
    assert_eq!(line["kind"], "command_accepted", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    // The resumed relay's reader forwards the accepted answer before its
    // cleanup forgets the id, so the state read below races that reader:
    // take its thread before the entry can go away, end the resumed fake
    // session, and join the reader before asserting the queue is clean.
    let resumed = crate::connection::lock(&harness.relays)
        .entries
        .iter_mut()
        .find(|entry| entry.session == END_SID && entry.epoch > harness.epoch)
        .and_then(|entry| entry.thread.take())
        .expect("the resumed relay is still published");
    assert!(
        harness.starter.stop(
            &contract::SessionId(END_SID.to_owned()),
            MUST_SUCCEED_WITHIN
        ),
        "the resumed fake session ended"
    );
    join_within_deadline(resumed);
    harness.end_reader(ack, thread);
    assert_eq!(
        harness.order.state(&CommandId("c_1".to_owned())),
        None,
        "the answered id is forgotten"
    );
    harness.client.get_mut().set_nonblocking(true).unwrap();
    let mut extra = String::new();
    let read = harness.client.read_line(&mut extra);
    harness.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the closing schedule answers exactly once: {extra:?}"
    );
}

/// The line-feed pause point (`#3619`, `#3616`): a write cut after the
/// session may have read a byte — a whole JSON body without its line feed
/// counts — settles one `indeterminate` answer, and the command is never
/// delivered again.
fn cut_before_line_feed_answers_indeterminate_once() {
    let mut harness = LiveWrite::start("e1p", false);
    crate::connection::lock(&harness.relays).before_command_write =
        Some(Box::new(move |cap, bytes| {
            *cap = Some(bytes.len() - 1);
        }));
    let route = harness.spawn_route();
    join_route(route, "the cut write reports its outcome");
    assert_eq!(
        harness.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Partial),
        "the cut write is partial"
    );
    let (ack, thread) = harness.start_reader();
    harness.say(
        &ack,
        json!({"kind": "command_accepted", "payload": {"command_id": "c_1"}}),
    );
    let line = harness.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "indeterminate", "{line}");
    harness.end_reader(ack, thread);
    assert!(
        harness.received_ids().is_empty(),
        "the cut command is never delivered"
    );
    assert_eq!(
        harness.order.state(&CommandId("c_1".to_owned())),
        None,
        "the answered id is forgotten"
    );
    harness.client.get_mut().set_nonblocking(true).unwrap();
    let mut extra = String::new();
    let read = harness.client.read_line(&mut extra);
    harness.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the cut write answers exactly once: {extra:?}"
    );
}

/// The close-grace pause point in the exit window (`#3279`, `#3326`): a
/// session-read command gets exactly one answer before shutdown.
fn exit_window_accept_is_forwarded_once() {
    let mut ended = Ended::start("turn_completed", true, &["c_1"], &[]);
    ended.say(json!({"kind": "command_accepted", "payload": {"command_id": "c_1"}}));
    let line = ended.line();
    assert_eq!(line["kind"], "command_accepted", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(
        ended.received_ids().is_empty(),
        "the command is never delivered again"
    );
    assert_eq!(ended.order.state(&CommandId("c_1".to_owned())), None);
    ended.client.get_mut().set_nonblocking(true).unwrap();
    let mut extra = String::new();
    let read = ended.client.read_line(&mut extra);
    ended.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the exit window answers exactly once: {extra:?}"
    );
}

/// The close-grace pause point while closing (`#3491`, `#3534`): a command
/// the session read while closing is answered `closing`, never stranded and
/// never `indeterminate`.
fn closing_while_closing_is_forwarded_once() {
    let mut ended = Ended::start("turn_completed", true, &["c_1"], &[]);
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(
        ended.received_ids().is_empty(),
        "the command is never delivered again"
    );
    assert_eq!(ended.order.state(&CommandId("c_1".to_owned())), None);
    ended.client.get_mut().set_nonblocking(true).unwrap();
    let mut extra = String::new();
    let read = ended.client.read_line(&mut extra);
    ended.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the closing answer arrives exactly once: {extra:?}"
    );
}

/// The resume pause point with no command (`#1662`, `#2094`): a connection
/// that ends before `hub_hello` completes no command.
fn exiting_hub_completes_no_command() {
    use std::io::Read;
    let held = fakes::TempDir::new("e1h");
    let hub = hub(&held);
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(true);
    assert!(
        matches!(
            hub.poll_accept(&stream, &stop),
            crate::connection::Accept::Exiting
        ),
        "an exiting hub drops the accept"
    );
    drop(stream);
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).unwrap();
    assert!(
        bytes.is_empty(),
        "EOF with no `hub_hello`: no command completes"
    );
}

/// The exit and resume pause points (`#2054`): after a clean exit the hub
/// treats a sent, unanswered command as a `closing` answer and delivers it to
/// the resumed session, once, in read order.
fn clean_exit_reroutes_sent_command_once() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended.close_session();
    let ack = ended.line();
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_1", "{ack}");
    assert_eq!(ended.received_ids(), ["c_1"], "delivered exactly once");
    assert_eq!(ended.starter.resumed().len(), 1);
    ended.client.get_mut().set_nonblocking(true).unwrap();
    let mut extra = String::new();
    let read = ended.client.read_line(&mut extra);
    ended.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the rerouted command is answered exactly once: {extra:?}"
    );
}

/// `docs/invocation.md:1145`, "What the hub speaks": a rejected answer
/// consumes the in-flight `Writing` attempt and settles to nothing to
/// forward, so the ordered forward writes the session's original rejection
/// to the client; a late write result is refused as unknown, so the
/// command is never delivered.
#[test]
fn a_rejected_answer_consumes_an_in_flight_write() {
    use std::sync::{Arc, Mutex};
    let held = fakes::TempDir::new("rwj");
    let hub = hub(&held);
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    let c_1 = CommandId("c_1".to_owned());
    order.enqueue(END_SID, &c_1);
    // The fixture stands in for `route`'s pre-write publish: the attempt
    // is kept as `Writing` and the canonical state moves with it.
    let kept: Kept = Arc::new(Mutex::new(vec![(
        "c_1".to_owned(),
        LiveWrite::stripped(),
        CommandState::Writing,
    )]));
    order
        .transition(&c_1, CommandState::Writing)
        .expect("the live write owns the in-flight attempt");
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client_writer = Arc::new(Mutex::new(client_write));
    let mut buf = serde_json::to_vec(&rejection("c_1", "invalid_arguments")).unwrap();
    buf.push(b'\n');
    let ack = acknowledgement(&buf);
    assert!(ack.is_some(), "the bytes parse as an acknowledgement");
    assert!(
        settle(ack.as_ref(), &kept, &hub, END_SID, false).is_none(),
        "a rejected answer to an in-flight attempt settles to nothing to forward"
    );
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
    // The forward runs on the test's wiring instead of the relay thread:
    // the paused write holds the relays lock the forward needs, and no
    // other seam orders the relay's settle before the write's publish.
    // The closing-answer tests cover the relay thread's schedule. The
    // epoch binds before the call so no relays guard is held across it:
    // a guard temporary would live to the call's end and deadlock the
    // forward's own lock.
    let epoch = crate::connection::lock(&relays).mint();
    crate::retire::forward(
        &buf,
        ack.as_ref(),
        &client_writer,
        &relays,
        &hub,
        &order,
        epoch,
    )
    .expect("the forward writes the rejection");
    let mut client = std::io::BufReader::new(client_read);
    let mut text = String::new();
    client
        .read_line(&mut text)
        .expect("the client reads its rejection");
    let line: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "invalid_arguments", "{line}");
    assert_eq!(order.state(&c_1), None, "the answered id is forgotten");
    // A late write result after the answer changes nothing: the id is
    // already forgotten, so the report is refused as unknown.
    report(&order, &c_1, CommandState::Written);
    assert_eq!(
        order.state(&c_1),
        None,
        "the late write result cannot resurrect the answer"
    );
}

/// `docs/invocation.md:1141`, "What the hub speaks": a write that accepts
/// nothing reports `Queued` when no acknowledgement got there first. The
/// relay's end passes the queued command to the resumed session, whose
/// accepted answer is the client's one acknowledgement
/// (`docs/invocation.md:1145`).
#[test]
fn a_failed_write_before_the_acknowledgement_keeps_the_command_queued() {
    let mut harness = LiveWrite::start("rwf", false);
    crate::connection::lock(&harness.relays).before_command_write =
        Some(Box::new(move |cap, _| {
            *cap = Some(0);
        }));
    let route = harness.spawn_route();
    join_route(route, "the cut write reports its outcome");
    {
        let kept = crate::connection::lock(&harness.kept);
        assert_eq!(kept.len(), 1, "the failed attempt is kept");
        assert_eq!(
            kept[0].2,
            CommandState::Queued,
            "nothing accepted stays unsent"
        );
    }
    assert_eq!(
        harness.order.state(&CommandId("c_1".to_owned())),
        Some(CommandState::Queued),
        "the write outcome wins first"
    );
    let (ack, thread) = harness.start_reader();
    harness.end_reader(ack, thread);
    assert!(
        harness.starter.await_received(1, MUST_SUCCEED_WITHIN),
        "the queued command reaches the resumed session"
    );
    assert_eq!(
        harness.received_ids(),
        ["c_1"],
        "the command is delivered exactly once"
    );
    let line = harness.line();
    assert_eq!(line["kind"], "command_accepted", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
}

/// `docs/invocation.md:1149`, "What the hub speaks" (#2054): a command is delivered once; a `closing` answer to it after it already rerouted once is rejected `closing`, never routed again.
#[test]
fn a_closing_answer_for_a_command_handed_once_is_forwarded_not_routed() {
    let mut ended = Ended::start("fiber_exited", true, &["c_1"], &[]);
    ended
        .order
        .transition(&CommandId("c_1".to_owned()), CommandState::Rerouted)
        .expect("the first handover claims the re-route");
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty(), "nothing is delivered");
}

/// `docs/invocation.md:1148`, "What the hub speaks" (#2054): after a clean exit the hub treats a sent, unanswered command as a `closing` answer and delivers it to the resumed session, once, in read order.
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

/// `docs/invocation.md:1176`, "Lifecycle": after a crash each sent, unanswered command gets one `indeterminate` and is never delivered again, and queued commands pass on.
/// `docs/invocation.md:1153`, "What the hub speaks": a command the session crashes without answering is rejected `indeterminate` and never delivered again.
/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
/// `docs/invocation.md:1157`, "What the hub speaks": the retiring relay passes a queued command on after the handover ahead is answered when exiting.
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

/// `docs/invocation.md:568`, "Lifecycle", "`rewind` starts a new session process": the session being rewound closes with `rewound` while it still holds its lock, and the hub starts the new one.
/// `docs/invocation.md:1154`, "What the hub speaks": a command the session is rewound or moved without answering is dropped, already answered at the rewind.
/// `docs/invocation.md:1157`, "What the hub speaks": the retiring relay passes the unsent queued command on.
/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
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

/// `docs/invocation.md:1156`, "What the hub speaks": the client disconnects, so every kept command drops with nothing delivered and nothing resumed.
#[test]
fn a_gone_entry_with_an_exited_log_drops_everything_and_routes_nothing() {
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let mut ended = Ended::start("fiber_exited", false, &["c_1"], &["c_2"]);
    *crate::connection::lock(&ended.hub.on_pass_on) = Some(Box::new(move |passed| {
        dropped_tx.send(passed).unwrap_or(());
    }));
    // `docs/invocation.md:1156`, "Lifecycle": a disconnected client gets
    // no `indeterminate` answer; the partly written command is dropped too.
    ended.order.enqueue(END_SID, &CommandId("c_p".to_owned()));
    lock(&ended.kept).push((
        "c_p".to_owned(),
        json!({"id": "c_p", "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        CommandState::Partial,
    ));
    ended.close_session();
    let passed = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&dropped_rx)
        .expect("the queue is cleared");
    ended.client.get_mut().set_nonblocking(true).unwrap();
    let mut answer = String::new();
    let read = ended.client.read_line(&mut answer);
    ended.client.get_mut().set_nonblocking(false).unwrap();
    assert!(
        matches!(read, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "a partial command with no relay entry gets no answer: {answer:?}"
    );
    assert!(matches!(passed, crate::retire::PassOn::Dropped(1)));
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(ended.received_ids().is_empty());
    for id in ["c_1", "c_2", "c_p"] {
        assert_eq!(ended.order.session_for(id), None);
    }
    assert!(ended.kept.lock().unwrap().is_empty());
}

/// `docs/invocation.md:1156`, "What the hub speaks": the client disconnects, so its `Written` command drops with nothing delivered and nothing resumed.
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

/// `docs/invocation.md:1148`, "What the hub speaks": a `closing` answer after the exited window reroutes the command, delivered once to the resumed session.
/// `docs/invocation.md:1177`, "Lifecycle": once the log ends in `fiber_exited`, each sent, unanswered command goes once to the resumed session, in read order.
/// `docs/invocation.md:1106`, "What the hub speaks", "Commands keep their order": the hub passes one connection's commands to a session in the order it read them, and the acknowledgements come back in that order.
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

/// `docs/invocation.md:1150`, "What the hub speaks": the session answers a command still running `indeterminate`, so the rejection is forwarded and the command is never replayed.
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

/// `docs/invocation.md:1142`, "What the hub speaks" (#3616): a command whose
/// write fails after the session may have read a byte — a whole JSON body
/// without its line feed counts — is answered `indeterminate` and never
/// delivered again. End-of-file framing delivers the cut body as a final
/// line, so the session answers it, and the hub suppresses that answer:
/// the client gets exactly one `indeterminate` answer, and the command is
/// never routed again (`docs/invocation.md:1159`).
#[test]
fn a_reply_cut_before_its_line_feed_is_answered_indeterminate_once() {
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";
    let held = fakes::TempDir::new("rc");
    let dir = held.path().join("h");
    std::fs::create_dir_all(&dir).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let timed: Arc<dyn contract::clock::Clock> = clock;
    let starter = crate::fake::FakeStarter::bind_and_hold(&dir);
    let hub = Arc::new(crate::testkit::hub(&dir, Arc::new(starter.clone()), timed));
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    let epoch = crate::connection::lock(&relays).mint();
    let kept: Kept = Arc::new(Mutex::new(Vec::new()));
    // The established relay: a parked live thread keeps the entry
    // writable while the relay thread under test reads the session, so
    // the routed command takes the write path with the cut armed.
    let (park_tx, park_rx) = std::sync::mpsc::channel::<()>();
    let parked = std::thread::Builder::new()
        .name("parked-relay".to_owned())
        .spawn(move || {
            let _released = park_rx.recv();
        })
        .unwrap();
    let (hub_session, mut session_peer) = UnixStream::pair().unwrap();
    session_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let reader = hub_session.try_clone().unwrap();
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client_writer: Arc<Mutex<UnixStream>> = Arc::new(Mutex::new(client_write));
    let thread = spawn_for_test(
        SID.to_owned(),
        epoch,
        reader,
        Arc::clone(&hub),
        Arc::clone(&client_writer),
        Arc::clone(&relays),
        Arc::clone(&kept),
    );
    crate::connection::lock(&relays).entries.push(Relay {
        session: SID.to_owned(),
        epoch,
        writer: hub_session,
        kept: Arc::clone(&kept),
        replayed: Replayed::default(),
        thread: Some(parked),
        retiring: None,
        target: RelayTarget::LiveUnreadable,
    });
    // Cut the command write before its line feed: the session's next
    // line never completes, but the hub counted accepted bytes.
    crate::connection::lock(&relays).before_command_write = Some(Box::new(move |cap, bytes| {
        *cap = Some(bytes.len() - 1);
    }));
    let stripped = json!({"id": "c_2", "command": "reply", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    crate::relay::route(
        &contract::CommandId("c_2".to_owned()),
        SID,
        stripped,
        &hub,
        &client_writer,
        &relays,
        None,
        false,
        None,
    );
    assert_eq!(
        order.state(&CommandId("c_2".to_owned())),
        Some(CommandState::Partial),
        "the cut write is partial"
    );
    // End-of-file framing delivers the cut body as a final line: the hub
    // already shut its write half when the capped write failed, so the
    // read ends with the valid `reply` body and no line feed.
    let body = {
        // Scoped past the read: the clone must drop before cleanup, or
        // the relay never sees end of file.
        let session_reader = session_peer.try_clone().unwrap();
        let mut read = BufReader::new(session_reader);
        support::framing::read_line(&mut read, support::framing::MAX_LINE)
            .expect("the cut body arrives")
            .expect("framing delivers the cut body as a final line")
    };
    let body: Value = serde_json::from_slice(&body).expect("a valid `reply` JSON body arrives");
    assert_eq!(body["command"], "reply", "{body}");
    assert_eq!(body["id"], "c_2", "{body}");
    // The session runs it and answers; the hub suppresses the answer.
    let mut accept =
        serde_json::to_vec(&json!({"kind": "command_accepted", "payload": {"command_id": "c_2"}}))
            .unwrap();
    accept.push(b'\n');
    session_peer.write_all(&accept).unwrap();
    session_peer.flush().unwrap();
    let mut text = String::new();
    BufReader::new(client_read)
        .read_line(&mut text)
        .expect("the cut command is answered within the deadline");
    let answer: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["command_id"], "c_2", "{answer}");
    assert_eq!(answer["payload"]["code"], "indeterminate", "{answer}");
    assert_eq!(
        answer["payload"]["message"],
        crate::retire::PARTIAL_WRITE_MAY_HAVE_RUN,
        "{answer}"
    );
    // `retire::forward` writes the rejection before it calls `order.done`,
    // so the client read does not synchronize cleanup: close the session
    // peer and join the relay thread before asserting the queue is clean.
    drop(session_peer);
    join_within_deadline(thread);
    // Exactly one answer: the suppressed accept is consumed, the queue is
    // clean, and nothing resumes to deliver the command again.
    assert!(
        crate::connection::lock(&kept).is_empty(),
        "the suppressed answer is consumed"
    );
    assert_eq!(order.session_for("c_2"), None);
    assert_eq!(order.state(&CommandId("c_2".to_owned())), None);
    assert!(starter.resumed().is_empty(), "nothing is resumed");
    drop(park_tx);
}

/// `docs/invocation.md:1145`, "What the hub speaks" (#3491, #3534): a command the session
/// read while closing is answered `closing`, never stranded and never
/// `indeterminate`. Without the exited window the hub forwards the
/// session's answer as promised instead of routing it again.
#[test]
fn a_command_read_while_closing_is_answered_closing_not_stranded() {
    let mut ended = Ended::start("turn_completed", true, &["c_1"], &[]);
    ended.say(rejection("c_1", "closing"));
    let line = ended.line();
    assert_eq!(line["kind"], "command_rejected", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    assert_eq!(line["payload"]["code"], "closing", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(
        ended.received_ids().is_empty(),
        "the command is never delivered again"
    );
    assert_eq!(ended.order.session_for("c_1"), None);
    assert_eq!(ended.order.state(&CommandId("c_1".to_owned())), None);
    assert!(ended.kept.lock().unwrap().is_empty());
}

/// `docs/invocation.md:1151`, "What the hub speaks", "Commands keep their order"
/// (#3279, #3326): a session-read command in the exit window gets exactly
/// one answer before shutdown. The session's accept is forwarded, and the
/// relay's end routes nothing, resumes nothing and refuses nothing.
#[test]
fn a_session_read_command_in_the_exit_window_is_answered_exactly_once() {
    let mut ended = Ended::start("turn_completed", true, &["c_1"], &[]);
    ended.say(json!({"kind": "command_accepted", "payload": {"command_id": "c_1"}}));
    let line = ended.line();
    assert_eq!(line["kind"], "command_accepted", "{line}");
    assert_eq!(line["payload"]["command_id"], "c_1", "{line}");
    ended.close_session();
    assert!(ended.starter.resumed().is_empty(), "nothing is resumed");
    assert!(
        ended.received_ids().is_empty(),
        "the command is never delivered again"
    );
    assert_eq!(ended.order.session_for("c_1"), None);
    assert_eq!(ended.order.state(&CommandId("c_1".to_owned())), None);
    assert!(ended.kept.lock().unwrap().is_empty());
}

/// `docs/invocation.md:1183`, "What the hub speaks" (#1662, #2094): a
/// connection that ends before `hub_hello` is a connect retry, never a
/// completed relayed command. While the hub is exiting, the accept is
/// dropped unanswered: the peer reads EOF with no line before it.
#[test]
fn an_exiting_hub_sends_no_hello_before_the_connection_ends() {
    use std::io::Read;
    let held = fakes::TempDir::new("rh");
    let hub = hub(&held);
    let (stream, mut peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(true);
    assert!(
        matches!(
            hub.poll_accept(&stream, &stop),
            crate::connection::Accept::Exiting
        ),
        "an exiting hub drops the accept"
    );
    drop(stream);
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).unwrap();
    assert!(
        bytes.is_empty(),
        "EOF with no `hub_hello`: no command completes"
    );
}

/// The typed `parse_typed` seam reads `line` exactly as the full-parse
/// `acknowledgement` does: the setup asserts the line acknowledges
/// through the old path, then the parity assert compares the two.
fn typed_parity(line: &[u8]) {
    let old = acknowledgement(line);
    assert!(old.is_some(), "setup: the line acknowledges");
    assert_eq!(super::ack::parse_typed(line), old);
}

/// Both paths read `line` as `None`: the line acknowledges nothing.
fn typed_none(line: &[u8]) {
    assert_eq!(acknowledgement(line), None, "setup: the line stays unread");
    assert_eq!(super::ack::parse_typed(line), None);
}

/// Parity proves the two paths agree; these pin the values the old full
/// parse produced, so a mutant that shifts both paths still fails.
#[test]
fn parse_typed_names_the_ack_absolute_values() {
    let line = |kind: &str, payload: Value| {
        serde_json::to_vec(&json!({"kind": kind, "ts": 1, "payload": payload})).unwrap()
    };
    let accepted = line(
        "command_accepted",
        json!({"command_id": "c", "result": {"last_seq": 41}}),
    );
    let ack = super::ack::parse_typed(&accepted).expect("the accepted line acknowledges");
    assert_eq!(ack.id, "c");
    assert!(matches!(ack.verdict, Verdict::Accepted));
    assert_eq!(ack.last_seq, Some(41));
    assert_eq!(ack.rewound, None);
    let closing = line(
        "command_rejected",
        json!({"command_id": "c_1", "code": "closing", "message": "m"}),
    );
    let ack = super::ack::parse_typed(&closing).expect("the closing line acknowledges");
    assert_eq!(ack.id, "c_1");
    assert!(matches!(ack.verdict, Verdict::Closing));
    assert_eq!(ack.last_seq, None);
    // An escaped `closing` takes the owned-string path and still closes.
    let escaped = b"{\"kind\": \"command_rejected\", \"payload\": {\"command_id\": \"c_1\", \"code\": \"clos\\u0069ng\", \"message\": \"m\"}}\n";
    let ack = super::ack::parse_typed(escaped).expect("the escaped closing line acknowledges");
    assert!(matches!(ack.verdict, Verdict::Closing));
    let rewind = line(
        "command_accepted",
        json!({"command_id": "c_rw", "result": {"new_session_id": "s_bbbbbbbbbbbbbbbb"}}),
    );
    let ack = super::ack::parse_typed(&rewind).expect("the rewind acknowledges");
    assert_eq!(ack.id, "c_rw");
    assert_eq!(
        ack.rewound.map(|next| next.0),
        Some("s_bbbbbbbbbbbbbbbb".to_owned())
    );
    assert_eq!(ack.last_seq, None);
    // A wrong-typed optional reads as absent with the ack still naming
    // its command, for a scalar, a sequence and a map alike.
    for fragment in [
        json!({"last_seq": "41"}),
        json!({"last_seq": -1}),
        json!({"last_seq": 4.5}),
        json!({"last_seq": [41]}),
        json!({"last_seq": {"n": 41}}),
    ] {
        let bytes = line(
            "command_accepted",
            json!({"command_id": "c", "result": fragment}),
        );
        let ack = super::ack::parse_typed(&bytes).expect("a wrong-typed last_seq keeps the ack");
        assert_eq!(ack.id, "c");
        assert!(matches!(ack.verdict, Verdict::Accepted));
        assert_eq!(ack.last_seq, None, "{fragment}");
        assert_eq!(ack.rewound, None, "{fragment}");
    }
    for fragment in [json!([1]), json!({"n": 1})] {
        let bytes = line(
            "command_rejected",
            json!({"command_id": "c_1", "code": fragment, "message": "m"}),
        );
        let ack = super::ack::parse_typed(&bytes).expect("a wrong-typed code keeps the ack");
        assert_eq!(ack.id, "c_1");
        assert!(matches!(ack.verdict, Verdict::Rejected));
    }
    for fragment in [
        json!("x"),
        json!(Value::Null),
        json!([41]),
        json!({"n": 41}),
    ] {
        let bytes = line(
            "command_accepted",
            json!({"command_id": "c", "result": fragment}),
        );
        let ack = super::ack::parse_typed(&bytes).expect("a non-object result keeps the ack");
        assert_eq!(ack.id, "c");
        assert_eq!(ack.last_seq, None, "{fragment}");
        assert_eq!(ack.rewound, None, "{fragment}");
    }
}

#[test]
fn parse_typed_matches_acknowledgement_for_verdicts() {
    let line = |kind: &str, payload: Value| {
        serde_json::to_vec(&json!({"kind": kind, "ts": 1, "payload": payload})).unwrap()
    };
    // An accepted acknowledgement carries a numeric `last_seq`.
    let accepted = line(
        "command_accepted",
        json!({"command_id": "c", "result": {"last_seq": 41}}),
    );
    assert_eq!(
        acknowledgement(&accepted)
            .as_ref()
            .and_then(|ack| ack.last_seq),
        Some(41)
    );
    typed_parity(&accepted);
    typed_parity(&line("command_accepted", json!({"command_id": "c"})));
    typed_parity(&line(
        "command_rejected",
        json!({"command_id": "c_1", "code": "closing", "message": "m"}),
    ));
    typed_parity(&line(
        "command_rejected",
        json!({"command_id": "c_1", "code": "busy", "message": "m"}),
    ));
    // A rewind carries the session it starts.
    typed_parity(&line(
        "command_accepted",
        json!({"command_id": "c_rw", "result": {"new_session_id": "s_bbbbbbbbbbbbbbbb"}}),
    ));
    // Missing and non-object `payload` acknowledge nothing.
    typed_none(&line("command_rejected", Value::Null));
    typed_none(&serde_json::to_vec(&json!({"kind": "command_accepted", "ts": 1})).unwrap());
    typed_none(b"not json\n");
}

#[test]
fn parse_typed_takes_the_last_duplicate_key() {
    // A top-level array is no acknowledgement: there is no map to read.
    typed_none(b"[{\"kind\": \"command_accepted\"}]\n");
    // Duplicate keys resolve last-wins, at the top level and inside the
    // nested `payload` and `result`: the replayed kind loses.
    typed_parity(b"{\"kind\": \"session_status\", \"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\"}}\n");
    typed_parity(b"{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c_old\"}, \"payload\": {\"command_id\": \"c\"}}\n");
    typed_parity(b"{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c_old\", \"command_id\": \"c\"}}\n");
    typed_parity(b"{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\", \"result\": {\"last_seq\": 7}, \"result\": {\"last_seq\": 41}}}\n");
    typed_parity(b"{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\", \"result\": {\"last_seq\": 7, \"last_seq\": 41}}}\n");
    assert_eq!(
        super::ack::parse_typed(
            b"{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\", \"result\": {\"last_seq\": 7, \"last_seq\": 41}}}\n"
        )
        .and_then(|ack| ack.last_seq),
        Some(41),
        "the last duplicate wins"
    );
    typed_none(b"{\"kind\": \"command_accepted\", \"kind\": \"session_status\", \"payload\": {\"command_id\": \"c\"}}\n");
}

#[test]
fn parse_typed_keeps_the_ack_on_wrong_typed_optionals() {
    let line = |payload: Value| {
        serde_json::to_vec(&json!({"kind": "command_accepted", "payload": payload})).unwrap()
    };
    // A wrong-typed `last_seq` reads no sequence, but the ack still
    // names its command.
    for last_seq in [
        json!("41"),
        json!(-1),
        json!(4.5),
        serde_json::from_str::<Value>("18446744073709551616").unwrap(),
    ] {
        let bytes = line(json!({"command_id": "c", "result": {"last_seq": last_seq}}));
        assert_eq!(
            acknowledgement(&bytes)
                .as_ref()
                .and_then(|ack| ack.last_seq),
            None,
            "setup: {last_seq} reads no sequence"
        );
        typed_parity(&bytes);
    }
    // A string or null `result` reads no sequence and starts nothing.
    for result in [json!("x"), Value::Null] {
        typed_parity(&line(json!({"command_id": "c", "result": result})));
    }
    // Wrong-typed `new_session_id` and `code` read no session and no
    // closing verdict, but the ack still names its command.
    typed_parity(&line(
        json!({"command_id": "c_rw", "result": {"new_session_id": 7}}),
    ));
    typed_parity(
        &serde_json::to_vec(&json!({"kind": "command_rejected", "payload": {"command_id": "c_1", "code": 7, "message": "m"}})).unwrap(),
    );
    // A malformed unknown value rejects the line exactly where the full
    // parse rejects it: an invalid escaped surrogate and an out-of-range
    // number acknowledge nothing.
    typed_none("{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\"}, \"x\": \"\\ud800\"}\n".as_bytes());
    typed_none(
        "{\"kind\": \"command_accepted\", \"payload\": {\"command_id\": \"c\"}, \"x\": 1e999}\n"
            .as_bytes(),
    );
}

/// A refused admission still answers the command already written to the
/// new socket (#3589, #3619): the exclusive attach finds the live relay,
/// so no thread ever reads the command, and the client must hear
/// `session_not_found` instead of waiting with no reply.
#[test]
fn a_refused_admission_answers_the_pending_command() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";

    let held = fakes::TempDir::new("rr");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, &CommandId("c_refused".to_owned()));
    // A live relay for the session, so the exclusive admission below
    // refuses: no production exclusive caller passes a command, but the
    // written one still needs its answer on this arm.
    {
        let mut held = crate::connection::lock(&relays);
        let epoch = held.mint();
        let (live_writer, _live_peer) = UnixStream::pair().unwrap();
        held.entries.push(Relay {
            session: SID.to_owned(),
            epoch,
            writer: live_writer,
            kept: Kept::default(),
            replayed: Replayed::default(),
            thread: None,
            retiring: None,
            target: RelayTarget::LiveUnreadable,
        });
    }
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let (session, _session_peer) = UnixStream::pair().unwrap();
    let command = json!({"id": "c_refused", "command": "reply", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let mut bytes = serde_json::to_vec(&command).unwrap();
    bytes.push(b'\n');

    attach(
        SID,
        session,
        &hub,
        &writer,
        &relays,
        None,
        Some((contract::CommandId("c_refused".to_owned()), bytes, command)),
        true,
    );

    let mut text = String::new();
    BufReader::new(peer)
        .read_line(&mut text)
        .expect("the refused command is answered within the deadline");
    let answer: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["code"], "session_not_found");
    assert_eq!(answer["payload"]["command_id"], "c_refused");
    assert_eq!(order.session_for("c_refused"), None);
}

/// A relay thread that never starts still answers the command already
/// written to the session socket (#3589, #3619): `route` enqueues before
/// `attach_inner` writes, so without an answer the client would wait with
/// no reply.
#[test]
fn a_spawn_failure_answers_the_written_command() {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";

    fn failing_spawner(
        task: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        drop(task);
        Err(std::io::Error::other("the relay thread never starts"))
    }

    let held = fakes::TempDir::new("rsf");
    let hub = Arc::new(hub(&held));
    let relays = Arc::new(Mutex::new(Relays::default()));
    let order = crate::connection::lock(&relays).order.clone();
    order.enqueue(SID, &CommandId("c_unspawned".to_owned()));
    let (writer, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
    let writer = Arc::new(Mutex::new(writer));
    let (session, session_peer) = UnixStream::pair().unwrap();
    session_peer
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let command = json!({"id": "c_unspawned", "command": "reply", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let mut bytes = serde_json::to_vec(&command).unwrap();
    bytes.push(b'\n');

    attach_inner_with_spawner(
        SID,
        session,
        &hub,
        &writer,
        &relays,
        None,
        Some((
            contract::CommandId("c_unspawned".to_owned()),
            bytes,
            command,
        )),
        false,
        None,
        false,
        failing_spawner,
    );

    // The command reached the session socket before the spawn failed.
    let mut written = String::new();
    BufReader::new(session_peer)
        .read_line(&mut written)
        .expect("the written command is readable within the deadline");
    let sent: Value = serde_json::from_str(written.trim_end()).unwrap();
    assert_eq!(sent["id"], "c_unspawned");
    // The client still gets its answer.
    let mut text = String::new();
    BufReader::new(peer)
        .read_line(&mut text)
        .expect("the unspawned command is answered within the deadline");
    let answer: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["code"], "session_not_found");
    assert_eq!(answer["payload"]["command_id"], "c_unspawned");
    assert_eq!(order.session_for("c_unspawned"), None);
}

/// `docs/invocation.md:1141-1142`, "What the hub speaks": `attach_inner`'s
/// pending-command write classifies its failure once. A write that accepted
/// nothing stays unsent (`Queued`, attach goes stale for a resume and
/// retry); a write that accepted bytes may have reached the session
/// (`Partial`, answered `indeterminate`, never resumed).
#[test]
fn an_attach_write_failure_is_queued_when_nothing_landed_and_partial_once_bytes_did() {
    use std::sync::{Arc, Mutex};
    const SID: &str = "s_0123456789abcdef";

    fn never_spawns(
        task: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        drop(task);
        panic!("a failed command write returns before establishing");
    }

    for (tag, stale) in [(0, true), (1, false)] {
        let held = fakes::TempDir::new("awf");
        let hub = Arc::new(hub(&held));
        let relays = Arc::new(Mutex::new(Relays::default()));
        let order = crate::connection::lock(&relays).order.clone();
        let id = format!("c_attach_{tag}");
        order.enqueue(SID, &CommandId(id.clone()));
        let (writer, _peer) = UnixStream::pair().unwrap();
        let writer = Arc::new(Mutex::new(writer));
        let (session, _session_peer) = UnixStream::pair().unwrap();
        let command = json!({"id": id, "command": "reply", "args": {}})
            .as_object()
            .unwrap()
            .clone();
        let mut bytes = serde_json::to_vec(&command).unwrap();
        bytes.push(b'\n');
        // The zero cap accepts nothing; any smaller cut accepts bytes the
        // session may have read.
        let take = if stale { 0 } else { bytes.len() - 1 };
        crate::connection::lock(&relays).before_command_write = Some(Box::new(move |cap, _| {
            *cap = Some(take);
        }));
        let outcome = attach_inner_with_spawner(
            SID,
            session,
            &hub,
            &writer,
            &relays,
            None,
            Some((contract::CommandId(id.clone()), bytes, command)),
            false,
            None,
            false,
            never_spawns,
        );
        if stale {
            assert!(
                matches!(outcome, AttachOutcome::Stale),
                "a zero-byte attach write goes stale for a resume and retry"
            );
            assert_eq!(
                order.state(&CommandId(id.clone())),
                Some(CommandState::Queued),
                "nothing accepted stays unsent"
            );
        } else {
            assert!(
                matches!(outcome, AttachOutcome::Indeterminate),
                "an attach write that accepted bytes is answered, never resumed"
            );
            assert_eq!(
                order.state(&CommandId(id.clone())),
                Some(CommandState::Partial),
                "accepted bytes may have reached the session"
            );
        }
    }
}

#[test]
fn parse_typed_matches_last_wrong_typed_duplicates_and_escaped_optional_objects() {
    use super::ack::Ack;

    let accepted = |id: &str, last_seq: Option<u64>| {
        Some(Ack {
            id: id.to_owned(),
            verdict: Verdict::Accepted,
            rewound: None,
            last_seq,
        })
    };
    let rejected = |id: &str| {
        Some(Ack {
            id: id.to_owned(),
            verdict: Verdict::Rejected,
            rewound: None,
            last_seq: None,
        })
    };
    let parity = |line: &[u8], expected: Option<Ack>| {
        assert_eq!(
            super::ack::parse_typed(line),
            expected,
            "{}",
            String::from_utf8_lossy(line)
        );
    };

    // The last value wins even when the earlier duplicate has the wrong type.
    parity(
        br#"{"kind":7,"kind":"command_accepted","payload":{"command_id":"c"}}"#,
        accepted("c", None),
    );
    parity(
        br#"{"kind":"command_accepted","kind":7,"payload":{"command_id":"c"}}"#,
        None,
    );
    parity(
        br#"{"kind":"command_accepted","payload":[],"payload":{"command_id":"c"}}"#,
        accepted("c", None),
    );
    parity(
        br#"{"kind":"command_accepted","payload":[1],"payload":{"command_id":"c"}}"#,
        accepted("c", None),
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c"},"payload":[]}"#,
        None,
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":7,"command_id":"c"}}"#,
        accepted("c", None),
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c","command_id":7}}"#,
        None,
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c","result":7,"result":{"last_seq":41}}}"#,
        accepted("c", Some(41)),
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c","result":{"last_seq":41},"result":7}}"#,
        accepted("c", None),
    );

    // A malformed optional object's escaped key is still consumed, and
    // the optional field remains absent exactly as in the old Value map.
    parity(
        br#"{"kind":"command_rejected","payload":{"command_id":"c","code":{"a\n":1}}}"#,
        rejected("c"),
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c","result":{"new_session_id":{"a\n":1}}}}"#,
        accepted("c", None),
    );
    parity(
        br#"{"kind":"command_accepted","payload":{"command_id":"c","result":{"last_seq":{"a\n":1}}}}"#,
        accepted("c", None),
    );
}
/// The command writer counts accepted bytes before a write or flush
/// failure. A whole write succeeds; a cut reports its accepted count; a
/// zero cap accepts nothing; a failed flush reports the whole line.
#[test]
fn the_command_writer_counts_accepted_bytes_before_its_failure() {
    use std::os::unix::net::UnixStream;
    let (writer, _peer) = UnixStream::pair().unwrap();
    let bytes = b"{\"id\":\"c_1\"}\n";
    assert!(write_client_command(&writer, bytes, None).is_ok());
    let cut = bytes.len() - 1;
    match write_client_command(&writer, bytes, Some(cut)) {
        Err(partial) => {
            assert_eq!(partial.written, cut);
        }
        Ok(()) => panic!("a cut write fails"),
    }
    match write_client_command(&writer, bytes, Some(0)) {
        Err(partial) => assert_eq!(partial.written, 0, "a zero cap writes nothing"),
        Ok(()) => panic!("a zero-capped write fails"),
    }

    struct FlushFails(Vec<u8>);
    impl std::io::Write for FlushFails {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the test failed the flush",
            ))
        }
    }

    let mut writer = FlushFails(Vec::new());
    match write_through(&mut writer, bytes) {
        Err(partial) => assert_eq!(partial.written, bytes.len(), "the line was accepted"),
        Ok(()) => panic!("a failed flush is a failed write"),
    }
    assert_eq!(
        writer.0, bytes,
        "the fake accepted the whole line before flush"
    );
}

/// `docs/invocation.md:1142`, "What the hub speaks": a command the hub had
/// started writing to the session when the write failed may have been
/// read, so it is rejected `indeterminate` however the session ends, and
/// is never delivered again. Any answer the session sends for the cut
/// command is suspect, so the hub suppresses it and settles one
/// `indeterminate` answer instead (`docs/invocation.md:1159`).
#[test]
fn a_closing_answer_for_a_partly_written_command_is_suppressed_for_indeterminate() {
    let held = fakes::TempDir::new("rs");
    let hub = hub(&held);
    let sid = "s_aaaaaaaaaaaaaaaa";
    let command = json!({"id": "c_1", "command": "prompt", "args": {}})
        .as_object()
        .unwrap()
        .clone();
    let closing = serde_json::to_vec(&json!({
        "kind": "command_rejected",
        "payload": {"command_id": "c_1", "code": "closing", "message": "m"},
    }))
    .unwrap();
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_1".to_owned(),
        command,
        CommandState::Partial,
    )]));
    assert!(
        matches!(settle(acknowledgement(&closing).as_ref(), &kept, &hub, sid, true),
            Some(Settled::Indeterminate(ref id)) if id == "c_1"),
        "a partly written command settles one indeterminate answer",
    );
    assert!(kept.lock().unwrap().is_empty(), "the ack is consumed");
}

#[test]
fn parse_typed_rejects_wrong_typed_payload_values_like_the_full_parse() {
    for payload in [
        "null", "true", "false", "7", "4.5", "\"text\"", "[]", "[1e999]",
    ] {
        let line = format!("{{\"kind\":\"command_accepted\",\"payload\":{payload}}}");
        typed_none(line.as_bytes());
    }
    typed_none(br#"{"kind":"command_accepted","payload":[1e999],"payload":{"command_id":"c"}}"#);
}

/// A failed `change_workspace` start keeps the old session's kept level:
/// the relay answers the start's own code, and the redirect at the
/// socket's end still finds the level, so `follow` retries the recorded
/// child once.
/// `docs/invocation.md:1155`, "What the hub speaks": a refused admission after
/// the write is answered with the refusal's code and never written again.
/// `docs/invocation.md:467`, "What each command does": the kept subscription
/// level survives the refusal.
/// `docs/invocation.md:1179`, "What the hub speaks": the recorded child is
/// retried once when the old process exits, at most one attempt.
#[test]
fn a_failed_change_workspace_start_keeps_the_old_subscription_for_the_retry() {
    let held = fakes::TempDir::new("mvkeep");
    let dir = held.path().join("h");
    std::fs::create_dir_all(&dir).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let timed: std::sync::Arc<dyn contract::clock::Clock> = clock;
    let starter = crate::fake::FakeStarter::exit_with(
        &dir,
        crate::fake::failure(contract::ErrorCode::SessionHeld, "the new process refused."),
    );
    let hub = std::sync::Arc::new(crate::testkit::hub(
        &dir,
        std::sync::Arc::new(starter.clone()),
        timed,
    ));
    let old = crate::testkit::id(1);
    let next = crate::testkit::id(2);
    let other = crate::testkit::id(3);
    let workspace = dir.join("w");
    std::fs::create_dir_all(&workspace).unwrap();
    crate::testkit::write_log(
        &dir,
        &old,
        &workspace.to_string_lossy(),
        None,
        &[json!({
            "kind": "moved", "session_id": old, "ts": 2, "schema_version": 1, "seq": 5,
            "payload": {
                "new_session_id": next, "seq": 3,
                "workspace": workspace.to_string_lossy(),
            },
        })],
    );
    let relays: std::sync::Arc<std::sync::Mutex<Relays>> =
        std::sync::Arc::new(std::sync::Mutex::new(Relays::default()));
    let epoch = crate::connection::lock(&relays).mint();
    // The connection holds a kept level for the moved session and for an
    // untouched one: the refusal must drop neither, so the redirect at the
    // socket's end still finds the moved one.
    let level = json!({"id": "c_sub1", "command": "subscribe", "args": {"level": "full"}})
        .as_object()
        .unwrap()
        .clone();
    let other_level = json!({"id": "c_sub9", "command": "subscribe", "args": {"level": "summary"}})
        .as_object()
        .unwrap()
        .clone();
    crate::connection::lock(&relays)
        .subscribed
        .push((old.clone(), level.clone()));
    crate::connection::lock(&relays)
        .subscribed
        .push((other.clone(), other_level.clone()));
    let kept: Kept = std::sync::Arc::new(std::sync::Mutex::new(vec![(
        "c_mv".to_owned(),
        json!({"id": "c_mv", "command": "change_workspace", "args": {}})
            .as_object()
            .unwrap()
            .clone(),
        CommandState::Written,
    )]));
    let (hub_session, mut session_peer) = UnixStream::pair().unwrap();
    let reader = hub_session.try_clone().unwrap();
    crate::connection::lock(&relays).entries.push(Relay {
        session: old.clone(),
        epoch,
        writer: hub_session,
        kept: std::sync::Arc::clone(&kept),
        replayed: Default::default(),
        thread: None,
        retiring: None,
        target: RelayTarget::LiveUnreadable,
    });
    let (client_write, client_read) = UnixStream::pair().unwrap();
    client_read
        .set_read_timeout(Some(MUST_SUCCEED_WITHIN))
        .unwrap();
    let client_writer: std::sync::Arc<std::sync::Mutex<UnixStream>> =
        std::sync::Arc::new(std::sync::Mutex::new(client_write));
    let thread = spawn_for_test(
        old.clone(),
        epoch,
        reader,
        std::sync::Arc::clone(&hub),
        std::sync::Arc::clone(&client_writer),
        std::sync::Arc::clone(&relays),
        kept,
    );
    // The session accepts the move, naming the new session: its start is
    // refused, so the relay answers the start's code instead.
    let ack = json!({
        "kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": "c_mv", "result": {"new_session_id": next}},
    });
    let mut bytes = serde_json::to_vec(&ack).unwrap();
    bytes.push(b'\n');
    session_peer.write_all(&bytes).unwrap();
    session_peer.flush().unwrap();
    let mut read = BufReader::new(client_read);
    let mut text = String::new();
    read.read_line(&mut text)
        .expect("the client reads the refusal within the deadline");
    assert!(!text.is_empty(), "the hub closed before the refusal");
    let rejection: Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(rejection["kind"], "command_rejected", "{rejection}");
    assert_eq!(rejection["payload"]["command_id"], "c_mv", "{rejection}");
    assert_eq!(rejection["payload"]["code"], "session_held", "{rejection}");
    assert!(
        !serde_json::to_string(&rejection).unwrap().contains(&next),
        "the rejection carries no new session id: {rejection}"
    );
    assert_eq!(starter.rewound().len(), 1, "the start was attempted once");
    // The session socket closes: the relay ends, and its redirect retries
    // the recorded child once through the kept level.
    drop(session_peer);
    join_within_deadline(thread);
    assert_eq!(
        starter.rewound().len(),
        2,
        "the relay's follow retries the child once"
    );
    let held = crate::connection::lock(&relays);
    assert_eq!(
        held.subscription(&old),
        Some(level),
        "the refused move keeps the old session's level"
    );
    assert_eq!(
        held.subscription(&other),
        Some(other_level),
        "the refusal keeps every other session's level"
    );
}
