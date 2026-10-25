//! A process extension's manifest fields (`docs/extensions.md`, "Process
//! extensions"): `required` defaults to false, and `exit_timeout_ms` has no
//! default. Zero is a representable allowance; a missing, negative or
//! fractional bound fails deserialization through the existing config
//! errors.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::Setup;
use config::read_manifest;
use contract::ErrorCode;

const BASE: &str = r#"{"name": "a", "version": "v1.0.0", "fiber": "0.1.0", "api": 1, "process": {"#;

fn manifest(process: &str) -> String {
    [BASE, process, "}}"].concat()
}

fn read(process: &str) -> Result<config::Manifest, config::ConfigError> {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    setup.write(&dir.join("extension.json"), &manifest(process));
    read_manifest(&dir)
}

#[test]
fn required_defaults_to_false() {
    let manifest = read(r#""program": "x", "exit_timeout_ms": 0"#).unwrap();
    let process = manifest.process.unwrap();
    assert_eq!(process.program, "x");
    assert!(process.args.is_empty());
    assert!(!process.required);
}

#[test]
fn required_true_is_accepted() {
    let manifest = read(r#""program": "x", "exit_timeout_ms": 100, "required": true"#).unwrap();
    let process = manifest.process.unwrap();
    assert!(process.required);
    assert_eq!(process.exit_timeout_ms, 100);
}

#[test]
fn zero_is_a_representable_allowance() {
    let manifest = read(r#""program": "x", "exit_timeout_ms": 0"#).unwrap();
    assert_eq!(manifest.process.unwrap().exit_timeout_ms, 0);
}

#[test]
fn exit_timeout_ms_has_no_default() {
    let err = read(r#""program": "x""#).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn a_negative_or_fractional_exit_bound_is_refused() {
    for bound in ["-1", "1.5"] {
        let err = read(&format!(r#""program": "x", "exit_timeout_ms": {bound}"#)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{bound}: {err}");
    }
}
