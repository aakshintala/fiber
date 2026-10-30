//! Whether a provider can be checked without running extension code:
//! data-only packages register, disabled or not, and every code, damaged
//! or incompatible case does not. A disabled entry script alone registers
//! nothing.

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
    listed["process"] = json!({"program": "run", "exit_timeout_ms": 0});
    installed(&setup, "acme", listed);
    let providers = loaded(&setup);
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(!providers.could_register("fake"));
}

/// Only the provider part ignores `enabled`: a disabled data-only
/// package's file still registers, so whether its models show depends on
/// its credentials.
#[test]
fn a_disabled_data_only_package_still_registers() {
    let setup = Setup::new();
    installed(&setup, "acme", manifest("acme"));
    let configuration = common::config(&setup, &["extensions.acme.enabled=false"]);
    let (providers, notices) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(providers.names().collect::<Vec<_>>(), ["fake"]);
    assert!(providers.could_register("fake"));
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

/// Installs the source package `short` with this manifest, an `init.lua`
/// and no provider data file, and returns nothing.
fn installed_code_only(setup: &Setup, short: &str, mut listed: serde_json::Value) {
    if let Some(into) = listed.as_object_mut() {
        into.insert(
            "providers".to_owned(),
            json!({"fake": ["http://127.0.0.1:1/v1"]}),
        );
    }
    let source = setup.source(short, &listed, &[]);
    write(&source.join("init.lua"), "this is not lua (((");
    install(&setup.home(), &source, "0.1.0").unwrap();
}

/// Hand-places the package `short` with this manifest, an `init.lua` and
/// no provider data file: installing already refuses another API.
fn placed_code_only(setup: &Setup, short: &str, mut listed: serde_json::Value) {
    if let Some(into) = listed.as_object_mut() {
        into.insert(
            "providers".to_owned(),
            json!({"fake": ["http://127.0.0.1:1/v1"]}),
        );
    }
    let dir = setup.home().join("extensions").join(short);
    write(&dir.join("extension.json"), &listed.to_string());
    write(&dir.join("init.lua"), "this is not lua (((");
    common::write_record(&dir);
}

#[test]
fn an_enabled_init_may_register_its_declared_provider() {
    let setup = Setup::new();
    installed_code_only(&setup, "acme", manifest("acme"));
    let providers = loaded(&setup);
    assert!(providers.names().next().is_none());
    assert!(providers.could_register("fake"));
}

#[test]
fn a_disabled_init_registers_nothing() {
    let setup = Setup::new();
    installed_code_only(&setup, "acme", manifest("acme"));
    let configuration = common::config(&setup, &["extensions.acme.enabled=false"]);
    let (providers, notices) = Providers::load_enabled(&setup.home(), &configuration).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    assert!(!providers.could_register("fake"));
}

#[test]
fn an_incompatible_init_registers_nothing() {
    let setup = Setup::new();
    let mut listed = manifest("acme");
    if let Some(into) = listed.as_object_mut() {
        into.insert("api".to_owned(), json!(999));
    }
    placed_code_only(&setup, "acme", listed);
    let (providers, notices) = Providers::load(&setup.home()).unwrap();
    assert!(!notices.is_empty());
    assert!(!providers.could_register("fake"));
}

#[test]
fn a_provider_script_may_register_its_named_provider() {
    let setup = Setup::new();
    let mut listed = manifest("acme");
    listed["providers"] =
        json!({"fake": ["http://127.0.0.1:1/v1"], "other": ["http://127.0.0.1:1/v1"]});
    let source = setup.source("acme", &listed, &[]);
    write(
        &source.join("providers/fake.lua"),
        "-- a provider script, never run by the check",
    );
    install(&setup.home(), &source, "0.1.0").unwrap();
    let providers = loaded(&setup);
    assert!(providers.could_register("fake"));
    assert!(!providers.could_register("other"));
}

#[test]
fn a_process_only_package_supplies_no_lua_registration() {
    let setup = Setup::new();
    let mut listed = manifest("acme");
    if let Some(into) = listed.as_object_mut() {
        into.insert(
            "process".to_owned(),
            json!({"program": "run", "exit_timeout_ms": 0}),
        );
        into.insert(
            "providers".to_owned(),
            json!({"fake": ["http://127.0.0.1:1/v1"]}),
        );
    }
    // No `init.lua`: process-only, with no Lua that could register.
    let source = setup.source("acme", &listed, &[]);
    install(&setup.home(), &source, "0.1.0").unwrap();
    let providers = loaded(&setup);
    assert!(providers.names().next().is_none());
    assert!(!providers.could_register("fake"));
}
