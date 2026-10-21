//! The model lists two real ask sessions and silent, stale and delegate sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]
mod support;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use support::{Setup, Socket, function_call, hello, run_to_exit, stream};

fn ask(setup: &Setup, args: &[&str], workspace: &Path) -> Vec<Value> {
    let mut command = setup.fiber(args);
    command.current_dir(workspace);
    let output = run_to_exit(setup.deadline, "listing ask", command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn line<'a>(lines: &'a [Value], kind: &str) -> &'a Value {
    let found: Vec<_> = lines.iter().filter(|line| line["kind"] == kind).collect();
    assert_eq!(found.len(), 1, "one {kind}");
    found[0]
}
fn listing(lines: &[Value]) -> Value {
    let completed = line(lines, "tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    let content = completed["payload"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn lists_running_sessions_marks_itself_and_waits_out_a_silent_one() {
    let setup = Setup::new();
    let deadline = setup.deadline;
    let w1 = fs::canonicalize(setup.workspace()).unwrap();
    let w2 = setup.root.path().join("w2");
    let w3 = setup.root.path().join("w3");
    fs::create_dir(&w2).unwrap();
    // The session reports its canonical cwd (/var is /private/var on macOS).
    let w2 = fs::canonicalize(w2).unwrap();
    fs::create_dir(&w3).unwrap();
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    let server = ProviderServer::start([
        Response::stall(200, prefix, prefix.len() + 100_000)
            .header("content-type", "text/event-stream"),
        stream(&[function_call("call_list", "session_list", &json!({}))]),
        hello(),
        stream(&[function_call("call_list2", "session_list", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let mut command = setup.fiber(&["ask", "fix the parser"]);
    command.current_dir(&w2);
    let (mut b, watchdog) = support::spawn_watched(&mut command);
    let group = b.id();
    let guard = support::KillGroup(group);
    let stdout = b.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let (eof, read_end) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
        if let Ok(()) = eof.send(()) {}
    });
    let first: Value =
        serde_json::from_str(&deadline.recv_or_fail(&rx, "B session started")).unwrap();
    assert_eq!(first["kind"], "session_started");
    let b_id = first["session_id"].as_str().unwrap().to_owned();
    assert!(server.await_requests(1, deadline));
    let socket = Socket::connect(deadline, &setup.session_socket(&b_id));
    socket.send(
        &json!({"id":doors::mint("c_"),"command":"subscribe","args":{"level":"summary"}})
            .to_string(),
    );
    let b_status = support::until(&socket, "B summary names its prompt", |line| {
        line["kind"] == "session_status" && line["payload"]["name"] == "fix the parser"
    });
    assert_eq!(
        b_status.last().unwrap()["payload"]["name"],
        "fix the parser"
    );
    drop(socket);

    let run = setup.home().join("run");
    let silent_id = doors::mint("s_");
    let silent = UnixListener::bind(run.join(&silent_id)).unwrap();
    let stale_id = doors::mint("s_");
    drop(UnixListener::bind(run.join(&stale_id)).unwrap());
    let hub = UnixListener::bind(run.join("hub")).unwrap();
    let delegate_id = doors::mint("s_");
    let delegate = UnixListener::bind(run.join(&delegate_id)).unwrap();
    let parent = b_id.clone();
    let delegate_workspace = w3.clone();
    let (done, peer_done) = mpsc::channel();
    thread::spawn(move || {
        delegate.set_nonblocking(true).unwrap();
        for _ in 0..2 {
            let mut fds = [rustix::event::PollFd::new(
                &delegate,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::event::Timespec::try_from(deadline.left()).unwrap();
            rustix::event::poll(&mut fds, Some(&timeout)).unwrap();
            let (mut stream, _) = delegate.accept().unwrap();
            // macOS inherits the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(deadline.left())).unwrap();
            stream.set_write_timeout(Some(deadline.left())).unwrap();
            let mut raw = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut raw)
                .unwrap();
            let command: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(command["command"], "subscribe");
            assert_eq!(command["args"], json!({"level":"summary"}));
            writeln!(stream, "{}", json!({"kind":"command_accepted","schema_version":contract::SCHEMA_VERSION,"payload":{"command_id":command["id"]}})).unwrap();
            writeln!(stream, "{}", json!({"kind":"session_status","payload":{
                "name":"delegate", "workspace":delegate_workspace, "parent":parent,
                "model":"m", "state":"idle", "since":0,"started":0,"calls":0,"turns":0,
                "spend":{"tokens":{"input":0,"cache_read":0,"cache_write":{},"output":0},"cost":0.0,"subscription_cost":0.0},
                "delegates":0,"jobs":0,"project":"p","clients":0
            }})).unwrap();
        }
        done.send(()).unwrap();
    });

    // The status observer folds a session's lines on its own thread, so A's
    // name reaches its summary after the prompt, later where git is slow
    // (macOS). Hold A's first model call until its summary names the prompt.
    server.hold_from(2, fakes::deadline::BUDGET);
    let mut command = setup.fiber(&["ask", "list the sessions"]);
    command.current_dir(&w1);
    let (mut a, a_watchdog) = support::spawn_watched(&mut command);
    let a_stdout = a.stdout.take().unwrap();
    let (a_tx, a_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(a_stdout).lines() {
            if a_tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first_a = deadline.recv_or_fail(&a_rx, "A session started");
    let a_socket = serde_json::from_str::<Value>(&first_a).unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let socket = Socket::connect(deadline, &setup.session_socket(&a_socket));
    socket.send(
        &json!({"id":doors::mint("c_"),"command":"subscribe","args":{"level":"summary"}})
            .to_string(),
    );
    support::until(&socket, "A summary names its prompt", |line| {
        line["kind"] == "session_status" && line["payload"]["name"] == "list the sessions"
    });
    drop(socket);
    server.release();
    let mut lines = vec![serde_json::from_str::<Value>(&first_a).unwrap()];
    while let Ok(line) = deadline.recv(&a_rx) {
        lines.push(serde_json::from_str(&line).unwrap());
    }
    assert!(a.wait().unwrap().success(), "A exits cleanly");
    a_watchdog.stand_down(deadline.cleanup_phase());
    assert_eq!(support::kinds(&lines), support::read_kinds());
    let a_id = line(&lines, "session_started")["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let effects = &line(&lines, "tool_call_started")["payload"];
    assert_eq!(effects["effects"], json!(["reads"]));
    assert_eq!(effects["paths"], json!([format!("{}/", run.display())]));
    let expected = |silent: bool| {
        let mut entries = vec![
            json!({"id":a_id,"name":"list the sessions","workspace":w1,"answering":true,"self":true}),
            json!({"id":b_id,"name":"fix the parser","workspace":w2,"answering":true}),
            json!({"id":delegate_id,"name":"delegate","workspace":w3,"parent":b_id,"answering":true}),
        ];
        if silent {
            entries.push(json!({"id":silent_id,"answering":false}));
        }
        entries.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Value::Array(entries)
    };
    assert_eq!(listing(&lines), expected(true));
    let requests = server.requests();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let definition = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "session_list")
        .unwrap();
    assert_eq!(definition["strict"], true);
    assert_eq!(
        definition["parameters"],
        json!({"type":"object","properties":{},"required":[],"additionalProperties":false})
    );

    drop(silent);
    let resumed = ask(&setup, &["ask", "--resume", &a_id, "list again"], &w1);
    let expected_kinds: Vec<_> = support::read_kinds()
        .into_iter()
        .filter(|kind| !["session_started", "opening_message"].contains(kind))
        .collect();
    assert_eq!(support::kinds(&resumed), expected_kinds);
    assert_eq!(listing(&resumed), expected(false));
    deadline.recv_or_fail(&peer_done, "delegate answered both calls");
    hub.set_nonblocking(true).unwrap();
    assert_eq!(
        hub.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    support::kill_group(deadline, group, "TERM").unwrap();
    let (tx, reaped) = mpsc::channel();
    thread::spawn(move || {
        tx.send(b.wait().unwrap()).unwrap();
    });
    deadline.cleanup_phase().recv_or_fail(&reaped, "B reaped");
    deadline
        .cleanup_phase()
        .recv_or_fail(&read_end, "B stdout closed");
    assert!(!support::group_alive(deadline, group));
    watchdog.stand_down(deadline.cleanup_phase());
    std::mem::forget(guard);
}
