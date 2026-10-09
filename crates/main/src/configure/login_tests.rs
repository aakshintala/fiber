//! `/login` through the seam: the rows it lists and the keys it stores
//! (`docs/tui.md`, "Logging in").

use std::fs;
use std::path::PathBuf;

use serde_json::json;
use tui::{Configure, LoginKind};

use super::super::Seam;

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-configure-login");
        fs::create_dir_all(root.path().join("home")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs a provider `name`.
    fn install(&self, name: &str) {
        let dir = self.home().join("extensions").join(name);
        fs::create_dir_all(dir.join("providers")).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            json!({
                "name": name,
                "models": [{"id": "m", "protocol": "openai-responses",
                    "base_url": "http://x/v1", "context_window": 1000}],
            })
            .to_string(),
        )
        .unwrap();
    }

    /// Installs the extension `extension` with no provider, declaring
    /// `secrets`.
    fn declare(&self, extension: &str, secrets: &[&str]) {
        let dir = self.home().join("extensions").join(extension);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("extension.json"),
            json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
                "secrets": secrets})
            .to_string(),
        )
        .unwrap();
    }
}

#[test]
fn login_targets_maps_providers_to_key_and_secrets_to_secret() {
    let setup = Setup::new();
    setup.install("acme");
    setup.declare("acme-secrets", &["acme.api_key"]);
    let rows = Seam::new(setup.home()).login_targets().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "acme");
    assert_eq!(rows[0].kind, LoginKind::Key);
    assert_eq!(rows[1].name, "acme.api_key");
    assert_eq!(rows[1].kind, LoginKind::Secret);
}

#[test]
fn store_key_lands_in_credentials_and_names_the_first_label() {
    let setup = Setup::new();
    setup.install("acme");
    let seam = Seam::new(setup.home());
    let stored = seam
        .store_key(
            "acme",
            Some("work"),
            contract::Secret::new("sk-a".to_owned()),
        )
        .unwrap();
    assert_eq!(stored.path, "credentials/acme/work");
    assert!(!stored.replaced);
    assert_eq!(
        fs::read_to_string(setup.home().join("credentials/acme/work")).unwrap(),
        "sk-a"
    );
    let global: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        global,
        json!({"providers": {"acme": {"credential": "work"}}})
    );
}

#[test]
fn store_key_refusal_is_usage_with_the_cli_message() {
    let setup = Setup::new();
    setup.install("acme");
    let seam = Seam::new(setup.home());
    seam.store_key("acme", None, contract::Secret::new("old".to_owned()))
        .unwrap();
    let error = seam
        .store_key("acme", None, contract::Secret::new("new".to_owned()))
        .err()
        .unwrap();
    assert_eq!(error.code, contract::ErrorCode::Usage);
    assert!(
        error.message.contains(
            "credentials/acme/default is already stored; log in under another label with --as <label>"
        ),
        "{}",
        error.message
    );
    assert!(
        !error.message.contains("fiber --help"),
        "no CLI hint: {}",
        error.message
    );
}

#[test]
fn store_key_with_an_empty_key_stores_nothing() {
    let setup = Setup::new();
    setup.install("acme");
    let error = Seam::new(setup.home())
        .store_key("acme", None, contract::Secret::new(String::new()))
        .err()
        .unwrap();
    assert_eq!(error.code, contract::ErrorCode::Usage);
    assert_eq!(error.message, "No key was given; nothing was stored.");
    assert!(!setup.home().join("credentials/acme/default").exists());
    assert!(!setup.home().join("config.json").exists());
}
