//! Reload through the session socket, including the next provider request.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
mod support;
use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::Child,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
use support::*;

/// Exactly one `clients` event: the startup subscribe attaches once, and
/// nothing else connects or leaves during the test.
fn assert_one_clients(stream: &[Value]) {
    let count = stream
        .iter()
        .filter(|line| line["kind"] == "clients")
        .count();
    assert_eq!(count, 1, "{stream:?}");
}

struct Session {
    child: Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    client: Socket,
    setup: Setup,
    id: String,
    stream: Vec<Value>,
    expected: Vec<&'static str>,
}

impl Session {
    fn start(setup: Setup) -> Self {
        let id = doors::mint("s_");
        let workspace = setup.workspace();
        let (mut child, watchdog) = spawn_watched(&mut setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            workspace.to_str().unwrap(),
        ]));
        let guard = KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (tx, stderr) = mpsc::channel();
        let stderr_snapshot = Arc::new(Mutex::new(String::new()));
        let stderr_copy = Arc::clone(&stderr_snapshot);
        thread::spawn(move || {
            let mut pipe = BufReader::new(stderr_pipe);
            let text = drain_stderr_to_snapshot(&mut pipe, &stderr_copy);
            tx.send(text).unwrap();
        });
        first_line(setup.deadline, &lines);
        let mut client = Socket::connect(setup.deadline, &setup.session_socket(&id));
        client.set_diag(Diag::new(stderr_snapshot, session_dir(&setup, &id)));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // `clients` is written whenever a `full` connection attaches, with
        // no order against the startup events (`docs/events.md`): the wait
        // returns once both `clients` and `extensions_loaded` have arrived,
        // in any order, keeping every line in arrival order.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        let stream = until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        assert_one_clients(&stream);
        Self {
            child,
            guard,
            watchdog,
            stderr,
            client,
            setup,
            id,
            stream,
            expected: vec![
                "command_accepted",
                "session_started",
                "fiber_started",
                "extensions_loaded",
            ],
        }
    }
    fn send_prompt(&self) {
        self.client.send(&json!({"id":doors::mint("c_"),"command":"prompt","args":{"content":[{"type":"text","text":"hello"}]}}).to_string());
    }
    fn turn(&mut self, first: bool) {
        self.send_prompt();
        self.stream
            .extend(until(&self.client, "turn completed", |line| {
                line["kind"] == "turn_completed"
            }));
        if first {
            self.expected.extend(["preamble_built", "opening_message"]);
        }
        self.expected.extend([
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]);
    }
    fn reload(&mut self) {
        self.client
            .send(&json!({"id":doors::mint("c_"),"command":"reload"}).to_string());
        self.stream
            .extend(until(&self.client, "reload preamble", |line| {
                line["kind"] == "preamble_built"
            }));
        self.expected.extend([
            "reloaded",
            "command_accepted",
            "extensions_loaded",
            "preamble_built",
        ]);
        assert_eq!(self.stream.last().unwrap()["payload"]["reason"], "reload");
        let reloaded = self
            .stream
            .iter()
            .rev()
            .find(|line| line["kind"] == "reloaded")
            .unwrap();
        assert_eq!(
            reloaded["payload"]["servers"],
            json!({"kept":[],"restarted":[],"started":[],"stopped":[]})
        );
        assert!(reloaded["payload"].get("failed").is_none());
    }
    fn dir(&self) -> std::path::PathBuf {
        session_dir(&self.setup, &self.id)
    }
    fn finish(mut self) -> (Vec<Value>, String) {
        self.client.send(r#"{"id":"close","command":"close"}"#);
        self.stream.extend(until_close(&self.client));
        self.expected.extend(["command_accepted", "fiber_exited"]);
        let group = self.child.id();
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(self.child.wait()).unwrap();
        });
        let status = self.setup.deadline.recv(&done).unwrap().unwrap();
        let stderr = self.setup.deadline.recv(&self.stderr).unwrap();
        assert!(status.success(), "{stderr}");
        assert!(!fakes::group_alive(group));
        std::mem::forget(self.guard);
        self.watchdog
            .stand_down(self.setup.deadline.cleanup_phase());
        assert_one_clients(&self.stream);
        assert_eq!(kinds(&self.stream), self.expected, "{:?}", self.stream);
        (self.stream, stderr)
    }
}

