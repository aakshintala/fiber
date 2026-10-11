//! Binary-level tests of the internal hub command
//! (`docs/invocation.md`, "The hub"): the built `fiber` runs `hub serve`
//! in its own process group with its own `FIBER_HOME`, holding an ordinary
//! provider whose base URL is the fake server. Clients reach sessions
//! through the hub, and the hub outlives no session.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;

use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

#[test]
fn a_turn_runs_through_the_hub_and_the_hub_outlives_no_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, hello) = connect_hub(&setup, &hub);
    assert_eq!(hello["kind"], "hub_hello");
    assert_eq!(hello["payload"]["fiber_version"], env!("CARGO_PKG_VERSION"));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &session);
    let rest = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        rest.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "the fake model's text arrived through the hub: {rest:?}"
    );
    drop(client);
    // SIGKILL the hub: the session keeps running on its own socket.
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    let status = hub.kill_and_wait();
    assert!(!status.success());
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    assert!(
        !setup.session_socket(&session).exists(),
        "the closed session unlinked its socket"
    );
    guard.wait_gone();
}

#[test]
fn prompts_sent_through_the_hub_page_back_newest_first_and_ask_adds_none() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, "first-prompt");
    subscribe(&client, &session);
    until(&client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    client.send(&format!(
        "{{\"id\":\"c_p2\",\"session_id\":\"{session}\",\"command\":\"prompt\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"second-prompt\"}}]}}}}"
    ));
    let lines = until(&client, "the second prompt's acknowledgement", |line| {
        line["payload"]["command_id"] == "c_p2"
    });
    assert_eq!(
        lines.last().unwrap()["kind"],
        "command_accepted",
        "{lines:?}"
    );
    until(&client, "the second turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    // `fiber ask` in the same workspace shares the project and appends nothing.
    let mut ask = setup.fiber(&["ask", "asked-prompt"]);
    ask.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", ask);
    assert!(output.status.success(), "{output:?}");
    let projects: Vec<_> = fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        projects.len(),
        1,
        "the ask shares the project: {projects:?}"
    );
    let sessions = fs::read_dir(
        setup
            .home()
            .join("projects")
            .join(&projects[0])
            .join("sessions"),
    )
    .unwrap()
    .count();
    assert_eq!(sessions, 2, "the hub's session and the ask's");
    client.send(&format!(
        "{{\"id\":\"c_hist\",\"command\":\"prompt_history\",\"args\":{{\"project\":\"{}\"}}}}",
        projects[0]
    ));
    let lines = until(&client, "the prompt_history answer", |line| {
        line["payload"]["command_id"] == "c_hist"
    });
    let answer = lines.last().unwrap();
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    let result = &answer["payload"]["result"];
    let texts: Vec<_> = result["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            assert_eq!(line["session_id"], session.as_str(), "{line}");
            assert!(line["ts"].is_u64(), "{line}");
            line["content"][0]["text"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(texts, ["second-prompt", "first-prompt"]);
    assert!(result.get("before").is_none(), "no older page: {result}");
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
}

/// One side of a two-thread meeting with a deadline: each side waits
/// under the test's [`Deadline`] for the other, and the other failing ends
/// the wait at once.
struct Meet {
    arrived: mpsc::Sender<()>,
    other: mpsc::Receiver<()>,
    deadline: Deadline,
}

impl Meet {
    fn pair(deadline: Deadline) -> (Self, Self) {
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        (
            Self {
                arrived: a_tx,
                other: b_rx,
                deadline,
            },
            Self {
                arrived: b_tx,
                other: a_rx,
                deadline,
            },
        )
    }

    /// Arrives, then waits for the other side, naming `what` on failure.
    #[track_caller]
    fn meet(&self, what: &str) {
        match self.arrived.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        match self.deadline.recv(&self.other) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the other client at {what}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the other client failed before {what}")
            }
        }
    }
}

#[test]
fn two_racing_clients_share_one_hub() {
    let setup = Setup::new();
    let (mine, theirs) = Meet::pair(setup.deadline);
    let setup = &setup;
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            theirs.meet("both clients connecting");
            let hub = Arc::new(Mutex::new(None));
            let (client, hello) = connect_hub(setup, &hub);
            // Both clients are registered once both hellos arrived.
            theirs.meet("both hellos");
            client.send(r#"{"id":"c_status","command":"status","args":{}}"#);
            let status = recv(&client, "the status acknowledgement");
            // Both stay open until both `status` answers are read.
            theirs.meet("both status answers");
            drop(client);
            (hello, status, hub)
        });
        mine.meet("both clients connecting");
        let hub = Arc::new(Mutex::new(None));
        let (client, hello) = connect_hub(setup, &hub);
        mine.meet("both hellos");
        client.send(r#"{"id":"c_status","command":"status","args":{}}"#);
        let status = recv(&client, "the status acknowledgement");
        mine.meet("both status answers");
        let (other_hello, other_status, other_hub) = first.join().unwrap();
        assert_eq!(hello["kind"], "hub_hello");
        assert_eq!(other_hello["kind"], "hub_hello");
        assert_eq!(status["payload"]["result"]["clients"], 2);
        assert_eq!(other_status["payload"]["result"]["clients"], 2);
        // One hub: exactly one `hub_started` line.
        let started = setup
            .hub_log()
            .lines()
            .filter(|line| line.contains("\"code\":\"hub_started\""))
            .count();
        assert_eq!(started, 1);
        drop(client);
        for slot in [&hub, &other_hub] {
            if let Some(running) = slot.lock().unwrap().take() {
                running.kill_and_wait();
            }
        }
    });
}

