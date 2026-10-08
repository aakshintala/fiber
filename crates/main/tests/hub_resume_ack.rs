//! Binary-level test for #1304 (`docs/invocation.md`, "What each command
//! does"): a client through `fiber hub serve` pipelines `subscribe` and its
//! next command, the session is killed after accepting, and the command
//! after the resume succeeds instead of being rejected `not_subscribed`.
//! It asserts the end state with distinct command ids. It cannot force the
//! interleaving where the session dies before the relay reads the
//! acceptance, so it may pass without the fix; the deterministic red is the
//! hub test.

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
use serde_json::Value;
use support::{HubProc, SessionGuard, connect_hub, run_to_exit};

#[test]
fn pipelined_subscribe_is_kept_across_resume() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    let output = run_to_exit(
        setup.deadline,
        "`fiber ask` to exit",
        setup.fiber(&["ask", "one"]),
    );
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let id = lines[0]["session_id"].as_str().unwrap().to_owned();

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    // Piped as the terminal opens a row: `subscribe` with its commands.
    // Distinct ids, so an acknowledgement names its own command.
    client.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    client.send(&format!(
        "{{\"id\":\"c_tools\",\"session_id\":\"{id}\",\"command\":\"tools\"}}"
    ));
    let sub_lines = support::until(&client, "the pipelined subscribe acknowledgement", |line| {
        line["payload"]["command_id"] == "c_sub"
            && (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
    });
    let sub_ack = sub_lines.last().unwrap();
    assert_eq!(sub_ack["kind"], "command_accepted", "{sub_ack}");
    let tools_lines = support::until(&client, "the pipelined tools acknowledgement", |line| {
        line["payload"]["command_id"] == "c_tools"
            && (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
    });
    let tools_ack = tools_lines.last().unwrap();
    assert_eq!(tools_ack["kind"], "command_accepted", "{tools_ack}");
    // Killed after accepting: the close exits the session.
    client.send(&format!(
        "{{\"id\":\"c_close\",\"session_id\":\"{id}\",\"command\":\"close\"}}"
    ));
    let exited = support::until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    assert!(
        !exited.iter().any(|line| line["payload"]["code"] == "not_subscribed"),
        "{exited:?}"
    );
    // The next command resumes and succeeds at the kept level.
    client.send(&format!(
        "{{\"id\":\"c_next\",\"session_id\":\"{id}\",\"command\":\"tools\"}}"
    ));
    let ack = support::recv_reply(&client, "the acknowledgement after resume");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_next");
    drop(client);
    hub.lock().unwrap().take().expect("the hub ran").kill_and_wait();
    guard.wait_gone();
}
