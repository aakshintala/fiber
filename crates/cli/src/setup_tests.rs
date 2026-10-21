//! The read/filter path behind the setup questions
//! (`docs/user/install.md`, "Setup"): signed-in marking at any label,
//! model choices from signed-in providers only, empty providers skipping
//! steps 1 and 3, receipt short-circuiting, and the shell mapping.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use config::{ImportSource, Secret, store_credential};
use extensions::Providers;
use serde_json::{Value, json};

use super::{
    completions_installed, import_offered, model_offers, provider_offers, receipt_completed,
    shell_from_env,
};
use crate::test_support::write_record;

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-setup");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs a provider `name` with `models`.
    fn install(&self, name: &str, models: Value) {
        let dir = self.home().join("extensions").join(name);
        let data = json!({
            "name": name,
            "models": models,
        });
        fs::create_dir_all(dir.join("providers")).unwrap();
        let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = Value::Object(
            [(name.to_owned(), json!(["http://x/v1"]))]
                .into_iter()
                .collect(),
        );
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
        write_record(&dir);
        fs::write(
            dir.join("providers").join(format!("{name}.json")),
            data.to_string(),
        )
        .unwrap();
    }

    fn providers(&self) -> Providers {
        let (providers, notices) = Providers::load(&self.home()).unwrap();
        assert!(notices.is_empty(), "{notices:?}");
        providers
    }
}

fn models(id: &str) -> Value {
    json!([{"id": id, "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000}])
}

fn thinking_models() -> Value {
    json!([{
        "id": "m",
        "protocol": "openai-responses",
        "base_url": "http://x/v1",
        "context_window": 1000,
        "thinking_levels": ["low", "high"],
        "thinking_default": "low",
    }])
}

fn var<'v>(vars: &'v HashMap<&str, &str>) -> impl Fn(&str) -> Option<OsString> + 'v {
    move |key| vars.get(key).map(OsString::from)
}

#[test]
fn signed_in_marks_any_stored_label() {
    let setup = Setup::new();
    setup.install("fake", models("m"));
    setup.install("other", models("o1"));
    let providers = setup.providers();
    let unsigned: Vec<String> = provider_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| format!("{}{}", offer.name, offer.signed_in))
        .collect();
    assert_eq!(unsigned, vec!["fakefalse", "otherfalse"]);
    store_credential(&setup.home(), "fake", "work", &Secret::new("k".into())).unwrap();
    let mut signed: Vec<(String, bool)> = provider_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| (offer.name, offer.signed_in))
        .collect();
    signed.sort();
    assert_eq!(
        signed,
        vec![("fake".to_owned(), true), ("other".to_owned(), false),]
    );
}

#[test]
fn model_choices_come_from_signed_in_providers_only() {
    let setup = Setup::new();
    setup.install("fake", thinking_models());
    setup.install("other", models("o1"));
    let providers = setup.providers();
    assert!(model_offers(&setup.home(), &providers).unwrap().is_empty());
    store_credential(&setup.home(), "fake", "default", &Secret::new("k".into())).unwrap();
    assert_eq!(
        model_offers(&setup.home(), &providers).unwrap(),
        vec![super::ModelOffer {
            reference: "fake/m".to_owned(),
            levels: vec!["low".to_owned(), "high".to_owned()],
        }]
    );
    store_credential(&setup.home(), "other", "default", &Secret::new("k".into())).unwrap();
    let references: Vec<String> = model_offers(&setup.home(), &providers)
        .unwrap()
        .into_iter()
        .map(|offer| offer.reference)
        .collect();
    assert_eq!(references, vec!["fake/m", "other/o1"]);
}

#[test]
fn empty_providers_offer_nothing() {
    let setup = Setup::new();
    let providers = setup.providers();
    assert!(
        provider_offers(&setup.home(), &providers)
            .unwrap()
            .is_empty()
    );
    assert!(model_offers(&setup.home(), &providers).unwrap().is_empty());
}

#[test]
fn completed_receipts_are_done_and_malformed_ones_are_offered() {
    let setup = Setup::new();
    let home_env = setup.home();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(!import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::create_dir_all(home_env.join(".claude")).unwrap();
    assert!(import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::write(setup.home().join("import-claude-code.json"), "not json").unwrap();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
    fs::write(
        setup.home().join("import-claude-code.json"),
        json!({"source": "codex"}).to_string(),
    )
    .unwrap();
    assert!(!receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    fs::write(
        setup.home().join("import-claude-code.json"),
        json!({"source": "claude-code"}).to_string(),
    )
    .unwrap();
    assert!(receipt_completed(&setup.home(), ImportSource::ClaudeCode));
    assert!(!import_offered(
        &home_env,
        &setup.home(),
        ImportSource::ClaudeCode
    ));
}

#[test]
fn shells_map_by_basename_with_a_dash_and_unknowns_map_to_none() {
    for (shell, expected) in [
        ("/bin/zsh", Some("zsh")),
        ("/usr/bin/bash", Some("bash")),
        ("/usr/local/bin/fish", Some("fish")),
        ("/bin/-zsh", Some("zsh")),
        ("/bin/sh", None),
        ("elvish", None),
    ] {
        let vars = HashMap::from([("SHELL", shell)]);
        assert_eq!(shell_from_env(&var(&vars)).as_deref(), expected, "{shell}");
    }
    let vars = HashMap::new();
    assert_eq!(shell_from_env(&var(&vars)), None);
    let vars = HashMap::from([("SHELL", "/bin/")]);
    assert_eq!(shell_from_env(&var(&vars)), None);
}

#[test]
fn completions_are_installed_only_with_the_line_present() {
    let setup = Setup::new();
    let home_env = setup.home();
    assert_eq!(completions_installed(&home_env, "zsh"), Some(false));
    assert_eq!(completions_installed(&home_env, "elvish"), None);
    fs::write(home_env.join(".zshrc"), "export X=1\n").unwrap();
    assert_eq!(completions_installed(&home_env, "zsh"), Some(false));
    fs::write(
        home_env.join(".zshrc"),
        "export X=1\neval \"$(fiber completion zsh)\"\n",
    )
    .unwrap();
    assert_eq!(completions_installed(&home_env, "zsh"), Some(true));
}

#[test]
fn a_startup_file_that_fails_to_read_is_neither_done_nor_missing() {
    let setup = Setup::new();
    let home_env = setup.home();
    // A directory fails to read with an error other than `NotFound`:
    // the step must ask again instead of reporting no line is held.
    fs::create_dir(home_env.join(".zshrc")).unwrap();
    assert_eq!(completions_installed(&home_env, "zsh"), None);
}
