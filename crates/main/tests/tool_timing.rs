//! Part 8 tool phase timers at the binary level (`docs/state.md`,
//! "Diagnostic logs"): the built `fiber` runs `ask` against the fake
//! provider server with an ordinary Lua hook fixture. At `debug` a tool
//! call writes `tool.permission`, one `tool.hooks` line per actual
//! before/check/after dispatch, and `tool.run`, each with exactly `op`,
//! `ms` and `tool`; at the default nothing debug is written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    KillGroup, Run, Setup, function_call, hello, is_status, spawn_watched, stream, text_reply,
};

// Parses stdout into one JSON value per line, skipping status lines.
// This file's copy; every binary test file carries its own.
impl From<std::process::Output> for Run {
    fn from(output: std::process::Output) -> Self {
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Setup {
    /// Installs the extension in `source`.
    fn install(&self, source: PathBuf) {
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and writes configuration naming `fake/m` with `extra`.
    fn provider_with_extra(&self, server: &ProviderServer, extra: &Value) {
        let source = self.root.path().join("src").join("fake");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}).to_string(),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            })
            .to_string(),
        );
        self.install(source);
        let mut config = json!({"model": "fake/m"});
        for (key, value) in extra.as_object().into_iter().flatten() {
            config[key] = value.clone();
        }
        write(&self.home().join("config.json"), &config.to_string());
    }

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
    /// group. A watchdog beside it kills that group if this process dies
    /// first.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        self.run_with_env(args, &[])
    }

    /// [`Self::run`] with extra environment `env`.
    #[track_caller]
    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            !fakes::group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Run::from(output)
    }

    /// Writes `text` to `note.txt` in the workspace, starts a server whose
    /// model reads it and then answers, and runs `fiber ask`.
    #[track_caller]
    fn read_note(&self, text: &str, config: &Value) -> (Run, ProviderServer) {
        fs::write(self.workspace().join("note.txt"), text).unwrap();
        let server = ProviderServer::start([
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ])
        .unwrap();
        self.provider_with_extra(&server, config);
        let run = self.run(&["ask", "read the note"]);
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        (run, server)
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// Every `op_timing` line, in file order.
fn op_lines(setup: &Setup) -> Vec<Value> {
    log_lines(setup)
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

fn ops(setup: &Setup, op: &str) -> Vec<Value> {
    op_lines(setup)
        .into_iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

/// The wire promise on every Part 8 tool line: the ask identity, exactly
/// the keys `op`, `ms` and `tool` with an unsigned `ms`, the registered
/// tool, and none of the session's prompt, output, arguments, credential
/// or content anywhere in the line.
#[track_caller]
fn assert_tool_wire(lines: &[Value], session_id: &str, tool: &str) {
    assert_tool_wire_process(lines, session_id, tool, "ask");
}

/// [`assert_tool_wire`] for hub sessions, whose process is `session`.
fn assert_tool_wire_process(lines: &[Value], session_id: &str, tool: &str, process: &str) {
    assert!(!lines.is_empty(), "the test proved its lines exist first");
    for line in lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], process, "{line}");
        assert_eq!(line["session_id"], session_id, "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(
            data.keys().collect::<Vec<_>>(),
            vec!["ms", "op", "tool"],
            "{line}"
        );
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        assert_eq!(line["data"]["tool"], tool, "{line}");
        let raw = line.to_string();
        for sentinel in [
            "read the note",
            "Hello.",
            "note.txt",
            "sk-test",
            "the note says hi",
        ] {
            assert!(
                !raw.contains(sentinel),
                "no prompt, output, argument, credential or content in {raw}"
            );
        }
    }
}

/// Every Part 8 `op_timing` line carries only `op`, `ms` and `tool`;
/// Part 7's lines carry only `op` and `ms`. Nothing else is recorded.
#[track_caller]
fn assert_keys(setup: &Setup) {
    for line in op_lines(setup) {
        let mut keys: Vec<String> = line["data"]
            .as_object()
            .expect("data is an object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert!(
            keys == vec!["ms".to_owned(), "op".to_owned()]
                || keys == vec!["ms".to_owned(), "op".to_owned(), "tool".to_owned()],
            "no field outside op, ms and tool: {line}"
        );
    }
}

