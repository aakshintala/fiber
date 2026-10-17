//! Whether a provider can be checked without running extension code:
//! data-only packages register, and every code, disabled, damaged or
//! incompatible case does not.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::{Setup, install, manifest, provider, write};
use extensions::Providers;
use serde_json::json;

/// Installs the source package `short` with this manifest and one provider
/// data file, and returns nothing.
fn installed(setup: &Setup, short: &str, listed: serde_json::Value) {
    let source = setup.source(short, &listed, &[provider("fake", &["m"])]);
    install(&setup.home(), &source, "0.1.0").unwrap();
}

/// The loaded providers over the test home.
fn loaded(setup: &Setup) -> Providers {
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    providers
}

#[test]
fn a_data_only_package_registers_its_provider() {
    let setup = Setup::new();
    installed(&setup, "acme", manifest("acme"));
    let providers = loaded(&setup);
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(providers.could_register("fake"));
}

#[test]
fn a_package_with_an_entry_script_does_not_register_without_running_it() {
    let setup = Setup::new();
    let listed = manifest("fiber.test/coded");
    let source = setup.source("coded", &listed, &[provider("fake", &["m"])]);
    // Syntactically invalid Lua: a load that ran code would fail it, so a
    // successful load proves the script never ran.
    write(&source.join("init.lua"), "this is not lua (((");
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(!providers.could_register("fake"));
}

#[test]
fn a_provider_script_does_not_register_without_running_it() {
    let setup = Setup::new();
    let source = setup.source("acme", &manifest("acme"), &[provider("fake", &["m"])]);
    write(
        &source.join("providers/fake.lua"),
        "-- a provider script, never run by the check",
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(!providers.could_register("fake"));
}

#[test]
fn a_process_only_package_does_not_register_without_running_it() {
    let setup = Setup::new();
    let mut listed = manifest("acme");
    listed["process"] = json!({"program": "run"});
    installed(&setup, "acme", listed);
    let providers = loaded(&setup);
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(!providers.could_register("fake"));
}

#[test]
fn a_disabled_package_registers_nothing() {
    let setup = Setup::new();
    installed(&setup, "acme", manifest("acme"));
    let configuration = common::config(&setup, &["extensions.acme.enabled=false"]);
    let (providers, notices) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(!providers.could_register("fake"));
    // The package itself is fine: unfiltered it registers.
    assert!(loaded(&setup).could_register("fake"));
}

#[test]
fn a_damaged_package_registers_nothing() {
    let setup = Setup::new();
    installed(&setup, "acme", manifest("acme"));
    let dir = setup.home().join("extensions/acme");
    // The install record is unreadable, so the directory is damaged
    // rather than installed.
    write(&dir.join(".fiber.json"), "not json");
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(!notices.is_empty());
    assert!(!providers.could_register("fake"));
}

#[test]
fn an_incompatible_package_registers_nothing() {
    let setup = Setup::new();
    // Hand-placed: installing already refuses another API.
    let mut listed = manifest("acme");
    listed["api"] = json!(999);
    let dir = setup.home().join("extensions/acme");
    write(&dir.join("extension.json"), &listed.to_string());
    common::write_record(&dir);
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(!notices.is_empty());
    assert!(!providers.could_register("fake"));
}

#[test]
fn an_unknown_provider_registers_nothing() {
    let setup = Setup::new();
    installed(&setup, "acme", manifest("acme"));
    assert!(!loaded(&setup).could_register("missing"));
}
