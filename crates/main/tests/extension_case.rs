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
use support::{SESSION_KINDS, Setup, expected};

const CREDENTIAL_PROVIDER: &str = r#"
fiber.provider("casefixture", {
  credential = {timeout = 60000, run = function(arg)
    local stored = host.oauth.refresh(function(old)
      if old ~= nil then
        local reply = host.http({url = "https://example.test/refresh"})
        return json.decode(reply.body)
      end
      if arg.login == "device" then
        host.oauth.show("https://example.test/device", "1234")
        return host.oauth.poll({url = "https://example.test/poll", interval = 1})
      end
      local pkce = host.oauth.pkce()
      host.oauth.open("https://example.test/" .. pkce.challenge)
      local query = host.oauth.callback({port = 1})
      return {token = query.code, expires_at = 1700001000, account = pkce.verifier}
    end)
    return {token = stored.token, expires_at = stored.expires_at, headers = { ["x-account"] = stored.account }}
  end}
})
"#;

#[test]
fn ordered_login_credential_and_sign_share_stored_credentials_and_oauth_script() {
    let setup = setup(CREDENTIAL_PROVIDER);
    let value = json!({
        "calls": [
            {"call": {"provider": "casefixture", "function": "login", "arg": {"method": "browser"}}, "returns": {"token": "new", "expires_at": 1700001000, "account": "fixed"}},
            {"call": {"provider": "casefixture", "function": "credential", "arg": {}}, "returns": {"token": "new", "expires_at": 1700001000, "headers": {"x-account": "fixed"}}},
            {"await": "credential_idle"},
            {"call": {"provider": "casefixture", "function": "sign", "arg": {"method": "POST", "url": "https://example.test", "headers": {}}}, "returns": {"authorization": "Bearer new", "x-account": "fixed"}}
        ],
        "host": {"oauth": [
            {"pkce": {"verifier": "fixed", "challenge": "challenge"}},
            {"open": {"url": "https://example.test/challenge"}},
            {"callback": {"reply": {"query": {"code": "new"}}}}
        ]},
        "exact_credentials": true,
        "expect_credentials": {"casefixture/default": {"token": "new", "expires_at": 1700001000, "account": "fixed"}}
    });
    assert_success(&run_case(&setup, "ordered-login", &value));
    let mut bad = value.clone();
    bad["expect_credentials"]["casefixture/default"]["token"] = json!("wrong");
    assert_failure(
        &run_case(&setup, "wrong-stored", &bad),
        "expect_credentials",
    );
    let mut omitted = value.clone();
    omitted["expect_credentials"]["casefixture/default"]
        .as_object_mut()
        .unwrap()
        .remove("account");
    assert_failure(
        &run_case(&setup, "exact-omitted-field", &omitted),
        "expect_credentials",
    );
    omitted["exact_credentials"] = json!(false);
    assert_success(&run_case(&setup, "subset-omitted-field", &omitted));
    let mut both = value;
    both["call"] = json!({"provider": "casefixture", "function": "models", "arg": {}});
    assert_malformed(&run_case(&setup, "both-calls", &both), "call and calls");
}

#[test]
fn a_call_clock_advances_a_device_poll_only_after_it_parks() {
    let setup = setup(CREDENTIAL_PROVIDER);
    let value = json!({
        "call": {"provider": "casefixture", "function": "login", "arg": {"method": "device", "label": "work"}},
        "returns": {"token": "device", "expires_at": 1700001000},
        "clock": [{"advance_ms": 1000}],
        "host": {
            "oauth": [{"show": {"url": "https://example.test/device", "code": "1234"}}],
            "http": [
                {"request": {"url": "https://example.test/poll"}, "reply": {"status": 200, "body": "{\"error\":\"authorization_pending\"}"}},
                {"request": {"url": "https://example.test/poll"}, "reply": {"status": 200, "body": "{\"token\":\"device\",\"expires_at\":1700001000,\"account\":\"work\"}"}}
            ]
        },
        "expect_credentials": {"casefixture/work": {"token": "device"}}
    });
    assert_success(&run_case(&setup, "device-clock", &value));
}

