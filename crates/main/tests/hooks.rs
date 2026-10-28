//! Binary-level tests of a Lua extension's `after_tool`, `before_tool` and
//! `before_message` hooks through `fiber ask` and `fiber session` (`docs/extensions.md`,
//! "Hooks"; `docs/testing.md`, "Levels"): the built `fiber` runs in its
//! own process group with its own `FIBER_HOME`, holding an ordinary
//! provider whose base URL is the fake server, and the extensions under
//! test. No test here waits on a hook's timeout; those run on the fake
//! clock in `extensions`.

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
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{
    KillGroup, Run, Setup, function_call, hello_single_delta as hello, is_status, on_disk,
    spawn_watched, stream,
};

/// The fixture extension's name.
const FIXTURE: &str = "fiber.test/lua-fixture";

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

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
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
}

/// The tool output the second request carries.
fn output_sent(server: &ProviderServer) -> String {
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The complete event kinds of a run whose model reads the note, then
/// answers: `loading` follows `extensions_loaded`, the notices loading
/// raised, and `hooked` precedes the read's `tool_call_completed`, the
/// notices its hooks raised.
fn read_kinds(loading: &[&'static str], hooked: &[&'static str]) -> Vec<&'static str> {
    [
        &["session_started", "fiber_started", "extensions_loaded"][..],
        loading,
        &[
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
        ],
        hooked,
        &[
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
    ]
    .concat()
}

/// The complete event kinds of a run whose call never started: as
/// [`read_kinds`] with no notices, without `tool_call_started`. A refused,
/// failed or denied-before-permission call writes no permission line.
fn unstarted_kinds() -> Vec<&'static str> {
    read_kinds(&[], &[])
        .into_iter()
        .filter(|kind| *kind != "tool_call_started")
        .collect()
}

/// An `after_tool` hook of `phase` that appends `|<tag>`.
fn tagging(tag: &str, phase: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ phase = \"{phase}\", on_failure = \"blocking\", timeout = 30000,\n\
           run = function(call) return {{ content = call.content .. \"|{tag}\" }} end }})\n"
    )
}

#[test]
fn an_installed_extension_is_loaded_before_the_first_request_and_its_hook_redacts_the_result() {
    let setup = Setup::new();
    setup.install(fakes::lua_fixture());
    let (run, server) = setup.read_note("the password is hunter2\n", &json!({}));

    // `extensions_loaded` follows `fiber_started`, before the first step.
    // The fixture's `models()` fails without its secret, so loading raises
    // one notice.
    assert_eq!(run.kinds(), read_kinds(&["notice"], &[]));
    assert_eq!(
        run.first("extensions_loaded")["payload"]["extensions"],
        json!([
            {"name": "fake", "version": "v0.0.0"},
            {"name": FIXTURE, "version": "v1.0.0"},
        ])
    );

    let completed = run.completed();
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "the password is [redacted]\n"}])
    );
    assert_eq!(completed["changed_by"], json!([FIXTURE]));
    assert_eq!(output_sent(&server), "the password is [redacted]\n");
    let dir = run.session_dir(&setup);
    assert!(!on_disk(&dir).contains("hunter2"));
    assert!(on_disk(&dir).contains("[redacted]"));
}

