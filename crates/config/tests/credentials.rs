//! `docs/model-routing.md`, "Credentials": a provider's key comes from the
//! stored credential first, then the source the person configured, then the
//! one its data declares. A stored credential that fails does not fall back.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::os::unix::fs::symlink;

use common::Setup;
use config::{Config, ConfigError, CredentialSource, ProviderData, Secret, store_credential};
use contract::ErrorCode;
use fakes::Deadline;

fn acme(credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: "acme".into(),
        credential,
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        login: None,
    }
}

fn shared(name: &str, stored: &str, credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential,
        credential_name: Some(stored.into()),
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        login: None,
    }
}

fn default_key(config: &Config, provider: &ProviderData) -> Result<Secret, ConfigError> {
    config.credentials().credential(provider, "default")
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
    store_credential(
        &setup.home(),
        "acme",
        "default",
        &Secret::new("stored\n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    let key = default_key(&config, &acme(command(&["printf", "from-command"]))).unwrap();
    assert_eq!(key.expose(), "stored");
}

#[test]
fn a_failing_stored_credential_does_not_fall_back() {
    let setup = Setup::new();
    store_credential(&setup.home(), "acme", "default", &Secret::new(" \n".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = default_key(&config, &acme(command(&["printf", "from-command"]))).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    assert!(
        err.to_string()
            .contains("credentials/acme/default is empty"),
        "{err}"
    );

    let setup = Setup::new();
    std::fs::create_dir_all(setup.home().join("credentials/acme")).unwrap();
    symlink("/etc/hosts", setup.home().join("credentials/acme/default")).unwrap();
    let config = setup.load(&[]).unwrap();
    let err = default_key(&config, &acme(command(&["printf", "from-command"]))).unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn a_declared_environment_variable_file_or_command_is_read() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    assert_eq!(manifest, env!("CARGO_MANIFEST_DIR"));
    let key = default_key(
        &config,
        &acme(Some(CredentialSource::Env("CARGO_MANIFEST_DIR".into()))),
    )
    .unwrap();
    assert_eq!(key.expose(), manifest);

    let file = setup.root().join("key");
    setup.write(&file, "from-file\n");
    let key = default_key(&config, &acme(Some(CredentialSource::File(file)))).unwrap();
    assert_eq!(key.expose(), "from-file");

    let key = default_key(&config, &acme(command(&["printf", "from-command\n"]))).unwrap();
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
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            file.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    let key = default_key(&config, &acme(command(&["printf", "declared"]))).unwrap();
    assert_eq!(key.expose(), "configured");
}

#[test]
fn a_repository_cannot_choose_the_source() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"providers": {"acme": {"credentials": {"default": {"command": ["printf", "repo"]}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let key = default_key(&config, &acme(command(&["printf", "declared"]))).unwrap();
    assert_eq!(key.expose(), "declared");
}

#[test]
fn no_key_anywhere_is_credential_missing_and_says_why() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let empty = setup.root().join("empty");
    setup.write(&empty, " \n");
    for (source, why) in [
        (None, "no source is configured for it".to_owned()),
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
        let err = default_key(&config, &acme(source.clone())).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CredentialMissing, "{source:?}");
        let message = err.to_string();
        assert!(message.contains(&why), "{message}");
        assert!(message.contains("fiber login acme"), "{message}");
    }
}

#[test]
fn a_failing_command_is_named_by_its_program_alone() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    for (argv, why) in [
        (
            &["false", "sk-in-argument"][..],
            "`false` failed (exit status: 1)",
        ),
        (
            &["printf", "%.0s", "sk-in-argument"],
            "`printf` printed no key",
        ),
        (
            &["/nonexistent/fiber-test-program", "sk-in-argument"],
            "`/nonexistent/fiber-test-program` could not be started",
        ),
    ] {
        let message = default_key(&config, &acme(command(argv)))
            .unwrap_err()
            .to_string();
        assert!(message.contains(why), "{message}");
        assert!(!message.contains("sk-in-argument"), "{message}");
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
    store_credential(
        &setup.home(),
        "opencode",
        "default",
        &Secret::new("shared\n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    assert_eq!(default_key(&config, &go).unwrap().expose(), "shared");
    assert_eq!(default_key(&config, &zen).unwrap().expose(), "shared");
}

#[test]
fn a_failing_shared_credential_names_the_file_actually_read() {
    let setup = Setup::new();
    store_credential(
        &setup.home(),
        "opencode",
        "default",
        &Secret::new(" \n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    let err = default_key(&config, &shared("opencode-go", "opencode", None)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    let message = err.to_string();
    assert!(
        message.contains("credentials/opencode/default is empty"),
        "{message}"
    );
    assert!(message.contains("opencode-go"), "{message}");
}

#[test]
fn a_provider_naming_no_shared_credential_reads_its_own_name() {
    let setup = Setup::new();
    store_credential(
        &setup.home(),
        "shared",
        "default",
        &Secret::new("shared\n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    let err = default_key(&config, &acme(None)).unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    let message = err.to_string();
    assert!(message.contains("credentials/acme"), "{message}");
}

#[test]
fn a_usable_shared_credential_wins_over_configured_and_declared_sources() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"mine": {"credentials": {"default": {"command": ["printf", "configured"]}}}}}"#,
    );
    store_credential(
        &setup.home(),
        "shared",
        "default",
        &Secret::new("stored\n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    let key = default_key(
        &config,
        &shared("mine", "shared", command(&["printf", "declared"])),
    )
    .unwrap();
    assert_eq!(key.expose(), "stored");
}

#[test]
fn an_empty_shared_credential_fails_without_falling_back() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"mine": {"credentials": {"default": {"command": ["printf", "configured"]}}}}}"#,
    );
    store_credential(
        &setup.home(),
        "shared",
        "default",
        &Secret::new(" \n".into()),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    let err = default_key(
        &config,
        &shared("mine", "shared", command(&["printf", "declared"])),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    let message = err.to_string();
    assert!(
        message.contains("credentials/shared/default is empty"),
        "{message}"
    );
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
            "opencode-go": {"credentials": {"default": {"command": ["printf", "go-global"]}}},
            "opencode-zen": {"credentials": {"default": {"command": ["printf", "zen-global"]}}}
        }}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {
            "opencode-go": {"credentials": {"default": {"command": ["printf", "go-project"]}}}
        }}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(default_key(&config, &go).unwrap().expose(), "go-project");
    assert_eq!(default_key(&config, &zen).unwrap().expose(), "zen-global");
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
    assert_eq!(default_key(&config, &go).unwrap().expose(), "go-declared");
    assert_eq!(default_key(&config, &zen).unwrap().expose(), "zen-declared");
}

#[test]
fn a_key_file_that_cannot_be_read_is_io_failed() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let err = default_key(
        &config,
        &acme(Some(CredentialSource::File(setup.root().to_path_buf()))),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed);
}

fn put(setup: &Setup, name: &str, label: &str, value: &str) {
    store_credential(&setup.home(), name, label, &Secret::new(value.into())).unwrap();
}

#[test]
fn the_label_the_configuration_names_picks_the_stored_credential() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "d");
    put(&setup, "acme", "work", "w\n");
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credential": "work"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let label = config.credentials().credential_label(&acme(None));
    assert_eq!(label, "work");
    assert_eq!(
        config
            .credentials()
            .credential(&acme(None), &label)
            .unwrap()
            .expose(),
        "w"
    );
    assert_eq!(
        config
            .credentials()
            .credential(&acme(None), "default")
            .unwrap()
            .expose(),
        "d"
    );
}