#[test]
fn per_call_clock_and_credential_idle_order_a_background_refresh() {
    let setup = setup(CREDENTIAL_PROVIDER);
    let sign = json!({"provider": "casefixture", "function": "sign", "arg": {"method": "POST", "url": "https://example.test", "headers": {}}});
    let value = json!({
        "credentials": {"casefixture/default": {"token": "old", "expires_at": 1700000301, "account": "old-account"}},
        "calls": [
            {"call": sign, "returns": {"authorization": "Bearer old", "x-account": "old-account"}},
            {"call": sign, "clock": [{"advance_ms": 2000}], "returns": {"authorization": "Bearer old", "x-account": "old-account"}},
            {"await": "credential_idle"},
            {"call": sign, "returns": {"authorization": "Bearer fresh", "x-account": "fresh-account"}}
        ],
        "host": {"http": [{"request": {"url": "https://example.test/refresh"}, "reply": {"status": 200, "body": "{\"token\":\"fresh\",\"expires_at\":1700001000,\"account\":\"fresh-account\"}"}}]},
        "expect_credentials": {"casefixture/default": {"token": "fresh", "account": "fresh-account"}}
    });
    assert_success(&run_case(&setup, "background-refresh", &value));
}

#[test]
fn unattended_calls_and_scripted_callback_errors_keep_their_codes() {
    let setup = setup(CREDENTIAL_PROVIDER);
    let mut value = json!({
        "call": {"provider": "casefixture", "function": "credential", "arg": {}},
        "attended": false,
        "host": {"oauth": [{"pkce": {"verifier": "v", "challenge": "c"}}]},
        "error": {"code": "authentication_failed"}
    });
    assert_success(&run_case(&setup, "unattended", &value));
    value["attended"] = json!(true);
    value["host"]["oauth"] = json!([
        {"pkce": {"verifier": "v", "challenge": "c"}},
        {"open": {"url": "https://example.test/c"}},
        {"callback": {"error": {"code": "io_failed", "message": "callback busy"}}}
    ]);
    value["error"] = json!({"code": "authentication_failed"});
    assert_success(&run_case(&setup, "callback-error", &value));
}

#[test]
fn login_labels_and_email_carry_into_the_following_sign() {
    for (label, email, stored_label) in [
        (None, None, "default"),
        (None, Some("person@example.test"), "person@example.test"),
        (Some("work"), Some("person@example.test"), "work"),
    ] {
        let lua = format!(
            r#"
            fiber.provider("casefixture", {{credential = {{timeout = 5000, run = function(arg)
                local stored = host.oauth.refresh(function(old)
                    assert(old ~= nil or arg.login ~= nil, "sign read the wrong credential label")
                    return {{token = "label-token", expires_at = 1700001000}}
                end)
                return {{token = stored.token, expires_at = stored.expires_at, email = {}}}
            end}}}})
        "#,
            email
                .map(|email| format!("{email:?}"))
                .unwrap_or_else(|| "nil".to_owned())
        );
        let setup = setup(&lua);
        let mut arg = json!({"method": "browser"});
        if let Some(label) = label {
            arg["label"] = json!(label);
        }
        let value = json!({
            "calls": [
                {"call": {"provider": "casefixture", "function": "login", "arg": arg}, "returns": {"token": "label-token"}},
                {"call": {"provider": "casefixture", "function": "sign", "arg": {"method": "GET", "url": "https://example.test", "headers": {}}}, "returns": {"authorization": "Bearer label-token"}}
            ],
            "expect_credentials": {format!("casefixture/{stored_label}"): {"token": "label-token"}}
        });
        assert_success(&run_case(&setup, "login-label", &value));
    }
}

#[test]
fn a_credential_call_reads_the_stored_credential_of_its_label() {
    let setup = setup(
        r#"
        fiber.provider("casefixture", {credential = {timeout = 5000, run = function(arg)
            local stored = host.oauth.refresh(function(old)
                assert(old ~= nil, "credential read no stored value for its label")
                return old
            end)
            return {token = stored.token, expires_at = stored.expires_at}
        end}})
    "#,
    );
    let value = json!({
        "credentials": {
            "casefixture/default": {"token": "default-token", "expires_at": 4102444800_u64},
            "casefixture/work": {"token": "work-token", "expires_at": 4102444800_u64}
        },
        "call": {"provider": "casefixture", "function": "credential", "arg": {"label": "work"}},
        "returns": {"token": "work-token"}
    });
    assert_success(&run_case(&setup, "credential-label", &value));
}

#[test]
fn sign_cases_forward_the_request_and_match_the_added_headers() {
    let setup = setup(
        r#"
        fiber.provider("casefixture", {sign = {timeout = 5000, run = function(arg)
            assert(arg.method == "POST")
            assert(arg.url == "https://example.test/signed")
            assert(arg.headers["x-input"] == "input")
            assert(arg.body_sha256 == host.sha256(""))
            assert(arg.time == 1700000000)
            return {["x-signed"] = "signature"}
        end}})
    "#,
    );
    let value = json!({
        "call": {"provider": "casefixture", "function": "sign", "arg": {"method": "POST", "url": "https://example.test/signed", "headers": {"x-input": "input"}}},
        "returns": {"x-signed": "signature"}
    });
    assert_success(&run_case(&setup, "sign-request", &value));
}