#[test]
fn a_result_no_hook_changes_names_nobody() {
    let setup = Setup::new();
    setup.install(fakes::lua_fixture());
    let (run, server) = setup.read_note("nothing secret\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&["notice"], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "nothing secret\n");
    assert!(completed.get("changed_by").is_none());
    assert_eq!(output_sent(&server), "nothing secret\n");
}

#[test]
fn the_artifact_holds_the_hooks_artifact_text() {
    let setup = Setup::new();
    setup.lua(
        "art",
        "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { content = \"summary\", artifact = \"the whole log\" } end })\n",
    );
    let (run, server) = setup.read_note("raw build output\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "summary");
    assert_eq!(completed["changed_by"], json!(["fiber.test/art"]));
    let dir = run.session_dir(&setup);
    let artifact = completed["artifact"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(dir.join(artifact)).unwrap(),
        "the whole log"
    );
    assert_eq!(output_sent(&server), "summary");
    assert!(!on_disk(&dir).contains("raw build output"));
}

#[test]
fn hooks_that_do_not_register_or_fail_softly_leave_the_output_with_a_notice() {
    let setup = Setup::new();
    setup.lua(
        "bad",
        "local run = function(call) return { content = \"BAD\" } end\n\
         fiber.hook(\"after_tool\", { on_failure = \"blocking\", run = run })\n\
         fiber.hook(\"after_tool\", { timeout = 30000, run = run })\n",
    );
    setup.lua(
        "soft",
        "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"non-blocking\",\n\
           run = function(call) error(\"soft failure\") end })\n",
    );
    let (run, server) = setup.read_note("original\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&["notice", "notice"], &["notice"]));
    let notices = run.all("notice");
    let registered: Vec<(&str, &str, &str)> = notices
        .iter()
        .map(|n| {
            (
                n["payload"]["code"].as_str().unwrap(),
                n["payload"]["extension"].as_str().unwrap_or_default(),
                n["payload"]["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        registered.contains(&(
            "extension_failed",
            "fiber.test/bad",
            "`after_tool` hook not registered: missing `timeout`"
        )),
        "{registered:?}"
    );
    assert!(
        registered.contains(&(
            "extension_failed",
            "fiber.test/bad",
            "`after_tool` hook not registered: missing `on_failure`"
        )),
        "{registered:?}"
    );
    let soft = registered
        .iter()
        .find(|(code, _, _)| *code == "hook_failed")
        .expect("a hook_failed notice");
    assert_eq!(soft.1, "fiber.test/soft");
    assert!(soft.2.contains("soft failure"), "{}", soft.2);
    // Neither extension changed the result.
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "original\n");
    assert!(completed.get("changed_by").is_none());
    assert_eq!(output_sent(&server), "original\n");
}

#[test]
fn a_blocking_hook_that_fails_withholds_the_output_and_keeps_the_status() {
    let setup = Setup::new();
    setup.lua(
        "hard",
        "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"hard failure\") end })\n",
    );
    let (run, server) = setup.read_note("secret text\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let withheld = "Output withheld: the `after_tool` hook of extension fiber.test/hard failed.";
    let completed = run.completed();
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": withheld}])
    );
    assert!(completed.get("artifact").is_none());
    assert_eq!(completed["changed_by"], json!(["fiber.test/hard"]));
    assert_eq!(output_sent(&server), withheld);
    let dir = run.session_dir(&setup);
    assert!(!on_disk(&dir).contains("secret text"));
    assert!(
        !dir.join("artifacts").exists()
            || fs::read_dir(dir.join("artifacts"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn hooks_run_by_phase_then_hooks_order_then_name() {
    let setup = Setup::new();
    setup.lua("a", &tagging("a", "transform"));
    setup.lua(
        "b",
        &(tagging("b-t", "transform") + &tagging("b-s", "sanitize")),
    );
    setup.lua("c", &tagging("c", "transform"));
    let (run, server) = setup.read_note(
        "x",
        &json!({"hooks": {"order": {"after_tool": ["fiber.test/c"]}}}),
    );
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "x|b-s|c|a|b-t");
    assert_eq!(
        completed["changed_by"],
        json!(["fiber.test/b", "fiber.test/c", "fiber.test/a"])
    );
    assert_eq!(output_sent(&server), "x|b-s|c|a|b-t");
}

#[test]
fn an_after_tool_hook_running_host_exec_logs_one_extension_exec() {
    let setup = Setup::new();
    setup.lua(
        "exec",
        "fiber.hook(\"after_tool\", { timeout = 10000, on_failure = \"non-blocking\",\n\
           run = function(call) host.exec(\"sh\", {\"-c\", \"true\"}) end })\n",
    );
    let (run, _server) = setup.read_note("nothing secret\n", &json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The hook sends the run while the loop is blocked in the hook, so the
    // loop writes it at the next step's drain, after that step's
    // `step_started`: the complete ordered list pins a misplaced event.
    let mut expected = read_kinds(&[], &[]);
    let at = expected
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap()
        + 2;
    assert_eq!(expected[at - 1], "step_started");
    assert_eq!(expected[at], "assistant_message_started");
    expected.insert(at, "extension_exec");
    assert_eq!(run.kinds(), expected);
    let execs = run.all("extension_exec");
    assert_eq!(execs.len(), 1, "one run outside a tool call logs one line");
    let payload = &execs[0]["payload"];
    assert_eq!(payload["extension"], "fiber.test/exec");
    assert_eq!(payload["program"], "sh");
    assert_eq!(payload["args"], json!(["-c", "true"]));
    assert_eq!(
        payload["cwd"],
        serde_json::Value::String(
            std::fs::canonicalize(setup.workspace())
                .unwrap()
                .display()
                .to_string()
        ),
        "the run happens in the workspace"
    );
    assert_eq!(payload["process"]["exit_code"], 0);
    assert_eq!(payload["process"]["timed_out"], false);
    assert!(payload["process"].get("signal").is_none());
}

#[test]
fn an_after_tool_hook_calling_host_log_writes_one_live_line_and_its_diagnostic_line() {
    let setup = Setup::new();
    setup.lua(
        "log",
        "fiber.hook(\"after_tool\", { timeout = 10000, on_failure = \"non-blocking\",\n\
           run = function(call) host.log(\"seen\") end })\n",
    );
    let (run, _server) = setup.read_note("nothing secret\n", &json!({}));
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let logs = run.all("extension_log");
    assert_eq!(logs.len(), 1, "one host.log call logs one line");
    assert_eq!(logs[0]["payload"]["extension"], "fiber.test/log");
    assert_eq!(logs[0]["payload"]["message"], "seen");
    let id = run.lines[0]["session_id"].as_str().unwrap();
    let text = fs::read_to_string(setup.home().join("logs").join(format!("ask-{id}.log"))).unwrap();
    assert_eq!(text.lines().count(), 1, "one diagnostic line");
    let line: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(line["level"], "info");
    assert_eq!(line["process"], "ask");
    assert_eq!(line["session_id"], id);
    assert_eq!(line["code"], "extension_log");
    assert_eq!(line["message"], "fiber.test/log: seen");
    let order = [
        "\"ts\":",
        "\"level\":",
        "\"process\":",
        "\"session_id\":",
        "\"code\":",
        "\"message\":",
    ];
    let mut at = 0;
    for key in order {
        let next = text.find(key).unwrap();
        assert!(next >= at, "{key} follows the key before it");
        at = next;
    }
}

/// A `before_tool` hook of `phase` with `body` as its run's body.
fn before_hook(phase: &str, failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_tool\", {{ phase = \"{phase}\", timeout = 30000, on_failure = \"{failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

fn rewrite_guard(path: &str) -> String {
    format!(
        "fiber.hook(\"before_tool\", {{ timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return {{ arguments = {{ path = \"{path}\" }} }} end }})\n"
    )
}

#[test]
fn a_before_tool_rewrite_is_checked_judged_and_run_and_the_model_sees_its_own_call() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "A\n").unwrap();
    fs::write(setup.workspace().join("b.txt"), "B\n").unwrap();
    setup.lua("guard", &rewrite_guard("b.txt"));
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "a.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds(&[], &[]));

    let started = run.first("tool_call_started");
    assert_eq!(started["payload"]["arguments"], json!({"path": "b.txt"}));
    assert_eq!(
        started["payload"]["changed_by"],
        json!(["fiber.test/guard"])
    );
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"path\":\"b.txt\"}.";
    let completed = run.completed();
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": format!("{line}\nB\n")}])
    );
    assert_eq!(output_sent(&server), format!("{line}\nB\n"));

    // The model's call is sent back as it wrote it.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let call = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(call["arguments"], json!({"path": "a.txt"}).to_string());
}

#[test]
fn a_rewrite_into_credentials_is_refused_by_the_credential_deny() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "A\n").unwrap();
    let secret = setup.home().join("credentials").join("secret.txt");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, "the secret is hunter2\n").unwrap();
    setup.lua("guard", &rewrite_guard(&secret.display().to_string()));
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "a.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    let mut expected = read_kinds(&[], &[]);
    let at = expected
        .iter()
        .position(|kind| *kind == "tool_call_started")
        .unwrap();
    expected[at] = "permission_resolved";
    assert_eq!(run.kinds(), expected);

    let resolved = run.first("permission_resolved");
    assert_eq!(resolved["payload"]["decided_by"], "credential_deny");
    let completed = run.completed();
    assert_eq!(completed["status"], "denied");
    assert_eq!(completed["reason"], "credentials");
    for line in &run.lines {
        assert!(
            !line.to_string().contains("hunter2"),
            "the secret reaches no stdout line"
        );
    }
    let dir = run.session_dir(&setup);
    assert!(!on_disk(&dir).contains("hunter2"));
}

#[test]
fn a_rewrite_that_fails_the_schema_fails_invalid_arguments_and_never_starts() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "A\n").unwrap();
    setup.lua(
        "guard",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { arguments = { path = 5 } } end })\n",
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "a.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), unstarted_kinds());
    let completed = run.completed();
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "invalid_arguments");
    assert!(
        completed["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(
                "Extension fiber.test/guard rewrote this call's arguments, and they do not match the tool's schema:"
            ),
        "{}",
        completed["content"][0]["text"]
    );
}

