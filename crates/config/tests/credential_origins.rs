//! `credential_origins` and `key_presence`: listing a provider's labels and
//! their origins reads no key, and only the selected label's key is ever
//! read, only to test whether it is blank.

#![allow(clippy::unwrap_used, reason = "test setup; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::Setup;
use config::{
    Config, ConfigError, CredentialSource, Origin, Presence, ProviderData, Secret, Sources,
    store_credential,
};
use contract::ErrorCode;
use fakes::Deadline;

fn fake(credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: "fake".into(),
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        credential_from: config::CredentialFrom::Stored {
            credential,
            login: None,
        },
    }
}

fn shared(name: &str, stored: &str, credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential_name: Some(stored.into()),
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        credential_from: config::CredentialFrom::Stored {
            credential,
            login: None,
        },
    }
}

fn command(argv: &[&str]) -> CredentialSource {
    CredentialSource::Command {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        timeout: Duration::from_secs(30),
    }
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn mkfifo(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fakes::fifo(path);
}

fn origins(config: &Config, provider: &ProviderData) -> Vec<(String, Result<Origin, ConfigError>)> {
    let credentials = config.credentials().clone();
    let provider = provider.clone();
    fakes::within(
        "credential_origins",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || credentials.credential_origins(&provider),
    )
}

/// The origins that all resolved, failing the test naming any refusal.
fn ok(found: Vec<(String, Result<Origin, ConfigError>)>) -> Vec<(String, Origin)> {
    found
        .into_iter()
        .map(|(label, origin)| {
            let origin = origin.unwrap_or_else(|error| panic!("{label}: {error}"));
            (label, origin)
        })
        .collect()
}

fn presence(
    config: &Config,
    provider: &ProviderData,
    label: &str,
    values: &BTreeMap<String, String>,
) -> Result<Presence, ConfigError> {
    let credentials = config.credentials().clone();
    let provider = provider.clone();
    let label = label.to_owned();
    let values = values.clone();
    fakes::within(
        "key_presence",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || credentials.key_presence(&provider, &label, &|name| values.get(name).cloned()),
    )
}

fn put(setup: &Setup, name: &str, label: &str, value: &str) {
    store_credential(&setup.home(), name, label, &Secret::new(value.into())).unwrap();
}

#[test]
fn no_labels_gives_no_origins() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(origins(&config, &fake(None)).is_empty());
}

#[test]
fn a_stored_label_is_a_stored_origin() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "key");
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        ok(origins(&config, &fake(None))),
        [("work".to_owned(), Origin::Stored)]
    );
}

#[test]
fn configured_sources_keep_their_kind() {
    let setup = Setup::new();
    let file = setup.root().join("key");
    setup.write(&file, "from-file");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"fake": {{"credentials": {{"ci": {{"env": "CI_KEY"}}, "file-label": {{"file": "{}"}}, "run": {{"command": ["op", "read", "--extra"]}}}}}}}}}}"#,
            file.display(),
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        ok(origins(&config, &fake(None))),
        [
            ("ci".to_owned(), Origin::Env("CI_KEY".into())),
            ("file-label".to_owned(), Origin::File(PathBuf::from(&file))),
            ("run".to_owned(), Origin::Command("op".into())),
        ]
    );
}

#[test]
fn the_providers_own_source_is_the_default_origin() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let file = setup.root().join("declared");
    assert_eq!(
        ok(origins(
            &config,
            &fake(Some(CredentialSource::File(file.clone())))
        )),
        [("default".to_owned(), Origin::File(file))]
    );
    assert_eq!(
        ok(origins(
            &config,
            &fake(Some(CredentialSource::Env("DECLARED".into())))
        )),
        [("default".to_owned(), Origin::Env("DECLARED".into()))]
    );
}

#[test]
fn a_stored_label_wins_over_a_configured_one() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "stored");
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"work": {"env": "WORK_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        ok(origins(&config, &fake(None))),
        [("work".to_owned(), Origin::Stored)]
    );
}

#[test]
fn labels_come_out_sorted_once_each() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "w");
    put(&setup, "fake", "home", "h");
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"home": {"env": "HOME_KEY"}, "ci": {"env": "CI_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let names: Vec<String> = origins(&config, &fake(Some(command(&["printf", "d"]))))
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(names, ["ci", "default", "home", "work"]);
}

#[test]
fn two_providers_sharing_a_stored_name_both_see_it_stored() {
    let setup = Setup::new();
    put(&setup, "shared", "a", "key");
    let config = setup.load(&[]).unwrap();
    for name in ["one", "two"] {
        assert_eq!(
            ok(origins(&config, &shared(name, "shared", None))),
            [("a".to_owned(), Origin::Stored)],
            "{name}"
        );
    }
}