#[test]
fn ordered_calls_require_a_provider_and_reject_mixed_providers() {
    let setup = setup(CREDENTIAL_PROVIDER);
    assert_malformed(
        &run_case(
            &setup,
            "await-only",
            &json!({"calls": [{"await": "credential_idle"}]}),
        ),
        "at least one provider call",
    );
    let value = json!({"calls": [
        {"call": {"provider": "casefixture", "function": "models", "arg": {}}, "returns": []},
        {"call": {"provider": "other", "function": "models", "arg": {}}, "returns": []}
    ]});
    assert_malformed(&run_case(&setup, "mixed-providers", &value), "one provider");
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
    // The case runner loads hook cases through `SessionExtensions`, so the
    // manifest declares every `fiber.provider` the script registers. A
    // cost, credential or sign-only provider lists no URL.
    let mut seen: Vec<&str> = Vec::new();
    for part in lua.split("fiber.provider(\"").skip(1) {
        if let Some(registered) = part.split('"').next()
            && !seen.contains(&registered)
        {
            seen.push(registered);
        }
    }
    let mut manifest = json!({"name": name, "version": "0.1.0", "fiber": "0.0.0", "api": 1});
    let declared: Vec<(String, Value)> = seen
        .iter()
        .map(|registered| ((*registered).to_owned(), json!([])))
        .collect();
    // A `models` case lists its models at `http://127.0.0.1:1/v1`.
    let declared: Vec<(String, Value)> = declared
        .into_iter()
        .map(|(registered, urls)| {
            if lua.contains("http://127.0.0.1:1/v1") {
                (registered, json!(["http://127.0.0.1:1/v1"]))
            } else {
                (registered, urls)
            }
        })
        .collect();
    manifest["providers"] = Value::Object(declared.into_iter().collect());
    fs::write(source.join("extension.json"), manifest.to_string()).unwrap();
    if seen.is_empty() {
        fs::write(source.join("init.lua"), lua).unwrap();
    } else {
        fs::create_dir_all(source.join("providers")).unwrap();
        let mixed = lua.contains("fiber.command(")
            || lua.contains("fiber.hook(")
            || lua.contains("fiber.tool(")
            || lua.contains("fiber.search_backend(");
        if seen.len() == 1 && !mixed {
            fs::write(
                source.join("providers").join(format!("{}.lua", seen[0])),
                lua,
            )
            .unwrap();
        } else {
            let mut parts = lua.split("fiber.provider(\"");
            let init = parts.next().unwrap();
            fs::write(source.join("init.lua"), init).unwrap();
            for part in parts {
                let name = part.split('"').next().unwrap();
                fs::write(
                    source.join("providers").join(format!("{name}.lua")),
                    format!("fiber.provider(\"{part}"),
                )
                .unwrap();
            }
        }
    }
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
  local reply = host.exec("/case-test-do-not-run", {}, {env = {FIBER_X = "1"}})
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
        "request": {"program": "/case-test-do-not-run", "env": {"FIBER_X": "1"}},
        "reply": {"code": 0, "stdout": "ok", "stderr": ""}
    }]});
    assert_success(&run_case(&setup, "exec", &value));
}

#[test]
fn a_scripted_exec_signal_reaches_the_extension_as_a_name() {
    let setup = setup(
        r#"fiber.hook("after_tool", { timeout = 5000, on_failure = "blocking", run = function()
  local reply = host.exec("/case-test-do-not-run", {}, {env = {FIBER_X = "1"}})
  assert(reply.signal == "SIGKILL" and reply.exit_code == nil)
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
        "request": {"program": "/case-test-do-not-run", "env": {"FIBER_X": "1"}},
        "reply": {"signal": 9, "stdout": "", "stderr": ""}
    }]});
    assert_success(&run_case(&setup, "exec-signal", &value));
}

/// A failing hang case names the stuck request, matches the whole stream
/// through `fiber_exited`, and panics nowhere.
fn assert_hang_failure(output: &Output, reason: &str) {
    assert_failure(output, reason);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("expect["),
        "the stream must match through fiber_exited: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("panicked") && !stderr.contains("panicked"),
        "the child must fail by verdict, not by panic: {stdout} {stderr}"
    );
}

/// One hanging exec with no `env` to redact: the request names the program.
fn hang_request(program: &str, secret: &str) -> Value {
    json!({"program": program, "env": {"FIBER_X": secret}})
}