#[test]
fn a_before_tool_refusal_is_denied_hook_and_the_tool_never_runs() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { refuse = \"no writes here\" } end })\n",
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_write",
            "write",
            &json!({"path": "out.txt", "content": "hi\n"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "write it"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), unstarted_kinds());
    let completed = run.completed();
    assert_eq!(completed["status"], "denied");
    assert_eq!(completed["reason"], "hook");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "Extension fiber.test/guard refused this call: no writes here. It did not run."}])
    );
    assert!(!setup.workspace().join("out.txt").exists());
    assert_eq!(
        output_sent(&server),
        "Extension fiber.test/guard refused this call: no writes here. It did not run."
    );
}

#[test]
fn a_check_hook_that_returns_a_change_has_failed() {
    let check = "return { arguments = call.arguments }";
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "original\n").unwrap();
    setup.lua("guard", &before_hook("check", "blocking", check));
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
    assert_eq!(run.kinds(), unstarted_kinds());
    let completed = run.completed();
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "hook_failed");

    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "original\n").unwrap();
    setup.lua("guard", &before_hook("check", "non-blocking", check));
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
    let mut expected = read_kinds(&[], &[]);
    let at = expected
        .iter()
        .position(|kind| *kind == "tool_call_started")
        .unwrap();
    expected.insert(at, "notice");
    assert_eq!(run.kinds(), expected);
    let notices = run.all("notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["payload"]["code"], "hook_failed");
    assert_eq!(notices[0]["payload"]["extension"], "fiber.test/guard");
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "original\n");
}

#[test]
fn a_blocking_before_tool_failure_fails_the_call_hook_failed_and_never_starts() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "original\n").unwrap();
    setup.lua(
        "guard",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
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
    assert_eq!(run.kinds(), unstarted_kinds());
    let completed = run.completed();
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "hook_failed");
    let text = completed["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "The `before_tool` hook of extension fiber.test/guard failed, so the call did not run: "
        ) && text.contains("boom"),
        "{text}"
    );
}

