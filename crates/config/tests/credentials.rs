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
        credential_name: None,
        headers: Default::default(),
        models: Vec::new(),
    }
}

fn shared(name: &str, stored: &str, credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential,
        credential_name: Some(stored.into()),
        headers: Default::default(),
        models: Vec::new(),
    }
}

fn command(argv: &[&str]) -> Option<CredentialSource> {
    Some(CredentialSource::Command(
        argv.iter().map(|s| (*s).to_owned()).collect(),
    ))
}

/// The real `opencode` package's providers, with their shared credential
/// wiring as shipped.
fn opencode_providers() -> Vec<ProviderData> {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers/opencode");
    config::read_providers(&dir).unwrap()
}

fn named(providers: &[ProviderData], name: &str) -> ProviderData {
    providers.iter().find(|p| p.name == name).unwrap().clone()
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
fn no_key_anywhere_is_credential_missing_and_says_why() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let empty = setup.root().join("empty");
    setup.write(&empty, " \n");
    for (source, why) in [
        (None, "declares no other source".to_owned()),
        (
            Some(CredentialSource::Env("FIBER_TEST_SURELY_UNSET_7f3a".into())),
            "FIBER_TEST_SURELY_UNSET_7f3a is not set".to_owned(),
        ),
        (
            Some(CredentialSource::File(setup.root().join("absent"))),
            "absent does not exist".to_owned(),
        ),
        (
            Some(CredentialSource::File(empty.clone())),
            format!("{} is empty", empty.display()),
        ),
        (
            command(&["false"]),
            "`false` failed (exit status: 1)".to_owned(),
        ),
        (command(&["printf", "  \n"]), "printed no key".to_owned()),
        (
            command(&["/nonexistent/fiber-test-program"]),
            "`/nonexistent/fiber-test-program` could not be started".to_owned(),
        ),
        (command(&[]), "the configured command is empty".to_owned()),
    ] {
        let err = config.credential(&acme(source.clone())).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CredentialMissing, "{source:?}");
        let message = err.to_string();
        assert!(message.contains(&why), "{message}");
        assert!(message.contains("fiber login acme"), "{message}");
    }
}

#[test]
fn one_stored_credential_serves_both_opencode_providers() {
    let providers = opencode_providers();
    let go = named(&providers, "opencode-go");
    let zen = named(&providers, "opencode-zen");
    assert_eq!(go.credential_name.as_deref(), Some("opencode"));
    assert_eq!(zen.credential_name.as_deref(), Some("opencode"));
    let setup = Setup::new();
    store_secret(&setup.home(), "opencode", &Secret::new("shared\n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.credential(&go).unwrap().expose(), "shared");
    assert_eq!(config.credential(&zen).unwrap().expose(), "shared");
}

#[test]
fn a_failing_shared_credential_names_the_file_actually_read() {
    let setup = Setup::new();
    store_secret(&setup.home(), "opencode", &Secret::new(" \n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credential(&shared("opencode-go", "opencode", None))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(
        message.contains("credentials/opencode is empty"),
        "{message}"
    );
    assert!(
        message.contains("No credential for `opencode-go`"),
        "{message}"
    );
}

#[test]
fn a_provider_naming_no_shared_credential_reads_its_own_name() {
    let setup = Setup::new();
    store_secret(&setup.home(), "shared", &Secret::new("shared\n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config.credential(&acme(None)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(message.contains("credentials/acme"), "{message}");
}

#[test]
fn a_usable_shared_credential_wins_over_configured_and_declared_sources() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"mine": {"credential": {"command": ["printf", "configured"]}}}}"#,
    );
    store_secret(&setup.home(), "shared", &Secret::new("stored\n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let key = config
        .credential(&shared("mine", "shared", command(&["printf", "declared"])))
        .unwrap();
    assert_eq!(key.expose(), "stored");
}

#[test]
fn an_empty_shared_credential_fails_without_falling_back() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"mine": {"credential": {"command": ["printf", "configured"]}}}}"#,
    );
    store_secret(&setup.home(), "shared", &Secret::new(" \n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credential(&shared("mine", "shared", command(&["printf", "declared"])))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(message.contains("credentials/shared is empty"), "{message}");
}

#[test]
fn shared_providers_keep_independent_provider_keyed_overrides() {
    let providers = opencode_providers();
    let go = named(&providers, "opencode-go");
    let zen = named(&providers, "opencode-zen");
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {
            "opencode-go": {"credential": {"command": ["printf", "go-global"]}},
            "opencode-zen": {"credential": {"command": ["printf", "zen-global"]}}
        }}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {
            "opencode-go": {"credential": {"command": ["printf", "go-project"]}}
        }}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.credential(&go).unwrap().expose(), "go-project");
    assert_eq!(config.credential(&zen).unwrap().expose(), "zen-global");
}

#[test]
fn shared_providers_fall_back_to_their_declared_sources() {
    // The real pair declares an environment variable, which a test cannot
    // set without `unsafe` on this toolchain; the swapped-in commands
    // exercise the same fall-through from the same names and shared wiring.
    let providers = opencode_providers();
    let mut go = named(&providers, "opencode-go");
    let mut zen = named(&providers, "opencode-zen");
    go.credential = command(&["printf", "go-declared"]);
    zen.credential = command(&["printf", "zen-declared"]);
    assert_eq!(go.credential_name.as_deref(), Some("opencode"));
    assert_eq!(zen.credential_name.as_deref(), Some("opencode"));
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.credential(&go).unwrap().expose(), "go-declared");
    assert_eq!(config.credential(&zen).unwrap().expose(), "zen-declared");
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
