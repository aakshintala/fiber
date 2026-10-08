//! Which label the session and its reviewer use
//! (`docs/model-routing.md`, "Which credential a session uses").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use config::{Config, ProjectKey, ProviderData, Secret, Sources, store_credential};

use super::{Labels, no_label, session_credential, switch_credential};

fn provider(name: &str) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential: None,
        credential_name: None,
        headers: Default::default(),
        placeholders: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
    }
}

/// A scripted provider named `scripted`.
fn scripted_provider() -> ProviderData {
    let mut providers = extensions::Providers::default();
    providers.add_scripted("scripted/a.json");
    providers.get("scripted").unwrap().clone()
}

/// A home holding `credentials/<name>/<label>` for each of `stored`, and
/// the configuration `config.json` text.
fn test_config(root: &fakes::TempDir, stored: &[(&str, &str)], text: &str) -> Config {
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    for (name, label) in stored {
        let secret = Secret::new(format!("{name}-{label}"));
        store_credential(&home, name, label, &secret).unwrap();
    }
    std::fs::write(home.join("config.json"), text).unwrap();
    Config::load(Sources {
        home,
        workspace,
        project: ProjectKey::new("test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap()
}

const SETTINGS: &str =
    r#"{"providers": {"acme": {"credential": "cfg"}, "other": {"credential": "own"}}}"#;
const STORED: [(&str, &str); 5] = [
    ("acme", "cfg"),
    ("acme", "rec"),
    ("acme", "session"),
    ("other", "own"),
    ("other", "session"),
];

#[test]
fn a_recorded_label_beats_the_configured_one() {
    let root = fakes::TempDir::new("fiber-credential");
    let config = test_config(&root, &STORED, SETTINGS);
    let (label, key) = session_credential(&config, &provider("acme"), Some("rec")).unwrap();
    assert_eq!((label.as_str(), key.expose()), ("rec", "acme-rec"));
    let (label, key) = session_credential(&config, &provider("acme"), None).unwrap();
    assert_eq!((label.as_str(), key.expose()), ("cfg", "acme-cfg"));
}

#[test]
fn a_recorded_label_that_names_nothing_is_credential_missing() {
    let root = fakes::TempDir::new("fiber-credential");
    let config = test_config(&root, &STORED, SETTINGS);
    let failure = session_credential(&config, &provider("acme"), Some("gone")).unwrap_err();
    assert_eq!(failure.code, contract::ErrorCode::CredentialMissing);
}

#[test]
fn the_reviewer_of_another_provider_uses_its_own_label() {
    let root = fakes::TempDir::new("fiber-credential");
    let config = test_config(&root, &STORED, SETTINGS);
    let (label, key) = session_credential(&config, &provider("other"), None).unwrap();
    assert_eq!((label.as_str(), key.expose()), ("own", "other-own"));
}

#[test]
fn a_switch_read_maps_a_config_error_to_its_code() {
    let root = fakes::TempDir::new("fiber-switch-credential");
    let config = test_config(&root, &[("acme", "cfg")], SETTINGS);
    let read = switch_credential(&config, &provider("acme"), "cfg", &|command| {
        command.output()
    })
    .unwrap();
    assert_eq!(read.secret.expose(), "acme-cfg");
    assert_eq!(read.file, None);
    let mut command = provider("acme");
    command.credential =
        Some(serde_json::from_value(serde_json::json!({"command": ["false"]})).unwrap());
    let failure = switch_credential(&config, &command, "default", &|command| command.output())
        .err()
        .unwrap();
    assert_eq!(failure.code, contract::ErrorCode::CredentialMissing);
    assert!(failure.message.contains("`false`"), "{}", failure.message);
}

#[test]
fn from_an_option_is_a_recorded_only_labels() {
    let asked: Labels<'_> = Labels::from(Some("a"));
    assert_eq!(asked, Labels::new(None, Some("a")));
    let neither: Labels<'_> = Labels::from(None);
    assert_eq!(neither, Labels::new(None, None));
    assert_eq!(neither, Labels::default());
}

#[test]
fn labels_prefer_the_asked_then_the_recorded_then_the_configured_label() {
    let root = fakes::TempDir::new("fiber-credential-labels");
    let config = test_config(&root, &STORED, SETTINGS);
    let acme = provider("acme");
    assert_eq!(
        Labels::new(Some("a"), Some("b"))
            .label(&config, &acme)
            .unwrap(),
        "a"
    );
    assert_eq!(
        Labels::new(None, Some("b")).label(&config, &acme).unwrap(),
        "b"
    );
    assert_eq!(
        Labels::new(None, None).label(&config, &acme).unwrap(),
        "cfg"
    );
    let plain = test_config(&root, &[], "{}");
    assert_eq!(
        Labels::new(None, None).label(&plain, &acme).unwrap(),
        "default"
    );
}

#[test]
fn labels_on_a_scripted_provider_reject_an_asked_label() {
    let root = fakes::TempDir::new("fiber-credential-scripted");
    let config = test_config(&root, &[], "{}");
    let scripted = scripted_provider();
    let failure = Labels::new(Some("x"), None)
        .label(&config, &scripted)
        .unwrap_err();
    assert_eq!(failure.code, contract::ErrorCode::CredentialMissing);
    assert!(
        failure.message.ends_with("are: none"),
        "{}",
        failure.message
    );
    assert!(
        failure.message.contains("`scripted`"),
        "{}",
        failure.message
    );
    let recorded = Labels::new(None, Some("default"))
        .label(&config, &scripted)
        .unwrap();
    assert_eq!(recorded, "default");
}

#[test]
fn no_label_lists_none_when_empty_and_names_when_not() {
    let empty = no_label("p", "x", &[]);
    assert_eq!(empty.code, contract::ErrorCode::CredentialMissing);
    assert!(empty.message.ends_with("are: none"), "{}", empty.message);
    assert_eq!(
        empty.message,
        "`p` has no credential label `x`. The labels for `p` are: none"
    );
    let full = no_label("p", "x", &["home".to_owned(), "work".to_owned()]);
    assert_eq!(full.code, contract::ErrorCode::CredentialMissing);
    assert!(
        full.message.ends_with("are: home, work"),
        "{}",
        full.message
    );
}