#[test]
fn a_rewrite_survives_a_resume_while_its_approval_is_pending() {
    use extension_harness::{
        kinds as session_kinds, send as session_send, until_session as session_until,
    };

    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_1",
            "shell",
            &json!({"command": "echo hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "session": {"idle_exit_ms": 0}}).to_string(),
    )
    .unwrap();
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": "echo"})
        ),
    )
    .unwrap();
    setup.lua(
        "guard",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call)\n\
             if call.tool == \"shell\" then return { arguments = { command = \"echo bye\" } } end\n\
           end })\n",
    );
    let id = doors::mint("s_");
    let first = setup.start_session(&id, &["--prompt", "run it"]);
    let (status, out, stderr) = first.wait();
    assert!(status.success(), "stderr: {stderr}");
    let requested = out
        .iter()
        .find(|line| line["kind"] == "permission_requested")
        .expect("the approval was raised");
    let request_id = requested["payload"]["request_id"].as_str().unwrap();
    let exited = out.last().expect("fiber_exited is the last stdout line");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["suspended_on"], request_id);
    // The first process's complete stream: the approval hangs the call,
    // so no `tool_call_started` follows the request.
    assert_eq!(
        session_kinds(&out),
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
            "permission_requested",
            "fiber_exited",
        ]
    );

    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "session": {"idle_exit_ms": 3600000}}).to_string(),
    )
    .unwrap();
    let mut running = setup.start_session(&id, &["--resume"]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    session_send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let asked = session_until(&client, "the re-raised request", |line| {
        line["kind"] == "permission_requested"
    });
    let raised = asked.last().expect("the request was raised again");
    assert_eq!(raised["payload"]["request_id"], request_id);
    assert_eq!(raised["payload"]["step"], "standing_ask");
    session_send(
        &client,
        &format!(
            r#"{{"id":"c_reply","command":"reply","args":{{"request_id":"{request_id}","decision":"allow"}}}}"#
        ),
    );
    let done = session_until(&client, "the turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    let started = done
        .iter()
        .find(|line| line["kind"] == "tool_call_started")
        .expect("the call started");
    assert_eq!(
        started["payload"]["arguments"],
        json!({"command": "echo bye"})
    );
    assert_eq!(
        started["payload"]["changed_by"],
        json!(["fiber.test/guard"])
    );
    let completed = done
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .expect("the call completed");
    let line = "Extension fiber.test/guard rewrote this call's arguments; it ran with {\"command\":\"echo bye\"}.";
    assert!(
        completed["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{line}\nbye")),
        "{}",
        completed["payload"]["content"][0]["text"]
    );
    session_send(&client, r#"{"id":"c_close","command":"close"}"#);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let at = out
        .iter()
        .position(|line| line["kind"] == "fiber_started")
        .expect("the resumed process writes fiber_started");
    let resumed: Vec<Value> = out[at..]
        .iter()
        .filter(|line| line["kind"] != "clients")
        .cloned()
        .collect();
    assert_eq!(
        session_kinds(&resumed),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
}

/// A `before_message` hook that redacts the secret and adds context.
fn sanitize() -> String {
    "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"blocking\",\n\
       run = function(call)\n\
         local text = call.content[1].text:gsub(\"sk%-123\", \"[redacted]\")\n\
         return { content = { { type = \"text\", text = text } }, context = \"ctx\" }\n\
       end })\n"
        .into()
}

/// The complete event kinds of a `fiber ask` whose model answers with text:
/// as `mcp.rs`'s `hello_kinds`, with the hook's `extra` after `turn_started`.
fn ask_text_kinds(extra: &[&'static str]) -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    kinds.extend(extra.iter().copied());
    kinds.extend(
        [
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
        .iter()
        .copied(),
    );
    kinds
}

#[test]
fn a_before_message_hook_replaces_the_prompt_and_only_the_replacement_is_logged_and_sent() {
    let setup = Setup::new();
    setup.lua("sanitize", &sanitize());
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "my key is sk-123"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ask_text_kinds(&["context_added"]));

    let started = run.first("turn_started");
    assert_eq!(
        started["payload"]["input"][0]["content"][0]["text"],
        "my key is [redacted]"
    );
    assert_eq!(
        started["payload"]["input"][0]["changed_by"],
        json!(["fiber.test/sanitize"])
    );
    let added = run.first("context_added");
    assert_eq!(added["payload"]["text"], "ctx");
    assert_eq!(added["payload"]["extension"], "fiber.test/sanitize");
    assert_eq!(added["payload"]["hook"], "before_message");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("[redacted]"), "{body}");
    assert!(body.contains("ctx"), "{body}");
    assert!(!body.contains("sk-123"), "the secret is never sent: {body}");

    assert!(!run.stderr.contains("sk-123"), "stderr: {}", run.stderr);
    let stdout: String = run.lines.iter().map(|line| line.to_string()).collect();
    assert!(!stdout.contains("sk-123"), "stdout: {stdout}");
    let dir = run.session_dir(&setup);
    assert!(
        !on_disk(&dir).contains("sk-123"),
        "no file holds the secret"
    );
    assert!(on_disk(&dir).contains("[redacted]"));
}

#[test]
fn a_refused_first_prompt_exits_1_message_refused_with_the_reason() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { refuse = \"no keys\" } end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "my key is sk-123"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
    let exited = run.lines.last().expect("fiber_exited is last");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(
        exited["payload"]["error"],
        json!({
            "code": "message_refused",
            "message": "Extension fiber.test/guard refused the message: no keys",
        })
    );
    assert!(
        run.stderr
            .contains("fiber: Extension fiber.test/guard refused the message: no keys"),
        "stderr names the refusal: {}",
        run.stderr
    );
    assert!(server.requests().is_empty(), "the model is never called");
}

#[test]
fn a_blocking_before_message_failure_exits_1_hook_failed() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
    let exited = run.lines.last().expect("fiber_exited is last");
    assert_eq!(exited["payload"]["error"]["code"], "hook_failed");
    assert!(
        run.stderr.starts_with(
            "fiber: The `before_message` hook of extension fiber.test/guard failed, \
             so the message was not sent: "
        ) && run.stderr.contains("boom"),
        "stderr names the failure: {}",
        run.stderr
    );
    assert!(server.requests().is_empty(), "the model is never called");
}

#[test]
fn a_non_blocking_before_message_failure_gives_a_notice_and_runs_the_original() {
    let setup = Setup::new();
    setup.lua(
        "soft",
        "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"non-blocking\",\n\
           run = function(call) error(\"soft failure\") end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "original"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let mut kinds = ask_text_kinds(&[]);
    kinds.insert(3, "notice");
    assert_eq!(run.kinds(), kinds);
    let started = run.first("turn_started");
    assert_eq!(
        started["payload"]["input"][0]["content"][0]["text"],
        "original"
    );
    assert!(started["payload"]["input"][0].get("changed_by").is_none());
}

#[test]
fn a_check_before_message_hook_that_replaces_has_failed() {
    let setup = Setup::new();
    setup.lua(
        "checker",
        "fiber.hook(\"before_message\", { phase = \"check\", timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { content = call.content } end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]
    );
    let exited = run.lines.last().expect("fiber_exited is last");
    assert_eq!(exited["payload"]["error"]["code"], "hook_failed");
    assert!(server.requests().is_empty(), "the model is never called");
}

#[test]
fn a_refused_steer_is_accepted_then_noticed_while_a_refused_prompt_is_rejected() {
    use extension_harness::{send as session_send, until_session as session_until};
    use support::{Diag, session_dir};

    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua(
        "guard",
        "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call)\n\
             if call.content[1].text == \"second\" then return end\n\
             return { refuse = \"no keys\" }\n\
           end })\n",
    );
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let mut client = running.connect(&setup.socket(&id));
    client.set_diag(Diag::new(
        running.stderr_snapshot.clone(),
        session_dir(&setup, &id),
    ));
    running.wait_for("extensions_loaded");
    session_send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    session_send(
        &client,
        r#"{"id":"c_s1","command":"steer","args":{"content":[{"type":"text","text":"aside"}]}}"#,
    );
    let accepted = session_until(&client, "the steer's at-once acceptance", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_s1"
    });
    assert!(
        !accepted.is_empty(),
        "the refused steer is still answered at once"
    );
    // The refusal lands after the answer, as a notice naming the code,
    // the extension and the reason: the loop never rejects a steer it
    // already accepted.
    let refused = session_until(&client, "the refused steer's notice", |line| {
        line["kind"] == "notice" && line["payload"]["code"] == "message_refused"
    });
    assert_eq!(
        refused.last().unwrap()["payload"]["message"],
        "Extension fiber.test/guard refused the message: no keys"
    );
    session_send(
        &client,
        r#"{"id":"c_p1","command":"prompt","args":{"content":[{"type":"text","text":"first"}]}}"#,
    );
    let refused = session_until(&client, "the refused prompt", |line| {
        line["kind"] == "command_rejected" && line["payload"]["command_id"] == "c_p1"
    });
    assert_eq!(
        refused.last().unwrap()["payload"]["code"],
        "message_refused"
    );
    assert_eq!(
        refused.last().unwrap()["payload"]["message"],
        "Extension fiber.test/guard refused the message: no keys"
    );
    session_send(
        &client,
        r#"{"id":"c_p2","command":"prompt","args":{"content":[{"type":"text","text":"second"}]}}"#,
    );
    let turned = session_until(&client, "the accepted prompt's turn", |line| {
        line["kind"] == "turn_started"
    });
    let input = turned.last().unwrap()["payload"]["input"].clone();
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input[0]["content"][0]["text"], "second");
    session_send(&client, r#"{"id":"c_close","command":"close"}"#);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn session_socket_timeout_dumps_received_lines_stderr_and_log_tail() {
    use extension_harness::{send as session_send, until_session as session_until};
    use support::{Deadline, Diag, assert_timeout_dump, session_dir};

    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let mut client = running.connect(&setup.socket(&id));
    client.set_diag(Diag::new(
        running.stderr_snapshot.clone(),
        session_dir(&setup, &id),
    ));
    running.wait_for("extensions_loaded");
    session_send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    session_send(
        &client,
        r#"{"id":"c_p1","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    // The prompt's acceptance arrives on the test's deadline; the held
    // model never answers, so the impossible kind never arrives: the wait
    // then expires on a zero bound instead of spending what remains of
    // the test's deadline (`docs/testing.md`, "Waits and timeouts").
    let mut got = session_until(&client, "the prompt's acceptance", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_p1"
    });
    client.set_deadline(Deadline::after(Duration::ZERO));
    let expired = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for _ in 0..1000 {
            match client.next("the turn that never completes", &got) {
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
    drop(client);
    let group = running.group;
    let cleanup = running.deadline.cleanup_phase();
    fakes::kill_group(group, "KILL").unwrap();
    let mut child = running.child;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || match done.send(child.wait()) {
        Ok(()) | Err(mpsc::SendError(_)) => {}
    });
    match cleanup.recv(&finished) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited until the cleanup deadline to reap the session"),
    };
    assert!(
        fakes::group_empties(group, cleanup),
        "the session left a process in its group behind"
    );
    running.watchdog.stand_down(cleanup);
    std::mem::forget(running.guard);
}

#[test]
fn a_redacted_socket_prompt_joins_the_history_redacted() {
    use extension_harness::{send as session_send, until_session as session_until};
    use support::session_dir;

    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua("redact", &sanitize());
    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &[]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    session_send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    session_send(
        &client,
        r#"{"id":"c_1","command":"prompt","args":{"content":[{"type":"text","text":"my key is sk-123"}]}}"#,
    );
    session_until(&client, "the prompt's acceptance", |line| {
        line["kind"] == "command_accepted" && line["payload"]["command_id"] == "c_1"
    });
    session_send(&client, r#"{"id":"c_close","command":"close"}"#);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");

    let history = session_dir(&setup, &id).join("../../history.jsonl");
    let text = fs::read_to_string(&history).unwrap();
    assert_eq!(text.lines().count(), 1);
    let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(
        line["content"],
        json!([{"type": "text", "text": "my key is [redacted]"}])
    );
    assert!(!text.contains("sk-123"), "the history holds no secret");
}

#[test]
fn an_over_mebibyte_shell_call_hands_the_hook_its_path_and_size() {
    let setup = Setup::new();
    setup.lua(
        "spill",
        "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { content = \"path=\" .. tostring(call.path) .. \" size=\" .. tostring(call.size), artifact = \"replaced\" } end })\n",
    );
    // The script prints 1,048,577 bytes.
    let script = setup.workspace().join("flood.sh");
    fs::write(&script, "head -c 1048577 /dev/zero | tr '\\0' a\n").unwrap();
    let command = format!("sh {}", script.display());
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_flood",
            "shell",
            &json!({"command": command}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({}));
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": command})
        ),
    )
    .unwrap();
    let run = setup.run(&["ask", "flood the output"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // `tool_call_delta` is ephemeral, and how many the flood fills depends
    // on when it writes, so the comparison leaves them out; every other
    // kind is pinned in order.
    let kinds: Vec<&str> = run
        .kinds()
        .into_iter()
        .filter(|kind| *kind != "tool_call_delta")
        .collect();
    assert_eq!(
        kinds,
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
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let completed = run.completed();
    let dir = run.session_dir(&setup);
    let artifact = completed["artifact"].as_str().unwrap();
    assert!(artifact.starts_with("artifacts/s_"), "{artifact}");
    assert!(artifact.ends_with(".txt"), "{artifact}");
    let path = dir.join(artifact);
    // The call's 1,048,577 bytes plus the 14 closing bytes.
    let expected = format!("path={} size=1048591", path.display());
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": expected}])
    );
    assert_eq!(output_sent(&server), expected);
    assert_eq!(fs::read_to_string(&path).unwrap(), "replaced");
    // No file under the session holds the raw output.
    let mut pending = vec![dir.clone()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                pending.push(entry);
            } else {
                assert!(
                    fs::metadata(&entry).unwrap().len() < 1_048_577,
                    "{} holds the raw output",
                    entry.display()
                );
            }
        }
    }
}

