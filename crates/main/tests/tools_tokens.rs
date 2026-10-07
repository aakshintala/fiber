//! The `tools` command gives sizes in tokens after the first request
//! (see #863): before it, sizes are bytes only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::*;

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// `fiber ask` with its stdout kept drained and its stderr kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    stdout: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

fn start(setup: &Setup, args: &[&str]) -> Running {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_copy = Arc::clone(&stderr_text);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut reader, &mut buf) {
            Ok(_) | Err(_) => {}
        }
        *stderr_copy.lock().unwrap() = buf;
    });
    let (tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    Running {
        child,
        watchdog,
        group,
        stdout: stdout_rx,
        stderr: stderr_text,
    }
}

fn first_line(stdout: &mpsc::Receiver<String>) -> Value {
    serde_json::from_str(
        &stdout
            .recv_timeout(DEADLINE)
            .expect("waited for fiber_started"),
    )
    .unwrap()
}

fn answered<'a>(lines: &'a [Value], id: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["payload"]["command_id"] == id)
        .unwrap_or_else(|| panic!("no answer for {id}"))
}

fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = finished
        .recv_timeout(DEADLINE)
        .expect("waited for fiber to exit")
        .unwrap();
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(!group_alive(group), "fiber left a process in its group");
    drop(stdout);
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_tools_command_gives_tokens_after_the_first_request() {
    let setup = Setup::new();
    let note = "alpha line\n";
    fs::write(setup.workspace().join("note.txt"), note).unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    server.hold();
    let running = start(&setup, &["ask", "read the note"]);
    let started = first_line(&running.stdout);
    let session_id = started["session_id"].as_str().unwrap().to_owned();
    assert!(
        server.await_requests(1, DEADLINE),
        "the held response was requested"
    );

    let client = Socket::connect(&setup.session_socket(&session_id));
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    assert_eq!(
        recv(&client, "the subscribe acknowledgement")["payload"]["command_id"],
        "c_sub"
    );

    client.send(r#"{"id":"c_t0","command":"tools"}"#);
    let before = until(&client, "the answer to c_t0", |line| {
        line["payload"]["command_id"] == "c_t0"
    });
    let missing = answered(&before, "c_t0");
    assert_eq!(missing["kind"], "command_accepted", "{missing}");
    let tools0 = missing["payload"]["result"]["tools"].as_array().unwrap();
    assert!(!tools0.is_empty());
    for tool in tools0 {
        assert!(tool.get("tokens").is_none(), "{tool}");
    }

    server.release_one();
    assert!(
        server.await_requests(2, DEADLINE),
        "the tool call sent a second request"
    );

    client.send(r#"{"id":"c_t1","command":"tools"}"#);
    let after = until(&client, "the answer to c_t1", |line| {
        line["payload"]["command_id"] == "c_t1"
    });
    let present = answered(&after, "c_t1");
    assert_eq!(present["kind"], "command_accepted", "{present}");
    let tools1 = present["payload"]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools1.len(), tools0.len());
    for (before_tool, tool) in tools0.iter().zip(tools1.iter()) {
        assert_eq!(tool["name"], before_tool["name"]);
        assert_eq!(tool["bytes"], before_tool["bytes"]);
        assert!(tool.get("tokens").is_some(), "{tool}");
    }
    assert!(
        tools1
            .iter()
            .any(|tool| tool["tokens"].as_u64().is_some_and(|tokens| tokens > 0)),
        "{tools1:?}",
    );

    server.release();
    until(&client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    finish(running);
}