#[test]
fn an_unset_label_is_default_and_a_repository_cannot_set_it() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"providers": {"acme": {"credential": "evil", "credentials": {"evil": {"command": ["printf", "x"]}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.credentials().credential_label(&acme(None)),
        "default"
    );
    let err = config
        .credentials()
        .credential(&acme(None), "evil")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
}

#[test]
fn a_shared_credential_name_holds_the_labels() {
    let setup = Setup::new();
    put(&setup, "opencode", "work", "shared-work");
    let config = setup.load(&[]).unwrap();
    let go = shared("opencode-go", "opencode", None);
    assert_eq!(
        config
            .credentials()
            .credential(&go, "work")
            .unwrap()
            .expose(),
        "shared-work"
    );
    let err = config.credentials().credential(&go, "other").unwrap_err();
    assert!(
        err.to_string()
            .contains("credentials/opencode/other, and no source"),
        "{err}"
    );
    assert!(err.to_string().contains("are: work."), "{err}");
}

#[test]
fn a_configured_label_reads_an_environment_variable_file_or_command() {
    let setup = Setup::new();
    let file = setup.root().join("key");
    setup.write(&file, "from-file\n");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{
                "e": {{"env": "CARGO_MANIFEST_DIR"}},
                "f": {{"file": "{}"}},
                "c": {{"command": ["printf", "from-command"]}}}}}}}}}}"#,
            file.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    let read = |label| config.credentials().credential(&acme(None), label).unwrap();
    assert_eq!(read("e").expose(), env!("CARGO_MANIFEST_DIR"));
    assert_eq!(read("f").expose(), "from-file");
    assert_eq!(read("c").expose(), "from-command");
}