#[test]
fn a_turn_start_hook_adds_context_the_first_request_carries() {
    let setup = Setup::new();
    setup.lua(
        "inbox",
        "fiber.hook(\"turn_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { context = \"inbox: 2\" } end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ask_text_kinds(&["context_added"]));

    let added = run.first("context_added");
    assert_eq!(added["payload"]["text"], "inbox: 2");
    assert_eq!(added["payload"]["extension"], "fiber.test/inbox");
    assert_eq!(added["payload"]["hook"], "turn_start");

    // The first request carries the context as its own user message after
    // the prompt.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(body.contains("inbox: 2"), "{body}");
    assert!(body.contains("hello"), "{body}");
    assert!(
        body.find("hello").unwrap() < body.find("inbox: 2").unwrap(),
        "{body}"
    );
}

#[test]
fn a_before_model_call_refusal_fails_the_turn_budget_exceeded_naming_the_extension() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"before_model_call\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { refuse = \"team budget spent\" } end })\n",
    );
    // An empty script: a request that wrongly reaches the model fails fast
    // with an assertion, not a hang.
    let server = ProviderServer::start([]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "failed");
    assert_eq!(end["payload"]["error"]["code"], "budget_exceeded");
    assert_eq!(
        end["payload"]["error"]["message"],
        "Extension fiber.test/guard refused the model request: team budget spent"
    );
    assert!(server.requests().is_empty(), "the model got no request");
}