impl Run {
    fn completed(&self) -> &Value {
        &self.first("tool_call_completed")["payload"]
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.lines[0]["session_id"].as_str().unwrap())
    }

    /// The session log's durable kinds: stdout carries the same journey.
    fn logged_kinds(&self, setup: &Setup) -> Vec<String> {
        let events = fs::read_to_string(self.session_dir(setup).join("events.jsonl")).unwrap();
        events
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

/// `kinds` without text deltas.
fn without_deltas<'a>(kinds: &[&'a str]) -> Vec<&'a str> {
    kinds
        .iter()
        .copied()
        .filter(|kind| *kind != "assistant_message_delta")
        .collect()
}

/// The complete event kinds of a run whose model reads the note, then
/// answers, ignoring text deltas: one streamed reply may arrive as one
/// line or two, and the journey is the same either way.
fn read_kinds() -> Vec<&'static str> {
    vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]
}

/// A pass-through `before_tool` hook, a pass-through `check` hook and a
/// pass-through `after_tool` hook: every actual dispatch exists, and none
/// changes the call.
fn pass_hooks() -> Vec<(&'static str, String)> {
    vec![
        (
            "pass",
            "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
               run = function(call) return {} end })\n"
                .to_owned(),
        ),
        (
            "check",
            "fiber.hook(\"before_tool\", { phase = \"check\", timeout = 30000, on_failure = \"blocking\",\n\
               run = function(call) return {} end })\n"
                .to_owned(),
        ),
        (
            "see",
            "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
               run = function(call) return {} end })\n"
                .to_owned(),
        ),
    ]
}

#[test]
fn a_debug_tool_call_times_permission_hooks_and_run() {
    let setup = Setup::new();
    for (name, init) in pass_hooks() {
        setup.lua(name, &init);
    }
    fs::write(setup.workspace().join("note.txt"), "the note says hi\n").unwrap();
    let (run, _server) = setup.read_note(
        "the note says hi\n",
        &json!({"diagnostics": {"level": "debug"}}),
    );
    let session_id = run.session_id().to_owned();
    assert_eq!(without_deltas(&run.kinds()), read_kinds());
    // Deltas ride the live feed, not the durable log: both sides compare
    // without them.
    assert_eq!(
        run.logged_kinds(&setup),
        without_deltas(&run.kinds())
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
    );
    let completed = run.completed();
    assert_eq!(completed["status"], "completed");
    let permission = ops(&setup, "tool.permission");
    assert_eq!(permission.len(), 1, "{:?}", op_lines(&setup));
    assert_tool_wire(&permission, &session_id, "read");
    let hooks = ops(&setup, "tool.hooks");
    assert_eq!(
        hooks.len(),
        3,
        "before, check and after dispatch: {:?}",
        hooks
    );
    assert_tool_wire(&hooks, &session_id, "read");
    let runs = ops(&setup, "tool.run");
    assert_eq!(runs.len(), 1);
    assert_tool_wire(&runs, &session_id, "read");
    assert_keys(&setup);
}

#[test]
fn at_default_no_tool_timing_is_written() {
    let setup = Setup::new();
    for (name, init) in pass_hooks() {
        setup.lua(name, &init);
    }
    let (run, _server) = setup.read_note("the note says hi\n", &json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(without_deltas(&run.kinds()), read_kinds());
    assert!(op_lines(&setup).is_empty());
    assert!(
        log_lines(&setup)
            .iter()
            .all(|line| line["level"] != "debug"),
        "the default stays silent"
    );
}

#[test]
fn a_repository_debug_level_writes_no_tool_timing() {
    let setup = Setup::new();
    for (name, init) in pass_hooks() {
        setup.lua(name, &init);
    }
    fs::write(setup.workspace().join("note.txt"), "the note says hi\n").unwrap();
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    write(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}).to_string(),
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.all("notice")
            .into_iter()
            .any(|notice| notice["payload"]["code"] == "config_key_ignored"),
        "the repository value stays ignored"
    );
    assert!(op_lines(&setup).is_empty());
}

/// An `openai-responses` stream calling `shell` with `true`: it declares
/// `executes`, so step 7 reviews it.
fn shell_true_call() -> fakes::Response {
    let events = [
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "shell", "arguments": "{\"command\":\"true\"}"}}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}}}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    fakes::Response::stream(body)
}

