//! Which label the session and its reviewer use
//! (`docs/model-routing.md`, "Which credential a session uses").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use config::{Config, ProjectKey, ProviderData, Secret, Sources, store_credential};

use super::{reviewer_credential, session_credential};

fn provider(name: &str) -> ProviderData {
    ProviderData {
        name: name.into(),
        credential: None,
        credential_name: None,
        headers: Default::default(),
        models: Vec::new(),
        reviewer_model: None,
    }
}

/// A home holding `credentials/<name>/<label>` for each of `stored`, and
/// the configuration `config.json` text.
fn config(root: &fakes::TempDir, stored: &[(&str, &str)], text: &str) -> Config {
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
    let config = config(&root, &STORED, SETTINGS);
    let (label, key) = session_credential(&config, &provider("acme"), Some("rec")).unwrap();
    assert_eq!((label.as_str(), key.expose()), ("rec", "acme-rec"));
    let (label, key) = session_credential(&config, &provider("acme"), None).unwrap();
    assert_eq!((label.as_str(), key.expose()), ("cfg", "acme-cfg"));
}

#[test]
fn a_recorded_label_that_names_nothing_is_credential_missing() {
    let root = fakes::TempDir::new("fiber-credential");
    let config = config(&root, &STORED, SETTINGS);
    let failure = session_credential(&config, &provider("acme"), Some("gone")).unwrap_err();
    assert_eq!(failure.code, contract::ErrorCode::CredentialMissing);
}

#[test]
fn the_reviewer_of_another_provider_uses_its_own_label() {
    let root = fakes::TempDir::new("fiber-credential");
    let config = config(&root, &STORED, SETTINGS);
    let key = reviewer_credential(&config, &provider("other"));
    assert_eq!(key.unwrap().expose(), "other-own");
}
