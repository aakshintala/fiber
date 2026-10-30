//! `Credentials::has_credential`: a provider is switched on exactly when it
//! has a credential (`docs/model-routing.md`, "Credentials"). Presence is
//! not validity: the check reads no key and runs no command. An ordinary
//! provider counts any label's stored or configured origin; a
//! `credential_label` provider (Bedrock, Vertex) reads nothing Fiber stores,
//! so only its environment key or a declared `credential_paths` entry counts.

#![allow(clippy::unwrap_used, reason = "test setup; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::Setup;
use config::{
    Config, CredentialFrom, CredentialLabelSources, CredentialPath, CredentialSource, ProviderData,
    Secret, delete_credential, store_credential,
};

fn acme(credential: Option<CredentialSource>) -> ProviderData {
    ProviderData {
        name: "acme".into(),
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        credential_from: CredentialFrom::Stored {
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
        credential_from: CredentialFrom::Stored {
            credential,
            login: None,
        },
    }
}

fn function(name: &str, paths: Vec<CredentialPath>) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
        credential_from: CredentialFrom::Function(CredentialLabelSources {
            names: "profile".into(),
            default: "the environment".into(),
            credential_paths: paths,
        }),
    }
}

/// A lookup that reads `vars` alone, never the process environment.
fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

fn present(config: &Config, provider: &ProviderData, vars: &[(&str, &str)]) -> bool {
    config.credentials().has_credential(provider, &env_of(vars))
}

fn put(setup: &Setup, name: &str, label: &str, value: &str) {
    store_credential(&setup.home(), name, label, &Secret::new(value.into())).unwrap();
}

fn remove(setup: &Setup, name: &str, label: &str) -> bool {
    delete_credential(&setup.home(), name, label).unwrap()
}

/// The shipped first-party package `providers/<name>`'s providers.
fn shipped(name: &str) -> Vec<ProviderData> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../providers")
        .join(name);
    config::read_providers(&dir).unwrap()
}

fn named(providers: &[ProviderData], name: &str) -> ProviderData {
    providers.iter().find(|p| p.name == name).unwrap().clone()
}

/// `vars` plus `HOME` at `home` and an unrelated configured key set: the
/// baseline environment a `credential_label` test reads, holding none of
/// the provider's own keys.
fn env_in<'a>(home: &'a str, mut vars: Vec<(&'a str, &'a str)>) -> Vec<(&'a str, &'a str)> {
    vars.insert(0, ("ACME_KEY", "unrelated"));
    vars.insert(0, ("HOME", home));
    vars
}

/// A `HOME` holding no declared path, under the setup's root.
fn empty_home(setup: &Setup) -> String {
    let home = setup.root().join("declared-home");
    fs::create_dir_all(&home).unwrap();
    home.to_str().unwrap().to_owned()
}

/// Stores labels, configured sources and a shared-name credential for the
/// `credential_label` provider `name`, every one of which its presence must
/// ignore. Returns the marker the configured command would touch if run.
fn seed_ignored_origins(setup: &Setup, name: &str) -> PathBuf {
    put(setup, name, "default", "stored-default");
    put(setup, name, "work", "stored-work");
    put(setup, "shared-store", "default", "shared-key");
    let key_file = setup.root().join("configured-key");
    setup.write(&key_file, "configured-key");
    let marker = setup.root().join("command-ran");
    let text = format!(
        concat!(
            "{{\"providers\": {{\"{name}\": {{\"credentials\": {{",
            "\"default\": {{\"env\": \"ACME_KEY\"}}, ",
            "\"work\": {{\"file\": \"{key}\"}}, ",
            "\"other\": {{\"command\": [\"sh\", \"-c\", \"touch '{marker}'\"]}}",
            "}}}}}}}}"
        ),
        name = name,
        key = key_file.display(),
        marker = marker.display()
    );
    setup.write(&setup.global(), &text);
    marker
}

