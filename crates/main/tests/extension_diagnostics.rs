//! Lua provider HTTP and hook chains at the binary level (`docs/state.md`,
//! "Diagnostic logs"): the built `fiber` runs `ask` and `session` with a
//! Lua provider as the session model. At debug each eligible Lua request
//! writes one `http_request` line without its URL, and each non-tool
//! hook dispatch writes one `op_timing` line; otherwise both stay silent.
//! A late model switch starts a fresh provider VM that keeps the sink.

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
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::thread;

use fakes::{Deadline, ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    HELLO_KINDS, KillGroup, Run, Setup, Socket, hello, is_status, run_to_exit, until, until_close,
    write_json,
};

/// Installs the Lua extension `test-ext` with hooks on every reachable
/// non-tool chain and a `test` provider whose models, credential, quota
/// and cost functions each make one real HTTP request, plus a signer that
/// sends none. Model URLs come from the test server, so no secret is
/// configured anywhere.
fn install_test_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("luasrc");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "test-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"test": [format!("{server_url}/v1")]}}),
    );
    fs::write(
        source.join("init.lua"),
        "fiber.hook(\"before_message\", { timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(msg) return nil end })\n\
         fiber.hook(\"turn_start\", { timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(ctx) return { context = \"t\" } end })\n\
         fiber.hook(\"before_model_call\", { timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(call) return nil end })\n\
         fiber.hook(\"turn_end\", { timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(reply) return nil end })\n",
    )
    .unwrap();
    fs::create_dir_all(source.join("providers")).unwrap();
    fs::write(
        source.join("providers/test.lua"),
        format!(
            "fiber.provider(\"test\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ url = \"{server_url}/lua-models\" }})\n\
                 return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{server_url}/v1\", context_window = 100000 }}}}\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 local r = host.http({{ url = \"{server_url}/lua-quota\" }})\n\
                 return {{ windows = {{ {{ name = \"w\", used_percent = 1 }} }} }}\n\
               end }},\n\
               cost = {{ timeout = 60000, run = function(c)\n\
                 local r = host.http({{ url = \"{server_url}/lua-cost\" }})\n\
                 return 0.25\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
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

/// Installs a second Lua extension with a hook-free `late` provider for
/// the switch test.
fn install_late_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("latesrc");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "late-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"late": [format!("{server_url}/v1")]}}),
    );
    fs::create_dir_all(source.join("providers")).unwrap();
    fs::write(
        source.join("providers/late.lua"),
        format!(
            "fiber.provider(\"late\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ url = \"{server_url}/lua-models\" }})\n\
                 return {{{{ id = \"m2\", protocol = \"openai-responses\", base_url = \"{server_url}/v1\", context_window = 100000 }}}}\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"late-tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
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

/// A server answering every Lua purpose plus native model calls, without
/// limit: turns retry and refreshes repeat against it.
fn start_routed() -> ProviderServer {
    ProviderServer::start_responding(|request| match request.path.as_str() {
        "/lua-models" | "/lua-token" | "/lua-quota" => Response::status(200, "{}"),
        "/lua-cost" => Response::status(200, "0.25"),
        "/v1/responses" => hello(),
        _ => Response::status(500, "unrouted"),
    })
    .unwrap()
}

fn configure(setup: &Setup, model: &str, debug: bool) {
    let mut config = json!({"model": model});
    if debug {
        config["diagnostics"] = json!({"level": "debug"});
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// Runs `fiber ask hi` in the workspace to exit and parses its stdout.
#[track_caller]
fn ask(setup: &Setup) -> Run {
    let mut command = setup.fiber(&["ask", "hi"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    let mut files: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// The diagnostic lines of one file.
fn diagnostic_file(setup: &Setup, name: &str) -> Vec<Value> {
    let text = fs::read_to_string(setup.home().join("logs").join(name)).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The turn a hook context joins: HELLO_KINDS with the hook's `context_added`.
fn hook_kinds() -> Vec<&'static str> {
    let mut kinds: Vec<&str> = HELLO_KINDS.into();
    kinds.insert(6, "context_added");
    kinds
}

/// Values no Lua or native diagnostic line may hold: the prompt, the
/// reply, the token the credential returned, and the request URLs.
fn sentinels(server_url: &str) -> Vec<String> {
    vec![
        "hi".to_owned(),
        "Hello.".to_owned(),
        // The credential value quoted: the bare word also matches the
        // native `first_token_ms` key.
        "\"tok\"".to_owned(),
        server_url.to_owned(),
        "/lua-models".to_owned(),
        "/lua-token".to_owned(),
        "/lua-quota".to_owned(),
        "/lua-cost".to_owned(),
    ]
}

#[test]
fn debug_ask_records_lua_requests_and_hook_chains() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    configure(&setup, "test/m1", true);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hook_kinds());
    // The models and credential purposes ran actual HTTP, plus the native
    // model call: the quota and cost fetches run off the request path, so
    // either can outlive an ask.
    for path in ["/lua-models", "/lua-token", "/v1/responses"] {
        assert!(
            server.requests().iter().any(|request| request.path == path),
            "received {path}: {:?}",
            server
                .requests()
                .iter()
                .map(|request| request.path.clone())
                .collect::<Vec<_>>()
        );
    }
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.path != "/lua-cost"),
        "no cost lookup inside an ask"
    );
    let id = run.session_id().to_owned();
    // Discovery and the credential read precede the session's attach, so
    // they land in the pid-named file without a session id.
    let pid_lines: Vec<Value> = log_lines(&setup)
        .into_iter()
        .filter(|line| line.get("session_id").is_none())
        .collect();
    let pid_requests: Vec<&Value> = pid_lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    assert_eq!(pid_requests.len(), 2);
    for line in &pid_requests {
        assert_eq!(line["level"], "debug");
        assert_eq!(line["process"], "ask");
        assert_eq!(line["message"], "An HTTP request ended.");
    }
    let mut pid_purposes: Vec<&str> = pid_requests
        .iter()
        .map(|line| line["data"]["purpose"].as_str().unwrap())
        .collect();
    pid_purposes.sort_unstable();
    assert_eq!(pid_purposes, ["model_list", "token_refresh"]);
    // Everything past the attach carries the Ask process and the session.
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    for line in &lines {
        assert_eq!(line["process"], "ask");
        assert_eq!(line["session_id"], id.as_str());
    }
    let requests: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request")
        .collect();
    // The native model call's record is always in the ask log. The quota
    // fetch runs off the request path and can outlive an ask, so its
    // record may not have landed yet; the session test asserts it instead.
    assert_eq!(
        requests
            .iter()
            .filter(|line| line["data"]["purpose"] == "model_call")
            .count(),
        1,
    );
    assert!(
        requests
            .iter()
            .all(|line| line["data"]["purpose"] == "model_call"
                || line["data"]["purpose"] == "quota"),
        "only the model call and the quota fetch write ask-log http_request lines: {requests:?}"
    );
    let native = requests
        .iter()
        .find(|line| line["data"]["purpose"] == "model_call")
        .expect("one model_call http_request");
    assert_eq!(native["data"]["purpose"], "model_call");
    assert_eq!(native["data"]["model"], "m1");
    // Each hook chain timed once per actual dispatch; unrelated installed
    // diagnostic operations are ignored, never asserted absent.
    let mut hooks: Vec<&str> = lines
        .iter()
        .filter(|line| line["code"] == "op_timing")
        .map(|line| line["data"]["op"].as_str().unwrap())
        .filter(|op| op.starts_with("hook."))
        .collect();
    hooks.sort_unstable();
    assert_eq!(
        hooks,
        [
            "hook.before_message",
            "hook.before_model_call",
            "hook.session_start",
            "hook.turn_end",
            "hook.turn_start"
        ]
    );
    for line in lines.iter().filter(|line| line["code"] == "op_timing") {
        assert_eq!(line["level"], "debug");
        assert_eq!(line["message"], "An operation ended.");
        let data = line["data"].as_object().unwrap();
        let mut keys: Vec<&String> = data.keys().collect();
        keys.sort();
        assert_eq!(keys, ["ms", "op"]);
        assert!(data["ms"].is_u64(), "whole milliseconds: {line}");
    }
    for line in lines.iter().chain(pid_requests.iter().copied()) {
        let wire = serde_json::to_string(line).unwrap();
        assert!(!wire.contains("null"), "no null: {wire}");
        for sentinel in sentinels(&url) {
            assert!(!wire.contains(&sentinel), "no {sentinel}: {wire}");
        }
    }
}

#[test]
fn default_ask_runs_silent() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    configure(&setup, "test/m1", false);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hook_kinds());
    // The same provider and hook work runs and its requests arrive.
    for path in ["/lua-models", "/lua-token", "/v1/responses"] {
        assert!(
            server.requests().iter().any(|request| request.path == path),
            "received {path}"
        );
    }
    // At the default level no debug line exists anywhere: the P5 lines
    // stay silent with the work itself unchanged.
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| line["code"] != "http_request" && line["code"] != "op_timing"),
        "{lines:?}"
    );
}

