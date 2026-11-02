//! Binary-level tests of `fiber extension test` (`docs/testing.md`, "Testing an extension").

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
use support::{SESSION_KINDS, Setup, expected};

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

fn assert_only_owner_marker(temp_root: &Path) {
    let names: Vec<String> = fs::read_dir(temp_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![".fiber-owner-pid".to_owned()],
        "case homes are removed; only the owner marker remains"
    );
}

#[test]
fn test_runs_cases_without_a_path_and_isolates_every_home() {
    let setup = Setup::new();
    let package = package(&setup);
    // Rooted at `/tmp` on purpose: case sockets bind under this root and
    // must stay under 104 bytes on macOS, while `fakes::TempDir` follows a
    // long `TMPDIR`.
    let temp_root = fakes::TempDir::under(Path::new("/tmp"), "ft");
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
    assert_only_owner_marker(temp_root.path());

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
    assert_only_owner_marker(temp_root.path());
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
    // About 50 bytes under `/tmp`, the length of macOS's default `TMPDIR`:
    // a case's session socket stays inside the 104-byte limit only if the
    // runner keeps its own directory names short.
    let temp_root =
        fakes::TempDir::under(Path::new("/tmp"), "long-tmp-dir-0123456789abcdefghijklmno");
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
    assert_only_owner_marker(temp_root.path());
}
