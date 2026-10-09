//! Binary-level test that a restarted hub lists the session the first hub
//! followed (`docs/invocation.md`, "The hub"): the feed mints a fresh
//! subscribe id per follow, so the fresh hub's subscribe is accepted
//! instead of rejected `duplicate_command`, and `sessions` and `feed`
//! list the still-running session.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

const PROMPT: &str = "the-volume-of-the-meeting-room";

/// Polls `sessions` until `live` names `session`, each poll under its own
/// command id. Every wait takes what remains of the test's deadline;
/// expiry names `what`.
fn poll_live(client: &Socket, session: &str, what: &str, ids: &mut u32) {
    loop {
        *ids += 1;
        client.send(&format!(
            "{{\"id\":\"c_poll{}\",\"command\":\"sessions\"}}",
            *ids
        ));
        let ack = recv_reply(client, what);
        assert_eq!(ack["kind"], "command_accepted", "{ack}");
        let live = ack["payload"]["result"]["live"].as_array().unwrap();
        if live
            .iter()
            .any(|row| row["session_id"].as_str() == Some(session))
        {
            return;
        }
    }
}

#[test]
fn a_restarted_hub_lists_the_session_the_first_hub_followed() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let slot = Arc::new(Mutex::new(None));
    let (client, hello) = connect_hub(&setup, &slot);
    assert_eq!(hello["kind"], "hub_hello");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    let mut ids = 0;
    poll_live(
        &client,
        &session,
        "the first hub to list the session",
        &mut ids,
    );
    let first = slot.lock().unwrap().take().expect("the starter ran");
    first.kill("TERM");
    first.wait();
    drop(client);
    // A fresh hub: its feed subscribe carries a new id, so the session
    // accepts it instead of answering `duplicate_command`.
    let (fresh, hello) = connect_hub(&setup, &slot);
    assert_eq!(hello["kind"], "hub_hello");
    poll_live(
        &fresh,
        &session,
        "the restarted hub to list the session",
        &mut ids,
    );
    fresh.send(&json!({"id": "c_feed", "command": "feed"}).to_string());
    let ack = recv_reply(&fresh, "the feed acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let mut seen: Vec<Value> = Vec::new();
    loop {
        let line = recv_reply(&fresh, "the feed snapshot to list the session");
        let listed = line["kind"] == "session_status"
            && line["session_id"].as_str() == Some(session.as_str());
        seen.push(line);
        if listed {
            break;
        }
    }
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
    let second = slot.lock().unwrap().take().expect("the restarted hub ran");
    second.kill("TERM");
    second.wait();
    for line in until_close(&fresh) {
        seen.push(line);
    }
    assert!(
        seen.iter().any(|line| line["kind"] == "session_status"
            && line["session_id"].as_str() == Some(session.as_str())),
        "the feed listed the session: {seen:?}"
    );
    drop(fresh);
}