fn hang_hook(timeout_ms: u32, secret: &str) -> String {
    hang_hook_with(timeout_ms, "blocking", secret)
}

fn hang_hook_with(timeout_ms: u32, on_failure: &str, secret: &str) -> String {
    format!(
        r#"fiber.hook("after_tool", {{ timeout = {timeout_ms}, on_failure = "{on_failure}", run = function()
  host.exec("/case-test-do-not-run", {{}}, {{env = {{FIBER_X = "{secret}"}}}})
end}})"#
    )
}

/// The `expect` entry for the tool call a blocking hook withholds: the
/// hook's timeout ran the callback to its end, so the result carries only
/// the withheld line.
fn withheld_tool_call_completed() -> Value {
    json!({
        "kind": "tool_call_completed",
        "payload": {
            "content": [{
                "type": "text",
                "text": "Output withheld: the `after_tool` hook of extension casefixture failed."
            }]
        }
    })
}

/// The stream a blocking hook leaves when its exec ends at the hook's own
/// timeout: the tool output is withheld and the turn still completes.
const HANG_KINDS: [&str; 19] = [
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
    // `fiber_exited` closes every run; it is appended per test below.
];

#[test]
fn a_hang_advance_releases_the_callback_to_its_timeout() {
    let setup = setup(&hang_hook(1000, "1"));
    write_tool_fixture(&setup);
    let mut kinds = HANG_KINDS.to_vec();
    kinds.push("fiber_exited");
    let mut expect = expected(&kinds);
    expect[12] = withheld_tool_call_completed();
    let mut value = session(tool_script(), "hi", expect);
    value["clock"] = json!([{"advance_ms": 1000, "hang": 1}]);
    value["host"] = json!({"exec": [{
        "request": hang_request("/case-test-do-not-run", "1"),
        "reply": {"hang": true}
    }]});
    assert_success(&run_case(&setup, "hang-released", &value));
}

#[test]
fn an_unclocked_hang_fails_naming_the_request() {
    let setup = setup(&hang_hook(1000, "1"));
    write_tool_fixture(&setup);
    let mut kinds = HANG_KINDS.to_vec();
    kinds.push("fiber_exited");
    let mut expect = expected(&kinds);
    expect[12] = withheld_tool_call_completed();
    let mut value = session(tool_script(), "hi", expect);
    value["host"] = json!({"exec": [{
        "request": hang_request("/case-test-do-not-run", "1"),
        "reply": {"hang": true}
    }]});
    assert_hang_failure(
        &run_case(&setup, "hang-unclocked", &value),
        "host.exec[1] hung",
    );
}