fn fixture() -> (Setup, ProviderServer) {
    let setup = Setup::new();
    let server = ProviderServer::start_with_fallback([], hello()).unwrap();
    setup.provider(&server);
    (setup, server)
}
fn reviewed_turn(session: &mut Session, first: bool) {
    session.send_prompt();
    session
        .stream
        .extend(until(&session.client, "reviewed turn completed", |line| {
            line["kind"] == "turn_completed"
        }));
    if first {
        session
            .expected
            .extend(["preamble_built", "opening_message"]);
    }
    session.expected.extend([
        "turn_started",
        "command_accepted",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "usage_recorded",
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);
}
fn bodies(server: &ProviderServer) -> Vec<Value> {
    server
        .requests()
        .into_iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}
fn extension(setup: &Setup, init: &str) {
    let source = setup.root.path().join("demo");
    write_json(
        &source.join("extension.json"),
        &json!({"name":"demo","version":"v0.0.0","fiber":"0.0.0","api":1}),
    );
    fs::write(source.join("init.lua"), init).unwrap();
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

#[test]
fn session_socket_timeout_dumps_received_lines_stderr_and_log_tail() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    let mut session = Session::start(setup);
    session.send_prompt();
    // The prompt's acceptance arrives on the test's deadline; the held
    // model never answers, so the impossible kind never arrives: the wait
    // then expires on a zero bound instead of spending what remains of
    // the test's deadline (`docs/testing.md`, "Waits and timeouts").
    let mut got = until(&session.client, "the prompt's acceptance", |line| {
        line["kind"] == "command_accepted"
    });
    session.client.set_deadline(Deadline::after(Duration::ZERO));
    let expired = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for _ in 0..1000 {
            match session.client.next("the turn that never completes", &got) {
                Some(line) => got.push(line),
                None => panic!(
                    "the socket closed while waiting for the turn that never completes; got {got:?}"
                ),
            }
        }
        panic!("the held model answered the turn that never completes; got {got:?}");
    }))
    .unwrap_err();
    let message = expired
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert_timeout_dump(&message);
    // The success-path deadline is spent proving the dump: free the held
    // turn, then kill and reap the session on the cleanup bound.
    server.release();
    let group = session.child.id();
    let cleanup = session.setup.deadline.cleanup_phase();
    fakes::kill_group(group, "KILL").unwrap();
    let Session {
        mut child,
        guard,
        watchdog,
        ..
    } = session;
    let (tx, done) = mpsc::channel();
    thread::spawn(move || {
        tx.send(child.wait()).unwrap();
    });
    match cleanup.recv(&done) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited until the cleanup deadline to reap the session"),
    };
    assert!(
        fakes::group_empties(group, cleanup),
        "the session left a process in its group behind"
    );
    watchdog.stand_down(cleanup);
    std::mem::forget(guard);
}

