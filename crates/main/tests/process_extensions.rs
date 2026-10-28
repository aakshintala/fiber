//! Binary-level tests of process extensions in real sessions
//! (`docs/extensions.md`, "Process extensions"): the built `fiber` runs
//! in its own process group with its own `FIBER_HOME`, holding a process
//! extension driven by the shared peer fixture, and an ordinary provider
//! whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{Setup, StdoutRun, function_call, hello_single_delta as hello, outputs_sent, stream};

/// One finished `fiber ask`: its exit code, its stdout lines parsed (less
/// `session_status`, which an observer thread writes), its raw stdout and
/// its stderr.
impl StdoutRun {
    fn payload(&self, kind: &str) -> &Value {
        &self
            .lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))["payload"]
    }
}

/// Runs `fiber` with `args` in the workspace, in its own process group,
/// waits for it under the test's deadline, and asserts that nothing it
/// started is left in the group. A watchdog kills the group if this
/// process dies first.
#[track_caller]
fn run_fiber(setup: &Setup, args: &[&str], what: &str) -> StdoutRun {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, what),
    };
    assert!(
        !fakes::group_alive(group),
        "`fiber` left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    StdoutRun::from(output)
}

/// Runs `fiber ask <prompt>` through [`run_fiber`], asserting success.
#[track_caller]
fn ask(setup: &Setup, prompt: &str) -> StdoutRun {
    let run = run_ask(setup, prompt);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    run
}

/// As [`ask`], without asserting the exit code: a required startup
/// failure exits nonzero, and the test reads its lines itself.
#[track_caller]
fn run_ask(setup: &Setup, prompt: &str) -> StdoutRun {
    run_fiber(setup, &["ask", prompt], "`fiber ask` to exit")
}

/// A server whose model makes `calls` in one step, then answers `Hello.`.
fn calling(calls: &[Value]) -> ProviderServer {
    ProviderServer::start([stream(calls), hello()]).unwrap()
}

