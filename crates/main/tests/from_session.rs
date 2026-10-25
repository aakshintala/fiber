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
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use serde_json::{Value, json};
use support::{
    HubProc, SessionGuard, Setup, Socket, connect_hub, function_call, hello, recv_answer,
    start_session, stream, subscribe, text_reply, until,
};

/// A probe the session's own `shell` runs: it opens the hub socket, sends
/// one command line, and prints the command's answer. The probe is a child
/// of the session process, so the kernel walk from its socket passes a
/// session process.
const PROBE: &str = r#"import json, socket, sys
path = sys.argv[1]
target = sys.argv[2]
sub_id = sys.argv[3]
lines = [json.loads(a) for a in sys.argv[4:]]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(path)
f = s.makefile("r")
f.readline()


def wait_for(cid):
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
            return v


if sub_id != "-":
    sub = {"id": sub_id, "session_id": target, "command": "subscribe", "args": {"level": "full"}}
    s.sendall((json.dumps(sub) + "\n").encode())
    wait_for(sub_id)
for line in lines:
    cid = line["id"]
    s.sendall((json.dumps(line) + "\n").encode())
    if line is lines[-1]:
        print(json.dumps(wait_for(cid)))
    else:
        wait_for(cid)
"#;

/// Writes the probe beside the workspace and returns its path.
fn write_probe(setup: &Setup) -> String {
    let path = setup.workspace().join("probe.py");
    fs::write(&path, PROBE).unwrap();
    path.to_string_lossy().into_owned()
}

/// A probe the session's shell runs to steer an ask session the same shell
/// started in the background: the ask's stdout is a fifo, so the probe
/// blocks reading its first lines until the ask session's id appears, then
/// subscribes to the ask and steers it. Both the ask and the probe are
/// children of the shell, so neither is the other's parent.
const PROBE_FIFO: &str = r#"import json, socket, sys, threading
fifo_path = sys.argv[1]
hub_path = sys.argv[2]
fifo = open(fifo_path)
ask_id = None
for line in fifo:
    try:
        v = json.loads(line)
    except Exception:
        continue
    if isinstance(v, dict) and v.get("session_id"):
        ask_id = v["session_id"]
        break
print(json.dumps({"ask_session": ask_id}), flush=True)

def drain():
    for _ in fifo:
        pass
threading.Thread(target=drain, daemon=True).start()

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(hub_path)
f = s.makefile("r")
f.readline()
sub = {"id": "c_fifo_sub", "session_id": ask_id, "command": "subscribe", "args": {"level": "full"}}
s.sendall((json.dumps(sub) + "\n").encode())
while True:
    v = json.loads(f.readline())
    if isinstance(v, dict) and v.get("payload", {}).get("command_id") == "c_fifo_sub":
        break
steer = {"id": "c_fifo_steer", "session_id": ask_id, "command": "steer", "args": {"content": [{"type": "text", "text": "from the shell"}]}}
s.sendall((json.dumps(steer) + "\n").encode())
while True:
    v = json.loads(f.readline())
    if isinstance(v, dict) and v.get("payload", {}).get("command_id") == "c_fifo_steer":
        print(json.dumps(v), flush=True)
        break
"#;

/// Writes the fifo probe beside the workspace and returns its path.
fn write_fifo_probe(setup: &Setup) -> String {
    let path = setup.workspace().join("probe_fifo.py");
    fs::write(&path, PROBE_FIFO).unwrap();
    path.to_string_lossy().into_owned()
}

/// A `fiber ask` child of the test with its stdout lines sent over a
/// channel: the test reads the ask session's id off the stream while the
/// drain keeps the ask from blocking on a full pipe. Stderr is drained the
/// same way and dropped.
struct Ask {
    child: Child,
    group: u32,
    lines: mpsc::Receiver<String>,
}

