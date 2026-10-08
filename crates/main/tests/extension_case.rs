//! Binary-level tests of the hidden extension case child (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "binary test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::Setup;

const SESSION_KINDS: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

fn expected(kinds: &[&str]) -> Vec<Value> {
    kinds.iter().map(|kind| json!({"kind": kind})).collect()
}

fn session(script: Value, prompt: &str, expect: Vec<Value>) -> Value {
    json!({"script": script, "prompt": prompt, "expect": expect})
}

fn setup(lua: &str) -> Setup {
    setup_named("casefixture", lua)
}

fn setup_named(name: &str, lua: &str) -> Setup {
    let setup = Setup::new();
    let source = setup.root.path().join("case-extension");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("extension.json"),
        json!({"name": name, "version": "0.1.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(source.join("init.lua"), lua).unwrap();
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
    setup
}

fn case_file(setup: &Setup, name: &str, value: &Value) -> String {
    let file = setup.workspace().join("tests").join(format!("{name}.json"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, value.to_string()).unwrap();
    format!("tests/{name}.json")
}

fn run_case(setup: &Setup, name: &str, value: &Value) -> Output {
    let path = case_file(setup, name, value);
    let mut command = setup.fiber(&["extension-case", &path]);
    command.current_dir(setup.workspace());
    support::run_to_exit(setup.deadline, "the extension case child", command)
}

fn assert_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("ok "));
}

fn assert_malformed(output: &Output, reason: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL "), "{stdout}");
    assert!(stdout.contains(reason), "{stdout}");
}
fn assert_failure(output: &Output, reason: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL "), "{stdout}");
    assert!(stdout.contains(reason), "{stdout}");
}

fn scripted(text: &str) -> Value {
    json!({"steps": [{"text": text}]})
}

fn tool_script() -> Value {
    json!({"steps": [
        {"tool_calls": [{"name": "read", "arguments": {"path": "fixture.txt"}}]},
        {"text": "Hello."}
    ]})
}

fn write_tool_fixture(setup: &Setup) {
    fs::write(setup.workspace().join("fixture.txt"), "fixture").unwrap();
}

fn run_ask(setup: &Setup, args: &[&str]) -> Output {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace()).stdin(Stdio::null());
    support::run_to_exit(setup.deadline, "fiber ask", command)
}

fn write_script(workspace: &Path, script: &Value) {
    fs::write(workspace.join("script.json"), script.to_string()).unwrap();
}

#[test]
fn a_text_case_asserts_the_complete_durable_stream() {
    let setup = setup("");
    let good = session(scripted("Hello."), "hi", expected(SESSION_KINDS));
    assert_success(&run_case(&setup, "complete", &good));

    let mut missing = SESSION_KINDS.to_vec();
    missing.remove(8);
    let bad = session(scripted("Hello."), "hi", expected(&missing));
    assert_failure(&run_case(&setup, "missing-kind", &bad), "expect[");
}

#[test]
fn scripted_host_http_is_used_and_an_unscripted_call_is_a_miss() {
    let server =
        ProviderServer::start([Response::status(200, r#"{"live":true}"#.as_bytes())]).unwrap();
    let lua = format!(
        r#"fiber.hook("after_tool", {{ timeout = 5000, on_failure = "blocking", run = function()
  local reply = host.http({{url = {:?}}})
  assert(reply.status == 200 and json.decode(reply.body).case == true)
  host.log("http response was used")
end }})"#,
        server.url()
    );
    let setup = setup(&lua);
    write_tool_fixture(&setup);
    let kinds = [
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
    ];
    let mut good = session(tool_script(), "hi", expected(&kinds));
    good["until"] = json!({"kind": "extension_log", "nth": 1});
    good["host"] = json!({"http": [{
        "request": {"method": "GET", "url": server.url()},
        "reply": {"status": 200, "body": "{\"case\":true}"}
    }]});
    assert_success(&run_case(&setup, "http", &good));
    assert!(
        server.requests().is_empty(),
        "case host.http opened a socket"
    );

    let bad = session(tool_script(), "hi", expected(&kinds));
    assert_failure(&run_case(&setup, "http-miss", &bad), "host.http[1] miss");
    assert!(
        server.requests().is_empty(),
        "an unscripted call opened a socket"
    );
}