#[test]
fn a_second_hang_past_the_advance_fails_naming_its_entry() {
    // The first hook is non-blocking so the chain still runs the second
    // after the advance releases the first at its timeout; a blocking
    // failure would stop the chain and leave entry 2 unused.
    let setup = setup(&format!(
        "{}\n{}",
        hang_hook_with(1000, "non-blocking", "1"),
        hang_hook(3000, "2")
    ));
    write_tool_fixture(&setup);
    let mut kinds = HANG_KINDS.to_vec();
    kinds.push("fiber_exited");
    let mut value = session(tool_script(), "hi", expected(&kinds));
    value["clock"] = json!([{"advance_ms": 1000, "hang": 1}]);
    value["host"] = json!({"exec": [
        {
            "request": hang_request("/case-test-do-not-run", "1"),
            "reply": {"hang": true}
        },
        {
            "request": hang_request("/case-test-do-not-run", "2"),
            "reply": {"hang": true}
        }
    ]});
    assert_hang_failure(
        &run_case(&setup, "hang-second", &value),
        "host.exec[2] hung",
    );
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
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("diagnostics:"),
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
        json!({"name": "fiber.test/openrouter-competitor", "version": "0.1.0", "fiber": "0.0.0", "api": 1, "providers": {"openrouter": []}}).to_string(),
    )
    .unwrap();
    fs::write(
        { std::fs::create_dir_all((competitor.join("providers/openrouter.lua")).parent().unwrap()).unwrap(); competitor.join("providers/openrouter.lua") },
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
fn provider_quota_call_cases_match_returns_errors_and_use_no_socket() {
    let server =
        ProviderServer::start([Response::status(200, r#"{"live":true}"#.as_bytes())]).unwrap();
    let url = format!("{}/key", server.url());
    let lua = format!(
        r#"fiber.provider("openrouter", {{ quota = {{ timeout = 5000, run = function(a)
  local reply = host.http({{url = {url:?}, headers = {{authorization = "Bearer " .. a.key}}}})
  if reply.status ~= 200 then error("quota failed " .. reply.status) end
  local data = json.decode(reply.body).data
  return {{ credit = {{ remaining = data.limit_remaining, limit = data.limit }} }}
end }} }})"#
    );
    let setup = setup_named("github.com/aakshintala/fiber/providers/openrouter", &lua);
    let mut good = json!({
        "call": {"provider": "openrouter", "function": "quota", "arg": {"key": "secret"}},
        "host": {"http": [{
            "request": {"method": "GET", "url": url, "headers": {"authorization": "Bearer secret"}},
            "reply": {"status": 200, "body": "{\"data\":{\"limit\":10,\"limit_remaining\":9.5}}"}
        }]},
        "returns": {"credit": {"remaining": 9.5, "limit": 10}}
    });
    assert_success(&run_case(&setup, "quota-match", &good));
    assert!(server.requests().is_empty(), "call case opened a socket");

    good["returns"] = json!({"credit": {"remaining": 9.5, "limit": 11}});
    assert_failure(&run_case(&setup, "quota-mismatch", &good), "returns");
    assert!(server.requests().is_empty(), "call case opened a socket");

    let label_setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { quota = { timeout = 5000, run = function(a)
  return { windows = { { name = a.label, used_percent = 0 } } }
end } })"#,
    );
    let label_case = json!({
        "call": {"provider": "openrouter", "function": "quota", "arg": {"label": "work"}},
        "returns": {"windows": [{"name": "work", "used_percent": 0}]}
    });
    assert_success(&run_case(&label_setup, "quota-label", &label_case));

    let error_setup = setup_named(
        "github.com/aakshintala/fiber/providers/openrouter",
        r#"fiber.provider("openrouter", { quota = { timeout = 5000, run = function() error("quota broke") end } })"#,
    );
    let error_case = json!({
        "call": {"provider": "openrouter", "function": "quota", "arg": {}},
        "error": {"code": "extension_failed"}
    });
    assert_success(&run_case(&error_setup, "quota-error", &error_case));
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
        "call": {"provider": "openrouter", "function": "usage", "arg": {}},
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
        stdout.contains("cost") && stdout.contains("models") && stdout.contains("quota"),
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

fn command_session(command: Value, until: Value, expect: Vec<Value>) -> Value {
    json!({
        "script": {"steps": [{"text": "unused"}]},
        "command": command,
        "until": until,
        "expect": expect,
    })
}

#[test]
fn a_command_case_runs_the_extension_command_with_no_turn() {
    let setup = setup(
        r#"fiber.command("greet", {timeout = 5000, run = function(text) host.status("saw " .. text) end})"#,
    );
    // The whole ordered stream a command case observes: no turn runs, so
    // nothing falls between `extensions_loaded` and `fiber_exited`
    // (docs/extensions.md:753-759; order pinned per docs/testing.md:110-16).
    let value = command_session(
        json!({"name": "greet", "text": "hi"}),
        json!({"kind": "extension_ui"}),
        expected(&[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ]),
    );
    assert_success(&run_case(&setup, "command-status", &value));
}

#[test]
fn a_command_case_with_an_unregistered_name_fails() {
    let setup = setup(
        r#"fiber.command("greet", {timeout = 5000, run = function(text) host.status("saw " .. text) end})"#,
    );
    let value = command_session(
        json!({"name": "nope"}),
        json!({"kind": "extension_ui"}),
        expected(&["session_started"]),
    );
    assert_failure(
        &run_case(&setup, "unknown-command", &value),
        "unknown_command",
    );
}

#[test]
fn malformed_command_cases_exit_2_with_no_fiber_line_on_stderr() {
    let setup = setup("");
    let script = json!({"steps": [{"text": "unused"}]});
    let expect = expected(&["session_started"]);
    let mut both = json!({"script": script.clone(), "expect": expect.clone()});
    both["prompt"] = json!("hi");
    both["command"] = json!({"name": "greet"});
    both["until"] = json!({"kind": "extension_ui"});
    let neither = json!({"script": script.clone(), "expect": expect.clone()});
    let mut no_until = json!({"script": script, "expect": expect});
    no_until["command"] = json!({"name": "greet"});
    for (name, value, reason) in [
        ("both-fields", both, "prompt and command"),
        ("neither-field", neither, "prompt or command"),
        ("command-without-until", no_until, "until"),
    ] {
        let output = run_case(&setup, name, &value);
        assert_malformed(&output, reason);
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("fiber: "),
            "malformed {name} wrote a fiber line: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