#[test]
fn the_providers_own_source_is_the_default_label_only() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let provider = acme(command(&["printf", "declared"]));
    assert_eq!(
        config
            .credentials()
            .credential(&provider, "default")
            .unwrap()
            .expose(),
        "declared"
    );
    let err = config
        .credentials()
        .credential(&provider, "work")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    assert!(err.to_string().contains("are: default."), "{err}");
}

#[test]
fn a_stored_label_owns_it_and_a_configured_source_is_not_tried() {
    let setup = Setup::new();
    put(&setup, "acme", "work", " \n");
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"work": {"command": ["printf", "configured"]}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let err = config
        .credentials()
        .credential(&acme(None), "work")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialFailed);
    assert!(
        err.to_string().contains("credentials/acme/work is empty"),
        "{err}"
    );
}

#[test]
fn a_label_that_names_nothing_lists_the_labels_there_are() {
    let setup = Setup::new();
    put(&setup, "acme", "work", "w");
    let dir = setup.home().join("credentials/acme");
    // Not labels: a lock file, a temporary file, a directory and a link.
    std::fs::write(dir.join("work.lock"), "").unwrap();
    std::fs::write(dir.join("work.1-2.tmp"), "").unwrap();
    std::fs::create_dir(dir.join("subdir")).unwrap();
    symlink("/etc/hosts", dir.join("linked")).unwrap();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"a-configured": {"env": "X"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let err = config
        .credentials()
        .credential(&acme(command(&["printf", "d"])), "personal")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::CredentialMissing);
    assert_eq!(
        err.to_string(),
        "No credential for `acme`: nothing is stored in credentials/acme/personal, and no source is configured for it. The labels for `acme` are: a-configured, default, work. Run `fiber login acme`."
    );
    let err = config
        .credentials()
        .credential(&acme(None), "personal")
        .unwrap_err();
    assert!(
        err.to_string().contains("are: a-configured, work."),
        "{err}"
    );
}

#[test]
fn a_label_named_in_configuration_with_no_source_is_the_same_error() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credential": "gone"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let label = config.credentials().credential_label(&acme(None));
    let err = config
        .credentials()
        .credential(&acme(None), &label)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "No credential for `acme`: nothing is stored in credentials/acme/gone, and no source is configured for it. The labels for `acme` are: none. Run `fiber login acme`."
    );
}

#[test]
fn a_bare_secret_named_like_the_provider_is_not_read_as_a_key() {
    let setup = Setup::new();
    std::fs::create_dir_all(setup.home().join("credentials")).unwrap();
    std::fs::write(setup.home().join("credentials/acme"), "bare").unwrap();
    let config = setup.load(&[]).unwrap();
    let err = config
        .credentials()
        .credential(&acme(command(&["printf", "declared"])), "default")
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::ConfigInvalid);
}

fn files(config: &Config, providers: &[ProviderData]) -> Vec<std::path::PathBuf> {
    let mut files = config.credentials().credential_files(providers);
    files.sort();
    files
}

