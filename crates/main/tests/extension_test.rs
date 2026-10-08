//! Binary-level tests of `fiber extension test` (`docs/testing.md`, "Testing an extension").

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

use doors::mint;
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

// Rooted at `/tmp` on purpose: case sockets bind under this root and must stay under 104 bytes on macOS, while `fakes::TempDir` follows a long `TMPDIR`.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = Path::new("/tmp").join(mint("ft"));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn is_empty(&self) -> bool {
        fs::read_dir(&self.0).unwrap().next().is_none()
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

/// A deliberately long `TMPDIR` under `/tmp`: long enough to overflow the
/// old runner layout, short enough for the shortened one. Removed on drop.
struct LongRoot(PathBuf);

impl LongRoot {
    fn new() -> Self {
        let path = Path::new("/tmp").join(format!("long-tmp-dir-{}{}", mint(""), mint("")));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn is_empty(&self) -> bool {
        fs::read_dir(&self.0).unwrap().next().is_none()
    }
}

impl Drop for LongRoot {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

fn expected(kinds: &[&str]) -> Vec<Value> {
    kinds.iter().map(|kind| json!({"kind": kind})).collect()
}

fn case(kinds: &[&str]) -> Value {
    json!({
        "script": {"steps": [{"text": "Hello."}]},
        "prompt": "hi",
        "expect": expected(kinds)
    })
}

fn package(setup: &Setup) -> PathBuf {
    let package = setup.workspace().join("package");
    fs::create_dir_all(package.join("tests")).unwrap();
    fs::write(
        package.join("extension.json"),
        json!({"name": "github.com/acme/extension-test-fixture", "version": "v1.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(package.join("init.lua"), "").unwrap();
    fs::write(
        package.join("tests/a.json"),
        case(SESSION_KINDS).to_string(),
    )
    .unwrap();
    fs::write(
        package.join("tests/b.json"),
        case(SESSION_KINDS).to_string(),
    )
    .unwrap();
    let short = &SESSION_KINDS[..SESSION_KINDS.len() - 1];
    fs::write(package.join("tests/c.json"), case(short).to_string()).unwrap();
    package
}

fn run(setup: &Setup, package: &Path, temp_root: &Path, args: &[&str]) -> Output {
    let mut command = setup.fiber(args);
    command.current_dir(package).env("TMPDIR", temp_root);
    support::run_to_exit(setup.deadline, "fiber extension test", command)
}

#[test]
fn test_runs_cases_without_a_path_and_isolates_every_home() {
    let setup = Setup::new();
    let package = package(&setup);
    let temp_root = TempRoot::new();
    fs::write(setup.home().join("owner.marker"), "leave this home alone").unwrap();

    let failed = run(&setup, &package, temp_root.path(), &["extension", "test"]);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&failed.stdout),
        "ok a\nok b\nFAIL c\n  event[12]: unexpected durable fiber_exited\n2 passed, 1 failed\n"
    );
    assert!(failed.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(setup.home().join("owner.marker")).unwrap(),
        "leave this home alone"
    );
    assert!(
        temp_root.is_empty(),
        "case homes remain in {}",
        temp_root.path().display()
    );

    fs::write(
        package.join("tests/c.json"),
        case(SESSION_KINDS).to_string(),
    )
    .unwrap();
    let package_arg = package.to_str().unwrap();
    let passed = run(
        &setup,
        &package,
        temp_root.path(),
        &["extension", "test", package_arg],
    );
    assert_eq!(passed.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&passed.stdout),
        "ok a\nok b\nok c\n3 passed, 0 failed\n"
    );
    assert!(passed.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(setup.home().join("owner.marker")).unwrap(),
        "leave this home alone"
    );
    assert!(
        temp_root.is_empty(),
        "case homes remain in {}",
        temp_root.path().display()
    );
}

#[test]
fn session_cases_pass_with_a_long_tmpdir() {
    let setup = Setup::new();
    let package = package(&setup);
    fs::write(
        package.join("tests/c.json"),
        case(SESSION_KINDS).to_string(),
    )
    .unwrap();
    let temp_root = LongRoot::new();
    assert!(
        temp_root.path().as_os_str().len() >= 50,
        "{} is too short to cover the long-TMPDIR layout",
        temp_root.path().display()
    );

    let passed = run(&setup, &package, temp_root.path(), &["extension", "test"]);
    assert_eq!(passed.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&passed.stdout),
        "ok a\nok b\nok c\n3 passed, 0 failed\n"
    );
    assert!(passed.stderr.is_empty());
    assert!(
        temp_root.is_empty(),
        "case homes remain in {}",
        temp_root.path().display()
    );
}