#[test]
fn no_source_is_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn a_stored_default_is_on() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "key");
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn an_empty_stored_file_is_on() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "");
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn a_nonselected_stored_label_is_on_while_the_selected_label_is_absent() {
    let setup = Setup::new();
    put(&setup, "acme", "work", "key");
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credential": "other"}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_env_unset_is_off() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_env_empty_is_off() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(
        &config,
        &acme(None),
        &[("FIBER_TEST_PRESENCE_KEY", "")]
    ));
}

#[test]
fn a_configured_env_whitespace_is_on() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(
        &config,
        &acme(None),
        &[("FIBER_TEST_PRESENCE_KEY", " ")]
    ));
}

#[test]
fn a_configured_env_set_is_on() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(
        &config,
        &acme(None),
        &[("FIBER_TEST_PRESENCE_KEY", "key")]
    ));
}

#[test]
fn a_declared_env_source_set_is_on_and_unset_is_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let provider = acme(Some(CredentialSource::Env(
        "FIBER_TEST_PRESENCE_KEY".into(),
    )));
    assert!(!present(&config, &provider, &[]));
    assert!(present(
        &config,
        &provider,
        &[("FIBER_TEST_PRESENCE_KEY", "key")]
    ));
}

#[test]
fn a_configured_missing_file_is_off() {
    let setup = Setup::new();
    let missing = setup.root().join("absent");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            missing.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn an_empty_configured_file_is_on() {
    let setup = Setup::new();
    let file = setup.root().join("key");
    setup.write(&file, "");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            file.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_file_is_on() {
    let setup = Setup::new();
    let file = setup.root().join("key");
    setup.write(&file, "key");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            file.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_directory_is_off() {
    let setup = Setup::new();
    let dir = setup.root().join("keydir");
    fs::create_dir_all(&dir).unwrap();
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            dir.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_fifo_is_off_and_never_opened() {
    let setup = Setup::new();
    let fifo = setup.root().join("pipe");
    fakes::fifo(&fifo);
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            fifo.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_dangling_symlink_is_off() {
    let setup = Setup::new();
    let link = setup.root().join("link");
    symlink(setup.root().join("gone"), &link).unwrap();
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            link.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn a_configured_file_symlink_to_a_regular_file_is_on() {
    let setup = Setup::new();
    let target = setup.root().join("key");
    setup.write(&target, "key");
    let link = setup.root().join("link");
    symlink(&target, &link).unwrap();
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"file": "{}"}}}}}}}}}}"#,
            link.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn a_stored_symlink_is_refused() {
    let setup = Setup::new();
    let target = setup.root().join("key");
    setup.write(&target, "key");
    let dir = setup.home().join("credentials/acme");
    fs::create_dir_all(&dir).unwrap();
    symlink(&target, dir.join("default")).unwrap();
    let config = setup.load(&[]).unwrap();
    let provider = acme(Some(CredentialSource::Env(
        "FIBER_TEST_PRESENCE_KEY".into(),
    )));
    assert!(!present(&config, &provider, &[]));
}

#[test]
fn a_refused_stored_label_does_not_block_another_label() {
    let setup = Setup::new();
    let target = setup.root().join("key");
    setup.write(&target, "key");
    let dir = setup.home().join("credentials/acme");
    fs::create_dir_all(&dir).unwrap();
    symlink(&target, dir.join("default")).unwrap();
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"work": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let provider = acme(Some(CredentialSource::Env(
        "FIBER_TEST_PRESENCE_KEY".into(),
    )));
    assert!(present(
        &config,
        &provider,
        &[("FIBER_TEST_PRESENCE_KEY", "key")]
    ));
}

#[test]
fn a_configured_command_counts_without_running() {
    let setup = Setup::new();
    let marker = setup.root().join("command-ran");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"default": {{"command": ["sh", "-c", "touch '{}'"]}}}}}}}}}}"#,
            marker.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn a_shared_credential_name_store_is_on() {
    let setup = Setup::new();
    put(&setup, "shared", "default", "key");
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &shared("mine", "shared", None), &[]));
}

#[test]
fn a_remaining_second_label_is_on() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "key");
    put(&setup, "acme", "work", "key");
    assert!(remove(&setup, "acme", "default"));
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
}