#[test]
fn idle_reload_rebuilds_the_next_request_after_a_turn() {
    let (setup, server) = fixture();
    let mut session = Session::start(setup);
    session.turn(true);
    session.reload();
    session.turn(false);
    let (stream, _) = session.finish();
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2);
    let built = stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(built["payload"]["reason"], "reload");
    assert_eq!(seen[1]["instructions"], seen[0]["instructions"]);
}
#[test]
fn reload_before_any_turn_builds_once_and_the_first_turn_opens_once() {
    let (setup, server) = fixture();
    let mut session = Session::start(setup);
    session.reload();
    session.expected.push("opening_message");
    session.turn(false);
    session.finish();
    assert_eq!(bodies(&server).len(), 1);
}
#[test]
fn append_system_edits_reach_requests_only_after_reload() {
    let (setup, server) = fixture();
    let path = setup.home().join("APPEND_SYSTEM.md");
    let mut session = Session::start(setup);
    session.turn(true);
    fs::write(path, "reload unique append marker").unwrap();
    session.turn(false);
    session.reload();
    session.turn(false);
    session.finish();
    let seen = bodies(&server);
    assert!(!seen[0].to_string().contains("reload unique append marker"));
    assert!(!seen[1].to_string().contains("reload unique append marker"));
    assert!(seen[2].to_string().contains("reload unique append marker"));
}
#[test]
fn model_configuration_changes_do_not_change_the_live_route() {
    let (setup, server) = fixture();
    let mut session = Session::start(setup);
    session.turn(true);
    write_json(
        &session.setup.home().join("config.json"),
        &json!({"model":"missing/model"}),
    );
    session.reload();
    session.turn(false);
    session.finish();
    assert_eq!(
        bodies(&server)
            .iter()
            .map(|body| body["model"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["m", "m"]
    );
}
#[test]
fn reload_is_busy_during_a_request_and_changes_nothing() {
    let (setup, server) = fixture();
    server.hold();
    let mut session = Session::start(setup);
    session.send_prompt();
    assert!(server.await_requests(1, session.setup.deadline));
    session.client.send(r#"{"id":"reload","command":"reload"}"#);
    session.client.send(r#"{"id":"barrier","command":"tools"}"#);
    session
        .stream
        .extend(until(&session.client, "reader barrier", |line| {
            line["kind"] == "command_accepted" && line["payload"]["command_id"] == "barrier"
        }));
    session.expected.extend([
        "preamble_built",
        "opening_message",
        "turn_started",
        "command_accepted",
        "step_started",
        "assistant_message_started",
        "command_accepted",
    ]);
    server.release();
    session
        .stream
        .extend(until(&session.client, "turn completed", |line| {
            line["kind"] == "turn_completed"
        }));
    assert!(
        session
            .stream
            .iter()
            .any(|line| line["kind"] == "command_rejected" && line["payload"]["code"] == "busy")
    );
    session.expected.extend([
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "command_rejected",
        "turn_completed",
    ]);
    session.finish();
}
#[test]
fn extension_tool_disablement_changes_the_next_tools_answer_and_request() {
    let (setup, server) = fixture();
    extension(
        &setup,
        "fiber.tool('extra',{description='extra',input_schema={type='object',properties={}},effects={effects={'reads'},reversible=true},timeout=5000,run=function() return 'ok' end})",
    );
    let mut session = Session::start(setup);
    session.turn(true);
    write_json(
        &session.setup.home().join("config.json"),
        &json!({"model":"fake/m","extensions":{"demo":{"tools":{"disabled":["extra"]}}}}),
    );
    session.turn(false);
    session.reload();
    session.turn(false);
    session.finish();
    let seen = bodies(&server);
    for body in &seen[..2] {
        assert!(
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "extra"),
            "tools: {:?}",
            body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| &tool["name"])
                .collect::<Vec<_>>()
        );
    }
    assert!(
        !seen[2]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "extra")
    );
}
#[test]
fn invalid_config_rejects_reload_without_closing_extension_state() {
    let (setup, server) = fixture();
    extension(
        &setup,
        "fiber.hook('before_message',{timeout=5000,on_failure='blocking',run=function() state.set('alive',true) end})",
    );
    let mut session = Session::start(setup);
    fs::write(session.setup.home().join("config.json"), "{").unwrap();
    session.client.send(r#"{"id":"reload","command":"reload"}"#);
    session
        .stream
        .extend(until(&session.client, "config rejection", |line| {
            line["kind"] == "command_rejected"
        }));
    assert_eq!(
        session.stream.last().unwrap()["payload"]["code"],
        "config_invalid"
    );
    session.expected.push("command_rejected");
    session.turn(true);
    let start = session
        .expected
        .iter()
        .position(|kind| *kind == "preamble_built")
        .unwrap();
    session.expected.insert(start, "extension_state_set");
    session.finish();
    assert_eq!(bodies(&server).len(), 1);
}
#[test]
fn skill_description_is_resent_once_even_after_a_turn_start_check() {
    let (setup, server) = fixture();
    let path = setup.workspace().join(".agents/skills/demo/SKILL.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "---\nname: demo\ndescription: original description\n---\nSkill body\n",
    )
    .unwrap();
    let mut session = Session::start(setup);
    session.turn(true);
    fs::write(
        &path,
        "---\nname: demo\ndescription: updated description\n---\nSkill body\n",
    )
    .unwrap();
    session.turn(false);
    session.reload();
    session
        .stream
        .extend(until(&session.client, "changed skill", |line| {
            line["kind"] == "skills_changed"
        }));
    session.expected.push("skills_changed");
    session.reload();
    session.turn(false);
    let (stream, _) = session.finish();
    let changes: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "skills_changed")
        .collect();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].to_string().contains("updated description"));
    let seen = bodies(&server);
    assert!(!seen[1].to_string().contains("updated description"));
    assert!(seen[2].to_string().contains("updated description"));
}

