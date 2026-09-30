//! The `resolve` jig (`docs/testing.md`, "Jigs"): `cargo run -p config
//! --example resolve` prints the merged configuration for a Fiber home and
//! project.

mod common;

use std::process::{Command, Output};

use common::{PROJECT, Setup};
use config::{Secret, store_secret};
use serde_json::{Value, json};

fn resolve(setup: &Setup, args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "config",
            "--example",
            "resolve",
            "--",
        ])
        .arg(setup.workspace())
        .arg(PROJECT)
        .args(args)
        .env("FIBER_HOME", setup.home())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
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
    let out = resolve(
        &setup,
        &["--model", "a/b", "--headless", "-c", "retry.attempts=9"],
    )
    .unwrap();
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
    assert_eq!(merged["permissions"]["mode"], json!("auto"));
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
    let out = resolve(&setup, &[]).unwrap();
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
    let out = resolve(&setup, &["--bogus"]).unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .starts_with("usage: resolve ")
    );
}