/// Installs a synthetic provider `syn` whose models answer from `server`
/// and whose reviewer is `reviewer`, if any.
fn review_fixture(setup: &Setup, server: &ProviderServer, reviewer: Option<&str>) {
    let source = setup.root.path().join("src");
    let models: Vec<Value> = ["sess", "over", "prov", "pkg"]
        .iter()
        .map(|id| {
            json!({"id": id, "protocol": "openai-responses",
                   "base_url": format!("{}/v1", server.url()),
                   "context_window": 100000})
        })
        .collect();
    let mut manifest = json!({"name": "syn", "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    if let Some(into) = manifest.as_object_mut() {
        into.insert(
            "providers".to_owned(),
            json!({"syn": [format!("{}/v1", server.url())]}),
        );
    }
    let mut data = json!({"name": "syn", "credential": {"env": "SYN_API_KEY"}, "models": models});
    if let Some(reviewer) = reviewer {
        data["reviewer_model"] = json!(reviewer);
    }
    write(&source.join("extension.json"), &manifest.to_string());
    write(&source.join("providers/syn.json"), &data.to_string());
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
fn a_debug_review_times_one_stage_with_permission_and_run() {
    let setup = Setup::new();
    let server = ProviderServer::start([shell_true_call(), text_reply("allow"), hello()]).unwrap();
    review_fixture(&setup, &server, Some("pkg"));
    write(
        &setup.home().join("config.json"),
        &json!({"model": "syn/sess", "diagnostics": {"level": "debug"}}).to_string(),
    );
    let run = setup.run_with_env(
        &["ask", "--model", "syn/sess", "Run true."],
        &[("SYN_API_KEY", "sk-test-syn")],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let session_id = run.session_id().to_owned();
    assert_eq!(
        without_deltas(&run.kinds()),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
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
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reviewed call resolves");
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    let reviewer = ops(&setup, "tool.reviewer");
    assert_eq!(reviewer.len(), 1);
    assert_tool_wire(&reviewer, &session_id, "shell");
    let permission = ops(&setup, "tool.permission");
    assert_eq!(permission.len(), 1);
    assert_tool_wire(&permission, &session_id, "shell");
    let runs = ops(&setup, "tool.run");
    assert_eq!(runs.len(), 1);
    assert_tool_wire(&runs, &session_id, "shell");
    assert_keys(&setup);
}

/// Fixture helpers for the escalation and resume journeys below: the
/// configured model, a standing ask, and the session log. Hub, socket,
/// session-guard and deadline handling come from the shared [`support`]
/// harness.
impl Setup {
    /// The configured model, with `session.idle_exit_ms` when given.
    fn idle(&self, ms: Option<u64>) {
        let config = match ms {
            Some(ms) => json!({"model": "fake/m", "session": {"idle_exit_ms": ms}}),
            None => json!({"model": "fake/m"}),
        };
        support::write_json(&self.home().join("config.json"), &config);
    }

    /// A standing ask for `shell` with `echo hi`: with a client the loop
    /// asks a person.
    fn standing_ask(&self) {
        fs::write(
            self.home().join("rules"),
            format!(
                "{}\n",
                json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
            ),
        )
        .unwrap();
    }

    /// The log of session `id`, wherever its project is.
    fn session_log(&self, id: &str) -> Vec<Value> {
        let projects = self.home().join("projects");
        let log = fs::read_dir(projects)
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .join("sessions")
                    .join(id)
                    .join("events.jsonl")
            })
            .find(|log| log.is_file())
            .expect("the session's log exists");
        fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn command(id: &str, session: &str, command: &str, args: Value) -> Value {
    json!({"id": id, "session_id": session, "command": command, "args": args})
}

/// Sends one hub command over the shared [`support`] socket.
fn send(client: &support::Socket, line: &Value) {
    client.send(&line.to_string());
}

fn subscribe(id: &str, session: &str) -> Value {
    command(id, session, "subscribe", json!({"level": "full"}))
}

fn close(session: &str) -> Value {
    json!({"id": "c_close", "session_id": session, "command": "close"})
}

fn allow(id: &str, session: &str, request: &str) -> Value {
    command(
        id,
        session,
        "reply",
        json!({"request_id": request, "decision": "allow"}),
    )
}

/// Whether `line` acknowledges command `id`.
fn acknowledges(line: &Value, id: &str) -> bool {
    line["kind"] == "command_accepted" && line["payload"]["command_id"] == id
}

/// A stream asking for `shell` with `echo hi`.
fn echo_call() -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_1",
        "call_id": "call_1",
        "name": "shell",
        "arguments": json!({"command": "echo hi"}).to_string()
    }})])
}