#[test]
fn repository_debug_stays_silent_with_a_notice() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    configure(&setup, "test/m1", false);
    write_json(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"diagnostics": {"level": "debug"}}),
    );
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    for path in ["/lua-models", "/lua-token", "/v1/responses"] {
        assert!(
            server.requests().iter().any(|request| request.path == path),
            "received {path}"
        );
    }
    let notice = run
        .all("notice")
        .into_iter()
        .find(|notice| notice["payload"]["code"] == "config_key_ignored")
        .expect("one config_key_ignored notice");
    let message = notice["payload"]["message"].as_str().unwrap();
    assert!(message.contains("diagnostics.level"), "{message}");
    let lines = log_lines(&setup);
    assert!(
        lines.iter().all(|line| line["level"] != "debug"),
        "the repository value stays ignored: {lines:?}"
    );
}

/// Starts `fiber session --id <id>` with its stdout drained on a thread.
#[track_caller]
fn start_session(setup: &Setup, id: &str) -> Running {
    let workspace = setup.workspace();
    let workspace = workspace.to_str().unwrap();
    let mut command = setup.fiber(&["session", "--id", id, "--workspace", workspace]);
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr_pipe = child.stderr.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    let (err_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        match std::io::Read::read_to_string(&mut BufReader::new(stderr_pipe), &mut text) {
            Ok(_) | Err(_) => {}
        }
        match err_tx.send(text) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    Running {
        child,
        watchdog,
        group,
        guard,
        lines,
        stderr: stderr_rx,
        deadline: setup.deadline,
    }
}

struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    guard: KillGroup,
    lines: mpsc::Receiver<String>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

impl Running {
    /// Waits for the first stdout line, then connects: the socket accepts
    /// by then, so the line is the signal it does.
    #[track_caller]
    fn connect(&self, socket: &std::path::Path) -> Socket {
        match self.deadline.recv(&self.lines) {
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's first stdout line")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session exited before its first stdout line")
            }
        }
        Socket::connect(self.deadline, socket)
    }

    /// Reads stdout lines until one of `kind` arrives.
    #[track_caller]
    fn wait_for(&self, kind: &str) {
        loop {
            let line = match self.deadline.recv(&self.lines) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for {kind} on the session's stdout")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("the session's stdout closed before {kind}")
                }
            };
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["kind"] == kind {
                return;
            }
        }
    }

    /// Waits for the process to exit, drains stdout to EOF, and asserts
    /// nothing it started is left in its group.
    #[track_caller]
    fn wait(self) -> (ExitStatus, String) {
        let deadline = self.deadline;
        self.wait_on(deadline)
    }

    /// As [`Running::wait`], under `deadline`.
    #[track_caller]
    fn wait_on(mut self, deadline: Deadline) -> (ExitStatus, String) {
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(self.child.wait()).unwrap());
        let status = match deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                support::expired(self.deadline, self.group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session's wait thread ended before the session exited")
            }
        };
        assert!(
            !fakes::group_alive(self.group),
            "the session left a process in its group behind"
        );
        std::mem::forget(self.guard);
        loop {
            match deadline.recv(&self.lines) {
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited until the deadline for the session's stdout to close")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let stderr = match deadline.recv(&self.stderr) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited until the deadline for the session's stderr")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr reader ended without its text")
            }
        };
        self.watchdog.stand_down(deadline.cleanup_phase());
        (status, stderr)
    }
}

