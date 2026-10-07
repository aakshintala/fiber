//! Binary-level tests of `recent.jsonl` and the hub's feed
//! (`docs/invocation.md`, "The hub"; `docs/state.md`, "Recently exited
//! sessions"): every session appends its own row as it exits, and the hub
//! serves `feed` and `recent` across projects.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::*;

/// The rows of `home`'s `recent.jsonl`, parsed.
fn rows(home: &Path) -> Vec<Value> {
    fs::read_to_string(home.join("recent.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The project key of `workspace`: the name of its `projects/<key>`
/// directory.
fn project_key(home: &Path, workspace: &Path) -> String {
    log::sessions_dir(home, &doors::project(workspace))
        .parent()
        .and_then(Path::file_name)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The `session_id` on the first stdout line of a run.
fn session_of(stdout: &[u8]) -> String {
    let first: Value =
        serde_json::from_str(String::from_utf8_lossy(stdout).lines().next().unwrap()).unwrap();
    first["session_id"].as_str().unwrap().to_owned()
}

#[test]
fn a_fiber_ask_run_appends_exactly_one_exited_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit("fiber ask", command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = session_of(&output.stdout);
    let rows = rows(&setup.home());
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row["session_id"], id.as_str());
    assert_eq!(row["how"], "exited");
    assert_eq!(
        row["project"],
        project_key(&setup.home(), &setup.workspace()).as_str()
    );
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    assert_eq!(
        fs::canonicalize(row["workspace"].as_str().unwrap()).unwrap(),
        workspace
    );
    assert_eq!(row["status"]["state"], "idle");
    assert_eq!(row["name"], row["status"]["name"]);
    assert!(row["ts"].as_u64().unwrap() > 0);
}

#[test]
fn a_session_that_exits_on_a_pending_approval_leaves_a_waiting_row() {
    let setup = Setup::new();
    let server = ProviderServer::start([stream(&[json!({
        "type": "response.output_item.done",
        "item": {"type": "function_call", "id": "fc_call_1", "call_id": "call_1", "name": "shell",
            "arguments": json!({"command": "echo hi"}).to_string()}
    })])])
    .unwrap();
    setup.provider(&server);
    // A standing ask: the session asks a person, and the idle delay of 0
    // ends it on the pending request (`docs/permissions.md`, "Headless").
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}),
    );
    let id = doors::mint("s_");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let output = run_to_exit(
        "the session",
        setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            &workspace,
            "--prompt",
            "run it",
        ]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = rows(&setup.home());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["session_id"], id.as_str());
    assert_eq!(rows[0]["how"], "exited");
    assert_eq!(rows[0]["status"]["state"], "waiting", "{:?}", rows[0]);
    assert_eq!(rows[0]["status"]["waiting"]["kind"], "approval");
}

#[test]
fn a_session_that_never_got_a_prompt_appends_no_row() {
    let setup = Setup::new();
    let server = ProviderServer::start(Vec::<fakes::Response>::new()).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}),
    );
    let id = doors::mint("s_");
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let output = run_to_exit(
        "the session",
        setup.fiber(&["session", "--id", &id, "--workspace", &workspace]),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rows(&setup.home()).is_empty());
    assert!(!setup.home().join("recent.jsonl").exists());
}

/// A second client on a running hub: connected once its `hub_hello`
/// arrives.
fn client(setup: &Setup) -> Socket {
    let client = Socket::connect(&setup.hub_socket());
    assert_eq!(recv(&client, "hub_hello")["kind"], "hub_hello");
    client
}