/// Waits for the relayed `fiber_exited`, then for the session process to
/// be gone, over the shared [`support`] socket and deadline helpers.
fn until_exited(client: &support::Socket, setup: &Setup) -> Vec<Value> {
    let lines = support::until(client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    let workspace = setup.workspace().to_string_lossy().into_owned();
    fakes::try_matching_exits(&workspace, setup.deadline).unwrap_or_else(|err| {
        panic!("waited until the deadline for the session process to exit: {err}")
    });
    lines
}

/// A block carries the call to a person: two stages, then the escalation's
/// wait. The person's allow holds both stages' work and the wait.
#[test]
fn a_debug_escalation_times_two_stages_and_the_wait() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        shell_true_call(),
        text_reply("check"),
        text_reply("block: nope"),
        hello(),
        hello(),
    ])
    .unwrap();
    review_fixture(&setup, &server, Some("pkg"));
    // The hub runs under the shared harness's bare environment, so the
    // `SYN_API_KEY` the `syn` provider declares never arrives: store the
    // key the way `fiber login syn` writes it.
    config::store_credential(
        &setup.home(),
        "syn",
        "default",
        &contract::Secret::new("sk-test-syn".to_owned()),
    )
    .unwrap();
    write(
        &setup.home().join("config.json"),
        &json!({
            "model": "syn/sess",
            "diagnostics": {"level": "debug"},
            "reviewer": {"block_limits": {"consecutive": 1}},
        })
        .to_string(),
    );
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = support::connect_hub(&setup, &hub);
    let guard = support::SessionGuard::arm(setup.deadline, &workspace);
    send(
        &client,
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "content": [{"type": "text", "text": "Run true."}]},
        }),
    );
    let started = support::recv(&client, "the start acknowledgement");
    assert!(acknowledges(&started, "c_start"), "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&client, &subscribe("c_sub", &id));
    support::until(&client, "the subscribe acknowledgement", |line| {
        acknowledges(line, "c_sub")
    });
    let asked = support::until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&client, &allow("c_reply", &id, &request));
    let done = support::until(&client, "the resumed turn", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        done.iter().any(|line| acknowledges(line, "c_reply")),
        "the reply was accepted: {done:?}"
    );
    let resolved = done
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the escalation");
    assert_eq!(resolved["payload"]["decided_by"], "person");
    assert!(
        done.iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the allowed call ran: {done:?}"
    );
    send(&client, &close(&id));
    until_exited(&client, &setup);
    guard.wait_gone();
    drop(client);
    // Two stages, one permission order holding both, one run.
    let reviewer: Vec<Value> = ops(&setup, "tool.reviewer")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(reviewer.len(), 2);
    assert_tool_wire_process(&reviewer, &id, "shell", "session");
    let permission: Vec<Value> = ops(&setup, "tool.permission")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(permission.len(), 1);
    assert_tool_wire_process(&permission, &id, "shell", "session");
    let runs: Vec<Value> = ops(&setup, "tool.run")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(runs.len(), 1);
    assert_tool_wire_process(&runs, &id, "shell", "session");
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}

/// A suspended approval resumes in a fresh process with a fresh segment:
/// the abandoned segment's record stays in the first process's file, and
/// the resumed decision writes the second.
#[test]
fn a_suspended_approval_resumes_with_a_fresh_segment() {
    let setup = Setup::new();
    let server = ProviderServer::start([echo_call(), hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    setup.standing_ask();
    // Debug with an idle exit: the approval wait suspends the session.
    write(
        &setup.home().join("config.json"),
        &json!({
            "model": "fake/m",
            "diagnostics": {"level": "debug"},
            "session": {"idle_exit_ms": 2000},
        })
        .to_string(),
    );
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = support::connect_hub(&setup, &hub);
    let guard = support::SessionGuard::arm(setup.deadline, &workspace);
    send(
        &client,
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "content": [{"type": "text", "text": "run it"}]},
        }),
    );
    let started = support::recv(&client, "the start acknowledgement");
    assert!(acknowledges(&started, "c_start"), "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&client, &subscribe("c_sub", &id));
    support::until(&client, "the subscribe acknowledgement", |line| {
        acknowledges(line, "c_sub")
    });
    server.release();
    let asked = support::until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let exited = until_exited(&client, &setup);
    assert_eq!(
        exited.last().unwrap()["payload"]["suspended_on"],
        request.as_str()
    );
    // The abandoned segment closed in the first process: one permission
    // record, no run.
    let first: Vec<Value> = ops(&setup, "tool.permission")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(first.len(), 1);
    assert_tool_wire_process(&first, &id, "shell", "session");
    assert!(
        ops(&setup, "tool.run")
            .iter()
            .all(|line| line["session_id"] != id),
        "the abandoned call never ran"
    );
    setup.idle(None);
    // The resume keeps debug: the idle exit is gone, the timers stay.
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}).to_string(),
    );
    send(&client, &allow("c_reply", &id, &request));
    let resumed = support::until(&client, "the resumed turn", |line| {
        line["kind"] == "turn_completed"
    });
    let raised = resumed
        .iter()
        .rev()
        .find(|line| line["kind"] == "permission_requested")
        .expect("the resumed session raised the request again");
    assert_eq!(raised["payload"]["request_id"], request.as_str());
    let resolved = resumed
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the request");
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["decided_by"], "person");
    assert!(
        resumed
            .iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the allowed call ran: {resumed:?}"
    );
    send(&client, &close(&id));
    until_exited(&client, &setup);
    guard.wait_gone();
    drop(client);
    // Two segments, one per process, under the same request id.
    let permission: Vec<Value> = ops(&setup, "tool.permission")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(permission.len(), 2, "one segment per process");
    assert_tool_wire_process(&permission, &id, "shell", "session");
    let runs: Vec<Value> = ops(&setup, "tool.run")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(runs.len(), 1, "only the resumed segment ran");
    assert_tool_wire_process(&runs, &id, "shell", "session");
    support::assert_one_continued_log(&setup.session_log(&id), 2);
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}