#[test]
fn every_configured_file_source_is_a_credential_file() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {
            "acme": {"credentials": {
                "work": {"file": "/keys/acme-work"},
                "home": {"env": "ACME_KEY"},
                "ci": {"command": ["printf", "k"]},
                "blank": {"file": ""}
            }},
            "other": {"credentials": {"default": {"file": "relative/key"}}}
        }}"#,
    );
    setup.write(
        &setup.project(),
        r#"{"providers": {"acme": {"credentials": {"project": {"file": "/keys/acme-project"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    // Each label's file, the per-project layer's included, as written; an
    // `env` or `command` source and an empty path name no file. A malformed
    // source never reaches the list: loading rejects it (`Kind::Credential`).
    assert_eq!(
        files(&config, &[]),
        ["/keys/acme-project", "/keys/acme-work", "relative/key"].map(std::path::PathBuf::from)
    );
}

#[test]
fn a_providers_own_file_source_is_a_credential_file_once() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"file": "/keys/acme"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let own = acme(Some(CredentialSource::File("/keys/acme".into())));
    let other = shared(
        "beta",
        "beta",
        Some(CredentialSource::File("/keys/beta".into())),
    );
    let env = shared(
        "gamma",
        "gamma",
        Some(CredentialSource::Env("GAMMA".into())),
    );
    // Shadowed by the configured label, the provider's own source is still
    // configured, and appears once.
    assert_eq!(
        files(&config, &[own, other, env]),
        ["/keys/acme", "/keys/beta"].map(std::path::PathBuf::from)
    );
}

#[test]
fn a_repositorys_file_source_is_not_one_the_credential_reader_reads() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"providers": {"acme": {"credentials": {"default": {"file": "/keys/repo"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(files(&config, &[]), Vec::<std::path::PathBuf>::new());
}

fn read_with(
    config: &Config,
    provider: &ProviderData,
    run: config::Runner<'_>,
) -> Result<config::Read, ConfigError> {
    config
        .credentials()
        .credential_with(provider, "default", run)
}

#[test]
fn the_runner_gets_the_built_command_with_stdin_and_stderr_null() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let seen = std::cell::RefCell::new(Vec::new());
    let script = "echo noise >&2; if read line; then printf got; else printf eof; fi";
    let read = read_with(
        &config,
        &acme(command(&["sh", "-c", script])),
        &|command: &mut std::process::Command| {
            seen.borrow_mut().push((
                command.get_program().to_owned(),
                command
                    .get_args()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>(),
            ));
            let output = command.output()?;
            // A null stderr collects nothing; an inherited or piped one would.
            assert!(output.stderr.is_empty());
            Ok(output)
        },
    )
    .unwrap();
    assert_eq!(read.secret.expose(), "eof");
    assert_eq!(read.file, None);
    assert_eq!(
        seen.into_inner(),
        [("sh".into(), vec!["-c".into(), script.into()])]
    );
}

#[test]
fn the_runners_output_is_read_as_a_command_source() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    for (argv, why) in [
        (&["false", "sk-in-argument"][..], "`false` failed"),
        (&["printf", "  \n"], "`printf` printed no key"),
    ] {
        let err = read_with(
            &config,
            &acme(command(argv)),
            &|command: &mut std::process::Command| command.output(),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::CredentialMissing);
        let message = err.to_string();
        assert!(message.contains(why), "{message}");
        assert!(!message.contains("sk-in-argument"), "{message}");
    }
    let err = read_with(&config, &acme(command(&["printf", "k"])), &|_| {
        Err(std::io::Error::other("refused"))
    })
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("`printf` could not be started: refused")
    );
}

#[test]
fn a_file_source_behind_a_symlink_names_its_canonical_target() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let target = setup.root().join("keys/real");
    setup.write(&target, "from-target\n");
    let link = setup.root().join("link");
    symlink(&target, &link).unwrap();
    let never = &|_: &mut std::process::Command| -> std::io::Result<std::process::Output> {
        panic!("no command runs for a file source")
    };
    let read = read_with(&config, &acme(Some(CredentialSource::File(link))), never).unwrap();
    assert_eq!(read.secret.expose(), "from-target");
    assert_eq!(read.file, Some(std::fs::canonicalize(&target).unwrap()));
}

