//! The public package seam (`super::package`): loading process packages
//! and registering through them, with the same outcomes the wire carries.
//!
//! `Package::load` crosses the public seam; `register` is the crate's
//! internal seam, where the transport will answer registration calls. A
//! refusal carries the Lua call's exact sentence for `command_rejected`;
//! a failure carries the package failure for `extension_failed`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use contract::CommandId;
use serde_json::{Value, json};

use super::package::{Package, PackageOptions};
use super::process::RegisterOutcome;

/// A package directory holding `manifest_extra` merged over a process
/// manifest for `fiber.test/proc`.
fn package_dir(extra: Value) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-process-package");
    let mut manifest = json!({
        "name": "fiber.test/proc",
        "version": "v1.2.3",
        "fiber": "0.1.0",
        "api": 1,
        "process": {"program": "proc", "exit_timeout_ms": 1000},
    });
    for (key, value) in extra.as_object().unwrap() {
        manifest[key] = value.clone();
    }
    std::fs::write(dir.path().join("extension.json"), manifest.to_string()).unwrap();
    dir
}

/// Loads the package in `extra`, keeping its directory alive.
fn load(extra: Value) -> (fakes::TempDir, Arc<Package>) {
    let dir = package_dir(extra);
    let package = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    (dir, package)
}

/// Answers one registration call through the package, as the transport
/// will: the call's command id with its arguments.
fn register(package: &Package, id: &str, call: &str, args: Value) -> RegisterOutcome {
    package.register(CommandId(id.to_owned()), call, args.as_array().unwrap())
}

/// A valid `fiber.tool` spec with `run`, as JSON.
fn tool_spec() -> Value {
    json!({
        "description": "Does the thing.",
        "input_schema": {"type": "object"},
        "effects": {"effects": [], "reversible": true},
        "timeout": 5000,
        "run": true,
    })
}

#[test]
fn process_package_loads_its_manifest() {
    let (_dir, package) = load(json!({}));
    assert_eq!(package.name(), "fiber.test/proc");
    assert!(package.is_process());
    let registry = package.registry().unwrap();
    assert_eq!(registry.owner(), "fiber.test/proc");
    assert!(registry.tools().is_empty());
    assert!(registry.commands().is_empty());
    assert!(registry.problems().is_empty());
}

#[test]
fn package_with_wrong_api_fails() {
    let dir = package_dir(json!({"api": 999}));
    let Err(crate::Error::ApiVersion { name, api }) = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    }) else {
        panic!("a wrong-api package must fail");
    };
    assert_eq!(name, "fiber.test/proc");
    assert_eq!(api, 999);
}

#[test]
fn lua_package_loads_without_a_process_registry() {
    let dir = package_dir(json!({"process": Value::Null}));
    let package = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    .unwrap();
    assert_eq!(package.name(), "fiber.test/proc");
    assert!(!package.is_process());
    assert!(package.registry().is_none());
}

#[test]
fn package_undeclared_provider_files_fail() {
    let dir = package_dir(json!({"providers": {"p": []}}));
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("providers/q.json"), "{}").unwrap();
    let Err(crate::Error::UndeclaredProvider {
        extension,
        provider,
    }) = Package::load(PackageOptions {
        dir: dir.path().to_path_buf(),
        clock: fakes::clock::FakeClock::new(),
    })
    else {
        panic!("an undeclared provider file must fail the package");
    };
    assert_eq!(extension, "fiber.test/proc");
    assert_eq!(provider, "q");
}

#[test]
fn package_registers_a_tool() {
    let (_dir, package) = load(json!({}));
    let outcome = register(&package, "c1", "fiber.tool", json!(["t", tool_spec()]));
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let registry = package.registry().unwrap();
    let entry = registry.tools().get("t").unwrap();
    assert_eq!(entry.run.command, CommandId("c1".to_owned()));
    assert_eq!(entry.run.generation, 1);
}

/// An invalid call refuses with the Lua sentence and its code, which the
/// wire carries as `command_rejected`.
#[test]
fn package_invalid_registration_is_rejected() {
    let (_dir, package) = load(json!({}));
    let mut spec = tool_spec();
    spec["timeout"] = json!(0);
    let outcome = register(&package, "c1", "fiber.tool", json!(["t", spec]));
    let RegisterOutcome::Rejected { code, message } = outcome else {
        panic!("an invalid tool refuses, not {outcome:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "`t` tool not registered: `timeout` must be a whole number of milliseconds above 0"
    );
    assert!(package.registry().unwrap().tools().is_empty());
}

/// An undeclared provider fails the package with `extension_failed`, and
/// the failure latches: later calls fail with the first failure.
#[test]
fn package_undeclared_provider_fails_the_package() {
    let (_dir, package) = load(json!({}));
    let outcome = register(
        &package,
        "c1",
        "fiber.provider",
        json!(["p", {"models": {"timeout": 1000, "run": true}}]),
    );
    let RegisterOutcome::FailedPackage { code, message } = outcome else {
        panic!("an undeclared provider fails the package, not {outcome:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "The extension `fiber.test/proc` registers the provider `p`, which its manifest does not declare under `providers`."
    );
    let latched = register(&package, "c2", "fiber.tool", json!(["t", tool_spec()]));
    let RegisterOutcome::FailedPackage { message: again, .. } = latched else {
        panic!("a failed package stays failed, not {latched:?}");
    };
    assert_eq!(again, message);
}

/// A `commands` key makes the package lazy-only: declared commands
/// register, and anything else fails the package.
#[test]
fn package_lazy_commands_restrict_registrations() {
    let (_dir, package) = load(json!({"commands": {"cmd": {"description": "Runs it."}}}));
    let outcome = register(
        &package,
        "c1",
        "fiber.command",
        json!(["cmd", {"timeout": 5000, "run": true, "description": "Runs it."}]),
    );
    assert!(matches!(outcome, RegisterOutcome::Accepted), "{outcome:?}");
    let other = register(
        &package,
        "c2",
        "fiber.command",
        json!(["nope", {"timeout": 5000, "run": true, "description": "Runs it."}]),
    );
    let RegisterOutcome::FailedPackage { code, message } = other else {
        panic!("an undeclared command fails the package, not {other:?}");
    };
    assert_eq!(code, contract::ErrorCode::ExtensionFailed);
    assert_eq!(
        message,
        "Command `nope` is not declared under `commands`; `fiber.test/proc` registers only the commands its manifest declares"
    );
}
