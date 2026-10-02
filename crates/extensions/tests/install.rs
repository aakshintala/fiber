//! `docs/extensions.md`, "Installing" and "The extension API version":
//! `fiber install <local path>` copies an extension into
//! `extensions/<name>/`, and refuses one this Fiber cannot run.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use std::fs;

use common::{Setup, install, manifest, provider, write};
use contract::ErrorCode;
use extensions::Error;
use serde_json::json;

#[test]
fn installing_copies_the_directory_into_its_slugged_name() {
    let setup = Setup::new();
    let source = setup.source(
        "local",
        &manifest("github.com/acme/fiber-acme"),
        &[provider("acme", &["m1"])],
    );
    write(&source.join("lib/helper.lua"), "return 1");
    let name = install(&setup.home(), &source, "0.1.0").unwrap();
    assert_eq!(name, "github.com/acme/fiber-acme");
    let target = setup.home().join("extensions/github.com-acme-fiber-acme");
    assert_eq!(
        fs::read_to_string(target.join("lib/helper.lua")).unwrap(),
        "return 1"
    );
    assert!(target.join("providers/acme.json").is_file());
    let left: Vec<_> = fs::read_dir(setup.home().join("extensions"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["github.com-acme-fiber-acme"]);
}

#[test]
fn installing_again_replaces_the_old_copy() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[provider("acme", &["m1"])]);
    write(&source.join("old.txt"), "old");
    install(&setup.home(), &source, "0.1.0").unwrap();
    fs::remove_file(source.join("old.txt")).unwrap();
    install(&setup.home(), &source, "0.1.0").unwrap();
    let target = setup.home().join("extensions/acme");
    assert!(!target.join("old.txt").exists());
    assert!(target.join("extension.json").is_file());
    assert_eq!(
        fs::read_dir(setup.home().join("extensions"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_symbolic_link_is_copied_as_a_link() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    std::os::unix::fs::symlink("extension.json", source.join("link")).unwrap();
    install(&setup.home(), &source, "0.1.0").unwrap();
    let link = setup.home().join("extensions/acme/link");
    assert_eq!(
        fs::read_link(link).unwrap().to_str(),
        Some("extension.json")
    );
}

#[test]
fn an_extension_needing_a_newer_fiber_is_refused_and_nothing_is_installed() {
    let setup = Setup::new();
    let mut needs = manifest("acme");
    needs["fiber"] = json!("0.3.0");
    let source = setup.source("local", &needs, &[]);
    for running in ["0.2.9", "0.2.10", "v0.2.99"] {
        let err = install(&setup.home(), &source, running).unwrap_err();
        assert!(matches!(err, Error::NeedsNewerFiber { .. }), "{err:?}");
        assert!(err.to_string().contains("0.3.0"), "{err}");
    }
    assert!(!setup.home().join("extensions/acme").exists());
    for running in ["0.3.0", "0.3.1", "0.10.0", "1.0.0"] {
        install(&setup.home(), &source, running).unwrap();
    }
}

#[test]
fn an_extension_for_another_api_major_is_refused() {
    let setup = Setup::new();
    for api in [0, 2] {
        let mut other = manifest("acme");
        other["api"] = json!(api);
        let source = setup.source(&format!("api{api}"), &other, &[]);
        let err = install(&setup.home(), &source, "9.0.0").unwrap_err();
        assert!(matches!(err, Error::ApiVersion { .. }), "{err:?}");
        assert!(err.to_string().contains(&format!("API {api}")), "{err}");
    }
    assert!(!setup.home().join("extensions/acme").exists());
}

#[test]
fn a_version_that_is_not_three_numbers_is_invalid() {
    let setup = Setup::new();
    for bad in ["0.3", "0.3.0.1", "x.1.0", ""] {
        let mut m = manifest("acme");
        m["fiber"] = json!(bad);
        let source = setup.source("local", &m, &[]);
        let err = install(&setup.home(), &source, "0.1.0").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{bad}: {err:?}");
    }
}

#[test]
fn a_name_that_does_not_slug_to_one_directory_is_refused() {
    let setup = Setup::new();
    for bad in ["", "..", ".hidden", "a\0b"] {
        let source = setup.source("local", &manifest(bad), &[]);
        let err = install(&setup.home(), &source, "0.1.0").unwrap_err();
        assert!(matches!(err, Error::BadName { .. }), "{bad:?}: {err:?}");
    }
    assert!(
        !setup.home().join("extensions").exists()
            || fs::read_dir(setup.home().join("extensions"))
                .unwrap()
                .count()
                == 0
    );
}

#[test]
fn provider_data_that_does_not_read_installs_nothing() {
    let setup = Setup::new();
    let source = setup.source("local", &manifest("acme"), &[]);
    write(
        &source.join("providers/acme.json"),
        r#"{"name": "acme", "models": [{"id": 1}]}"#,
    );
    let err = install(&setup.home(), &source, "0.1.0").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid, "{err:?}");
    assert_eq!(
        fs::read_dir(setup.home().join("extensions"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_directory_without_a_manifest_is_refused() {
    let setup = Setup::new();
    let err = install(&setup.home(), &setup.workspace(), "0.1.0").unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed, "{err:?}");
}

#[test]
fn a_source_holding_fiber_home_or_inside_it_is_refused() {
    let setup = Setup::new();
    let outer = setup.source("outer", &manifest("acme"), &[]);
    let home = outer.join("home");
    let err = install(&home, &outer, "0.1.0").unwrap_err();
    assert!(matches!(err, Error::Overlaps { .. }), "{err:?}");
    assert!(!home.join("extensions/acme").exists());

    let installed = setup.source("installed", &manifest("acme"), &[]);
    install(&setup.home(), &installed, "0.1.0").unwrap();
    let inner = setup.home().join("extensions/acme");
    let err = install(&setup.home(), &inner, "0.1.0").unwrap_err();
    assert!(matches!(err, Error::Overlaps { .. }), "{err:?}");
    assert!(inner.join("extension.json").is_file());
}
