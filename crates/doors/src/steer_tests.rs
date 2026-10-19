//! `doors::steer`'s commit point: commit runs only after the subscribe
//! is answered (`docs/delegates.md`, "Talking to a delegate"). This lives
//! beside the code, not in `tests/`, because the `#[cfg(test)]` pause
//! point it forces the interleaving through is invisible to the
//! integration test crate.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use contract::{SCHEMA_VERSION, SessionId};
use fakes::Deadline;

use super::{AckWait, Steered, hold_ack_read_for_test, hold_ack_wait_for_test, steer};
use crate::mint;

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[path = "../tests/support/mod.rs"]
mod support;

use support::{DEADLINE, Temp};

/// The `command_accepted` line for `command`, speaking this build's schema.
fn accepted(command: &str) -> Vec<u8> {
    let line = serde_json::json!({
        "kind": "command_accepted",
        "schema_version": SCHEMA_VERSION,
        "payload": {"command_id": command},
    });
    format!("{line}\n").into_bytes()
}

#[test]
fn the_tools_probe_carries_no_args_key() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join(&id.0)).unwrap();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut raw = String::new();
        reader.read_line(&mut raw).unwrap();
        let sub: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        raw.clear();
        reader.read_line(&mut raw).unwrap();
        let steered: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(steered["command"], "steer");
        let steer_id = steered["id"].as_str().unwrap().to_owned();
        writer.write_all(&accepted(&steer_id)).unwrap();
        writer.flush().unwrap();
        raw.clear();
        reader.read_line(&mut raw).unwrap();
        let probe: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let probe_id = probe["id"].as_str().unwrap().to_owned();
        // `Command::Tools` is a unit variant, so it writes no `args`
        // key, where the hand-built line wrote `"args":{}`.
        assert_eq!(
            probe,
            serde_json::json!({"id": probe_id, "command": "tools"}),
            "the tools probe carries no args key"
        );
    });
    let mut commit = || true;
    let mut on_queued = || {};
    assert_eq!(
        steer(&home, &id, &mint("c_"), "pin", &mut commit, &mut on_queued),
        Steered::Accepted
    );
    served.join().expect("the server read the probe line");
}

#[test]
fn commit_runs_only_after_the_subscribe_is_answered() {
    let temp = Temp::new();
    let home = temp.0.join("h");
    let id = SessionId(mint("s_"));
    let run = home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join(&id.0)).unwrap();
    // The acknowledgement is held until the client reaches its wait: the
    // pause point below fires after the subscribe write and before the
    // answer is read, so nothing may commit before the answer is written.
    let (release_ack_tx, release_ack_rx) = mpsc::channel::<()>();
    let served = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut raw = String::new();
        reader.read_line(&mut raw).unwrap();
        let sub: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let sub_id = sub["id"].as_str().unwrap().to_owned();
        Deadline::after(DEADLINE)
            .recv(&release_ack_rx)
            .expect("the test releases the acknowledgement");
        let mut writer = stream.try_clone().unwrap();
        writer.write_all(&accepted(&sub_id)).unwrap();
        writer.flush().unwrap();
        // The steer line proves the commit returned true; it is never
        // answered, so this attempt ends gone.
        raw.clear();
        reader.read_line(&mut raw).unwrap();
        let steered: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(steered["command"], "steer");
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let read_before_commit = Arc::new(AtomicBool::new(true));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_pause_tx, release_pause_rx) = mpsc::channel::<()>();
    let (read_tx, read_rx) = mpsc::channel::<()>();
    hold_ack_wait_for_test(AckWait {
        entered: entered_tx,
        release: release_pause_rx,
    });
    hold_ack_read_for_test(read_tx);
    let (done_tx, done_rx) = mpsc::channel();
    let home_client = home.clone();
    let counted = Arc::clone(&calls);
    let read_first = Arc::clone(&read_before_commit);
    let held_back = thread::spawn(move || {
        let mut commit = || {
            // The read-completion signal fires after the acknowledgement
            // read returns and before this runs: a commit before the
            // answer finds the channel empty, on every run.
            if read_rx.try_recv().is_err() {
                read_first.store(false, Ordering::SeqCst);
            }
            counted.fetch_add(1, Ordering::SeqCst);
            true
        };
        let mut on_queued = || {};
        let outcome = steer(
            &home_client,
            &id,
            &mint("c_"),
            "held",
            &mut commit,
            &mut on_queued,
        );
        let _sent = done_tx.send(outcome);
    });
    // The client wrote its subscribe and waits for the held
    // acknowledgement: with the answer still held, the commit must not
    // have run. A client that commits before reading the answer runs it
    // before this signal, so the check below fails deterministically.
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the client reached the acknowledgement wait");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "commit runs only after the subscribe is answered"
    );
    release_ack_tx.send(()).unwrap();
    let _sent = release_pause_tx.send(());
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&done_rx)
            .expect("steer answered"),
        Steered::Gone
    );
    held_back.join().expect("the steer thread ended");
    assert!(
        read_before_commit.load(Ordering::SeqCst),
        "commit runs only after the acknowledgement is read"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "commit ran once");
    served.join().expect("the server read the steer line");
}