/// Installs `fiber.test/<short>` as a process extension running the
/// shared peer fixture with `plan`, with `extra` merged into its
/// manifest's `process` table.
fn process_with(setup: &Setup, short: &str, extra: &Value, plan: &Value) {
    let (program, argv) = fakes::ProcessFixture::command("peer", Some(plan));
    let mut args = vec![argv[0].clone()];
    args.extend(argv[1..].iter().cloned());
    let mut process = serde_json::Map::new();
    process.insert("program".to_owned(), json!(program.display().to_string()));
    process.insert(
        "args".to_owned(),
        Value::Array(args.into_iter().map(Value::String).collect()),
    );
    process.insert("exit_timeout_ms".to_owned(), json!(1000));
    if let Some(table) = extra.as_object() {
        for (key, value) in table {
            process.insert(key.clone(), value.clone());
        }
    }
    let source = setup.root.path().join("src").join(short);
    std::fs::create_dir_all(&source).unwrap();
    support::write_json(
        &source.join("extension.json"),
        &json!({
            "name": format!("fiber.test/{short}"),
            "version": "v1",
            "fiber": "0.1.0",
            "api": 1,
            "process": process,
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// A valid `fiber.tool` spec with `run`, as JSON.
fn tool_spec() -> Value {
    json!({
        "description": "Probes the session.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

/// A valid `after_tool` hook spec with `run`, as JSON.
fn hook_spec() -> Value {
    json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    })
}

/// Sends one registration call, then waits for its answer.
fn register(action: &str, id: &str, args: Value) -> Value {
    json!([
        {"send": {"id": id, "command": "call", "args": {"call": action, "args": args}}},
        {"read_until": {"path": "payload.command_id", "equals": id}},
    ])
}

/// The peer plan registering a tool and a transforming `after_tool`
/// hook, then answering both: the tool answers `probe-ok`, the hook
/// replaces the content with `probe-ok|p`. The hello is written to
/// `hello_marker` before anything else, so the test reads the exact
/// session identity the owner was spawned under.
fn tool_hook_plan(hello_marker: &str) -> Value {
    let tool_branch = json!([
        {"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "probe-ok"}}},
    ]);
    let hook_branch = json!([
        {"send": {"id": "a-hook", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": {"content": "probe-ok|p"}}}},
    ]);
    let dispatch = json!([
        {"if": {"path": "req.payload.registration", "equals": "t1", "then": tool_branch, "else": [
            {"if": {"path": "req.payload.registration", "equals": "h1", "then": hook_branch}},
        ]}},
    ]);
    let serve = json!([
        {"read": {"as": "req"}},
        {"if": {"path": "req.kind", "equals": "extension_request", "then": dispatch}},
    ]);
    let mut plan = vec![
        json!({"read": {"as": "hello"}}),
        json!({"append_file": {"path": hello_marker, "value": {"$ref": "hello"}}}),
    ];
    for action in register("fiber.tool", "t1", json!(["probe", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    for action in register("fiber.hook", "h1", json!(["after_tool", hook_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"loop": serve}));
    Value::Array(plan)
}

/// A process extension's tool runs through the model and its
/// `after_tool` hook visibly changes the outcome, as a Lua extension's do.
#[test]
fn process_tool_runs_and_its_after_tool_hook_changes_the_outcome() {
    let setup = Setup::new();
    let server = calling(&[function_call("call_probe", "probe", &json!({}))]);
    setup.provider(&server);
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "probe",
        &json!({}),
        &tool_hook_plan(hello_marker.to_str().unwrap()),
    );
    let run = ask(&setup, "probe the session");
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "probe-ok|p"}])
    );
    assert_eq!(completed["changed_by"], json!(["fiber.test/probe"]));
    assert_eq!(outputs_sent(&server), ["probe-ok|p"]);
}

/// The hello a process extension receives carries the session's real
/// identity, why it started, its workspace and the extension's data
/// directories, with the folded state it was spawned under.
#[test]
fn process_hello_carries_session_identity_workspace_and_data_dirs() {
    let setup = Setup::new();
    let server = calling(&[function_call("call_probe", "probe", &json!({}))]);
    setup.provider(&server);
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "probe",
        &json!({}),
        &tool_hook_plan(hello_marker.to_str().unwrap()),
    );
    let run = ask(&setup, "probe the session");
    let hello: Value = serde_json::from_str(
        &std::fs::read_to_string(&hello_marker).expect("the peer writes its hello"),
    )
    .expect("the hello is one JSON object");
    assert_eq!(hello["kind"], json!("extension_hello"));
    let payload = &hello["payload"];
    assert_eq!(
        payload["session_id"], run.lines[0]["session_id"],
        "the hello names the session that spawned the owner"
    );
    assert_eq!(payload["reason"], json!("new"));
    assert_eq!(
        std::fs::canonicalize(payload["workspace"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(setup.workspace()).unwrap()
    );
    let machine = payload["data_dirs"]["machine"].as_str().unwrap();
    let project = payload["data_dirs"]["project"].as_str().unwrap();
    assert!(
        machine.starts_with(setup.home().to_str().unwrap())
            && machine.ends_with("fiber.test-probe"),
        "the machine directory lives under Fiber home: {machine}"
    );
    assert!(
        project.contains("projects") && project.ends_with("fiber.test-probe"),
        "the project directory lives under the session's project: {project}"
    );
}

/// Configures one fixture MCP server `fx` in `dir` behind a rendezvous
/// wrapper: the wrapper makes the `gate` FIFO, blocks writing `started`
/// to it until the process peer opens it for reading, records
/// `mcp-started`, then execs the real fixture server. The MCP handshake
/// therefore cannot complete before the peer arrives, and the peer's
/// `wait_fifo` cannot pass before this wrapper starts: only a
/// concurrent start opens both ends.
fn configure_gated_fx(setup: &Setup, dir: &Path) {
    let wrapper = dir.join("gate-wrapper.sh");
    std::fs::write(
        &wrapper,
        "#!/bin/bash\ndir=\"$1\"\nfixture=\"$2\"\nmkfifo \"$dir/gate\"\nprintf started > \"$dir/gate\"\nprintf started > \"$dir/mcp-started\"\nexec /bin/bash \"$fixture\" \"$dir\"\n",
    )
    .unwrap();
    let mode = std::fs::metadata(&wrapper).unwrap().permissions().mode();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(mode | 0o111)).unwrap();
    let server = json!({
        "command": "/bin/bash",
        "args": [
            wrapper.display().to_string(),
            dir.display().to_string(),
            fakes::mcp_fixture().display().to_string(),
        ],
        "startup_timeout_ms": 600000,
    });
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "mcp": {"servers": {"fx": server}}}),
    );
}

/// The peer plan that rendezvouses with the MCP wrapper before
/// registering: hello, then `wait_fifo` on the gate, then the probe
/// tool and the serve loop.
fn gated_plan(gate: &str, hello_marker: &str) -> Value {
    let serve = json!([
        {"read": {"as": "req"}},
        {"if": {"path": "req.kind", "equals": "extension_request", "then": [
            {"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "gated-ok"}}},
        ]}},
    ]);
    let mut plan = vec![
        json!({"read": {"as": "hello"}}),
        json!({"write_file": {"path": hello_marker, "value": {"$ref": "hello"}}}),
        json!({"wait_fifo": {"path": gate}}),
    ];
    for action in register("fiber.tool", "t1", json!(["probe", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"loop": serve}));
    Value::Array(plan)
}

/// A silent child never readies: the startup deadline passes and the
/// session continues without it, with the miss noticed under the
/// extension's name and its tool absent from the preamble.
#[test]
fn optional_process_miss_leaves_a_notice_and_starts_without_it() {
    let setup = Setup::new();
    let server = calling(&[]);
    setup.provider(&server);
    let plan = json!([
        {"read": {"as": "hello"}},
        {"hold": {"release_on": "never"}},
    ]);
    process_with(&setup, "silent", &json!({}), &plan);
    let run = ask(&setup, "say hi");
    let notices: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "notice")
        .collect();
    assert!(
        notices
            .iter()
            .any(|notice| notice["payload"]["extension"] == json!("fiber.test/silent")),
        "the silent miss is noticed: {notices:?}"
    );
    let tools = run.payload("preamble_built")["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| tool["registered_by"] != json!("fiber.test/silent")),
        "no tool of the missed extension is declared"
    );
}

/// The peer plan answering one tool call per generation, then dying:
/// every generation reads hello, appends it to the marker, registers
/// the `flaky` tool, readies, answers one request and exits. The first
/// death restarts on the next call; the second leaves the tool
/// unavailable, with no third spawn.
fn flaky_plan(hello_marker: &str) -> Value {
    let mut plan = vec![
        json!({"read": {"as": "hello"}}),
        json!({"append_file": {"path": hello_marker, "value": {"$ref": "hello"}}}),
    ];
    for action in register("fiber.tool", "t1", json!(["flaky", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    plan.push(json!({"read": {"as": "req"}}));
    plan.push(json!({"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "flaky-ok"}}}));
    plan.push(json!({"exit": {}}));
    Value::Array(plan)
}

/// The peer plan for the timeout test: the tool answers `late-ok`,
/// the `after_tool` hook request is met with silence, and the next
/// hook request — `turn_end`, which fires when the turn completes —
/// carries the first hook's late reply with it. The late reply names
/// the retired request, so it drops; the turn-end reply is normal,
/// and answering it writes `alive_marker`, proving the child was not
/// stopped by the timeout it missed.
fn late_plan(alive_marker: &str, hello_marker: &str) -> Value {
    let tool = json!({"send": {"id": "a-tool", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": "late-ok"}}});
    let mut plan = vec![
        json!({"read": {"as": "hello"}}),
        json!({"append_file": {"path": hello_marker, "value": {"$ref": "hello"}}}),
    ];
    for action in register("fiber.tool", "t1", json!(["late", tool_spec()]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    let after_tool = json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "blocking",
        "run": true,
    });
    for action in register("fiber.hook", "h1", json!(["after_tool", after_tool]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    let turn_end = json!({
        "phase": "transform",
        "timeout": 5000,
        "on_failure": "non-blocking",
        "run": true,
    });
    for action in register("fiber.hook", "h2", json!(["turn_end", turn_end]))
        .as_array()
        .unwrap()
    {
        plan.push(action.clone());
    }
    plan.push(json!({"send": {"id": "ready1", "command": "ready"}}));
    plan.push(json!({"read_until": {"path": "kind", "equals": "command_accepted"}}));
    // The tool call answers; its hook stays silent and times out.
    // The owner answers every reply with a `command_accepted` ack, so
    // each reply is followed by a `read_until` consuming its ack before
    // the next `read` names the request it must answer. Without those
    // the next `read` would catch the ack instead of the hook request.
    plan.push(json!({"read": {"as": "req"}}));
    plan.push(tool);
    plan.push(json!({"read_until": {"path": "payload.command_id", "equals": "a-tool"}}));
    plan.push(json!({"read": {"as": "first"}}));
    // The turn's end carries the first hook's late reply, then answers
    // the turn-end hook normally.
    plan.push(json!({"read": {"as": "req"}}));
    plan.push(json!({"send": {"id": "a-late", "command": "reply", "args": {"request_id": {"$ref": "first.payload.request_id"}, "result": {"content": "too-late"}}}}));
    plan.push(json!({"read_until": {"path": "payload.command_id", "equals": "a-late"}}));
    plan.push(json!({"send": {"id": "a-turn", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": {"content": "te-ok"}}}}));
    plan.push(json!({"read_until": {"path": "payload.command_id", "equals": "a-turn"}}));
    plan.push(json!({"write_file": {"path": alive_marker, "value": "answered"}}));
    plan.push(json!({"loop": [
        {"read": {"as": "req"}},
        {"if": {"path": "req.kind", "equals": "extension_request", "then": [
            {"send": {"id": "a-rest", "command": "reply", "args": {"request_id": {"$ref": "req.payload.request_id"}, "result": null}}},
        ]}},
    ]}));
    Value::Array(plan)
}

/// Resuming a session recomposes its extensions under the resume
/// reason: the resumed run's hello carries `resume` for the same
/// session id that started `new`.
#[test]
fn process_resume_recomposes_under_the_resume_reason() {
    let setup = Setup::new();
    let first_server = calling(&[function_call("call_probe_1", "probe", &json!({}))]);
    setup.provider(&first_server);
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "probe",
        &json!({}),
        &tool_hook_plan(hello_marker.to_str().unwrap()),
    );
    let first = ask(&setup, "probe the session");
    let id = first.lines[0]["session_id"].as_str().unwrap().to_owned();
    let second_server = calling(&[function_call("call_probe_2", "probe", &json!({}))]);
    setup.provider(&second_server);
    let run = run_fiber(
        &setup,
        &["ask", "--resume", &id, "probe again"],
        "`fiber ask --resume` to exit",
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let hellos: Vec<Value> = std::fs::read_to_string(&hello_marker)
        .expect("the peer writes one hello per run")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each hello is one JSON object"))
        .collect();
    assert_eq!(hellos.len(), 2, "one hello per run: {hellos:?}");
    assert_eq!(hellos[0]["payload"]["reason"], json!("new"));
    assert_eq!(hellos[1]["payload"]["reason"], json!("resume"));
    assert_eq!(hellos[0]["payload"]["session_id"], json!(id));
    assert_eq!(hellos[1]["payload"]["session_id"], json!(id));
}

/// A hook past its timeout fails as its `on_failure` says, the child
/// is not stopped, and the reply that arrives late changes nothing.
#[test]
fn process_hook_timeout_applies_on_failure_and_drops_the_late_reply() {
    let setup = Setup::new();
    let server = calling(&[function_call("call_late_1", "late", &json!({}))]);
    setup.provider(&server);
    let alive_marker = setup.workspace().join("alive.marker");
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "late",
        &json!({}),
        &late_plan(
            alive_marker.to_str().unwrap(),
            hello_marker.to_str().unwrap(),
        ),
    );
    let run = ask(&setup, "run the late tool");
    assert!(
        alive_marker.is_file(),
        "the child answers the turn-end hook after its missed timeout"
    );
    let completed: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 1, "the tool call completes: {completed:?}");
    assert_eq!(completed[0]["payload"]["status"], json!("completed"));
    let withheld = completed[0]["payload"]["content"].to_string();
    assert!(
        withheld.contains("withheld") && !withheld.contains("too-late"),
        "the timed-out hook withholds the output and the late reply changes nothing: {withheld}"
    );
}

/// A killed child restarts on its next call and the new generation
/// answers; killed twice, its tool calls fail while its declarations
/// stay put and no third child spawns.
#[test]
fn process_death_restarts_once_then_leaves_the_tool_unavailable() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call("call_flaky_1", "flaky", &json!({}))]),
        stream(&[function_call("call_flaky_2", "flaky", &json!({}))]),
        stream(&[function_call("call_flaky_3", "flaky", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "flaky",
        &json!({}),
        &flaky_plan(hello_marker.to_str().unwrap()),
    );
    let run = ask(&setup, "run the flaky tool");
    let completed: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(
        completed.len(),
        3,
        "three tool calls complete: {completed:?}"
    );
    assert_eq!(completed[0]["payload"]["status"], json!("completed"));
    assert_eq!(
        completed[0]["payload"]["content"],
        json!([{"type": "text", "text": "flaky-ok"}])
    );
    assert_eq!(completed[1]["payload"]["status"], json!("completed"));
    assert_eq!(
        completed[1]["payload"]["content"],
        json!([{"type": "text", "text": "flaky-ok"}])
    );
    assert_eq!(completed[2]["payload"]["status"], json!("failed"));
    // Exactly two generations spawned: one restart, never a third.
    let hellos = std::fs::read_to_string(&hello_marker).expect("the peer appends two hellos");
    assert_eq!(hellos.lines().count(), 2, "one restart, no third spawn");
    // The declarations the model saw never move across the deaths.
    let tool_lists: Vec<Value> = server
        .requests()
        .iter()
        .filter_map(|request| {
            serde_json::from_slice::<Value>(&request.body)
                .ok()
                .and_then(|body| body.get("tools").cloned())
        })
        .collect();
    assert!(
        tool_lists.len() >= 3,
        "the model lists tools on every step: {}",
        tool_lists.len()
    );
    for list in &tool_lists {
        assert_eq!(list, &tool_lists[0], "stable declarations across deaths");
    }
    assert!(
        tool_lists[0]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == json!("flaky")),
        "the flaky tool stays declared"
    );
}

/// A required silent child fails the session start: the run exits
/// nonzero, the required miss is reported under the extension's name,
/// and no session is committed to the log.
#[test]
fn required_process_miss_fails_the_session_start() {
    let setup = Setup::new();
    let server = calling(&[]);
    setup.provider(&server);
    let plan = json!([
        {"read": {"as": "hello"}},
        {"hold": {"release_on": "never"}},
    ]);
    process_with(&setup, "silent", &json!({"required": true}), &plan);
    let run = run_ask(&setup, "say hi");
    assert_ne!(run.code, Some(0), "the required miss fails the start");
    assert!(
        run.stderr.contains("fiber.test/silent"),
        "the miss names the extension: {}",
        run.stderr
    );
    let sessions = setup.home().join("projects");
    let committed = std::fs::read_dir(&sessions)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .path()
                    .join("sessions")
                    .read_dir()
                    .map(|mut inner| inner.next().is_some())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    assert!(!committed, "no session is committed");
}

/// Process startup and MCP startup overlap: the peer cannot ready
/// before the MCP wrapper starts, and the MCP handshake cannot finish
/// before the peer arrives. A serialized start fails this test, in
/// either order, instead of merely running slower.
#[test]
fn process_and_mcp_start_in_parallel() {
    let setup = Setup::new();
    // Armed before any start: the gated wrapper names the root in its
    // command line, so a failing exit still kills it with the setup.
    let _guard = fakes::Watchdog::matching(&setup.root.path().display().to_string());
    let fx = setup.workspace().join("fx");
    std::fs::create_dir_all(&fx).unwrap();
    let server = calling(&[function_call("call_probe", "probe", &json!({}))]);
    setup.provider(&server);
    configure_gated_fx(&setup, &fx);
    let gate = fx.join("gate");
    let hello_marker = setup.workspace().join("hello.json");
    process_with(
        &setup,
        "probe",
        &json!({}),
        &gated_plan(gate.to_str().unwrap(), hello_marker.to_str().unwrap()),
    );
    let run = ask(&setup, "probe the session");
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    assert!(fx.join("mcp-started").is_file(), "the MCP wrapper started");
}