#[test]
fn a_fifo_or_link_in_the_stored_directory_is_no_label() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "key");
    let dir = setup.home().join("credentials/fake");
    mkfifo(&dir.join("pipe"));
    symlink(dir.join("work"), dir.join("alias")).unwrap();
    let (home, workspace) = (setup.home(), setup.workspace());
    let found = fakes::within(
        "credential_origins",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let config = Config::load(Sources {
                home,
                workspace,
                project: common::key(),
                overrides: Vec::new(),
            })
            .unwrap();
            config.credentials().credential_origins(&fake(None))
        },
    );
    assert_eq!(ok(found), [("work".to_owned(), Origin::Stored)]);
}

#[test]
fn a_configured_label_whose_stored_path_is_a_fifo_is_not_plain() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "key");
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"ci": {"env": "CI_KEY"}}}}}"#,
    );
    mkfifo(&setup.home().join("credentials/fake/ci"));
    let found = origins(&setup.load(&[]).unwrap(), &fake(None));
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].0, "ci");
    assert!(
        matches!(found[0].1, Err(ConfigError::NotPlain { .. })),
        "{:?}",
        found[0].1
    );
    assert_eq!(found[1].0, "work");
    assert!(matches!(found[1].1, Ok(Origin::Stored)), "{:?}", found[1].1);
}

#[test]
fn credential_with_never_reads_another_labels_fifo() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"fake": {{"credentials": {{"work": {{"env": "FIBER_TEST_SURELY_UNSET_9d1c"}}, "bad": {{"file": "{}"}}}}}}}}}}"#,
            setup.home().join("credentials/fake/bad").display(),
        ),
    );
    mkfifo(&setup.home().join("credentials/fake/bad"));
    let (home, workspace) = (setup.home(), setup.workspace());
    let missing = fakes::within(
        "credential_with",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let config = Config::load(Sources {
                home,
                workspace,
                project: common::key(),
                overrides: Vec::new(),
            })
            .unwrap();
            config
                .credentials()
                .credential_with(&fake(None), "work", &fakes::children::grouped_output)
                .unwrap_err()
        },
    );
    assert_eq!(missing.code(), ErrorCode::CredentialMissing);
    let message = missing.to_string();
    assert!(
        message.contains(
            "nothing is stored in credentials/fake/work, and the environment variable FIBER_TEST_SURELY_UNSET_9d1c is not set"
        ),
        "{message}"
    );
    assert!(!message.contains("labels for"), "{message}");
}

#[test]
fn credential_with_lists_labels_by_name_even_past_a_fifo() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"bad": {"env": "FIBER_TEST_SURELY_UNSET_9d1c"}}}}}"#,
    );
    mkfifo(&setup.home().join("credentials/fake/bad"));
    let (home, workspace) = (setup.home(), setup.workspace());
    let missing = fakes::within(
        "credential_with",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || {
            let config = Config::load(Sources {
                home,
                workspace,
                project: common::key(),
                overrides: Vec::new(),
            })
            .unwrap();
            config
                .credentials()
                .credential_with(&fake(None), "work", &fakes::children::grouped_output)
                .unwrap_err()
        },
    );
    assert_eq!(missing.code(), ErrorCode::CredentialMissing);
    let message = missing.to_string();
    assert!(
        message.contains("The labels for `fake` are: bad"),
        "{message}"
    );
}

#[test]
fn an_invalid_label_name_is_a_secret_name_error() {
    let setup = Setup::new();
    put(&setup, "fake", "work", "key");
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"work.lock": {"env": "LOCK_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let found = origins(&config, &fake(None));
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].0, "work");
    assert!(matches!(found[0].1, Ok(Origin::Stored)), "{:?}", found[0].1);
    assert_eq!(found[1].0, "work.lock");
    assert!(
        matches!(found[1].1, Err(ConfigError::SecretName { .. })),
        "{:?}",
        found[1].1
    );
    let error = presence(&config, &fake(None), "work.lock", &vars(&[])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert!(matches!(error, ConfigError::SecretName { .. }));
}

#[test]
fn presence_reads_only_what_it_must() {
    let setup = Setup::new();
    put(&setup, "fake", "full", "key\n");
    setup.write(&setup.home().join("credentials/fake/blank"), "  \n");
    let config = setup.load(&[]).unwrap();
    let empty = vars(&[]);
    assert!(matches!(
        presence(&config, &fake(None), "full", &empty),
        Ok(Presence::Present)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "blank", &empty),
        Ok(Presence::Empty)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "absent", &empty),
        Ok(Presence::Absent)
    ));
}