/// One `ask_user` question: the native tool asks whoever drives the session.
fn one_question() -> Value {
    json!([{"header": "Pick", "question": "Which one?", "options": [{"label": "a"}, {"label": "b"}]}])
}

/// A stream whose one item calls `ask_user` with `questions`.
fn ask_user_call(questions: &Value) -> fakes::Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_ask",
        "call_id": "call_ask",
        "name": "ask_user",
        "arguments": json!({"questions": questions}).to_string()
    }})])
}

/// A suspended interaction resumes with no second permission order: its
/// call was already allowed, so only the resumed run is timed.
#[test]
fn a_suspended_interaction_resumes_with_no_second_order() {
    let setup = Setup::new();
    let server = ProviderServer::start([ask_user_call(&one_question()), hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    write(
        &setup.home().join("config.json"),
        &json!({
            "model": "fake/m",
            "diagnostics": {"level": "debug"},
            "session": {"idle_exit_ms": 2000},
        })
        .to_string(),
    );
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let hub = Arc::new(Mutex::new(None));
    let (client, _) = support::connect_hub(&setup, &hub);
    let guard = support::SessionGuard::arm(setup.deadline, &workspace);
    send(
        &client,
        &json!({
            "id": "c_start",
            "command": "start",
            "args": {"workspace": workspace, "content": [{"type": "text", "text": "ask-me"}]},
        }),
    );
    let started = support::recv(&client, "the start acknowledgement");
    assert!(acknowledges(&started, "c_start"), "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    send(&client, &subscribe("c_sub", &id));
    support::until(&client, "the subscribe acknowledgement", |line| {
        acknowledges(line, "c_sub")
    });
    server.release();
    let asked = support::until(&client, "interaction_requested", |line| {
        line["kind"] == "interaction_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let exited = until_exited(&client, &setup);
    assert_eq!(
        exited.last().unwrap()["payload"]["suspended_on"],
        request.as_str()
    );
    // The live fast-path order ran once, and its worker was entered once.
    let permission: Vec<Value> = ops(&setup, "tool.permission")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(permission.len(), 1);
    assert_tool_wire_process(&permission, &id, "ask_user", "session");
    let runs: Vec<Value> = ops(&setup, "tool.run")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(runs.len(), 1);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "diagnostics": {"level": "debug"}}).to_string(),
    );
    send(
        &client,
        &command(
            "c_reply",
            &id,
            "reply",
            json!({"request_id": request, "answers": [{"labels": ["a"]}]}),
        ),
    );
    let done = support::until(&client, "the resumed turn", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(
        done.iter().any(|line| acknowledges(line, "c_reply")),
        "the reply was accepted: {done:?}"
    );
    assert!(
        done.iter()
            .any(|line| line["kind"] == "tool_call_completed"),
        "the answered call ran again: {done:?}"
    );
    send(&client, &close(&id));
    until_exited(&client, &setup);
    guard.wait_gone();
    drop(client);
    // No second permission order; the resumed run is timed.
    let permission: Vec<Value> = ops(&setup, "tool.permission")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(permission.len(), 1, "an allowed call is never judged again");
    let runs: Vec<Value> = ops(&setup, "tool.run")
        .into_iter()
        .filter(|line| line["session_id"] == id)
        .collect();
    assert_eq!(runs.len(), 2, "both entered runs are timed");
    assert_tool_wire_process(&runs, &id, "ask_user", "session");
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}