#[test]
fn the_case_clock_fires_after_and_every_timers_only_on_advances() {
    let after = setup(
        r#"fiber.hook("after_tool", { timeout = 5000, on_failure = "blocking", run = function()
  host.after(5000, function() host.log("after timer") end, {timeout = 5000})
end })"#,
    );
    write_tool_fixture(&after);
    let kinds = [
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
    ];
    let mut value = session(tool_script(), "hi", expected(&kinds));
    value["clock"] = json!([{"after": {"kind": "turn_completed"}, "advance_ms": 5000}]);
    value["until"] = json!({"kind": "extension_log", "nth": 1});
    assert_success(&run_case(&after, "after", &value));

    let mut no_advance = session(tool_script(), "hi", expected(&kinds));
    no_advance["until"] = json!({"kind": "extension_log", "nth": 1});
    assert_failure(
        &run_case(&after, "after-without-advance", &no_advance),
        "wait",
    );

    let every = setup(
        r#"fiber.hook("after_tool", { timeout = 5000, on_failure = "blocking", run = function()
  host.every(1000, function() host.log("every timer") end, {timeout = 5000})
end })"#,
    );
    write_tool_fixture(&every);
    let mut value = session(tool_script(), "hi", expected(&kinds));
    value["clock"] = json!([{"advance_ms": 1000}, {"advance_ms": 1000}]);
    value["until"] = json!({"kind": "extension_log", "nth": 2});
    assert_success(&run_case(&every, "every", &value));
}

#[test]
fn a_scripted_exec_is_delivered_as_an_extension_exec_event() {
    let setup = setup(
        r#"fiber.hook("after_tool", { timeout = 5000, on_failure = "blocking", run = function()
  local reply = host.exec("/case-test-do-not-run", {}, {})
  assert(reply.exit_code == 0 and reply.stdout == "ok")
  host.log("exec response was used")
end })"#,
    );
    write_tool_fixture(&setup);
    let kinds = [
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
        "extension_exec",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ];
    let mut value = session(tool_script(), "hi", expected(&kinds));
    value["until"] = json!({"kind": "extension_log", "nth": 1});
    value["host"] = json!({"exec": [{
        "request": {"program": "/case-test-do-not-run"},
        "reply": {"code": 0, "stdout": "ok", "stderr": ""}
    }]});
    assert_success(&run_case(&setup, "exec", &value));
}

#[test]
fn the_retry_deadline_is_fixed_before_retry_scheduled_even_with_a_timer() {
    for timer_ms in [2000, 500] {
        let setup = setup(&format!(
            "fiber.hook(\"after_tool\", {{timeout = 5000, on_failure = \"blocking\", run = function()\n  host.every({timer_ms}, function() host.log(\"retry timer\") end, {{timeout = 5000}})\nend}})"
        ));
        write_tool_fixture(&setup);
        let kinds = [
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
            "usage_recorded",
            "assistant_message_completed",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ];
        let mut value = session(
            json!({"steps": [
                {"tool_calls": [{"name": "read", "arguments": {"path": "fixture.txt"}}]},
                {"error": {"code": "rate_limited", "message": "try later", "retry_after_ms": 2000}},
                {"text": "Recovered."}
            ]}),
            "hi",
            expected(&kinds),
        );
        value["clock"] = json!([{
            "after": {"kind": "retry_scheduled"},
            "advance_ms": 2000
        }]);
        value["until"] = json!({"kind": "extension_log", "nth": 1});
        assert_success(&run_case(&setup, &format!("retry-{timer_ms}"), &value));
    }
}

#[test]
fn slow_script_fragments_need_one_advance_each() {
    let setup = setup("");
    let script = json!({"steps": [{"text": ["a", "b", "c"], "every_ms": 200}]});
    let kinds = [
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ];
    let mut value = session(script.clone(), "hi", expected(&kinds));
    value["expect"][8]["payload"] = json!({"text": "abc"});
    value["clock"] = json!([
        {"advance_ms": 200},
        {"after": {"kind": "assistant_message_delta", "nth": 2}, "advance_ms": 200}
    ]);
    assert_success(&run_case(&setup, "two-advances", &value));

    value["clock"] = json!([
        {"advance_ms": 200},
        {"after": {"kind": "assistant_message_delta", "nth": 2}, "advance_ms": 400}
    ]);
    let output = run_case(&setup, "mismatched-advance", &value);
    assert_failure(&output, "no waiter parked");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("clock advance[2] was not reached"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn provider_cost_call_cases_match_returns_errors_and_host_requests() {
    let server = ProviderServer::start([Response::status(
        200,
        r#"{"data":{"total_cost":0.5}}"#.as_bytes(),
    )])
    .unwrap();
    let setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { cost = { timeout = 5000, run = function(call)
  local reply = host.http({url = call.base_url .. "/generation?id=" .. call.generation_id,
                           headers = {authorization = "Bearer " .. call.key}})
  return json.decode(reply.body).data.total_cost
end } })"#,
    );
    let competitor = setup.root.path().join("case-extension-competitor");
    fs::create_dir_all(&competitor).unwrap();
    fs::write(
        competitor.join("extension.json"),
        json!({"name": "fiber.test/openrouter-competitor", "version": "0.1.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(
        competitor.join("init.lua"),
        r#"fiber.provider("openrouter", { cost = { timeout = 5000, run = function() return 0.25 end } })"#,
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(competitor),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let mut good = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {
            "generation_id": "gen-abc", "base_url": server.url(), "key": "secret"
        }},
        "host": {"http": [{
            "request": {
                "url": format!("{}/generation?id=gen-abc", server.url()),
                "headers": {"authorization": "Bearer secret"}
            },
            "reply": {"status": 200, "body": "{\"data\":{\"total_cost\":0.5}}"}
        }]},
        "returns": 0.5
    });
    assert_success(&run_case(&setup, "cost-match", &good));
    assert!(server.requests().is_empty(), "call case opened a socket");

    good["returns"] = json!(0.4);
    assert_failure(&run_case(&setup, "cost-mismatch", &good), "returns");
    assert!(server.requests().is_empty(), "call case opened a socket");

    let error_setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { cost = { timeout = 5000, run = function() error("lookup failed") end } })"#,
    );
    let error_case = json!({
        "call": {"provider": "openrouter", "function": "cost", "arg": {
            "generation_id": "gen-abc", "base_url": "https://example.test"
        }},
        "error": {"code": "extension_failed"}
    });
    assert_success(&run_case(&error_setup, "cost-error", &error_case));
}