#[test]
fn reviewer_model_changes_apply_to_the_next_reviewed_call() {
    let setup = Setup::new();
    let call = || {
        stream(&[
            json!({"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"shell","arguments":"{\"command\":\"touch .fiber-review-marker\"}"}}),
            json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":10,"output_tokens":3}}}),
        ])
    };
    let server = ProviderServer::start([
        call(),
        text_reply("allow"),
        hello(),
        call(),
        text_reply("allow"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let data = setup.home().join("extensions/fake/providers/fake.json");
    let mut provider: Value = serde_json::from_slice(&fs::read(&data).unwrap()).unwrap();
    let mut next = provider["models"][0].clone();
    next["id"] = json!("n");
    provider["models"].as_array_mut().unwrap().push(next);
    write_json(&data, &provider);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model":"fake/m", "reviewer":{"model":"fake/m"}}),
    );
    let mut session = Session::start(setup);
    reviewed_turn(&mut session, true);
    write_json(
        &session.setup.home().join("config.json"),
        &json!({"model":"fake/m", "reviewer":{"model":"fake/n"}}),
    );
    session.reload();
    reviewed_turn(&mut session, false);
    let (stream, _) = session.finish();
    let decisions: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .map(|line| &line["payload"]["reviewer"]["model"])
        .collect();
    assert_eq!(decisions, [&json!("fake/m"), &json!("fake/n")]);
    let seen = bodies(&server);
    assert_eq!(seen[1]["model"], "m");
    assert_eq!(seen[4]["model"], "n");
}

#[test]
fn the_retained_route_provider_can_write_state_when_signing_after_reload() {
    let (setup, server) = fixture();
    fs::write(setup.home().join("extensions/fake/providers/fake.lua"), "fiber.provider('fake',{sign={timeout=5000,run=function() state.set('signed',(state.get('signed') or 0)+1); return {} end}})").unwrap();
    let mut session = Session::start(setup);
    session.turn(true);
    let completed = session.expected.len() - 1;
    session.expected.insert(completed, "extension_state_set");
    session.reload();
    session.turn(false);
    let completed = session.expected.len() - 1;
    session.expected.insert(completed, "extension_state_set");
    let (stream, _) = session.finish();
    let writes: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "extension_state_set")
        .collect();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[1]["payload"]["value"], 2);
    let reload = stream
        .iter()
        .position(|line| line["kind"] == "reloaded")
        .unwrap();
    assert!(
        stream
            .iter()
            .skip(reload + 1)
            .any(|line| line["kind"] == "extension_state_set")
    );
    assert_eq!(bodies(&server).len(), 2);
}