#[test]
fn a_turn_end_hook_continues_the_turn_once() {
    let setup = Setup::new();
    setup.lua(
        "goal",
        "local sent = false\n\
         fiber.hook(\"turn_end\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call)\n\
             if sent then return end\n\
             sent = true\n\
             return { message = \"keep going\" }\n\
           end })\n",
    );
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second = String::from_utf8_lossy(&requests[1].body);
    assert!(second.contains("keep going"), "{second}");

    let steering = run.all("steering_applied");
    assert_eq!(steering.len(), 1);
    assert_eq!(
        steering[0]["payload"]["content"],
        json!([{"type": "text", "text": "keep going"}])
    );
    assert_eq!(steering[0]["payload"]["source"], "extension");
    assert_eq!(steering[0]["payload"]["extension"], "fiber.test/goal");
    assert!(steering[0]["payload"].get("command_id").is_none());

    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "completed");
}

#[test]
fn a_blocking_turn_start_failure_fails_the_turn_hook_failed() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"turn_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
    );
    let server = ProviderServer::start([]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "failed");
    assert_eq!(end["payload"]["error"]["code"], "hook_failed");
    assert!(server.requests().is_empty(), "the model got no request");
}

#[test]
fn a_blocking_before_model_call_failure_fails_the_turn_hook_failed() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"before_model_call\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
    );
    let server = ProviderServer::start([]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "failed");
    assert_eq!(end["payload"]["error"]["code"], "hook_failed");
    assert!(server.requests().is_empty(), "the model got no request");
}

#[test]
fn a_blocking_turn_end_failure_fails_the_turn_hook_failed() {
    let setup = Setup::new();
    setup.lua(
        "goal",
        "fiber.hook(\"turn_end\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "hello"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), ask_text_kinds(&[]));
    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "failed");
    assert_eq!(end["payload"]["error"]["code"], "hook_failed");
    assert_eq!(server.requests().len(), 1);
}

