//! Binary-level tests that the worked `executes` cases in `docs/extensions.md`,
//! "Testing a tool that executes", pass under `fiber extension test`: the
//! doc cannot drift from what the runner does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "binary test helpers; a failure is the test's"
)]

mod support;

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use support::run_to_exit;

/// The doc the test compiles in, so the gate fails on drift instead of
/// reading the file at run time (`docs/testing.md`).
const DOC: &str = include_str!("../../../docs/extensions.md");

/// The subsection's fenced blocks in order, each as `(tag, body)`: the
/// tool as `lua`, then the allowed case as `json`, then the denied case as
/// `json`. A doc edit that adds, drops or reorders a fence changes this
/// list, which the tests pin. The section must name its case file.
fn documented_blocks() -> Vec<(String, String)> {
    let heading = "### Testing a tool that executes";
    let start = DOC
        .find(heading)
        .expect("docs/extensions.md has no '### Testing a tool that executes'");
    let rest = &DOC[start + heading.len()..];
    let mut end = rest.len();
    for marker in ["\n## ", "\n### "] {
        if let Some(found) = rest.find(marker) {
            end = end.min(found);
        }
    }
    let section = &rest[..end];
    let mut blocks = Vec::new();
    let mut lines = section.lines();
    while let Some(line) = lines.next() {
        let Some(tag) = line.trim_start().strip_prefix("```") else {
            continue;
        };
        let mut body = String::new();
        for body_line in lines.by_ref() {
            if body_line.trim_start().starts_with("```") {
                break;
            }
            body.push_str(body_line);
            body.push('\n');
        }
        blocks.push((tag.trim().to_owned(), body));
    }
    assert!(
        section.contains("tests/launch-allowed.json"),
        "the section must name its case file"
    );
    assert!(
        section.contains("tests/launch-denied.json"),
        "the section must name its denied case file"
    );
    assert_eq!(
        blocks
            .iter()
            .map(|(tag, _)| tag.as_str())
            .collect::<Vec<_>>(),
        ["lua", "json", "json"]
    );
    blocks
}

/// An extension package holding `lua` as `init.lua` and each
/// `(file stem, case JSON)` pair under `tests/`.
fn package(setup: &support::Setup, lua: &str, cases: &[(&str, &str)]) -> PathBuf {
    let package = setup.workspace().join("package");
    fs::create_dir_all(package.join("tests")).unwrap();
    fs::write(
        package.join("extension.json"),
        json!({"name": "github.com/acme/launch-fixture", "version": "v1.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(package.join("init.lua"), lua).unwrap();
    for (name, case) in cases {
        fs::write(package.join("tests").join(format!("{name}.json")), case).unwrap();
    }
    package
}

fn run(setup: &support::Setup, package: &Path, temp_root: &Path, args: &[&str]) -> Output {
    let mut command = setup.fiber(args);
    command.current_dir(package).env("TMPDIR", temp_root);
    run_to_exit(setup.deadline, "fiber extension test", command)
}

fn temp_root() -> fakes::TempDir {
    // Rooted at `/tmp` on purpose: case sockets bind under this root and
    // must stay under 100 bytes, while `fakes::TempDir` follows a long
    // `TMPDIR`.
    fakes::TempDir::under(Path::new("/tmp"), "ft")
}

#[test]
fn the_documented_executes_case_passes_under_fiber_extension_test() {
    let blocks = documented_blocks();
    let setup = support::Setup::new();
    let package = package(
        &setup,
        &blocks[0].1,
        &[("launch-allowed", blocks[1].1.as_str())],
    );
    let temp_root = temp_root();
    let package_arg = package.to_str().unwrap();
    let output = run(
        &setup,
        &package,
        temp_root.path(),
        &["extension", "test", package_arg],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok launch-allowed\n1 passed, 0 failed\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn the_documented_allow_case_needs_its_reviewer_config() {
    let blocks = documented_blocks();
    let mut allowed: Value = serde_json::from_str(&blocks[1].1).unwrap();
    allowed
        .as_object_mut()
        .unwrap()
        .remove("config")
        .expect("the allow case must set config reviewer.model");
    // Without the reviewer the failure escalates to a person and the case
    // would wait out the runner's until bound, so close at the escalation
    // for a fast verdict. The failure still comes from the deleted config:
    // the stream carries permission_requested where the allowed stream
    // expects the reviewer's verdict.
    allowed
        .as_object_mut()
        .unwrap()
        .insert("until".to_owned(), json!({"kind": "permission_requested"}));
    let setup = support::Setup::new();
    let temp_root = temp_root();
    let without_reviewer = package(
        &setup,
        &blocks[0].1,
        &[("launch-allowed", allowed.to_string().as_str())],
    );
    let denied = run(
        &setup,
        &without_reviewer,
        temp_root.path(),
        &["extension", "test", without_reviewer.to_str().unwrap()],
    );
    assert_eq!(
        denied.status.code(),
        Some(1),
        "the allow case denied without its reviewer must fail"
    );
    let stdout = String::from_utf8_lossy(&denied.stdout).into_owned();
    assert!(
        stdout.starts_with("FAIL launch-allowed\n"),
        "unexpected verdict: {stdout}"
    );
    assert!(
        stdout.ends_with("0 passed, 1 failed\n"),
        "unexpected summary: {stdout}"
    );
    assert!(denied.stderr.is_empty());
}

#[test]
fn the_documented_denied_case_passes_under_fiber_extension_test() {
    let blocks = documented_blocks();
    let denied: Value = serde_json::from_str(&blocks[2].1).unwrap();
    // The denial the ticket names: nobody attached and no reviewer set up.
    assert_eq!(denied.get("attended"), Some(&json!(false)));
    assert!(
        denied.get("config").is_none(),
        "the denied case sets no reviewer"
    );
    let setup = support::Setup::new();
    let package = package(
        &setup,
        &blocks[0].1,
        &[("launch-denied", blocks[2].1.as_str())],
    );
    let temp_root = temp_root();
    let package_arg = package.to_str().unwrap();
    let output = run(
        &setup,
        &package,
        temp_root.path(),
        &["extension", "test", package_arg],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "ok launch-denied\n1 passed, 0 failed\n"
    );
    assert!(output.stderr.is_empty());
}
