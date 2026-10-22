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
    // The rewritten template is declared beside the installed URL, so the
    // session admits the model.
    write_json(
        &setup.home().join("extensions/fake/extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "providers": {"fake": ["https://{workspace}/v1"]}}),
    );
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
         which has no value, and `FIBER_TEST_1128_UNSET_HOST` is not set in the hub's \
         environment (your login shell). Run `fiber login fake`, \
         or set `workspace` in `config/fake.json` in Fiber home, \
         or export it in your shell profile and run `fiber hub refresh`."
    );
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_host_value_that_is_not_a_host_never_reaches_the_provider() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    // The rewritten template is declared beside the installed URL, so the
    // session admits the model.
    write_json(
        &setup.home().join("extensions/fake/extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "providers": {"fake": ["https://{workspace}/v1"]}}),
    );
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

#[test]
fn a_model_at_an_unlisted_base_url_is_never_chosen() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    // The installed URL is not listed: the model is left out, so `ask`
    // exits before any session.
    write_json(
        &setup.home().join("extensions/fake/extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "providers": {"fake": ["https://other.example/v1"]}}),
    );
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "hi"]));
    assert_pre_session(&output, 1, "no_model");
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("projects").exists());
}

/// Installs provider `fake` with one model per id in `ids`, each at
/// `https://{workspace}/v1`, whose placeholder `workspace` declares no
/// environment variable and has no setting.
fn install_unset_host(setup: &Setup, ids: &[String]) {
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("extensions/fake/extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "providers": {"fake": ["https://{workspace}/v1"]}}),
    );
    let models: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            json!({"id": id, "protocol": "openai-responses",
                   "base_url": "https://{workspace}/v1", "context_window": 1000})
        })
        .collect();
    write_json(
        &setup.home().join("extensions/fake/providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {}},
            "models": models,
        }),
    );
}

/// The one `fake` notice `fiber models` prints for `count` models left out.
fn fake_notice(count: usize) -> String {
    format!(
        "`fake`: {count} models need the setting `workspace` for their base URL, \
         which has no value. Run `fiber login fake`, \
         or set `workspace` in `config/fake.json` in Fiber home.\n"
    )
}

/// Runs `fiber` with `args` to its exit, asserting exit 0, and returns its
/// stdout and stderr.
fn models_output(setup: &Setup, args: &[&str]) -> (String, String) {
    let output = run_to_exit(setup.deadline, "fiber models", setup.fiber(args));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn models_prints_one_notice_for_an_unset_host_of_many_models() {
    let setup = Setup::new();
    let ids: Vec<String> = (1..=30).map(|n| format!("m-{n}")).collect();
    install_unset_host(&setup, &ids);
    let (stdout, stderr) = models_output(&setup, &["models"]);
    assert_eq!(stdout, "");
    assert_eq!(stderr, fake_notice(30));
}

#[test]
fn models_search_filters_the_count() {
    let setup = Setup::new();
    let ids: Vec<String> = (1..=10)
        .map(|n| format!("alpha-{n}"))
        .chain((1..=20).map(|n| format!("beta-{n}")))
        .collect();
    install_unset_host(&setup, &ids);
    let (stdout, stderr) = models_output(&setup, &["models", "ALPHA"]);
    assert_eq!(stdout, "");
    assert_eq!(stderr, fake_notice(10));
}

#[test]
fn models_json_keeps_the_notice_on_stderr() {
    let setup = Setup::new();
    let ids: Vec<String> = (1..=30).map(|n| format!("m-{n}")).collect();
    install_unset_host(&setup, &ids);
    let (stdout, stderr) = models_output(&setup, &["models", "--json"]);
    assert_eq!(stdout, "");
    assert_eq!(stderr, fake_notice(30));
}

#[test]
fn the_bedrock_package_prints_one_line() {
    let setup = Setup::new();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package("bedrock", &to, "https://", "https://");
    let installed = run_to_exit(
        setup.deadline,
        "fiber extension install",
        setup.fiber(&["extension", "install", to.to_str().unwrap()]),
    );
    assert_eq!(installed.status.code(), Some(0), "{installed:?}");
    let data: serde_json::Value = serde_json::from_str(include_str!(
        "../../../providers/bedrock/providers/bedrock.json"
    ))
    .unwrap();
    let count = data["models"].as_array().unwrap().len();
    let (stdout, stderr) = models_output(&setup, &["models"]);
    assert_eq!(stdout, "");
    assert_eq!(
        stderr,
        format!(
            "`bedrock`: {count} models need the setting `region` for their base URL, \
             which has no value, and `AWS_REGION` is not set in the hub's environment \
             (your login shell). Set `region` in `config/bedrock.json` in Fiber home, \
             or export it in your shell profile and run `fiber hub refresh`.\n"
        )
    );
}