/// The user texts of an `openai-responses` request body, in order.
fn user_messages(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .map(|item| item["content"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_session_start_hook_s_context_follows_the_opening_message_in_the_first_request() {
    let setup = Setup::new();
    setup.lua(
        "starter",
        "fiber.hook(\"session_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { context = \"remember this\" } end })\n",
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "do it"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "context_added",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let added = run.first("context_added");
    assert_eq!(added["payload"]["extension"], "fiber.test/starter");
    assert_eq!(added["payload"]["hook"], "session_start");
    assert_eq!(added["payload"]["text"], "remember this");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let users = user_messages(&requests[0].body);
    assert_eq!(users.len(), 3);
    assert!(
        users[0].starts_with("This message is from Fiber"),
        "the first user message is the opening message"
    );
    assert_eq!(users[1], "remember this");
    assert_eq!(users[2], "do it");
}

#[test]
fn a_blocking_session_start_failure_exits_1_hook_failed() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"session_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) error(\"boom\") end })\n",
    );
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let run = setup.run(&["ask", "do it"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "fiber_exited",
        ]
    );
    let exited = run.first("fiber_exited");
    assert_eq!(exited["payload"]["error"]["code"], "hook_failed");
    let message = exited["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "The `session_start` hook of extension fiber.test/guard failed, \
             so the session did not start: "
        ) && message.contains("boom"),
        "the failure wording: {message}"
    );
    assert!(
        run.stderr.starts_with(
            "fiber: The `session_start` hook of extension fiber.test/guard failed, \
             so the session did not start: "
        ) && run.stderr.contains("boom"),
        "stderr names the failure: {}",
        run.stderr
    );
    assert!(server.requests().is_empty(), "the server got no request");
}

#[test]
fn session_start_runs_again_on_resume_and_appends_its_context() {
    let setup = Setup::new();
    setup.lua(
        "echoer",
        "fiber.hook(\"session_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { context = \"reason:\" .. call.reason } end })\n",
    );
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider_with_extra(&server, &json!({}));
    let first = setup.run(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    let second = setup.run(&["ask", "--resume", &id[..8], "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.session_id(), id);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    // The first request holds the start context once.
    let started = user_messages(&requests[0].body);
    assert_eq!(started.len(), 3);
    assert!(started[0].starts_with("This message is from Fiber"));
    assert_eq!(&started[1..], ["reason:start", "one"]);
    // The second holds it twice, the resume's after the first turn.
    let resumed = user_messages(&requests[1].body);
    assert_eq!(resumed.len(), 5);
    assert!(resumed[0].starts_with("This message is from Fiber"));
    assert_eq!(
        &resumed[1..],
        ["reason:start", "one", "reason:resume", "two"]
    );
    let body = String::from_utf8_lossy(&requests[1].body);
    assert!(
        body.find("Hello.").is_some_and(|replied| body
            .rfind("reason:resume")
            .is_some_and(|added| replied < added)),
        "the resume context follows the first turn's reply: {body}"
    );
}

#[test]
fn a_handoff_runs_session_start_with_reason_handoff() {
    let setup = Setup::new();
    setup.lua(
        "echoer",
        "fiber.hook(\"session_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { context = \"reason:\" .. call.reason } end })\n",
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        support::text_reply("The note."),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({"handoff": {"tokens": 10, "nudge": false}}));
    fs::write(
        setup.workspace().join("note.txt"),
        "the password is hunter2\n",
    )
    .unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "context_added",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "context_added",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let done = run.first("handoff_completed");
    assert_eq!(done["payload"]["outcome"], "completed");
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    // The request after the handoff is the new opening message, the
    // handoff context, the turn's input, the note.
    let users = user_messages(&requests[2].body);
    assert_eq!(users.len(), 4);
    assert!(users[0].starts_with("This message is from Fiber"));
    assert_eq!(
        &users[1..],
        ["reason:handoff", "read the note", "The note."]
    );
}

#[test]
fn a_blocking_session_start_failure_at_a_handoff_fails_the_handoff_and_the_session_goes_on() {
    let setup = Setup::new();
    setup.lua(
        "guard",
        "fiber.hook(\"session_start\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call)\n\
             if call.reason == \"handoff\" then error(\"boom\") end\n\
             return {} end })\n",
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        support::text_reply("The note."),
        hello(),
    ])
    .unwrap();
    setup.provider_with_extra(&server, &json!({"handoff": {"tokens": 10, "nudge": false}}));
    fs::write(
        setup.workspace().join("note.txt"),
        "the password is hunter2\n",
    )
    .unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
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
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let done = run.first("handoff_completed");
    assert_eq!(done["payload"]["outcome"], "failed");
    assert_eq!(done["payload"]["error"]["code"], "hook_failed");
    let message = done["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(
            "The `session_start` hook of extension fiber.test/guard failed, \
             so the handoff did not complete: "
        ) && message.contains("boom"),
        "the failure wording: {message}"
    );
    let end = run.first("turn_completed");
    assert_eq!(end["payload"]["outcome"], "completed");
}

/// Installs `fiber.test/<short>` with `init` as its entry script.
fn install_hook_extension(setup: &Setup, short: &str, init: &str) {
    let source = setup.root.path().join(format!("src-{short}"));
    write(
        &source.join("extension.json"),
        &json!({"name": format!("fiber.test/{short}"), "version": "0.1.0", "fiber": "0.1.0", "api": 1})
            .to_string(),
    );
    write(&source.join("init.lua"), init);
    setup.install(source);
}

/// The diagnostic lines of one file.
fn diagnostic_file(setup: &Setup, name: &str) -> Vec<Value> {
    let text = fs::read_to_string(setup.home().join("logs").join(name)).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A `before_handoff` hook whose run is `body`.
fn noting_hook(on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_handoff\", {{ timeout = 60000, on_failure = \"{on_failure}\",\n\
           run = function(h) {body} end }})\n"
    )
}

/// Runs `fiber ask` where the model reads a file, then the five-token
/// trigger hands off, with `init` installed and `extra` configured.
fn handoff_run(
    setup: &Setup,
    init: &str,
    script: Vec<Response>,
    extra: &Value,
) -> (Run, ProviderServer) {
    install_hook_extension(setup, "notes", init);
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start(script).unwrap();
    setup.provider_with_extra(&server, extra);
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    (run, server)
}

/// The complete event kinds of a handoff turn whose note a hook wrote:
/// `tools.rs`'s `HANDOFF_TURN` without the note reply's six lines. Each
/// `hello` below streams its answer in one delta.
const HOOK_HANDOFF_TURN: &[&str] = &[
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "handoff_started",
    "handoff_completed",
    "opening_message",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

fn handoff_prefix() -> Vec<&'static str> {
    vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
    ]
}

#[test]
fn a_before_handoff_hook_writes_the_note_and_no_note_request_is_made() {
    let setup = Setup::new();
    let (run, server) = handoff_run(
        &setup,
        &noting_hook(
            "non-blocking",
            r#"return { note = "Hook note: " .. #h.messages .. " messages" }"#,
        ),
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({"handoff": {"tokens": 5}, "diagnostics": {"level": "debug"}}),
    );
    assert_eq!(
        run.kinds(),
        [handoff_prefix(), HOOK_HANDOFF_TURN.into()].concat()
    );
    let completed = run.first("handoff_completed");
    let note_text = completed["payload"]["note_text"].as_str().unwrap();
    assert!(note_text.starts_with("Hook note: "), "{note_text}");
    assert_eq!(completed["payload"]["extension"], "fiber.test/notes");
    assert!(completed["payload"].get("note").is_none());
    // Two requests, not three: the note request never ran, and the second
    // holds the hook's note.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let body = second.to_string();
    assert!(body.contains(note_text), "{body}");
    // The debug log holds the hook's chain scope and no `turn.request`
    // line for the handoff: the call's and the answer's lines only.
    let id = run.session_id().to_owned();
    let lines = diagnostic_file(&setup, &format!("ask-{id}.log"));
    let timings: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "op_timing")
        .collect();
    assert_eq!(
        timings
            .iter()
            .filter(|line| line["data"]["op"] == "hook.before_handoff")
            .count(),
        1
    );
    assert_eq!(
        timings
            .iter()
            .filter(|line| line["data"]["op"] == "turn.request")
            .count(),
        2,
        "{timings:?}"
    );
}