#[test]
fn deleting_the_last_label_is_off() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "key");
    put(&setup, "acme", "work", "key");
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
    assert!(remove(&setup, "acme", "default"));
    assert!(remove(&setup, "acme", "work"));
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
    assert!(
        setup.home().join("credentials/acme").is_dir(),
        "the provider directory stays behind and proves no login"
    );
    setup.write(&setup.home().join("credentials/acme/stale.lock"), "");
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn an_empty_provider_directory_is_off() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("credentials/acme")).unwrap();
    let config = setup.load(&[]).unwrap();
    assert!(!present(&config, &acme(None), &[]));
}

#[test]
fn env_outlives_a_deleted_store() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "key");
    setup.write(
        &setup.global(),
        r#"{"providers": {"acme": {"credentials": {"default": {"env": "FIBER_TEST_PRESENCE_KEY"}}}}}"#,
    );
    let vars = [("FIBER_TEST_PRESENCE_KEY", "key")];
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &vars));
    assert!(remove(&setup, "acme", "default"));
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &vars));
}

#[test]
fn an_ordinary_provider_with_the_same_sources_is_on_without_running_them() {
    let setup = Setup::new();
    put(&setup, "acme", "default", "key");
    let marker = setup.root().join("command-ran");
    setup.write(
        &setup.global(),
        &format!(
            r#"{{"providers": {{"acme": {{"credentials": {{"other": {{"command": ["sh", "-c", "touch '{}'"]}}}}}}}}}}"#,
            marker.display()
        ),
    );
    let config = setup.load(&[]).unwrap();
    assert!(present(&config, &acme(None), &[]));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn bedrock_ignores_stored_labels() {
    let setup = Setup::new();
    put(&setup, "bedrock", "default", "stored-default");
    put(&setup, "bedrock", "work", "stored-work");
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_ignores_configured_origins_without_running_them() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "bedrock");
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn bedrock_ignores_a_shared_credential_name_store() {
    let setup = Setup::new();
    put(&setup, "shared-store", "default", "shared-key");
    let config = setup.load(&[]).unwrap();
    let mut bedrock = named(&shipped("bedrock"), "bedrock");
    bedrock.credential_name = Some("shared-store".into());
    let home = empty_home(&setup);
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_ignores_all_origins_combined() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "bedrock");
    let config = setup.load(&[]).unwrap();
    let mut bedrock = named(&shipped("bedrock"), "bedrock");
    bedrock.credential_name = Some("shared-store".into());
    let home = empty_home(&setup);
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn vertex_ignores_stored_labels() {
    let setup = Setup::new();
    put(&setup, "vertex", "default", "stored-default");
    put(&setup, "vertex", "work", "stored-work");
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
}

#[test]
fn vertex_ignores_configured_origins_without_running_them() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "vertex");
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn vertex_ignores_a_shared_credential_name_store() {
    let setup = Setup::new();
    put(&setup, "shared-store", "default", "shared-key");
    let config = setup.load(&[]).unwrap();
    let mut vertex = named(&shipped("vertex"), "vertex");
    vertex.credential_name = Some("shared-store".into());
    let home = empty_home(&setup);
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
}

#[test]
fn vertex_ignores_all_origins_combined() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "vertex");
    let config = setup.load(&[]).unwrap();
    let mut vertex = named(&shipped("vertex"), "vertex");
    vertex.credential_name = Some("shared-store".into());
    let home = empty_home(&setup);
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn bedrock_bearer_key_is_on_despite_ignored_origins() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "bedrock");
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let vars = env_in(&home, vec![("AWS_BEARER_TOKEN_BEDROCK", "key")]);
    assert!(present(&config, &bedrock, &vars));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn bedrock_key_pair_is_on() {
    let setup = Setup::new();
    let marker = seed_ignored_origins(&setup, "bedrock");
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let vars = env_in(
        &home,
        vec![
            ("AWS_ACCESS_KEY_ID", "id"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
        ],
    );
    assert!(present(&config, &bedrock, &vars));
    assert!(
        !marker.exists(),
        "the command must never run to check presence"
    );
}

#[test]
fn bedrock_half_pairs_are_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(
        &config,
        &bedrock,
        &env_in(&home, vec![("AWS_ACCESS_KEY_ID", "id")])
    ));
    assert!(!present(
        &config,
        &bedrock,
        &env_in(&home, vec![("AWS_SECRET_ACCESS_KEY", "secret")])
    ));
    assert!(!present(
        &config,
        &bedrock,
        &env_in(
            &home,
            vec![("AWS_ACCESS_KEY_ID", "id"), ("AWS_SECRET_ACCESS_KEY", "")]
        )
    ));
}