/// Starts `fiber ask` with `args` as a child of the test, in its own
/// process group. The prompt names the workspace so the workspace guard
/// reaps the child when the test fails before its explicit wait.
fn spawn_ask(setup: &Setup, args: &[&str]) -> Ask {
    use std::os::unix::process::CommandExt as _;
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(args)
        .current_dir(setup.workspace())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.home().parent().unwrap())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout).lines() {
            match tx.send(line.unwrap_or_default()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    thread::spawn(move || {
        use std::io::Read as _;
        let mut sink = Vec::new();
        let _drained = std::io::BufReader::new(stderr).read_to_end(&mut sink);
    });
    Ask {
        child,
        group,
        lines,
    }
}

/// The ask session's id, read off the ask child's stdout stream within the
/// one deadline: every receive takes what remains of it, and expiry names
/// the wait.
fn ask_session_id(ask: &Ask, deadline: fakes::Deadline) -> String {
    loop {
        match deadline.recv(&ask.lines) {
            Ok(line) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && let Some(id) = value.get("session_id").and_then(Value::as_str)
                {
                    return id.to_owned();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the ask session's id")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the ask child exited before printing its session id")
            }
        }
    }
}

/// Waits under the deadline for the ask child to exit, killing its group
/// when it does not.
fn wait_ask(ask: Ask, deadline: fakes::Deadline, what: &str) {
    let Ask {
        mut child,
        group,
        lines: _,
    } = ask;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    match deadline.recv(&finished) {
        Ok(status) => {
            status.expect("the ask child's wait keeps its status");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            support::expired(deadline, group, &finished, &format!("{what} to exit"));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the wait thread of {what} ended before it exited")
        }
    }
    assert!(
        !fakes::group_alive(group),
        "{what} left a process in its group behind"
    );
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
    probe_answer_full(
        client,
        session,
        session,
        &format!("python3 {probe} {hub_socket}"),
        subscribe_first,
        line,
    )
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
    for (n, session) in sessions.iter().enumerate() {
        let id = format!("c_close_{n}");
        client.send(&json!({"id": id, "session_id": session, "command": "close"}).to_string());
        let closed = recv_answer(&client, &id, "the close acknowledgement");
        assert_eq!(closed["kind"], "command_accepted", "{closed}");
    }
    // Wait for every session to exit before killing the hub: a close the
    // hub never forwards leaves the session up and `wait_gone` times out.
    // One wait collects every requested exit: `until` consumes the lines
    // it reads, so a wait per session drops another session's exit when
    // it arrives first and the next wait times out.
    let mut remaining: Vec<&str> = sessions.to_vec();
    until(&client, "the sessions exited", |line| {
        if line["kind"] == "fiber_exited"
            && let Some(exited) = line["session_id"].as_str()
            && let Some(pos) = remaining.iter().position(|wanted| *wanted == exited)
        {
            remaining.remove(pos);
        }
        remaining.is_empty()
    });
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// The job id the `Started delegate <id>.` receipt names.
fn job_id_of(text: &str) -> String {
    let marker = "Started delegate ";
    let start = text.find(marker).unwrap() + marker.len();
    text[start..]
        .split([' ', '.', '"', '\\'])
        .next()
        .unwrap()
        .to_owned()
}

/// As [`probe_answer`], but the probe's parent runs `runner` with the quoted
/// command lines appended (instead of `python3 {probe} {hub_socket}`), and
/// the probe subscribes to `target` while the shell runs in `session`: a
/// fake session ancestor for runners like `xargs sh session ...`.
fn probe_answer_full(
    client: &Socket,
    session: &str,
    target: &str,
    runner: &str,
    subscribe_first: bool,
    line: &Value,
) -> Value {
    // The subscribe id is unique to this probe call: command ids are
    // admitted once across connections, so a second probe reusing
    // `c_probe_sub` would be rejected and proceed unsubscribed.
    let sub_id = if subscribe_first {
        format!("c_sub_{}", line["id"].as_str().unwrap())
    } else {
        "-".to_owned()
    };
    let mut args = format!("{runner} '{target}' '{sub_id}'");
    args.push_str(&format!(" '{line}'"));
    let command = args;
    let shell_id = format!("c_shell_{}", line["id"].as_str().unwrap());
    client.send(
        &json!({
            "id": shell_id,
            "session_id": session,
            "command": "shell",
            "args": {"command": command, "send": false},
        })
        .to_string(),
    );
    let ack = recv_answer(client, &shell_id, "the shell acknowledgement");
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

/// The probe subscribes to `target` while running in `session`'s shell.
fn probe_answer_to(
    client: &Socket,
    session: &str,
    target: &str,
    probe: &str,
    hub_socket: &str,
    line: &Value,
) -> Value {
    probe_answer_full(
        client,
        session,
        target,
        &format!("python3 {probe} {hub_socket}"),
        true,
        line,
    )
}

/// Installs provider `name` with model `m` at `server`'s URL.
fn install_model(setup: &Setup, name: &str, server: &fakes::ProviderServer) {
    let source = setup.root.path().join(format!("src-{name}"));
    let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(name.to_owned(), json!([format!("{}/v1", server.url())]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    support::write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Points the home at `pa/m` with a `pa/m` reviewer and a far idle exit so
/// delegates stay up, and allows the delegate tools plus the probe shell.
fn configure_delegates(setup: &Setup) {
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "delegate_message", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": ""}),
        ),
    )
    .unwrap();
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

#[test]
fn shell_child_prompt_and_question_reply_to_own_session_are_refused() {
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
            "id": "c_probe_prompt",
            "session_id": session,
            "command": "prompt",
            "args": {"content": [{"type": "text", "text": "from the shell"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    assert!(
        answer["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(&session),
        "{answer}"
    );
    let answer = probe_answer(
        &client,
        &session,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_reply",
            "session_id": session,
            "command": "reply",
            "args": {"request_id": "r_1", "confirmed": true},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    assert!(
        answer["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(&session),
        "{answer}"
    );
    finish(hub, client, guard, &[&session]);
}

#[test]
fn steer_to_a_hub_started_session_is_allowed() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    setup.provider(&server);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let first = start_session(&client, &workspace, "first");
    subscribe(&client, &first);
    let second = start_session(&client, &workspace, "second");
    subscribe(&client, &second);
    let answer = probe_answer_to(
        &client,
        &first,
        &second,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_steer",
            "session_id": second,
            "command": "steer",
            "args": {"content": [{"type": "text", "text": "from a sibling"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    finish(hub, client, guard, &[&first, &second]);
}

#[test]
fn question_reply_to_a_hub_started_session_is_allowed() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start_responding(|request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let answered = body["input"].as_array().is_some_and(|input| {
            input
                .iter()
                .any(|item| item["type"] == "function_call_output")
        });
        if answered || !text.contains("second") {
            return hello();
        }
        stream(&[function_call(
            "call_ask",
            "ask_user",
            &json!({"questions": [
                {"header": "Pick", "question": "Which one?",
                 "options": [{"label": "a"}, {"label": "b"}]},
            ]}),
        )])
    })
    .unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    setup.provider(&server);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let first = start_session(&client, &workspace, "first");
    subscribe(&client, &first);
    let second = start_session(&client, &workspace, "second");
    subscribe(&client, &second);
    let asked = until(&client, "the question was asked", |line| {
        line["kind"] == "interaction_requested" && line["session_id"] == second
    });
    let request_id = asked.last().expect("the question arrived")["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let answer = probe_answer_to(
        &client,
        &first,
        &second,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_reply",
            "session_id": second,
            "command": "reply",
            "args": {"request_id": request_id, "answers": [{"labels": ["a"], "text": ""}]},
        }),
    );
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    let resolved = until(&client, "the question resolved", |line| {
        line["kind"] == "interaction_resolved" && line["session_id"] == second
    });
    assert_eq!(
        resolved.last().expect("the resolution arrived")["payload"]["request_id"],
        request_id.as_str(),
        "{resolved:?}"
    );
    finish(hub, client, guard, &[&first, &second]);
}

#[test]
fn session_message_to_itself_is_delivered() {
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
            "id": "c_probe_message",
            "session_id": session,
            "command": "message",
            "args": {"from_session_id": session, "text": "to myself"},
        }),
    );
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    assert!(
        server.await_requests(2, setup.deadline),
        "the message starts a turn"
    );
    finish(hub, client, guard, &[&session]);
}

/// The parent's `delegate_message` to its running delegate is accepted and
/// steers it: the delegate's provider sees a second model call.
#[test]
fn delegate_message_to_a_running_delegate_is_accepted() {
    const MARK: &str = "parent-message-marker-9d3f";
    let setup = Setup::new();
    let server_parent = fakes::ProviderServer::start_responding(|request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("now-message") && !text.contains("call_msg") {
            let job = job_id_of(&text);
            return stream(&[function_call(
                "call_msg",
                "delegate_message",
                &json!({"id": job, "message": MARK}),
            )]);
        }
        if text.contains("Started delegate") {
            return hello();
        }
        stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let server_child =
        fakes::ProviderServer::start([text_reply("d-one"), text_reply("d-two")]).unwrap();
    install_model(&setup, "pa", &server_parent);
    install_model(&setup, "pb", &server_child);
    configure_delegates(&setup);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let parent = start_session(&client, &workspace, "run it");
    subscribe(&client, &parent);
    let started = until(&client, "the delegate started", |line| {
        line["kind"] == "delegate_started"
    });
    let delegate = started.last().expect("the event arrived")["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(delegate, parent, "the delegate is its own session");
    assert!(
        server_child.await_requests(1, setup.deadline),
        "the delegate is running"
    );
    client.send(
        &json!({
            "id": "c_prompt_msg",
            "session_id": parent,
            "command": "prompt",
            "args": {"content": [{"type": "text", "text": "now-message please"}]},
        })
        .to_string(),
    );
    let prompted = recv_answer(&client, "c_prompt_msg", "the prompt acknowledgement");
    assert_eq!(prompted["kind"], "command_accepted", "{prompted}");
    assert!(
        server_child.await_requests(2, setup.deadline),
        "the delegate_message steers the delegate"
    );
    let requested = until(&client, "the delegate_message call", |line| {
        line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_message"
    });
    let action = requested.last().expect("the call arrived")["action_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let done = until(&client, "the delegate_message completed", |line| {
        line["kind"] == "tool_call_completed" && line["action_id"] == action
    });
    assert_eq!(
        done.last().expect("the completion arrived")["payload"]["status"],
        "completed",
        "{done:?}"
    );
    // `now` stops the delegate as a stop does; a graceful close waits out
    // the delegate's far idle exit. The guard reaps the delegate, which
    // shares the parent's workspace, once the parent is gone.
    client.send(
        &json!({"id": "c_close_parent", "session_id": parent, "command": "close", "args": {"now": true}}).to_string(),
    );
    let closed = recv_answer(&client, "c_close_parent", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    until(&client, "the parent exited", |line| {
        line["kind"] == "fiber_exited" && line["session_id"] == parent
    });
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

/// R starts D, D starts G: D's `delegate_message` to G is accepted and
/// steers it, while D's shell child is refused `steer` to G and to R. G's
/// first turn runs a slow tool so its run stays open and bound while the
/// probes run, and D waits out its delegate's job; neither gate seals
/// mid-probe. The queued steer reaches G's next model call.
#[test]
fn nested_delegate_message_accepted_while_shell_steers_refused() {
    const G_MARK: &str = "nested-message-marker-71ac";
    let setup = Setup::new();
    let server_r = fakes::ProviderServer::start_responding(|request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("Started delegate") {
            return hello();
        }
        stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    // The grandchild's first turn runs a slow tool call, so the turn and
    // the run stay open while the probes run: the grandchild is never
    // resumed, stays bound, and stays reachable. The steered message joins
    // the open turn, and the next model call carries it. A question would
    // not hold the run: the turn ends with questions pending and the
    // socket goes away with it.
    let marked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen_mark = Arc::clone(&marked);
    let server_g = fakes::ProviderServer::start_responding(move |request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        if text.contains(G_MARK) {
            seen_mark.store(true, std::sync::atomic::Ordering::SeqCst);
            return text_reply("g-two-done");
        }
        stream(&[function_call(
            "call_sleep",
            "shell",
            &json!({"command": "sleep 5"}),
        )])
    })
    .unwrap();
    let server_g = std::sync::Arc::new(server_g);
    let gated_g = Arc::clone(&server_g);
    let deadline = setup.deadline;
    let server_d = fakes::ProviderServer::start_responding(move |request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("call_g_msg") {
            return hello();
        }
        if !text.contains("Started delegate") {
            return stream(&[spawn_call("fiber:pc/m")]);
        }
        // The delegate_message goes out only once the grandchild runs:
        // its first model call proves its socket is up.
        assert!(
            gated_g.await_requests(1, deadline),
            "the grandchild runs before it is steered"
        );
        let job = job_id_of(&text);
        stream(&[function_call(
            "call_g_msg",
            "delegate_message",
            &json!({"id": job, "message": G_MARK}),
        )])
    })
    .unwrap();
    install_model(&setup, "pa", &server_r);
    install_model(&setup, "pb", &server_d);
    install_model(&setup, "pc", &server_g);
    configure_delegates(&setup);
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let parent = start_session(&client, &workspace, "run it");
    subscribe(&client, &parent);
    let d_started = until(&client, "the delegate started", |line| {
        line["kind"] == "delegate_started"
    });
    let delegate = d_started.last().expect("the event arrived")["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        server_d.await_requests(1, setup.deadline),
        "the delegate is running"
    );
    assert!(
        server_g.await_requests(1, setup.deadline),
        "the grandchild is running"
    );
    // The grandchild's start is durable once it runs: read its id from the
    // delegate's log. Delegates run closed to prompts, so no prompt is
    // sent: the delegate_message goes out on the spawn receipt instead.
    let g_started: Value =
        std::fs::read_to_string(support::session_dir(&setup, &delegate).join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .find(|line: &Value| {
                line["kind"] == "delegate_started" && line["session_id"] == delegate
            })
            .expect("the grandchild started");
    let grandchild = g_started["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Subscribe only now: the replay delivers the requested call. The
    // completion waits past the probes: the steered turn starts only
    // after the release ends the grandchild's first turn.
    subscribe(&client, &delegate);
    let requested = until(&client, "the nested delegate_message call", |line| {
        line["kind"] == "tool_call_requested"
            && line["session_id"] == delegate
            && line["payload"]["name"] == "delegate_message"
    });
    let action = requested.last().expect("the call arrived")["action_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The probes run while the grandchild waits on its question: it is
    // alive, bound, and reachable, and the delegate's gate stays open.
    for (id, target, what) in [
        ("c_probe_steer_g", &grandchild, "to the grandchild"),
        ("c_probe_steer_r", &parent, "to the parent"),
    ] {
        let target: &str = target;
        let answer = probe_answer_to(
            &client,
            &delegate,
            target,
            &probe,
            &hub_socket,
            &json!({
                "id": id,
                "session_id": target,
                "command": "steer",
                "args": {"content": [{"type": "text", "text": "from the delegate shell"}]},
            }),
        );
        assert_eq!(answer["kind"], "command_rejected", "{answer}");
        assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
        assert!(
            answer["payload"]["message"]
                .as_str()
                .unwrap()
                .contains(target),
            "a shell steer {what} is refused for its target: {answer}"
        );
    }
    // The sleep ends on its own: the open turn continues, and its next
    // model call carries the steered message.
    assert!(
        server_g.await_requests(2, setup.deadline),
        "the nested delegate_message steers the grandchild"
    );
    assert!(
        marked.load(std::sync::atomic::Ordering::SeqCst),
        "the steered message reaches the grandchild's next model call"
    );
    let done = until(&client, "the nested delegate_message completed", |line| {
        line["kind"] == "tool_call_completed" && line["action_id"] == action
    });
    assert_eq!(
        done.last().expect("the event arrived")["payload"]["status"],
        "completed",
        "{done:?}"
    );
    // The delegate exits on its own once the grandchild's job ends; the
    // teardown only closes the parent.
    client.send(
        &json!({"id": "c_close_nested", "session_id": parent, "command": "close", "args": {"now": true}}).to_string(),
    );
    let closed = recv_answer(&client, "c_close_nested", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    until(&client, "the parent exited", |line| {
        line["kind"] == "fiber_exited" && line["session_id"] == parent
    });
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

/// A fake session-command ancestor adds refusals: the probe runs under
/// `sh session --id=<fake>`, so steering the fake id is refused while an
/// unrelated session stays allowed.
#[test]
fn fake_session_argv_ancestor_adds_refusal() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([hello(), hello(), hello(), hello()]).unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    setup.provider(&server);
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let first = start_session(&client, &workspace, "first");
    subscribe(&client, &first);
    let second = start_session(&client, &workspace, "second");
    subscribe(&client, &second);
    let fake = "s_abcdef0123456789";
    fs::write(setup.workspace().join("session"), "shift\n\"$@\" & wait\n").unwrap();
    let runner = format!(
        "PATH=\"{}:$PATH\" xargs sh session --id={fake} python3 {probe} {hub_socket} < /dev/null",
        setup.workspace().to_string_lossy(),
    );
    let answer = probe_answer_full(
        &client,
        &first,
        fake,
        &runner,
        true,
        &json!({
            "id": "c_probe_fake",
            "session_id": fake,
            "command": "steer",
            "args": {"content": [{"type": "text", "text": "from the fake"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    assert!(
        answer["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(fake),
        "{answer}"
    );
    let answer = probe_answer_to(
        &client,
        &first,
        &second,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_sibling",
            "session_id": second,
            "command": "steer",
            "args": {"content": [{"type": "text", "text": "from a sibling"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    finish(hub, client, guard, &[&first, &second]);
}

/// Files under `run/` change nothing: a stray file comes and goes, and the
/// owned steer is still refused.
#[test]
fn run_directory_edits_change_nothing() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([hello(), hello(), hello()]).unwrap();
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let (hub, client, guard, session) = start_hub_and_session(&setup, &server, "hello");
    let run = setup.hub_socket().parent().unwrap().to_path_buf();
    fs::write(run.join("stray-test-file"), "stray").unwrap();
    fs::remove_file(run.join("stray-test-file")).unwrap();
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

/// A shell under `fiber ask` answers an approval on the hub: the kernel
/// walk from its socket passes the ask session process, so the reply is
/// refused `from_session`. Nobody can be asked inside an ask session — the
/// ask answers its own approvals — so the probe replies to a fabricated
/// request: the hub refuses before the session ever validates it. The ask
/// stays alive on a sleeping shell call the test closes at the end.
#[test]
fn ask_descendant_reply_to_approval_is_refused_from_session() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        json!({"decision": "allow", "tool": "shell", "prefix": "sleep"}).to_string(),
    )
    .unwrap();
    let server = fakes::ProviderServer::start([
        stream(&[function_call(
            "call_sleep",
            "shell",
            &json!({"command": "sleep 20"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let ask = spawn_ask(&setup, &["ask", &format!("sleeper run it in {workspace}")]);
    let session = ask_session_id(&ask, setup.deadline);
    subscribe(&client, &session);
    assert!(
        server.await_requests(1, setup.deadline),
        "the ask starts its sleeping shell call"
    );

    let answer = probe_answer_to(
        &client,
        &session,
        &session,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_reply",
            "session_id": session,
            "command": "reply",
            "args": {"request_id": "r_1", "decision": "allow"},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");

    client.send(
        &json!({"id": "c_close_ask", "session_id": session, "command": "close", "args": {"now": true}})
            .to_string(),
    );
    let closed = recv_answer(&client, "c_close_ask", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    wait_ask(ask, setup.deadline, "the ask child");
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

/// A shell under `fiber ask` prompts the ask session's own socket: the
/// target is on the connection's ancestor walk, so the prompt is refused
/// `from_session`. The ask stays alive on a sleeping shell call the test
/// closes at the end.
#[test]
fn ask_descendant_prompt_to_own_ask_socket_is_refused_from_session() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}",
            json!({"decision": "allow", "tool": "shell", "prefix": "sleep"}),
            json!({"decision": "ask", "tool": "shell", "prefix": "echo"}),
        ),
    )
    .unwrap();
    let server = fakes::ProviderServer::start([
        stream(&[function_call(
            "call_sleep",
            "shell",
            &json!({"command": "sleep 20"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let ask = spawn_ask(&setup, &["ask", &format!("sleeper run it in {workspace}")]);
    let session = ask_session_id(&ask, setup.deadline);
    subscribe(&client, &session);
    assert!(
        server.await_requests(1, setup.deadline),
        "the ask starts its sleeping shell call"
    );

    let answer = probe_answer_to(
        &client,
        &session,
        &session,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_prompt",
            "session_id": session,
            "command": "prompt",
            "args": {"content": [{"type": "text", "text": "from the shell"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");

    client.send(
        &json!({"id": "c_close_ask", "session_id": session, "command": "close", "args": {"now": true}})
            .to_string(),
    );
    let closed = recv_answer(&client, "c_close_ask", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    wait_ask(ask, setup.deadline, "the ask child");
    drop(client);
    hub.lock().unwrap().take().unwrap().kill_and_wait();
    guard.wait_gone();
}

/// A shell under `fiber ask` steers a session the hub started: the target
/// shares no session process with the connection's walk, so the steer is
/// accepted.
#[test]
fn ask_descendant_steering_an_unrelated_hub_started_session_is_allowed() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        json!({"decision": "allow", "tool": "shell", "prefix": "sleep"}).to_string(),
    )
    .unwrap();
    let server = fakes::ProviderServer::start_responding(|request: &fakes::Request| {
        let text = String::from_utf8_lossy(&request.body);
        if text.contains("sleeper") {
            return stream(&[function_call(
                "call_sleep",
                "shell",
                &json!({"command": "sleep 20"}),
            )]);
        }
        hello()
    })
    .unwrap();
    setup.provider(&server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let probe = write_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let first = start_session(&client, &workspace, "first");
    subscribe(&client, &first);
    let ask = spawn_ask(&setup, &["ask", &format!("sleeper run it in {workspace}")]);
    let session = ask_session_id(&ask, setup.deadline);
    subscribe(&client, &session);

    let answer = probe_answer_to(
        &client,
        &session,
        &first,
        &probe,
        &hub_socket,
        &json!({
            "id": "c_probe_steer",
            "session_id": first,
            "command": "steer",
            "args": {"content": [{"type": "text", "text": "from a sibling"}]},
        }),
    );
    assert_eq!(answer["kind"], "command_accepted", "{answer}");

    client.send(
        &json!({"id": "c_close_ask", "session_id": session, "command": "close", "args": {"now": true}})
            .to_string(),
    );
    let closed = recv_answer(&client, "c_close_ask", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    wait_ask(ask, setup.deadline, "the ask child");
    finish(hub, client, guard, &[&first]);
}

/// A session's shell starts `fiber ask` in the background and steers it
/// from a second client the same shell runs: the ask's parent walk passes
/// the parent session, and the steering client is not the ask's direct
/// parent, so the steer is refused `from_session`.
#[test]
fn shell_started_ask_steered_by_the_shell_is_refused_from_session() {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        json!({"decision": "allow", "tool": "shell", "prefix": "sleep"}).to_string(),
    )
    .unwrap();
    // The ask sleeps once and then answers plainly: every later model call
    // still carries the marker in its history, so answering every one with
    // a sleep would loop forever and the shell would never complete.
    let slept = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = fakes::ProviderServer::start_responding({
        let slept = Arc::clone(&slept);
        move |request: &fakes::Request| {
            let text = String::from_utf8_lossy(&request.body);
            if text.contains("ask-target-marker")
                && !slept.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return stream(&[function_call(
                    "call_sleep",
                    "shell",
                    &json!({"command": "sleep 10"}),
                )]);
            }
            hello()
        }
    })
    .unwrap();
    setup.provider(&server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let probe = write_fifo_probe(&setup);
    let hub_socket = setup.hub_socket().to_string_lossy().into_owned();
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let session = start_session(&client, &workspace, "hello");
    subscribe(&client, &session);
    let fiber = env!("CARGO_BIN_EXE_fiber");
    let fifo = setup
        .workspace()
        .join("ask.fifo")
        .to_string_lossy()
        .into_owned();
    let shell_id = "c_shell_ask";
    client.send(
        &json!({
            "id": shell_id,
            "session_id": session,
            "command": "shell",
            "args": {"command": format!("rm -f {fifo}; mkfifo {fifo}; {fiber} ask --model fake/m 'ask-target-marker run it in {workspace}' > {fifo} 2>&1 & python3 {probe} {fifo} {hub_socket}"), "send": false},
        })
        .to_string(),
    );
    let ack = recv_answer(&client, shell_id, "the shell acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let output = &ack["payload"]["result"];
    let text = output
        .get("output")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| output.to_string());
    let mut ask_id = None;
    let mut answer = None;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('{'))
    {
        let value: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        if value.get("ask_session").is_some() {
            ask_id = value["ask_session"].as_str().map(str::to_owned);
        }
        if value.get("kind").is_some() {
            answer = Some(value);
        }
    }
    let ask_id = ask_id.expect("the probe reports the ask session");
    let answer = answer.expect("the probe prints the steer answer");
    assert_eq!(answer["kind"], "command_rejected", "{answer}");
    assert_eq!(answer["payload"]["code"], "from_session", "{answer}");
    assert!(
        answer["payload"]["message"]
            .as_str()
            .unwrap()
            .contains(&ask_id),
        "{answer}"
    );

    subscribe(&client, &ask_id);
    client.send(
        &json!({"id": "c_close_ask", "session_id": ask_id, "command": "close", "args": {"now": true}})
            .to_string(),
    );
    let closed = recv_answer(&client, "c_close_ask", "the close acknowledgement");
    assert_eq!(closed["kind"], "command_accepted", "{closed}");
    finish(hub, client, guard, &[&session]);
}
