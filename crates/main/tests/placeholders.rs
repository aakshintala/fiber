//! Binary-level tests of per-account host placeholders
//! (`docs/model-routing.md`, "A per-account host"): the built `fiber` runs
//! in its own process group with its own `FIBER_HOME`, holding a provider
//! whose model's `base_url` names a host placeholder.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::ProviderServer;
use serde_json::json;
use support::{Setup, run_to_exit, write_json};

/// The one line stdout holds when the process failed before any session:
/// kind `fiber_exited`, no `session_id`, the exit code and error, and the
/// message as one stderr sentence. Returns the message.
fn assert_pre_session(output: &std::process::Output, exit: i32, code: &str) -> String {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout:?}");
    let line: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["kind"], "fiber_exited");
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], exit);
    assert_eq!(line["payload"]["error"]["code"], code);
    let message = line["payload"]["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert_eq!(stderr, format!("fiber: {message}\n"), "{stderr:?}");
    message
}

#[test]
fn an_unconfigured_model_exits_model_unconfigured_naming_the_setting() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("extensions/fake/providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {"env": "FIBER_TEST_1128_UNSET_HOST"}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        }),
    );
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "hi"]));
    let message = assert_pre_session(&output, 1, "model_unconfigured");
    assert_eq!(
        message,
        "The model `fake/m` needs the setting `workspace` for its base URL, \
         which has no value, and `FIBER_TEST_1128_UNSET_HOST` has none either."
    );
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_host_value_that_is_not_a_host_never_reaches_the_provider() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("extensions/fake/providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        }),
    );
    write_json(
        &setup.home().join("config/fake.json"),
        &json!({"workspace": "127.0.0.1@evil.example/x"}),
    );
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "hi"]));
    let message = assert_pre_session(&output, 1, "model_unconfigured");
    assert_eq!(
        message,
        "The model `fake/m` needs the setting `workspace` for its base URL, \
         whose value is not a host."
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert!(!stdout.contains("evil.example"), "{stdout:?}");
    assert!(!stderr.contains("evil.example"), "{stderr:?}");
    assert!(server.requests().is_empty());
}