#[test]
fn sigterm_stops_the_hub_and_leaves_sessions_accepting() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let session = start_session(&client, &workspace, PROMPT);
    subscribe(&client, &session);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    let status = hub.wait();
    assert_eq!(status.code(), Some(143));
    // The session keeps accepting on its own socket.
    let direct = Socket::connect(setup.deadline, &setup.session_socket(&session));
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
    drop(client);
    let log = setup.hub_log();
    for code in [
        "hub_started",
        "client_connected",
        "session_started",
        "hub_stopped",
    ] {
        assert!(
            log.contains(&format!("\"code\":\"{code}\"")),
            "{code}:\n{log}"
        );
    }
    assert!(log.contains("The hub stopped: signal."));
    for line in log.lines() {
        assert!(!line.contains(PROMPT), "no prompt text in the log");
        assert!(!line.contains(&workspace), "no workspace path in the log");
    }
}

#[test]
fn an_idle_hub_exits_on_its_own() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 200}}),
    );
    let hub = HubProc::spawn(&setup);
    let status = hub.wait();
    assert_eq!(status.code(), Some(0));
    assert!(!setup.hub_socket().exists());
    let lines = hub_log(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "{lines:?}"
    );
}

#[test]
fn a_debug_hub_writes_peak_memory_just_before_it_stops() {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"diagnostics": {"level": "debug"}, "hub": {"idle_exit_ms": 200}}),
    );
    let hub = HubProc::spawn(&setup);
    assert_eq!(hub.wait().code(), Some(0));
    let lines = hub_log(&setup);
    let [.., peak, stopped] = lines.as_slice() else {
        panic!("fewer than two lines: {lines:?}");
    };
    assert_eq!(stopped["code"], "hub_stopped", "{lines:?}");
    assert_eq!(peak["code"], "peak_memory", "{lines:?}");
    assert_eq!(peak["level"], "debug");
    assert_eq!(peak["process"], "hub");
    assert!(peak["data"]["peak_kib"].as_u64().unwrap() > 0, "{peak}");
}