#[test]
fn bedrock_session_token_alone_is_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(
        &config,
        &bedrock,
        &env_in(&home, vec![("AWS_SESSION_TOKEN", "token")])
    ));
}

#[test]
fn bedrock_empty_bearer_is_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(
        &config,
        &bedrock,
        &env_in(&home, vec![("AWS_BEARER_TOKEN_BEDROCK", "")])
    ));
}

#[test]
fn vertex_project_settings_are_not_credentials() {
    let setup = Setup::new();
    let credentials_file = setup.root().join("adc.json");
    setup.write(&credentials_file, "{}");
    let credentials = credentials_file.to_str().unwrap().to_owned();
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    let vars = env_in(
        &home,
        vec![
            ("GOOGLE_CLOUD_PROJECT", "example-project"),
            ("GOOGLE_APPLICATION_CREDENTIALS", credentials.as_str()),
        ],
    );
    assert!(!present(&config, &vertex, &vars));
}

#[test]
fn bedrock_declared_file_is_on() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    let credentials = Path::new(&home).join(".aws/credentials");
    fs::create_dir_all(credentials.parent().unwrap()).unwrap();
    fs::write(&credentials, "").unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_declared_env_override_is_on() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let moved = setup.root().join("creds");
    fs::write(&moved, "").unwrap();
    let moved = moved.to_str().unwrap().to_owned();
    let vars = env_in(&home, vec![("AWS_SHARED_CREDENTIALS_FILE", moved.as_str())]);
    assert!(present(&config, &bedrock, &vars));
}

#[test]
fn bedrock_declared_directories_count_one_entry() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let cache = Path::new(&home).join(".aws/sso/cache");
    let cli_cache = Path::new(&home).join(".aws/cli/cache");
    fs::create_dir_all(&cli_cache).unwrap();
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::create_dir_all(&cache).unwrap();
    assert!(
        !present(&config, &bedrock, &env_in(&home, Vec::new())),
        "an empty cache directory is left behind by a logout"
    );
    let entry = cache.join("token.json");
    fs::write(&entry, "{}").unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(&entry).unwrap();
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_declared_symlinks_are_followed() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let declared = Path::new(&home).join(".aws/credentials");
    fs::create_dir_all(declared.parent().unwrap()).unwrap();
    let target = setup.root().join("target");
    fs::write(&target, "key").unwrap();
    symlink(&target, &declared).unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    let full = setup.root().join("full");
    fs::create_dir_all(&full).unwrap();
    fs::write(full.join("entry"), "x").unwrap();
    symlink(&full, &declared).unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    let empty = setup.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    symlink(&empty, &declared).unwrap();
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    symlink(setup.root().join("gone"), &declared).unwrap();
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_directory_entries_are_never_opened() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    let cache = Path::new(&home).join(".aws/sso/cache");
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(cache.join("child")).unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_dir(cache.join("child")).unwrap();
    fakes::fifo(cache.join("pipe"));
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(cache.join("pipe")).unwrap();
    symlink(cache.join("gone"), cache.join("alias")).unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn bedrock_evidence_removed_is_off_despite_ignored_origins() {
    let setup = Setup::new();
    seed_ignored_origins(&setup, "bedrock");
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let home = empty_home(&setup);
    assert!(present(
        &config,
        &bedrock,
        &env_in(&home, vec![("AWS_BEARER_TOKEN_BEDROCK", "key")])
    ));
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
    let credentials = Path::new(&home).join(".aws/credentials");
    fs::create_dir_all(credentials.parent().unwrap()).unwrap();
    fs::write(&credentials, "").unwrap();
    assert!(present(&config, &bedrock, &env_in(&home, Vec::new())));
    fs::remove_file(&credentials).unwrap();
    assert!(!present(&config, &bedrock, &env_in(&home, Vec::new())));
}

