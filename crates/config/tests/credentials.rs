//! `docs/model-routing.md`, "Credentials": a provider's key comes from the
//! stored credential first, then the source the person configured, then the
//! one its data declares. A stored credential that fails does not fall back.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use std::os::unix::fs::symlink;

use common::Setup;
use config::{CredentialSource, ProviderData, Secret, store_secret};
use contract::ErrorCode;

fn acme(credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: "acme".into(),
        credential,
        headers: Default::default(),
        models: Vec::new(),
    }
}

fn command(argv: &[&str]) -> Option<CredentialSource> {
    Some(CredentialSource::Command(
        argv.iter().map(|s| (*s).to_owned()).collect(),
    ))
}

#[test]
fn the_stored_credential_comes_first() {
    let setup = Setup::new();
    store_secret(&setup.home(), "acme", &Secret::new("stored\n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let key = config
        .credential(&acme(command(&["printf", "from-command"])))
        .unwrap();
    assert_eq!(key.expose(), "stored");
}

#[test]
fn a_failing_stored_credential_does_not_fall_back() {
    let setup = Setup::new();
    store_secret(&setup.home(), "acme", &Secret::new(" \n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credential(&acme(command(&["printf", "from-command"])))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    assert!(
        err.to_string().contains("credentials/acme is empty"),
        "{err}"
    );

    let setup = Setup::new();
    std::fs::create_dir_all(setup.home().join("credentials")).unwrap();
    symlink("/etc/hosts", setup.home().join("credentials/acme")).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credential(&acme(command(&["printf", "from-command"])))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn a_declared_environment_variable_file_or_command_is_read() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let home = std::env::var("HOME").unwrap();
    let key = config
        .credential(&acme(Some(CredentialSource::Env("HOME".into()))))
        .unwrap();
    assert_eq!(key.expose(), home.trim());

    let file = setup.root().join("key");
    setup.write(&file, "from-file\n");
    let key = config
        .credential(&acme(Some(CredentialSource::File(file))))
        .unwrap();
    assert_eq!(key.expose(), "from-file");

    let key = config
        .credential(&acme(command(&["printf", "from-command\n"])))
        .unwrap();
    assert_eq!(key.expose(), "from-command");
}

#[test]
fn the_configured_source_replaces_the_declared_one() {
    let setup = Setup::new();
    let file = setup.root().join("key");
    setup.write(&file, "configured");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credential": {{"file": "{}"}}}}}}}}"#,
            file.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    let key = config
        .credential(&acme(command(&["printf", "declared"])))
        .unwrap();
    assert_eq!(key.expose(), "configured");
}

#[test]
fn a_repository_cannot_choose_the_source() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"providers": {"acme": {"credential": {"command": ["printf", "repo"]}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let key = config
        .credential(&acme(command(&["printf", "declared"])))
        .unwrap();
    assert_eq!(key.expose(), "declared");
}

#[test]
fn no_key_anywhere_is_credential_missing() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    for source in [
        None,
        Some(CredentialSource::Env("FIBER_TEST_SURELY_UNSET_7f3a".into())),
        Some(CredentialSource::File(setup.root().join("absent"))),
        command(&["false"]),
        command(&["printf", "  \n"]),
        command(&["/nonexistent/fiber-test-program"]),
    ] {
        let err = config.credential(&acme(source.clone())).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CredentialMissing, "{source:?}");
        assert!(err.to_string().contains("fiber login acme"), "{err}");
    }
}

#[test]
fn a_key_file_that_cannot_be_read_is_io_failed() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credential(&acme(Some(CredentialSource::File(
            setup.root().to_path_buf(),
        ))))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed);
}
