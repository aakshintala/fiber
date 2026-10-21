//! Binary-level tests of `fiber doctor` (`docs/invocation.md`, "Commands
//! and flags"): the built `fiber` says whether a session can start, reads
//! nothing it would run, and exits non-zero when one cannot. Each run goes
//! to its exit under the test's [`support::Deadline`].

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod support;

use std::fs;
use std::process::Output;

use fakes::ProviderServer;
use serde_json::json;
use support::{Setup, run_to_exit, write_json};

/// The `fiber <version>` first line, as `version` pins it: the version
/// and the recorded commit's first 7 characters.
fn version_line() -> String {
    match option_env!("FIBER_COMMIT") {
        Some(commit) => format!(
            "fiber {} ({})",
            env!("CARGO_PKG_VERSION"),
            commit.get(..7).unwrap_or(commit)
        ),
        None => format!("fiber {}", env!("CARGO_PKG_VERSION")),
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn a_ready_home_reports_the_model_and_requests_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let output = run_to_exit(setup.deadline, "doctor", setup.fiber(&["doctor"]));
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(version_line()).as_deref(), "{text}");
    assert!(text.contains("model: fake/m (provider fake)"), "{text}");
    assert!(
        server.requests().is_empty(),
        "doctor runs no provider request"
    );
}

#[test]
fn a_missing_key_blocks_with_the_login_line() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let mut doctor = setup.fiber(&["doctor"]);
    doctor.env_remove("FIBER_TEST_FAKE_KEY");
    let output = run_to_exit(setup.deadline, "doctor without a key", doctor);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(
        text.contains("no key for fake: run `fiber login fake`"),
        "{text}"
    );
}

#[test]
fn a_missing_home_reports_no_model_and_leaves_the_tree() {
    let setup = Setup::new();
    let missing = setup.home().join("missing");
    let mut doctor = setup.fiber(&["doctor"]);
    doctor.env("FIBER_HOME", &missing);
    let output = run_to_exit(setup.deadline, "doctor with a missing home", doctor);
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(
        text.contains(
            "No model was chosen. Pass `--model provider/model`, or set `model` in configuration."
        ),
        "{text}"
    );
    assert!(
        text.contains("user guide not installed: run `fiber update`"),
        "{text}"
    );
    assert!(!missing.exists(), "doctor leaves a missing home missing");
}

#[test]
fn a_planted_crash_is_named() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let crashes = setup.home().join("crashes");
    fs::create_dir_all(&crashes).unwrap();
    fs::write(
        crashes.join("s_0123456789abcdef-1700000000000.txt"),
        "traceback",
    )
    .unwrap();
    let output = run_to_exit(
        setup.deadline,
        "doctor with a crash",
        setup.fiber(&["doctor"]),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(
        text.contains("newest crash:") && text.contains("s_0123456789abcdef-1700000000000.txt"),
        "{text}"
    );
}

#[test]
fn an_unreadable_script_matches_the_session_sentence() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/missing.json"}),
    );
    let output = run_to_exit(
        setup.deadline,
        "doctor with a missing script",
        setup.fiber(&["doctor"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    let script = text
        .lines()
        .find(|line| line.starts_with("The script `"))
        .unwrap();
    let (first, _) = script.split_once(". ").unwrap();
    let sentence = format!("{first}.");

    let ask = run_to_exit(
        setup.deadline,
        "ask with a missing script",
        setup.fiber(&["ask", "hi"]),
    );
    assert_eq!(ask.status.code(), Some(1));
    let event: serde_json::Value =
        serde_json::from_str(stdout_text(&ask).lines().next().unwrap()).unwrap();
    let message = event["payload"]["error"]["message"].as_str().unwrap();
    assert_eq!(message.lines().next().unwrap(), sentence, "{message}");
}

#[test]
fn a_userinfo_url_is_refused_without_naming_credentials() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "fake/m",
            "mcp": {"servers": {"docs": {
                "url": "https://user:pw@example.invalid/mcp",
                "required": true,
            }}},
        }),
    );
    fs::create_dir_all(setup.home().join("docs")).unwrap();
    let output = run_to_exit(
        setup.deadline,
        "doctor with a userinfo URL",
        setup.fiber(&["doctor"]),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        stderr_text(&output)
    );
    let said = stdout_text(&output) + &stderr_text(&output);
    assert!(
        said.contains(
            "The required MCP server `docs` has a user name or password in its `url`, \
             so Fiber cannot start it. Take them out of the URL."
        ),
        "{said}"
    );
    assert!(!said.contains("user:pw"), "{said}");
    assert!(!said.contains("user@"), "{said}");
    assert!(!said.contains(":pw@"), "{said}");
    assert!(!said.contains("example.invalid"), "{said}");
}

#[test]
fn bedrock_s_aws_profile_is_its_source_and_never_a_missing_key() {
    let setup = Setup::new();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package(
        "bedrock",
        &to,
        "https://bedrock-runtime.{region}.amazonaws.com",
        "http://127.0.0.1:1",
    );
    let installed = run_to_exit(
        setup.deadline,
        "extension install",
        setup.fiber(&["extension", "install", to.to_str().unwrap()]),
    );
    assert_eq!(
        installed.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&installed)
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "bedrock/global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "providers": {"bedrock": {"credential": "work"}},
        }),
    );
    let mut doctor = setup.fiber(&["doctor"]);
    doctor.env("AWS_REGION", "us-east-1");
    let output = run_to_exit(setup.deadline, "doctor with an AWS profile", doctor);

    // Bedrock's `credential()` reads the AWS profile, which doctor cannot
    // check without running it, so nothing blocks (#3020).
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    assert!(
        text.contains("bedrock:\n  * work  AWS profile work, read by credential() (not checked)\n"),
        "{text}"
    );
    assert!(!text.contains("fiber login bedrock"), "{text}");
}