#[test]
fn a_call_case_finds_the_provider_when_the_package_name_differs() {
    let server =
        ProviderServer::start([Response::status(200, r#"{"live":true}"#.as_bytes())]).unwrap();
    let lua = format!(
        r#"local reply = host.http({{url = {:?}}})
assert(reply.status == 200 and json.decode(reply.body).case == true)
fiber.provider("acme", {{ cost = {{ timeout = 5000, run = function() return 0.5 end }} }})"#,
        server.url()
    );
    let setup = setup_named("casefixture", &lua);
    let value = json!({
        "call": {"provider": "acme", "function": "cost", "arg": {
            "generation_id": "gen-abc", "base_url": server.url(), "key": "secret"
        }},
        "host": {"http": [{
            "request": {"url": server.url()},
            "reply": {"status": 200, "body": "{\"case\":true}"}
        }]},
        "returns": 0.5
    });
    assert_success(&run_case(&setup, "cost-other-name", &value));
    assert!(server.requests().is_empty(), "call case opened a socket");
}

#[test]
fn unsupported_provider_call_is_a_malformed_case() {
    let setup = setup("");
    let value = json!({
        "call": {"provider": "openrouter", "function": "quota", "arg": {}},
        "returns": []
    });
    let output = run_case(&setup, "unsupported", &value);
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL "), "{stdout}");
    assert!(
        stdout.contains("cost") && stdout.contains("models"),
        "{stdout}"
    );
}

#[test]
fn provider_models_call_cases_match_returns_errors_and_use_no_socket() {
    let server =
        ProviderServer::start([Response::status(200, r#"{"live":true}"#.as_bytes())]).unwrap();
    let setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { models = { timeout = 5000, run = function() return {
  { id = "m1", protocol = "openai-responses", base_url = "http://127.0.0.1:1/v1", context_window = 1000 },
  { id = "m2", protocol = "openai-responses", base_url = "http://127.0.0.1:1/v1", context_window = 1000 },
} end } })"#,
    );
    let good = json!({
        "call": {"provider": "openrouter", "function": "models", "arg": {}},
        "returns": [{"id": "m1"}, {"id": "m2"}]
    });
    assert_success(&run_case(&setup, "models-match", &good));
    assert!(server.requests().is_empty(), "call case opened a socket");

    let mismatch = json!({
        "call": {"provider": "openrouter", "function": "models", "arg": {}},
        "returns": [{"id": "m1"}]
    });
    assert_failure(&run_case(&setup, "models-mismatch", &mismatch), "returns");
    assert!(server.requests().is_empty(), "call case opened a socket");

    let error_setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { models = { timeout = 5000, run = function() error("discovery failed") end } })"#,
    );
    let error_case = json!({
        "call": {"provider": "openrouter", "function": "models", "arg": {}},
        "error": {"code": "extension_failed"}
    });
    assert_success(&run_case(&error_setup, "models-error", &error_case));
}

#[test]
fn invalid_json_is_a_malformed_case() {
    let setup = setup("");
    let file = setup.workspace().join("tests").join("broken.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "{not json").unwrap();
    let mut command = setup.fiber(&["extension-case", "tests/broken.json"]);
    command.current_dir(setup.workspace());
    let output = support::run_to_exit(setup.deadline, "the extension case child", command);
    assert_malformed(&output, "invalid JSON");
}

#[test]
fn ordinary_ask_has_neither_the_case_clock_nor_scripted_host_calls() {
    let server =
        ProviderServer::start([Response::status(200, r#"{"live":true}"#.as_bytes())]).unwrap();
    let lua = format!(
        r#"fiber.hook("after_tool", {{ timeout = 5000, on_failure = "blocking", run = function()
  local reply = host.http({{url = {:?}}})
  assert(reply.status == 200 and json.decode(reply.body).live == true)
  host.log("real HTTP response was used")
end }})"#,
        server.url()
    );
    let setup = setup(&lua);
    write_tool_fixture(&setup);
    write_script(&setup.workspace(), &tool_script());
    let output = run_ask(&setup, &["ask", "--model", "scripted/script.json", "hi"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.requests().len(), 1, "fiber ask uses real host.http");
}