#[test]
fn close_now_cancels_a_reload_blocked_in_extension_initialization() {
    let (setup, _server) = fixture();
    extension(&setup, "");
    let fifo = setup.root.path().join("load.fifo");
    fakes::fifo(&fifo);
    let mut session = Session::start(setup);
    fs::write(
        session.setup.home().join("extensions/demo/init.lua"),
        format!("host.fs.read({:?})", fifo.to_str().unwrap()),
    )
    .unwrap();
    session.client.send(r#"{"id":"reload","command":"reload"}"#);
    let _writer = bounded(
        session.setup.deadline,
        "extension opens the fifo",
        move || fs::File::create(fifo).unwrap(),
    );
    session
        .client
        .send(r#"{"id":"close_now","command":"close","args":{"now":true}}"#);
    session.stream.extend(until_close(&session.client));
    let group = session.child.id();
    let (tx, done) = mpsc::channel();
    thread::spawn(move || {
        tx.send(session.child.wait()).unwrap();
    });
    let status = session.setup.deadline.recv(&done).unwrap().unwrap();
    let stderr = session.setup.deadline.recv(&session.stderr).unwrap();
    assert!(status.success(), "{stderr}");
    assert!(!fakes::group_alive(group));
    std::mem::forget(session.guard);
    session
        .watchdog
        .stand_down(session.setup.deadline.cleanup_phase());
    session
        .expected
        .extend(["command_accepted", "command_rejected", "fiber_exited"]);
    assert_one_clients(&session.stream);
    // Compact JSON per line: the payloads the kinds list hides.
    let dump = session
        .stream
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        kinds(&session.stream),
        session.expected,
        "stream mismatch:\n{dump}"
    );
    let rejection = session
        .stream
        .iter()
        .find(|line| line["kind"] == "command_rejected")
        .unwrap();
    assert_eq!(rejection["payload"]["code"], "closing");
}

#[test]
fn startup_holds_extensions_loaded_behind_a_fifo_and_counts_clients_once() {
    // `clients`-first cannot be forced from the test side: the socket
    // binds in `Session::open` after `parts_with` loads the extensions,
    // so a blocked startup init leaves no socket to subscribe to, and no
    // pause point lies between the bind and `extensions_loaded`. This
    // pins the reachable half instead: `extensions_loaded` held behind
    // the fifo while the wait still collects both events.
    let (setup, _server) = fixture();
    let fifo = setup.root.path().join("startup.fifo");
    fakes::fifo(&fifo);
    // The startup extension initialization blocks reading the fifo: the
    // session's socket and first stdout line only exist once it completes,
    // so the writer below rendezvouses before the subscribe, and the
    // startup wait still collects both events whatever order they land in.
    extension(
        &setup,
        &format!("host.fs.read({:?})", fifo.to_str().unwrap()),
    );
    let (tx, written) = mpsc::channel();
    thread::spawn(move || {
        fs::write(fifo, b"").unwrap();
        tx.send(()).unwrap();
    });
    let session = Session::start(setup);
    session.setup.deadline.recv(&written).unwrap();
    session.finish();
}

/// The credential file's text for the reload scenario. It is not a path
/// fragment and not a word the denial sentence uses, so a hit in an output
/// can only be the file's bytes.
const RELOAD_MARKER: &str = "reload-quorum-xylophone-77";

fn assert_no_reload_marker(label: &str, bytes: &[u8]) {
    assert!(
        !holds_marker(bytes, RELOAD_MARKER),
        "{label} holds the credential file's text"
    );
}

/// Every file under `dir`, symlink targets included. `events.jsonl` is one
/// of them; `artifacts/` is entered even when a call wrote nothing there.
/// Returns each file's display path with its bytes: the caller snapshots
/// before `finish`, whose return drops the session's temporary root.
fn snapshot_session_files(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    assert!(dir.join("events.jsonl").is_file());
    assert!(dir.join("artifacts").is_dir());
    let mut snapshots = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            continue;
        }
        snapshots.push((path.display().to_string(), fs::read(&path).unwrap()));
    }
    snapshots
}

