//! The `resolve` jig (`docs/testing.md`, "Jigs"): the `resolve` example prints the
//! merged configuration for a Fiber home and project.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{PROJECT, Setup};
use config::{Secret, store_secret};
use serde_json::{Value, json};

/// How long the resolve jig may take. `.config/nextest.toml` kills a slow
/// test after a 30s period times 4 (120s total); with the 5s watchdog stand-down
/// the deadlines sum to 55s, so 120s is at least twice them:
/// `docs/testing.md`, "Waits and timeouts", needs nextest's timeout to be
/// at least twice the test's own deadlines, so a hang reports which wait
/// expired.
const JIG_DEADLINE: Duration = Duration::from_secs(50);

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/resolve`), so the test starts no cargo and
/// waits on no build lock.
fn example_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.join("examples").join("resolve")
}

fn resolve(setup: &Setup, args: &[&str]) -> Output {
    let child = Command::new(example_path())
        .arg(setup.workspace())
        .arg(PROJECT)
        .args(args)
        .env("FIBER_HOME", setup.home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let watchdog = fakes::Watchdog::group(child.id());
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = finished
        .recv_timeout(JIG_DEADLINE)
        .expect("the resolve jig finished: the resolve example")
        .unwrap();
    watchdog.stand_down(Duration::from_secs(5));
    output
}

#[test]
fn it_prints_the_merged_configuration_and_the_notices() {
    let setup = Setup::new();
    let secret = "sk-live-5e6f7a8b9c";
    store_secret(&setup.home(), "openrouter", &Secret::new(secret.into())).unwrap();
    setup.write(
        &setup.global(),
        r#"{"model": "openrouter/anthropic/claude-sonnet-5",
            "models": {"a/b": {"cache": {"lifetime": "5m"}}}}"#,
    );
    setup.write(
        &setup.repository(),
        r#"{"model": "databricks/databricks-claude-opus-5", "tui": {"hover": false}}"#,
    );
    setup.write(&setup.project(), r#"{"handoff": {"tokens": 100}}"#);
    let out = resolve(&setup, &["--model", "a/b", "-c", "retry.attempts=9"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{stderr}");
    let merged: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        merged["model"],
        json!("databricks/databricks-claude-opus-5")
    );
    assert_eq!(merged["handoff"]["tokens"], json!(100));
    assert_eq!(merged["retry"]["attempts"], json!(9));
    assert_eq!(merged["cache"]["lifetime"], json!("5m"));
    assert_eq!(merged["tui"]["hover"], json!(true));
    assert_eq!(merged["session"]["idle_exit_ms"], json!(1_800_000));
    assert_eq!(
        stderr,
        format!(
            "notice: {}: ignored `tui.hover`, which a repository may not set.\n",
            setup.repository().display()
        )
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains(secret));
}

#[test]
fn it_reports_an_invalid_file_and_fails() {
    let setup = Setup::new();
    setup.write(&setup.global(), "{,}");
    let out = resolve(&setup, &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!(
            "config_invalid: {} is not valid JSON (line 1, column 2). Fix the file and try again.\n",
            setup.global().display()
        )
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn it_prints_its_usage_for_bad_arguments() {
    let setup = Setup::new();
    let out = resolve(&setup, &["--bogus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("usage: resolve ")
    );
}