#[track_caller]
fn subscribe(client: &Socket) {
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    let lines = until(client, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    assert_eq!(lines.len(), 1);
}

#[track_caller]
fn prompt(client: &Socket, id: &str, text: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"prompt","args":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
    ));
}

#[track_caller]
fn switch_model(client: &Socket, id: &str, reference: &str) {
    client.send(&format!(
        r#"{{"id":"{id}","command":"model","args":{{"model":"{reference}"}}}}"#
    ));
}

#[track_caller]
fn close(client: &Socket) {
    client.send(r#"{"id":"c_close","command":"close"}"#);
}

#[test]
fn late_switch_starts_fresh_with_the_sink() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    install_late_provider(&setup, &url);
    configure(&setup, "test/m1", true);
    let id = doors::mint("s_");
    let running = start_session(&setup, &id);
    let client = running.connect(&setup.session_socket(&id));
    running.wait_for("extensions_loaded");
    subscribe(&client);
    prompt(&client, "c_prompt", "hi");
    let mut stream = until(&client, "the first turn", |line| {
        line["kind"] == "turn_completed"
    });
    // The switch's own acknowledgement lands before the next prompt.
    switch_model(&client, "c_model", "late/m2");
    stream.extend(until(&client, "the switch acknowledgement", |line| {
        line["kind"] == "model_changed"
    }));
    prompt(&client, "c_again", "again");
    stream.extend(until(&client, "the second turn", |line| {
        line["kind"] == "turn_completed"
    }));
    // No diagnostic line leaks into the durable stream: every line has
    // a kind. `session_status` is ephemeral and unpinned by design.
    for line in &stream {
        assert!(line.get("kind").is_some(), "no diagnostic line: {line}");
    }
    let kinds: Vec<&str> = stream
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .filter(|kind| *kind != "session_status")
        .collect();
    assert_eq!(
        kinds,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "clients",
            "preamble_built",
            "opening_message",
            "turn_started",
            "command_accepted",
            "context_added",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "model_changed",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "context_added",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let changed = stream
        .iter()
        .find(|line| line["kind"] == "model_changed")
        .unwrap();
    assert_eq!(changed["payload"]["before"]["model"], "test/m1");
    assert_eq!(changed["payload"]["after"]["model"], "late/m2");
    close(&client);
    until_close(&client);
    drop(client);
    let (status, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // The late provider was unloaded after startup, so the switch starts
    // a fresh VM: its credential read records through the retained sink
    // into the session's own writer.
    let lines = diagnostic_file(&setup, &format!("session-{id}.log"));
    for line in &lines {
        assert_eq!(line["process"], "session");
        assert_eq!(line["session_id"], id.as_str());
    }
    let late: Vec<&Value> = lines
        .iter()
        .filter(|line| {
            line["code"] == "http_request"
                && line["data"]["provider"] == "late"
                && line["data"]["purpose"] == "token_refresh"
        })
        .collect();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0]["data"]["purpose"], "token_refresh");
    assert_eq!(late[0]["data"]["attempt"], 1);
    assert_eq!(late[0]["message"], "An HTTP request ended.");
    let wire = serde_json::to_string(late[0]).unwrap();
    assert!(!wire.contains("late-tok"), "no credential: {wire}");
    assert!(!wire.contains(&url), "no URL: {wire}");
}