#[test]
fn reload_applies_a_changed_shell_read_only_list() {
    let setup = Setup::new();
    let secret = setup.home().join("credentials").join("secret.txt");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, format!("{RELOAD_MARKER}\n")).unwrap();
    let absolute = fs::canonicalize(&secret)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!absolute.contains(RELOAD_MARKER), "{absolute}");
    let events = vec![function_call(
        "shell_0",
        "shell",
        &json!({"command": format!("jq . {absolute}")}),
    )];
    let script = serde_json::to_string(&events).unwrap();
    assert!(
        !script.contains(RELOAD_MARKER),
        "the scripted arguments contain the credential file's text"
    );
    let call = || stream(&events);
    let server = ProviderServer::start([call(), hello(), call(), hello()]).unwrap();
    setup.provider(&server);
    let mut session = Session::start(setup);

    // Turn 1: `jq` is unlisted, so the call goes to review. With no
    // reviewer model the review escalates to the person, who denies it.
    let turn = session.stream.len();
    session.send_prompt();
    session
        .stream
        .extend(until(&session.client, "permission requested", |line| {
            line["kind"] == "permission_requested"
        }));
    let requested = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .cloned()
        .unwrap();
    let request = session
        .stream
        .last()
        .cloned()
        .filter(|line| line["kind"] == "permission_requested")
        .unwrap();
    assert_eq!(request["payload"]["step"], "review");
    assert_eq!(request["payload"]["escalation"]["cause"], "reviewer_failed");
    assert_eq!(
        request["payload"]["escalation"]["error"]["code"],
        "no_model"
    );
    assert_eq!(request["action_id"], requested["action_id"]);
    let request_id = request["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    session.client.send(
        &json!({"id":doors::mint("c_"),"command":"reply","args":{"request_id":request_id,"decision":"deny"}})
            .to_string(),
    );
    session
        .stream
        .extend(until(&session.client, "denied turn completed", |line| {
            line["kind"] == "turn_completed"
        }));
    let resolved = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "person");
    assert_eq!(resolved["payload"]["request_id"], request_id);
    assert_eq!(resolved["action_id"], requested["action_id"]);
    let completed = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "denied");
    assert_eq!(completed["payload"]["reason"], "person");
    assert_eq!(completed["action_id"], requested["action_id"]);
    assert!(
        !session.stream[turn..]
            .iter()
            .any(|line| line["kind"] == "tool_call_started"),
        "the denied call never started"
    );
    session.expected.extend([
        "preamble_built",
        "opening_message",
        "turn_started",
        "command_accepted",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "notice",
        "permission_requested",
        "command_accepted",
        "permission_resolved",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);

    // The reload lists `jq` as read-only with no flags: the same call on
    // the credential path now declares `reads`, and the credential deny
    // refuses it before review.
    write_json(
        &session.setup.home().join("config.json"),
        &json!({"model":"fake/m","shell":{"read_only":{"jq":{"flags":[]}}}}),
    );
    session.reload();
    let turn = session.stream.len();
    session.send_prompt();
    session.stream.extend(until(
        &session.client,
        "credential-denied turn completed",
        |line| line["kind"] == "turn_completed",
    ));
    let requested = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .cloned()
        .unwrap();
    let resolved = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .cloned()
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "credential_deny");
    assert_eq!(resolved["action_id"], requested["action_id"]);
    let completed = session.stream[turn..]
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .cloned()
        .unwrap();
    assert_eq!(completed["payload"]["status"], "denied");
    assert_eq!(completed["payload"]["reason"], "credentials");
    assert_eq!(completed["action_id"], requested["action_id"]);
    assert!(
        !session.stream[turn..]
            .iter()
            .any(|line| line["kind"] == "permission_requested"),
        "the listed call never reached review"
    );
    assert!(
        !session.stream[turn..]
            .iter()
            .any(|line| line["kind"] == "tool_call_started"),
        "the denied call never started"
    );
    session.expected.extend([
        "turn_started",
        "command_accepted",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "permission_resolved",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);

    let dir = session.dir();
    let snapshots = snapshot_session_files(&dir);
    let (_, stderr) = session.finish();
    let seen = bodies(&server);
    assert_eq!(seen.len(), 4, "no reviewer request in either turn");
    assert_no_reload_marker("stderr", stderr.as_bytes());
    for (index, request) in server.requests().iter().enumerate() {
        assert_no_reload_marker(&format!("request {index}"), &request.body);
    }
    for (label, bytes) in &snapshots {
        assert_no_reload_marker(label, bytes);
    }
}