#[test]
fn an_env_or_command_source_names_no_file() {
    let setup = Setup::new();
    store_credential(&setup.home(), "stored", "default", &Secret::new("s".into())).unwrap();
    let config = setup.load(&[]).unwrap();
    let output = &|command: &mut std::process::Command| command.output();
    for provider in [
        acme(Some(CredentialSource::Env("CARGO_MANIFEST_DIR".into()))),
        acme(command(&["printf", "k"])),
        shared("stored", "stored", None),
    ] {
        let read = read_with(&config, &provider, output).unwrap();
        assert_eq!(read.file, None, "{}", provider.name);
    }
}

/// `docs/configuration.md`, "Secrets": a command runs once per process, so
/// every caller of one `Config` and its clones shares the key it gave.
#[test]
fn a_command_runs_once_however_often_the_credential_is_read() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let marker = setup.root().join("runs");
    let script = format!("echo run >> '{}'; printf key", marker.display());
    let provider = acme(command(&["sh", "-c", &script]));
    let read = marker.clone();
    let (runs, keys) = within(move || {
        let keys = [
            default_key(&config, &provider).unwrap(),
            default_key(&config, &provider).unwrap(),
            default_key(&config.clone(), &provider).unwrap(),
        ];
        (std::fs::read_to_string(read).unwrap().lines().count(), keys)
    });
    assert_eq!(runs, 1);
    assert!(keys.iter().all(|key| key.expose() == "key"));
}

#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    Deadline::after(std::time::Duration::from_secs(10))
        .recv(&rx)
        .unwrap_or_else(|_| panic!("the credential reads did not return in time"))
}

#[test]
fn labels_lists_stored_configured_and_declared_labels_once_sorted() {
    let setup = Setup::new();
    put(&setup, "acme", "work", "w");
    put(&setup, "acme", "home", "h");
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"home": {"env": "ACME_KEY"}, "ci": {"env": "CI_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    // A stored label, a configured label, `default` only when the
    // provider's data declares a source; each once, sorted
    // (`docs/model-routing.md`, "Which credential a session uses").
    assert_eq!(
        config
            .credentials()
            .labels(&acme(command(&["printf", "d"]))),
        ["ci", "default", "home", "work"].map(str::to_owned),
    );
    assert_eq!(
        config.credentials().labels(&acme(None)),
        ["ci", "home", "work"].map(str::to_owned),
    );
}

#[test]
fn labels_follows_a_shared_credential_name() {
    let setup = Setup::new();
    put(&setup, "shared", "work", "w");
    put(&setup, "other", "own", "o");
    let config = setup.load(&[]).unwrap();
    // A key that is not stored takes its label from the configuration
    // that points at it (`docs/model-routing.md`, "Credentials").
    assert_eq!(
        config.credentials().labels(&shared("acme", "shared", None)),
        ["work"].map(str::to_owned),
    );
}

#[test]
fn labels_of_an_absent_directory_is_what_the_message_lists() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"a-configured": {"env": "X"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    // An unreadable directory lists nothing from it.
    assert_eq!(
        config.credentials().labels(&acme(None)),
        ["a-configured"].map(str::to_owned),
    );
    let err = config
        .credentials()
        .credential(&acme(None), "personal")
        .unwrap_err();
    assert!(err.to_string().contains("are: a-configured."), "{err}");
}

#[test]
fn credential_sources_lists_each_configured_label_in_label_order() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {
            "f": {"file": "/keys/acme"},
            "e": {"env": "ACME_KEY"},
            "c": {"command": ["op", "read", "x"]}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.credentials().credential_sources("acme"),
        vec![
            (
                "c".to_owned(),
                CredentialSource::Command(vec!["op".into(), "read".into(), "x".into()]),
            ),
            ("e".to_owned(), CredentialSource::Env("ACME_KEY".into())),
            (
                "f".to_owned(),
                CredentialSource::File(std::path::PathBuf::from("/keys/acme")),
            ),
        ]
    );
    assert!(config.credentials().credential_sources("other").is_empty());
}

#[test]
fn listed_names_the_labels_or_none() {
    assert_eq!(config::Credentials::listed(&[]), "none");
    assert_eq!(
        config::Credentials::listed(&["default".into(), "work".into()]),
        "default, work"
    );
}