#[test]
fn vertex_declared_files_are_on() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
    let adc = Path::new(&home).join(".config/gcloud/application_default_credentials.json");
    fs::create_dir_all(adc.parent().unwrap()).unwrap();
    fs::write(&adc, "not json").unwrap();
    assert!(present(&config, &vertex, &env_in(&home, Vec::new())));
}

#[test]
fn vertex_declared_env_override_and_join_are_on() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    let moved = setup.root().join("gcloud");
    fs::create_dir_all(&moved).unwrap();
    fs::write(moved.join("access_tokens.db"), "").unwrap();
    let moved = moved.to_str().unwrap().to_owned();
    let vars = env_in(&home, vec![("CLOUDSDK_CONFIG", moved.as_str())]);
    assert!(present(&config, &vertex, &vars));
}

#[test]
fn vertex_legacy_credentials_directory_counts_one_entry() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    let legacy = Path::new(&home).join(".config/gcloud/legacy_credentials");
    fs::create_dir_all(&legacy).unwrap();
    assert!(
        !present(&config, &vertex, &env_in(&home, Vec::new())),
        "an empty cache directory is left behind by a logout"
    );
    let entry = legacy.join("account");
    fs::create_dir_all(&entry).unwrap();
    assert!(present(&config, &vertex, &env_in(&home, Vec::new())));
    fs::remove_dir(&entry).unwrap();
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
}

#[test]
fn vertex_declared_symlinks_are_followed() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let vertex = named(&shipped("vertex"), "vertex");
    let home = empty_home(&setup);
    let declared = Path::new(&home).join(".config/gcloud/credentials.db");
    fs::create_dir_all(declared.parent().unwrap()).unwrap();
    let target = setup.root().join("target");
    fs::write(&target, "key").unwrap();
    symlink(&target, &declared).unwrap();
    assert!(present(&config, &vertex, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    let full = setup.root().join("full");
    fs::create_dir_all(&full).unwrap();
    fs::write(full.join("entry"), "x").unwrap();
    symlink(&full, &declared).unwrap();
    assert!(present(&config, &vertex, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    let empty = setup.root().join("empty");
    fs::create_dir_all(&empty).unwrap();
    symlink(&empty, &declared).unwrap();
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
    fs::remove_file(&declared).unwrap();
    symlink(setup.root().join("gone"), &declared).unwrap();
    assert!(!present(&config, &vertex, &env_in(&home, Vec::new())));
}

#[test]
fn home_unset_leaves_declared_paths_off() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let bedrock = named(&shipped("bedrock"), "bedrock");
    let vertex = named(&shipped("vertex"), "vertex");
    assert!(!present(&config, &bedrock, &[]));
    assert!(!present(&config, &vertex, &[]));
    assert!(!present(
        &config,
        &bedrock,
        &[("AWS_SHARED_CREDENTIALS_FILE", "/x/creds")]
    ));
}

#[test]
fn a_synthetic_function_provider_uses_paths_only() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    let home = empty_home(&setup);
    let declared = Path::new(&home).join(".acme/key");
    let provider = function(
        "acme-func",
        vec![config::CredentialPath {
            path: declared.display().to_string(),
            env: None,
            join: None,
        }],
    );
    assert!(!present(&config, &provider, &env_in(&home, Vec::new())));
    put(&setup, "acme-func", "default", "stored");
    assert!(
        !present(&config, &provider, &env_in(&home, Vec::new())),
        "a stored label never switches a function provider on"
    );
    fs::create_dir_all(declared.parent().unwrap()).unwrap();
    fs::write(&declared, "").unwrap();
    assert!(present(&config, &provider, &env_in(&home, Vec::new())));
}