/// One read-only fixture tool: calls take the permission fast path, as in
/// `mcp.rs`' echo tool.
fn fixture_tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": "A fixture tool.",
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// A fixture server directory holding `tools.json` and one result file
/// per call: `fakes::mcp_fixture`'s `server.sh` serves the directory.
fn mcp_dir(setup: &Setup, name: &str, tools: &Value, calls: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = setup.root.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
    for (file, body) in calls {
        fs::write(dir.join(file), body).unwrap();
    }
    dir
}

fn mcp_entry(dir: &std::path::Path) -> Value {
    json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
    })
}

fn tool_names_in(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn reload_replaces_restarts_starts_and_drops_mcp_servers() {
    let setup = Setup::new();
    let hi = r#"{"content":[{"type":"text","text":"hi"}]}"#;
    let keep = mcp_dir(
        &setup,
        "keep",
        &json!([fixture_tool("keep_tool")]),
        &[("call-keep_tool.json", hi)],
    );
    let change = mcp_dir(
        &setup,
        "change",
        &json!([fixture_tool("v1")]),
        &[("call-v1.json", hi)],
    );
    let change_next = mcp_dir(
        &setup,
        "change-next",
        &json!([fixture_tool("v2")]),
        &[(
            "call-v2.json",
            r#"{"content":[{"type":"text","text":"v2hi"}]}"#,
        )],
    );
    let die = mcp_dir(
        &setup,
        "die",
        &json!([fixture_tool("die_tool")]),
        &[("call-die_tool.json", hi)],
    );
    let drop = mcp_dir(
        &setup,
        "drop",
        &json!([fixture_tool("drop_tool")]),
        &[("call-drop_tool.json", hi)],
    );
    let new = mcp_dir(
        &setup,
        "new",
        &json!([fixture_tool("new_tool")]),
        &[("call-new_tool.json", hi)],
    );
    let server = ProviderServer::start([
        stream(&[function_call("call_v2", "mcp__change__v2", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {
            "keep": mcp_entry(&keep),
            "change": mcp_entry(&change),
            "die": mcp_entry(&die),
            "drop": mcp_entry(&drop),
        }}}),
    );
    let mut session = Session::start(setup);
    let keep_cache = fs::read(session.setup.home().join("cache/mcp/keep.json")).unwrap();

    // `die` exits while idle: the reload restarts it beside the old slot,
    // carrying the death before the ready.
    let pid: u32 = fs::read_to_string(die.join("pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    fakes::kill_pid(pid, "KILL").unwrap();
    write_json(
        &session.setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {
            "keep": mcp_entry(&keep),
            "change": mcp_entry(&change_next),
            "die": mcp_entry(&die),
            "new": mcp_entry(&new),
        }}}),
    );
    session
        .client
        .send(&json!({"id":doors::mint("c_"),"command":"reload"}).to_string());
    session
        .stream
        .extend(until(&session.client, "reload preamble", |line| {
            line["kind"] == "preamble_built"
        }));
    session.expected.extend([
        "mcp_server_ready",
        "mcp_server_failed",
        "mcp_server_ready",
        "mcp_server_ready",
        "reloaded",
        "command_accepted",
        "extensions_loaded",
        "preamble_built",
    ]);
    let at = session
        .stream
        .iter()
        .position(|line| line["kind"] == "reloaded")
        .unwrap();
    let before = &session.stream[at - 4..at];
    assert_eq!(
        kinds(before),
        [
            "mcp_server_ready",
            "mcp_server_failed",
            "mcp_server_ready",
            "mcp_server_ready"
        ]
    );
    assert_eq!(before[0]["payload"]["server"], "change");
    assert_eq!(before[1]["payload"]["server"], "die");
    assert_eq!(before[1]["payload"]["will_restart"], true);
    assert_eq!(before[2]["payload"]["server"], "die");
    assert_eq!(before[3]["payload"]["server"], "new");
    let reloaded = &session.stream[at];
    assert_eq!(
        reloaded["payload"]["servers"],
        json!({"kept":["keep"],"restarted":["change","die"],"started":["new"],"stopped":["drop"]})
    );
    assert!(reloaded["payload"].get("failed").is_none());

    // Each cached list is updated; the kept one is byte-identical.
    let cache = |server: &str| {
        fs::read_to_string(
            session
                .setup
                .home()
                .join("cache/mcp")
                .join(format!("{server}.json")),
        )
        .unwrap()
    };
    let tools_of = |server: &str| {
        serde_json::from_str::<Value>(&cache(server)).unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(tools_of("change"), ["v2"]);
    assert_eq!(tools_of("new"), ["new_tool"]);
    assert_eq!(tools_of("die"), ["die_tool"]);
    assert_eq!(
        fs::read(session.setup.home().join("cache/mcp/keep.json")).unwrap(),
        keep_cache
    );
    // The replacement answers from its own server.
    assert_ne!(
        fs::read_to_string(die.join("pid.txt")).unwrap().trim(),
        pid.to_string()
    );

    // The reload rebuilds the preamble once — the one prompt-cache miss —
    // with the new tool set and without the dropped tool.
    let preambles: Vec<_> = session
        .stream
        .iter()
        .filter(|line| line["kind"] == "preamble_built")
        .collect();
    // The startup never builds a preamble: the reload's rebuild is the
    // only one, the one prompt-cache miss the reload costs.
    assert_eq!(preambles.len(), 1);
    let rebuilt = preambles[0];
    assert_eq!(rebuilt["payload"]["reason"], "reload");
    let names: Vec<_> = rebuilt["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for tool in [
        "mcp__keep__keep_tool",
        "mcp__change__v2",
        "mcp__die__die_tool",
        "mcp__new__new_tool",
    ] {
        assert!(names.contains(&tool), "{names:?}");
    }
    assert!(!names.contains(&"mcp__drop__drop_tool"), "{names:?}");

    // The new executable answers. The opening message goes out with the
    // first turn, after the reload's preamble.
    session.send_prompt();
    session
        .stream
        .extend(until(&session.client, "tool turn completed", |line| {
            line["kind"] == "turn_completed"
        }));
    session.expected.extend([
        "opening_message",
        "turn_started",
        "command_accepted",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ]);
    let completed = session
        .stream
        .iter()
        .rev()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "v2hi");
    let log = fs::read_to_string(change_next.join("requests.log")).unwrap();
    assert!(log.contains(r#""method":"tools/call""#), "log:\n{log}");
    assert!(log.contains(r#""name":"v2""#), "log:\n{log}");

    // The turn's requests carry the rebuilt tool set under the session's
    // prompt-cache key: the one miss, then the chain continues.
    let seen = bodies(&server);
    assert_eq!(seen.len(), 2);
    for body in &seen {
        assert_eq!(body["prompt_cache_key"], json!(session.id));
        let names = tool_names_in(body);
        assert!(names.contains(&"mcp__change__v2".to_owned()), "{names:?}");
        assert!(
            !names.contains(&"mcp__drop__drop_tool".to_owned()),
            "{names:?}"
        );
    }
    session.finish();
}