/// Subscribes `client` to the feed.
fn feed(client: &Socket) {
    client.send(r#"{"id":"c_feed","command":"feed"}"#);
    let ack = recv(client, "the feed acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_feed");
}

/// The session ids `recent` lists with `args`.
fn recent(client: &Socket, args: &Value) -> Vec<String> {
    client.send(&json!({"id": "c_recent", "command": "recent", "args": args}).to_string());
    let ack = recv(client, "the recent answer");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    ack["payload"]["result"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["session_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_feed_shows_sessions_across_projects_and_recent_lists_the_exited() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    // Every turn waits on the model until released: all three run at once.
    server.hold();
    let hub = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    feed(&watch);
    let control = client(&setup);
    let first = setup.root.path().join("wa");
    let second = setup.root.path().join("wb");
    let outside = setup.root.path().join("wc");
    for workspace in [&first, &second, &outside] {
        fs::create_dir_all(workspace).unwrap();
    }
    let first_text = first.to_string_lossy().into_owned();
    let second_text = second.to_string_lossy().into_owned();
    let first_guard = SessionGuard::arm(&first_text);
    let second_guard = SessionGuard::arm(&second_text);
    let a = start_session(&control, &first_text, "first");
    let b = start_session(&control, &second_text, "second");
    // A `fiber ask` the hub did not start: it finds it in `run/`.
    let mut ask = setup.fiber(&["ask", "third"]);
    ask.current_dir(&outside);
    let asking = std::thread::spawn(move || run_to_exit("fiber ask", ask));
    assert!(server.await_requests(3, DEADLINE), "all three turns run");
    let mut seen = std::collections::BTreeSet::new();
    let statuses = until(&watch, "three sessions' status", |line| {
        if line["kind"] == "session_status" {
            seen.insert(line["session_id"].as_str().unwrap().to_owned());
        }
        seen.len() == 3
    });
    assert!(
        statuses
            .iter()
            .all(|line| line["kind"] == "session_status" && line["payload"].get("parent").is_none()),
        "{statuses:?}"
    );
    server.release();
    let output = asking.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = session_of(&output.stdout);
    assert!(
        seen.contains(&a) && seen.contains(&b) && seen.contains(&c),
        "{seen:?}"
    );
    let left = until(&watch, "the ask session's session_left", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == c.as_str()
    });
    assert_eq!(left.last().unwrap()["payload"]["how"], "exited");
    // `close` ends the first session: it leaves the feed exited.
    control.send(&format!(
        r#"{{"id":"c_sub","session_id":"{a}","command":"subscribe","args":{{"level":"summary"}}}}"#
    ));
    until(&control, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    control.send(&format!(
        r#"{{"id":"c_close","session_id":"{a}","command":"close"}}"#
    ));
    until(&control, "the close acknowledgement", |line| {
        line["payload"]["command_id"] == "c_close"
    });
    let left = until(&watch, "the first session's session_left", |line| {
        line["kind"] == "session_left" && line["payload"]["session_id"] == a.as_str()
    });
    assert_eq!(
        left.last().unwrap(),
        &json!({
            "kind": "session_left",
            "ts": left.last().unwrap()["ts"],
            "schema_version": 1,
            "payload": {"session_id": a, "how": "exited"},
        })
    );
    // Its process appends its row as it exits.
    first_guard.wait_gone();
    // A fresh feed: only the second session, still running.
    let fresh = client(&setup);
    feed(&fresh);
    let snapshot = until(&fresh, "the second session's status", |line| {
        line["session_id"] == b.as_str()
    });
    assert!(
        snapshot.iter().all(|line| line["kind"] == "session_status"),
        "{snapshot:?}"
    );
    let first_key = project_key(&setup.home(), &first);
    let second_key = project_key(&setup.home(), &second);
    assert_ne!(first_key, second_key);
    assert_eq!(recent(&fresh, &json!({"project": first_key})), [a.as_str()]);
    assert!(!recent(&fresh, &json!({"project": second_key})).contains(&a));
    let everything = recent(&fresh, &json!({}));
    assert!(
        everything.contains(&a) && everything.contains(&c),
        "{everything:?}"
    );
    assert!(!everything.contains(&b), "a running session is not recent");
    // The second session ends on its own socket; then the hub.
    close_session(&Socket::connect(&setup.session_socket(&b)));
    second_guard.wait_gone();
    drop((watch, control, fresh));
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    hub.wait();
}

#[test]
fn a_killed_session_is_crashed_until_dismissed() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // The turn never ends: the log's last line is not `fiber_exited`.
    server.hold();
    let hub = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    feed(&watch);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(&workspace);
    let control = client(&setup);
    let id = start_session(&control, &workspace, "doomed");
    assert!(server.await_requests(1, DEADLINE), "the turn runs");
    until(&watch, "the session's status", |line| {
        line["kind"] == "session_status" && line["session_id"] == id.as_str()
    });
    fakes::kill_matching(&workspace).unwrap();
    guard.wait_gone();
    let left = until(&watch, "the crash's session_left", |line| {
        line["kind"] == "session_left"
    });
    assert_eq!(
        left.last().unwrap()["payload"],
        json!({"session_id": id, "how": "crashed"})
    );
    let rows = rows(&setup.home());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["session_id"], id.as_str());
    assert_eq!(rows[0]["how"], "crashed");
    // A fresh feed is sent the crash first: its status, then its
    // `session_left`.
    let fresh = client(&setup);
    feed(&fresh);
    assert_eq!(
        recv(&fresh, "the crashed status")["session_id"],
        id.as_str()
    );
    assert_eq!(recv(&fresh, "the crash")["payload"]["how"], "crashed");
    fresh.send(
        &json!({"id": "c_dismiss", "command": "dismiss", "args": {"session": id}}).to_string(),
    );
    let ack = recv(&fresh, "the dismissal");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let again = client(&setup);
    again.send(
        &json!({"id": "c_dismiss", "command": "dismiss", "args": {"session": id}}).to_string(),
    );
    let ack = recv(&again, "the second dismissal");
    assert_eq!(ack["payload"]["code"], "stale_request", "{ack}");
    drop((watch, control, fresh, again));
    server.release();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.kill("TERM");
    hub.wait();
}
