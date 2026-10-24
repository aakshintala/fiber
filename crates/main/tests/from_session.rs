//! A session-owned process is refused `from_session` when it answers an
//! approval, steers its own session, or asks for a pairing code
//! (`docs/permissions.md`, "Sessions on the sockets"). The socket client in
//! each test is a shell the session itself runs, so the kernel sees a
//! session process in its ancestry; a client the test process opens
//! directly is never session-owned.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{
    HubProc, SessionGuard, Setup, Socket, connect_hub, function_call, hello, recv_answer,
    start_session, stream, subscribe, until,
};

/// A probe the session's own `shell` runs: it opens the hub socket, sends
/// one command line, and prints the command's answer. The probe is a child
/// of the session process, so the kernel walk from its socket passes a
/// session process.
const PROBE: &str = r#"import json, socket, sys
path = sys.argv[1]
lines = [json.loads(a) for a in sys.argv[2:]]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(path)
f = s.makefile("r")
f.readline()
for line in lines:
    cid = line["id"]
    s.sendall((json.dumps(line) + "\n").encode())
    while True:
        l = f.readline()
        if not l:
            print("SOCKET_CLOSED")
            sys.exit(0)
        try:
            v = json.loads(l)
        except Exception:
            continue
        p = v.get("payload", {})
        if isinstance(p, dict) and p.get("command_id") == cid:
            if line is lines[-1]:
                print(json.dumps(v))
            break
"#;

/// Writes the probe beside the workspace and returns its path.
fn write_probe(setup: &Setup) -> String {
    let path = setup.workspace().join("probe.py");
    fs::write(&path, PROBE).unwrap();
    path.to_string_lossy().into_owned()
}

/// Sends `shell` running the probe with `line` to `session` through the
/// hub, and returns the probe's printed answer line.
fn probe_answer(
    client: &Socket,
    session: &str,
    probe: &str,
    hub_socket: &str,
    line: &Value,
) -> Value {
    probe_answer_via(client, session, probe, hub_socket, true, line)
}

/// As [`probe_answer`], subscribing the probe's own hub connection to
/// `session` first when `subscribe_first`: relayed session commands need
/// the connection subscribed, and the refusal under test applies after
/// that check.
fn probe_answer_via(
    client: &Socket,
    session: &str,
    probe: &str,
    hub_socket: &str,
    subscribe_first: bool,
    line: &Value,
) -> Value {
    let mut args = format!("python3 {probe} {hub_socket}");
    if subscribe_first {
        let sub = json!({"id": "c_probe_sub", "session_id": session, "command": "subscribe", "args": {"level": "full"}});
        args.push_str(&format!(" '{}'", sub));
    }
    args.push_str(&format!(" '{}'", line));
    let command = args;
    client.send(
        &json!({
            "id": "c_shell",
            "session_id": session,
            "command": "shell",
            "args": {"command": command, "send": false},
        })
        .to_string(),
    );
    let ack = recv_answer(client, "c_shell", "the shell acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let result = &ack["payload"]["result"];
    let text = result
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| result.to_string());
    let printed = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("the shell printed no answer line: {ack}"));
    serde_json::from_str(printed).unwrap()
}

/// Starts the hub, arms the workspace guard, and starts one idle session.
fn start_hub_and_session(
    setup: &Setup,
    server: &fakes::ProviderServer,
    content: &str,
) -> (Arc<Mutex<Option<HubProc>>>, Socket, SessionGuard, String) {
    setup.provider(server);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, content);
    subscribe(&client, &session);
    (hub, client, guard, session)
}

#[track_caller]
fn finish(
    hub: Arc<Mutex<Option<HubProc>>>,
    client: Socket,
    guard: SessionGuard,
    sessions: &[&str],
) {
    for session in sessions {
        client
            .send(&json!({"id": "c_close", "session_id": session, "command": "close"}).to_string());
    }
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

#[test]
fn shell_child_reply_to_approval_is_refused_from_session() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        serde_json::json!({"decision": "ask", "tool": "shell", "prefix": "echo"}).to_string(),
    )
    .unwrap();
    let server = fakes::ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let (hub, client, guard, session) = start_hub_and_session(&setup, &server, "run it");
    let requested = until(&client, "the approval request", |line| {
        line["kind"] == "permission_requested"
    });
    let request_id = requested
        .iter()
        .find(|line| line["kind"] == "permission_requested")
        .expect("the approval was raised")["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let answer = probe_answer(
        &client,
        &session,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_reply",
            "session_id": session,
            "command": "reply",
            "args": {"request_id": request_id, "decision": "allow"},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");

    // The request stays pending: the person's own client can still answer it.
    client.send(
        &json!({
            "id": "c_person_reply",
            "session_id": session,
            "command": "reply",
            "args": {"request_id": request_id, "decision": "deny"},
        })
        .to_string(),
    );
    let resolved = recv_answer(&client, "c_person_reply", "the person's reply");
    assert_eq!(resolved["kind"], "command_accepted", "{resolved}");
    finish(hub, client, guard, &[&session]);
}

#[test]
fn shell_child_pairing_code_is_refused_from_session() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([hello(), hello()]).unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let (hub, client, guard, session) = start_hub_and_session(&setup, &server, "hello");
    let answer = probe_answer_via(
        &client,
        &session,
        &probe,
        &hub_socket,
        false,
        &json!({"id": "c_probe_pair", "command": "pairing_code", "args": {"device": "probe"}}),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    finish(hub, client, guard, &[&session]);
}

#[test]
fn shell_child_steer_of_own_session_is_refused_from_session() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([hello(), hello(), hello()]).unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let (hub, client, guard, session) = start_hub_and_session(&setup, &server, "hello");
    let answer = probe_answer(
        &client,
        &session,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_steer",
            "session_id": session,
            "command": "steer",
            "args": {"content": [{"type": "text", "text": "from the shell"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    finish(hub, client, guard, &[&session]);
}