/// The parsed lines of the hub's diagnostic log.
fn hub_log(setup: &Setup) -> Vec<Value> {
    fs::read_to_string(setup.home().join("logs").join("hub.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_relative_workspace_is_invalid_arguments() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    client.send(r#"{"id":"c_start","command":"start","args":{"workspace":"relative/path"}}"#);
    let rejected = recv(&client, "the start rejection");
    assert_eq!(rejected["kind"], "command_rejected");
    assert_eq!(rejected["payload"]["code"], "invalid_arguments");
    assert_eq!(rejected["payload"]["command_id"], "c_start");
    drop(client);
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}

#[test]
fn a_session_left_running_is_killed_by_its_guard() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let mut guard = SessionGuard::arm(setup.deadline, &workspace);
    start_session(&client, &workspace, PROMPT);
    // The watchdog stands down first, so only the drop can kill.
    guard.stand_down_watchdog();
    drop(guard);
    fakes::try_matching_exits(&workspace, setup.deadline.left()).unwrap_or_else(|err| {
        panic!("waited until the deadline for the session to die after its guard dropped: {err}")
    });
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
}

#[test]
fn a_session_stuck_in_setup_is_killed_by_its_guard() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    // An MCP server that never answers holds the session in setup, before
    // its log and lock exist. Its command line carries a marker.
    let marker = format!("{workspace}/blocked-mcp");
    let fifo = setup.root.path().join("ready.fifo");
    let mut mkfifo = std::process::Command::new("mkfifo");
    mkfifo
        .arg(&fifo)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let made = run_to_exit(setup.deadline, "mkfifo", mkfifo);
    assert!(made.status.success(), "{made:?}");
    let ready = fakes::children::Ready::at(&fifo, &|| setup.deadline.left());
    let ready_path = ready.path().to_string_lossy().into_owned();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"blocked": {
            "command": "sh",
            "args": ["-c", "echo $$ > \"$1\"; while read -r line; do :; done", marker, ready_path],
            "startup_timeout_ms": 600_000
        }}}}),
    );
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let mut guard = SessionGuard::arm(setup.deadline, &workspace);
    client.send(&format!(
        "{{\"id\":\"c_start\",\"command\":\"start\",\"args\":{{\"workspace\":\"{workspace}\"}}}}"
    ));
    ready.wait(setup.deadline.left());
    let sessions = log::sessions_dir(&setup.home(), &fs::canonicalize(&workspace).unwrap());
    let locks = fs::read_dir(&sessions)
        .map(|dirs| {
            dirs.flatten()
                .filter(|dir| dir.path().join("session.lock").exists())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(locks, 0, "the session is still in setup: no lock yet");
    guard.stand_down_watchdog();
    drop(guard);
    let rejected = recv(&client, "the start rejection");
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    fakes::try_matching_exits(&workspace, setup.deadline.left()).unwrap_or_else(|err| panic!("waited until the deadline for the stuck session and its server to die after the guard dropped: {err}"));
    drop(client);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill_and_wait();
}

/// The lines of `home`'s `logs/hub.log`, parsed.
fn hub_log_lines(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join("logs").join("hub.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_hub_that_cannot_start_says_why_on_stderr() {
    let setup = Setup::new();
    fs::write(setup.home().join("config.json"), "{").unwrap();
    let output = run_to_exit(
        setup.deadline,
        "the failing hub",
        setup.fiber(&["hub", "serve"]),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("fiber: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(!setup.hub_socket().exists());
}

#[test]
fn an_invalid_config_is_written_to_the_hub_log() {
    let setup = Setup::new();
    fs::write(setup.home().join("config.json"), "{").unwrap();
    let output = run_to_exit(
        setup.deadline,
        "the failing hub",
        setup.fiber(&["hub", "serve"]),
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines = hub_log_lines(&setup.home());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "error");
    assert_eq!(lines[0]["process"], "hub");
    assert_eq!(lines[0]["code"], "config_invalid");
    let message = lines[0]["message"].as_str().unwrap();
    assert_eq!(stderr.trim_end(), format!("fiber: {message}"));
}

#[test]
fn a_too_long_fiber_home_is_reported_on_stderr_and_in_the_hub_log() {
    let setup = Setup::new();
    // One component long enough that `<home>/run/hub` passes 107 bytes,
    // the longer of the macOS and Linux limits.
    let home = setup.home().join("p".repeat(120));
    fs::create_dir_all(&home).unwrap();
    let mut command = setup.fiber(&["hub", "serve"]);
    command.env("FIBER_HOME", &home);
    let output = run_to_exit(setup.deadline, "the failing hub", command);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("fiber: "), "{stderr}");
    assert!(stderr.contains("FIBER_HOME"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    let lines = hub_log_lines(&home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["level"], "error");
    assert_eq!(line["process"], "hub");
    assert_eq!(line["code"], "usage");
    assert!(
        line["message"].as_str().unwrap().contains("FIBER_HOME"),
        "{line}"
    );
    assert!(!home.join("run").join("hub").exists());
}

#[test]
#[should_panic(expected = "for the hub to start and say hub_hello")]
fn a_hub_that_never_says_hello_fails_the_connect_wait() {
    let setup = Setup::new();
    let run = setup.home().join("run");
    fs::create_dir_all(&run).unwrap();
    // Accepts connections in the kernel's backlog and never speaks.
    let _silent = std::os::unix::net::UnixListener::bind(run.join("hub")).unwrap();
    let hub = Arc::new(Mutex::new(None));
    connect_hub_within(&setup, &hub, std::time::Duration::from_millis(500));
}

#[test]
fn read_file_answers_an_artifact_and_refuses_an_escape() {
    let setup = Setup::new();
    let session = setup
        .home()
        .join("projects")
        .join("k")
        .join("sessions")
        .join("s_00000000000000f1");
    fs::create_dir_all(session.join("artifacts")).unwrap();
    fs::write(
        session.join("events.jsonl"),
        "{\"kind\":\"session_started\",\"seq\":0}\n",
    )
    .unwrap();
    fs::write(session.join("artifacts").join("a.txt"), b"hello").unwrap();
    let marker = "marker-outside-artifacts-9f2c";
    fs::write(session.parent().unwrap().join("secret"), marker).unwrap();
    std::os::unix::fs::symlink("../../secret", session.join("artifacts").join("out.txt")).unwrap();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    client.send(r#"{"id":"c_r1","command":"read_file","args":{"session":"s_00000000000000f1","path":"artifacts/a.txt"}}"#);
    let answered = recv(&client, "the read_file acknowledgement");
    assert_eq!(answered["kind"], "command_accepted");
    assert_eq!(
        answered["payload"],
        json!({"command_id": "c_r1", "result": {"data": "aGVsbG8=", "mime_type": "text/plain"}})
    );
    let mut lines = vec![answered];
    for (id, path) in [
        ("c_r2", "artifacts/../events.jsonl"),
        ("c_r3", "artifacts/out.txt"),
    ] {
        client.send(&format!(
            "{{\"id\":\"{id}\",\"command\":\"read_file\",\"args\":{{\"session\":\"s_00000000000000f1\",\"path\":\"{path}\"}}}}"
        ));
        let rejected = recv(&client, "the read_file rejection");
        assert_eq!(rejected["kind"], "command_rejected");
        assert_eq!(rejected["payload"]["command_id"], id);
        assert_eq!(rejected["payload"]["code"], "invalid_arguments");
        lines.push(rejected);
    }
    for line in &lines {
        assert!(
            !line.to_string().contains(marker),
            "no escape reached the client: {line}"
        );
    }
    drop(client);
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}