#[test]
fn a_before_handoff_hook_that_returns_nothing_leaves_the_note_to_the_model() {
    let setup = Setup::new();
    let (run, server) = handoff_run(
        &setup,
        &noting_hook("non-blocking", r#"return nil"#),
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
            hello(),
        ],
        &json!({"handoff": {"tokens": 5}}),
    );
    assert_eq!(
        run.kinds(),
        [
            handoff_prefix(),
            vec![
                "turn_started",
                "step_started",
                "assistant_message_started",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_started",
                "tool_call_completed",
                "step_started",
                "handoff_started",
                "assistant_message_started",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
                "opening_message",
                "assistant_message_started",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
                "fiber_exited",
            ],
        ]
        .concat()
    );
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn a_non_blocking_before_handoff_hook_with_a_bad_return_leaves_the_note_to_the_model() {
    let setup = Setup::new();
    let (run, server) = handoff_run(
        &setup,
        &noting_hook("non-blocking", r#"return "x""#),
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
            hello(),
        ],
        &json!({"handoff": {"tokens": 5}}),
    );
    // The model's turn, with the hook's failure as a notice after
    // `handoff_started`.
    let mut want = handoff_prefix();
    want.extend([
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "handoff_started",
        "notice",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "handoff_completed",
        "opening_message",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(run.kinds(), want);
    let notice = run.first("notice");
    assert_eq!(notice["payload"]["code"], "hook_failed");
    assert_eq!(notice["payload"]["extension"], "fiber.test/notes");
    let completed = run.first("handoff_completed");
    assert_eq!(completed["payload"]["outcome"], "completed");
    assert!(completed["payload"].get("note").is_some());
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn a_blocking_before_handoff_failure_fails_the_handoff_hook_failed() {
    let setup = Setup::new();
    let (run, server) = handoff_run(
        &setup,
        &noting_hook("blocking", r#"error("boom")"#),
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({"handoff": {"tokens": 5}}),
    );
    let completed = run.first("handoff_completed");
    assert_eq!(completed["payload"]["outcome"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "hook_failed");
    let message = completed["payload"]["error"]["message"].as_str().unwrap();
    assert!(message.contains("fiber.test/notes"), "{message}");
    assert!(message.contains("boom"), "{message}");
    // No note request: the turn completes from the old context.
    assert_eq!(server.requests().len(), 2);
    assert_eq!(run.kinds().last(), Some(&"fiber_exited"));
}

#[test]
fn a_hook_note_survives_resume() {
    let setup = Setup::new();
    let (run, server) = handoff_run(
        &setup,
        &noting_hook(
            "non-blocking",
            r#"return { note = "Hook note: " .. #h.messages .. " messages" }"#,
        ),
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
            hello(),
        ],
        &json!({"handoff": {"tokens": 5}}),
    );
    let id = run.session_id().to_owned();
    let note_text = run.first("handoff_completed")["payload"]["note_text"]
        .as_str()
        .unwrap()
        .to_owned();
    let resumed = setup.run(&["ask", "--resume", &id, "again"]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    // The resumed request carries the whole rebuilt conversation: the
    // hook's note among it, read through `Note::Hook`.
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let third: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let body = third.to_string();
    assert!(body.contains(&note_text), "{body}");
    assert!(body.contains("again"), "{body}");
    assert!(
        body.contains("This message is from Fiber"),
        "the rebuilt conversation starts from the opening message: {body}"
    );
}