/// Installs `fiber.test/hook-http`, whose `turn_start` hook fetches one
/// URL and discards it: the fetch is a non-provider `host.http` call.
fn install_hook_http(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("hooksrc");
    fs::create_dir_all(&source).unwrap();
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fiber.test/hook-http", "version": "0.1.0", "fiber": "0.1.0", "api": 1}),
    );
    fs::write(
        source.join("init.lua"),
        format!(
            "fiber.hook(\"turn_start\", {{ timeout = 60000, on_failure = \"blocking\",\n\
               run = function(ctx)\n\
               local r = host.http({{ url = \"{server_url}/lua-quota\" }})\n\
               return nil end }})\n"
        ),
    )
    .unwrap();
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

/// A hook's `host.http` call writes one `extension` line with the
/// extension's name and no `host` or `path`: the rule is where the call
/// is made, outside any provider function.
#[test]
fn debug_ask_records_a_hook_http_call_as_extension() {
    let setup = Setup::new();
    let server = start_routed();
    let url = server.url();
    install_test_provider(&setup, &url);
    install_hook_http(&setup, &url);
    configure(&setup, "test/m1", true);
    let run = ask(&setup);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    for kind in hook_kinds() {
        assert!(kinds.contains(&kind), "{kind} in {kinds:?}");
    }
    assert!(
        server.await_requests(5, setup.deadline),
        "the hook fetch joins the provider fetches and the model call"
    );
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let hooks: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "http_request" && line["data"]["purpose"] == "extension")
        .collect();
    assert_eq!(hooks.len(), 1, "one hook call writes one line: {lines:?}");
    let line = hooks[0];
    let data = &line["data"];
    assert_eq!(data["extension"], "fiber.test/hook-http", "{data}");
    assert_eq!(data["status"], 200, "{data}");
    assert_eq!(data["attempt"], 1, "{data}");
    assert!(data.get("provider").is_none(), "{data}");
    assert!(data.get("model").is_none(), "{data}");
    assert!(data.get("host").is_none(), "{data}");
    assert!(data.get("path").is_none(), "{data}");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["code"] == "http_request"
                && line["data"]["purpose"] == "model_call")
            .count(),
        1,
        "the native model call's line is intact"
    );
}
