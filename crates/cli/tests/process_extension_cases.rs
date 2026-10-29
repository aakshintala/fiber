//! Process-package prechecks through the public extension test runner
//! (`docs/extensions.md`, "Testing an extension"). Binary cases live in
//! main's process_extensions target, which owns the built Fiber binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fakes::{Deadline, ProcessFixture, TempDir};
use serde_json::json;

const CHILD: &str = "FIBER_PROCESS_CASE_PRECHECK_CHILD";

fn package(root: &Path, data: bool) {
    let hello = root.join("hello.json");
    let invoked = root.join("invoked.json");
    let plan = json!([
        {"read": {"as": "hello"}},
        {"write_file": {"path": hello, "value": {"$ref": "hello"}}},
        {"send": {"id": "provider", "command": "call", "args": {"call": "fiber.provider", "args": ["remote", {"credential": {"timeout": 5000, "run": true}}]}}},
        {"read_until": {"path": "payload.command_id", "equals": "provider"}},
        {"send": {"id": "ready", "command": "ready"}},
        {"loop": [
            {"read": {"as": "line"}},
            {"if": {"path": "line.kind", "equals": "extension_request", "then": [
                {"write_file": {"path": invoked, "value": {"$ref": "line"}}}
            ]}}
        ]}
    ]);
    let (program, args) = ProcessFixture::command("peer", Some(&plan));
    std::fs::write(
        root.join("extension.json"),
        json!({
            "name": "fiber.test/precheck", "version": "v1", "fiber": "0.0.0", "api": 1,
            "providers": {"remote": []},
            "process": {"program": program, "args": args, "exit_timeout_ms": 0}
        })
        .to_string(),
    )
    .unwrap();
    if data {
        std::fs::create_dir(root.join("providers")).unwrap();
        std::fs::write(
            root.join("providers/remote.json"),
            json!({
                "name": "remote", "credential": {"env": "UNSET_PROCESS_CASE_KEY"},
                "models": [{"id": "m", "protocol": "google-generative-ai", "base_url": "http://127.0.0.1:1"}]
            })
            .to_string(),
        )
        .unwrap();
    }
    std::fs::create_dir(root.join("tests")).unwrap();
    std::fs::write(
        root.join("tests/models.json"),
        json!({
            "call": {"provider": "remote", "function": "models", "arg": {}},
            "returns": []
        })
        .to_string(),
    )
    .unwrap();
}

/// Credential declaration drift must stop before discovering or executing
/// cases, and inspecting a registered credential function must not invoke it.
#[test]
fn process_credential_drift_stops_before_cases_without_invoking_functions() {
    if let Some(package) = std::env::var_os(CHILD) {
        std::process::exit(cli::extension_test(
            Some(Path::new(&package)),
            Ok("/missing-fiber-must-not-run".into()),
            Arc::new(fakes::clock::SystemClock),
        ));
    }
    let root = TempDir::new("fiber-process-case-precheck");
    package(root.path(), true);
    let output = fakes::rerun_prepared(
        "process_credential_drift_stops_before_cases_without_invoking_functions",
        &[],
        Deadline::after(Duration::from_secs(30)),
        |command| {
            command.env(CHILD, root.path()).env("TMPDIR", "/tmp");
        },
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let context = format!(
        "status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1), "{context}");
    assert!(
        stderr.contains("registers credential() but its data declares neither"),
        "{context}"
    );
    assert!(
        root.path().join("hello.json").is_file(),
        "precheck actually starts the process; {context}"
    );
    assert!(
        !root.path().join("invoked.json").exists(),
        "precheck never invokes a registered function; {context}"
    );
    assert!(!stdout.contains("no cases"), "{context}");
}

/// A provider without a data file is exempt from the drift comparison,
/// even if it registers credential(). Reaching case-child startup proves
/// the exception; an absent binary prevents the case from invoking callbacks.
#[test]
fn process_data_less_provider_reaches_case_child_startup() {
    if let Some(package) = std::env::var_os(CHILD) {
        std::process::exit(cli::extension_test(
            Some(Path::new(&package)),
            Ok("/missing-fiber-must-not-run".into()),
            Arc::new(fakes::clock::SystemClock),
        ));
    }
    let root = TempDir::new("fiber-process-case-data-less");
    package(root.path(), false);
    let output = fakes::rerun_prepared(
        "process_data_less_provider_reaches_case_child_startup",
        &[],
        Deadline::after(Duration::from_secs(30)),
        |command| {
            command.env(CHILD, root.path()).env("TMPDIR", "/tmp");
        },
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let context = format!(
        "status: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1), "{context}");
    assert!(stdout.contains("FAIL models"), "{context}");
    assert!(!stdout.contains("no cases"), "{context}");
    assert!(stderr.is_empty(), "{context}");
    assert!(!root.path().join("invoked.json").exists(), "{context}");
}
