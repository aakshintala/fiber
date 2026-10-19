//! Binary-level test that the denied `executes` case in `docs/extensions.md`,
//! "Testing a tool that executes", passes under `fiber extension test`: with
//! no `reviewer.model` and `attended: false` the call is denied with
//! `decided_by: no_reviewer`, no `permission_requested` is written, and
//! `tool_call_completed` has `status: denied`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "binary test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use support::run_to_exit;

/// The doc the test compiles in, so the gate fails on drift instead of
/// reading the file at run time (`docs/testing.md`).
const DOC: &str = include_str!("../../../docs/extensions.md");

/// The subsection's fenced blocks in order: the tool as `lua`, the allowed
/// case as `json`, then the denied case as `json`. A doc edit that adds,
/// drops or reorders a fence changes this list, which the test pins.
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
        serde_json::json!({"name": "github.com/acme/launch-fixture", "version": "v1.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
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
fn the_documented_denied_case_passes_under_fiber_extension_test() {
    let blocks = documented_blocks();
    let denied: serde_json::Value = serde_json::from_str(&blocks[2].1).unwrap();
    // The denial the ticket names: no reviewer, nobody attached, denied
    // with no_reviewer and no permission_requested.
    assert_eq!(denied.get("attended"), Some(&serde_json::json!(false)));
    assert!(
        denied.get("config").is_none(),
        "the denied case sets no reviewer"
    );
    let expect = denied["expect"]
        .as_array()
        .expect("the denied case lists its expected events");
    assert!(
        expect
            .iter()
            .any(|line| line.get("kind").and_then(|kind| kind.as_str())
                == Some("permission_resolved")
                && line
                    .pointer("/payload/decided_by")
                    .and_then(|by| by.as_str())
                    == Some("no_reviewer")
                && line
                    .pointer("/payload/decision")
                    .and_then(|decision| decision.as_str())
                    == Some("deny")),
        "the denied case expects the no_reviewer denial"
    );
    assert!(
        expect
            .iter()
            .any(|line| line.get("kind").and_then(|kind| kind.as_str())
                == Some("tool_call_completed")
                && line
                    .pointer("/payload/status")
                    .and_then(|status| status.as_str())
                    == Some("denied")),
        "the denied case expects the denied completion"
    );
    assert!(
        expect
            .iter()
            .all(|line| line.get("kind").and_then(|kind| kind.as_str())
                != Some("permission_requested")),
        "the denied case writes no permission_requested"
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
