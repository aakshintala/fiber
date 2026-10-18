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
    io::{BufRead, BufReader, Read},
    os::unix::process::CommandExt,
    process::{Child, Command},
    sync::mpsc,
    thread,
};
use support::*;

/// The event kinds without `clients`, in arrival order: it is written
/// whenever a `full` connection attaches or leaves, so where it lands
/// among the startup events depends on when the subscribe is handled.
fn kinds_without_clients(stream: &[Value]) -> Vec<&str> {
    kinds(stream)
        .into_iter()
        .filter(|kind| *kind != "clients")
        .collect()
}

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
        thread::spawn(move || {
            let mut text = String::new();
            stderr_pipe
                .take(u64::MAX)
                .read_to_string(&mut text)
                .unwrap();
            tx.send(text).unwrap();
        });
        first_line(setup.deadline, &lines);
        let client = Socket::connect(setup.deadline, &setup.session_socket(&id));
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
    fn finish(mut self) -> Vec<Value> {
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
        assert!(!group_alive(self.setup.deadline, group));
        std::mem::forget(self.guard);
        self.watchdog.stand_down(self.setup.deadline.cleanup());
        assert_one_clients(&self.stream);
        assert_eq!(
            kinds_without_clients(&self.stream),
            self.expected,
            "{:?}",
            self.stream
        );
        self.stream
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
fn idle_reload_rebuilds_the_next_request_after_a_turn() {
    let (setup, server) = fixture();
    let mut session = Session::start(setup);
    session.turn(true);
    session.reload();
    session.turn(false);
    let stream = session.finish();
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
    assert!(server.await_requests(1, session.setup.deadline.left()));
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
    let stream = session.finish();
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
            json!({"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"shell","arguments":"{\"command\":\"true\"}"}}),
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
    let stream = session.finish();
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
    let stream = session.finish();
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
    let mut command = Command::new("mkfifo");
    command.arg(&fifo).process_group(0);
    assert!(
        run_to_exit(setup.deadline, "make reload fifo", command)
            .status
            .success()
    );
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
    assert!(!group_alive(session.setup.deadline, group));
    std::mem::forget(session.guard);
    session
        .watchdog
        .stand_down(session.setup.deadline.cleanup());
    session
        .expected
        .extend(["command_accepted", "command_rejected", "fiber_exited"]);
    assert_one_clients(&session.stream);
    assert_eq!(kinds_without_clients(&session.stream), session.expected);
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
    let mut command = Command::new("mkfifo");
    command.arg(&fifo).process_group(0);
    assert!(
        run_to_exit(setup.deadline, "make startup fifo", command)
            .status
            .success()
    );
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