#[test]
fn presence_checks_an_environment_source_through_the_reader() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"fake": {"credentials": {"ci": {"env": "CI_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(matches!(
        presence(&config, &fake(None), "ci", &vars(&[("CI_KEY", "key")])),
        Ok(Presence::Present)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "ci", &vars(&[("CI_KEY", "  ")])),
        Ok(Presence::Absent)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "ci", &vars(&[])),
        Ok(Presence::Absent)
    ));
}

#[test]
fn presence_reads_a_configured_file_but_never_a_command() {
    let setup = Setup::new();
    let full = setup.root().join("full");
    let blank = setup.root().join("blank");
    setup.write(&full, "from-file\n");
    setup.write(&blank, " \n");
    let marker = setup.root().join("command-ran");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"fake": {{"credentials": {{"full": {{"file": "{}"}}, "blank": {{"file": "{}"}}, "missing": {{"file": "{}"}}, "run": {{"command": ["sh", "-c", "touch {}"]}}}}}}}}}}"#,
            full.display(),
            blank.display(),
            setup.root().join("absent").display(),
            marker.display(),
        ),
    );
    let config = setup.load(&[]).unwrap();
    let empty = vars(&[]);
    assert!(matches!(
        presence(&config, &fake(None), "full", &empty),
        Ok(Presence::Present)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "blank", &empty),
        Ok(Presence::Absent)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "missing", &empty),
        Ok(Presence::Absent)
    ));
    assert!(matches!(
        presence(&config, &fake(None), "run", &empty),
        Ok(Presence::Unchecked)
    ));
    assert!(!marker.exists(), "a command source ran during key_presence");
}

#[test]
fn a_directory_at_a_configured_file_is_an_io_error() {
    let setup = Setup::new();
    let dir = setup.root().join("keys");
    fs::create_dir(&dir).unwrap();
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"fake": {{"credentials": {{"dir": {{"file": "{}"}}}}}}}}}}"#,
            dir.display(),
        ),
    );
    let config = setup.load(&[]).unwrap();
    let error = presence(&config, &fake(None), "dir", &vars(&[])).unwrap_err();
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert!(
        matches!(error, ConfigError::Io { .. }),
        "a directory keeps the session's non-NotFound Io arm: {error}"
    );
    if let ConfigError::Io { source, .. } = error {
        assert_eq!(source.kind(), std::io::ErrorKind::IsADirectory);
    }
}

#[test]
fn a_fifo_at_a_configured_file_is_absent_without_a_wait() {
    let setup = Setup::new();
    let pipe = setup.root().join("pipe");
    mkfifo(&pipe);
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"fake": {{"credentials": {{"pipe": {{"file": "{}"}}}}}}}}}}"#,
            pipe.display(),
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(matches!(
        presence(&config, &fake(None), "pipe", &vars(&[])),
        Ok(Presence::Absent)
    ));
}

#[test]
fn a_stored_link_or_fifo_is_not_plain() {
    let setup = Setup::new();
    let target = setup.root().join("target");
    setup.write(&target, "key");
    fs::create_dir_all(setup.home().join("credentials/linked")).unwrap();
    symlink(&target, setup.home().join("credentials/linked/link")).unwrap();
    fs::create_dir_all(setup.home().join("credentials/piped")).unwrap();
    mkfifo(&setup.home().join("credentials/piped/pipe"));
    let config = setup.load(&[]).unwrap();
    for (provider, label) in [
        (
            ProviderData {
                credential_name: Some("linked".into()),
                ..fake(None)
            },
            "link",
        ),
        (
            ProviderData {
                credential_name: Some("piped".into()),
                ..fake(None)
            },
            "pipe",
        ),
    ] {
        let error = presence(&config, &provider, label, &vars(&[])).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{label}");
        assert!(matches!(error, ConfigError::NotPlain { .. }), "{label}");
    }
}

#[test]
fn a_missing_stored_file_still_reaches_the_configured_source() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"gone": {"credentials": {"default": {"env": "GONE_KEY"}}}}}"#,
    );
    let gone = shared("gone", "gone", None);
    let config = setup.load(&[]).unwrap();
    // Nothing stored and a configured source besides: the source answers.
    assert!(matches!(
        presence(&config, &gone, "default", &vars(&[("GONE_KEY", "key")])),
        Ok(Presence::Present)
    ));
}
