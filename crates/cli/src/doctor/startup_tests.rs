//! The data-only startup checks: every blocker row through `startup`,
//! and every `with_fix` annotation row through `fix_at`.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use super::{Checked, Startup, startup};
use crate::doctor::fix_at;
use config::{Config, ModelData, Protocol, ProviderData, Secret, Sources, store_credential};
use contract::ErrorCode;
use contract::shapes::Failure;
use serde_json::{Value, json};

/// A home with a workspace under it.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-doctor-startup");
        fs::create_dir_all(root.path().join("home")).unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Hand-places the extension `name` with these provider files: the
    /// manifest declares the providers, and the record marks it installed.
    fn place(&self, name: &str, providers: &[Value], manifest: Value) {
        let dir = self.home().join("extensions").join(name);
        fs::create_dir_all(dir.join("providers")).unwrap();
        let mut manifest = manifest;
        if manifest.get("providers").is_none() {
            let mut map = serde_json::Map::new();
            for data in providers {
                let provider = data["name"].as_str().unwrap();
                let mut urls = Vec::new();
                for model in data["models"].as_array().unwrap() {
                    let url = model["base_url"].as_str().unwrap();
                    if !urls.contains(&url) {
                        urls.push(url);
                    }
                }
                map.insert(
                    provider.to_owned(),
                    Value::Array(urls.into_iter().map(Value::from).collect()),
                );
            }
            manifest["providers"] = Value::Object(map);
        }
        fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
        for data in providers {
            let provider = data["name"].as_str().unwrap();
            fs::write(
                dir.join("providers").join(format!("{provider}.json")),
                data.to_string(),
            )
            .unwrap();
        }
        fakes::write_record(&dir);
    }

    /// Places `fake` with one model `m` on `openai-responses` and the
    /// default env key source.
    fn fake(&self) {
        self.place(
            "fake",
            &[provider(
                "fake",
                &[model("m", "openai-responses", 1_000, None)],
                None,
                None,
            )],
            manifest("fake"),
        );
    }

    /// A provider `cloud` whose `credential()` reads nothing stored: its
    /// label names a profile outside Fiber (`docs/model-routing.md`, "What a
    /// provider extension declares").
    fn profiled(&self) {
        let data = json!({
            "name": "cloud",
            "models": [model("m", "openai-responses", 1_000, None)],
            "credential_label": {"names": "cloud profile", "default": "the environment"},
        });
        self.place("cloud", &[data], manifest("cloud"));
    }

    fn write_config(&self, value: &Value) {
        fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    /// Writes the per-project configuration for this workspace's project.
    fn write_project(&self, value: &Value) {
        let (_, project) = crate::project_of(&self.home(), &self.workspace()).unwrap();
        let dir = self.home().join("projects").join(project.as_str());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), value.to_string()).unwrap();
    }

    fn store(&self, name: &str, label: &str, text: &str) {
        store_credential(&self.home(), name, label, &Secret::new(text.into())).unwrap();
    }

    /// Runs `startup` over this fixture with these overrides.
    fn check(&self, overrides: &[&str], env: &HashMap<String, String>) -> Startup {
        startup(
            &self.home(),
            &self.workspace(),
            &overrides
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            &|name| env.get(name).cloned(),
            &|url| url.contains('@'),
        )
    }

    /// The blocker, which the row expects.
    fn blocked(&self, overrides: &[&str], env: &HashMap<String, String>) -> (ErrorCode, String) {
        let checked = self.check(overrides, env);
        let blocker = checked.blocker.unwrap();
        (blocker.code, blocker.line)
    }

    /// The report is ready: no blocker.
    fn ready(&self, overrides: &[&str], env: &HashMap<String, String>) -> Vec<String> {
        let checked = self.check(overrides, env);
        assert!(
            checked.blocker.is_none(),
            "{:?}",
            checked.blocker.map(|b| b.line)
        );
        checked.lines
    }
}

fn manifest(name: &str) -> Value {
    json!({"name": name, "version": "v1.0.0", "fiber": "0.1.0", "api": 1})
}

fn model(id: &str, protocol: &str, window: u64, levels: Option<Vec<&str>>) -> Value {
    let mut model = json!({
        "id": id,
        "protocol": protocol,
        "base_url": "http://127.0.0.1:1/v1",
        "context_window": window,
    });
    if let Some(levels) = levels {
        model["thinking_levels"] = Value::Array(levels.into_iter().map(Value::from).collect());
    }
    model
}

fn provider(
    name: &str,
    models: &[Value],
    source: Option<Value>,
    credential_name: Option<&str>,
) -> Value {
    let mut data = json!({"name": name, "models": models});
    if let Some(source) = source {
        data["credential"] = source;
    } else {
        data["credential"] = json!({"env": "FIBER_TEST_UNSET_KEY"});
    }
    if let Some(stored) = credential_name {
        data["credential_name"] = Value::String(stored.to_owned());
    }
    data
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

#[test]
fn an_empty_missing_home_gives_no_model() {
    let setup = Setup::new();
    let home = setup.home().join("missing");
    let checked = startup(&home, &setup.workspace(), &[], &|_| None, &|_| false);
    let blocker = checked.blocker.unwrap();
    assert_eq!(blocker.code, ErrorCode::NoModel);
    assert!(!blocker.key);
    assert_eq!(
        blocker.line,
        "No model was chosen. Pass `--model provider/model`, or set `model` in configuration."
    );
    assert!(!home.exists(), "a missing home stays missing");
}

#[test]
fn with_fix_leaves_a_no_model_failure_unchanged() {
    // The `NoModel` line equals a session's startup message, so there is
    // nothing to annotate.
    let setup = Setup::new();
    let message =
        "No model was chosen. Pass `--model provider/model`, or set `model` in configuration.";
    let failure = failure(ErrorCode::NoModel, message);
    let fixed = fix_at(failure, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.code, ErrorCode::NoModel);
    assert_eq!(fixed.message, message);
}

#[test]
fn a_model_from_an_unknown_provider_gives_provider_missing() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "unknown/x"}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::ExtensionMissing);
    assert!(line.starts_with("The provider `unknown` is not installed."));
}

#[test]
fn a_bare_id_two_providers_serve_gives_ambiguous() {
    let setup = Setup::new();
    setup.place(
        "a",
        &[provider(
            "a",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("a"),
    );
    setup.place(
        "b",
        &[provider(
            "b",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("b"),
    );
    setup.write_config(&json!({"model": "m"}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::ModelAmbiguous);
    assert!(line.contains("more than one provider"), "{line}");
}

#[test]
fn a_placeholder_model_without_its_value_gives_unconfigured() {
    let setup = Setup::new();
    let data = json!({
        "name": "acme",
        "credential": {"env": "FIBER_TEST_UNSET_KEY"},
        "placeholders": {"workspace": {}},
        "models": [{"id": "m", "protocol": "openai-responses",
                    "base_url": "https://{workspace}/v1", "context_window": 1000}],
    });
    setup.place("acme", &[data], manifest("acme"));
    setup.write_config(&json!({"model": "acme/m"}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::ModelUnconfigured);
    assert!(line.contains("acme/m"), "{line}");
}

#[test]
fn a_disabled_package_gives_provider_missing_but_keeps_its_block() {
    let setup = Setup::new();
    setup.place(
        "off",
        &[provider(
            "fake",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("off"),
    );
    setup.write_config(&json!({"model": "fake/m", "extensions": {"off": {"enabled": false}}}));
    let checked = setup.check(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    let blocker = checked.blocker.unwrap();
    assert!(
        blocker
            .line
            .starts_with("The provider `fake` is not installed."),
        "{}",
        blocker.line
    );
    assert!(
        checked.lines.iter().any(|line| line == "fake:"),
        "{:?}",
        checked.lines
    );
    assert!(
        checked.lines.iter().all(|line| !line.contains("(no key)")),
        "{:?}",
        checked.lines
    );
    // Enabled for this project, the same package is ready: the key the
    // injected environment holds answers.
    setup.write_project(&json!({"extensions": {"off": {"enabled": true}}}));
    let lines = setup.ready(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    assert!(lines.iter().any(|line| line == "fake:"), "{lines:?}");
}

#[test]
fn a_disabled_package_with_malformed_provider_json_is_informational() {
    // The session starts from the enabled set and skips the disabled
    // package before reading its provider file; doctor must not exit 1
    // for what the session never reads.
    let setup = Setup::new();
    setup.fake();
    setup.place(
        "off",
        &[provider(
            "bogus",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("off"),
    );
    fs::write(
        setup.home().join("extensions/off/providers/bogus.json"),
        "{",
    )
    .unwrap();
    setup.write_config(&json!({"model": "fake/m", "extensions": {"off": {"enabled": false}}}));
    let lines = setup.ready(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("extensions do not load:")),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line == "fake:"), "{lines:?}");
}

#[test]
fn an_empty_key_under_a_control_label_stays_on_one_line() {
    // Labels permit newlines and terminal escapes; the selected empty
    // label is interpolated three times, all cleaned.
    let setup = Setup::new();
    setup.fake();
    let label = "we\nird\u{1b}[2J";
    setup.write_config(&json!({"model": "fake/m", "providers": {"fake": {"credential": label}}}));
    setup.store("fake", label, "   ");
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::CredentialFailed);
    assert_eq!(line.lines().count(), 1, "{line:?}");
    assert!(!line.chars().any(|c| c.is_control()), "{line:?}");
    assert_eq!(line.matches("we ird [2J").count(), 3, "{line:?}");
}

#[test]
fn a_provider_with_no_label_gives_the_plain_login_line() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::CredentialMissing);
    assert_eq!(line, "no key for fake: run `fiber login fake`");
}

#[test]
fn a_selected_label_with_nothing_under_it_gives_the_as_line() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m", "providers": {"fake": {"credential": "work"}}}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::CredentialMissing);
    assert_eq!(
        line,
        "no key for fake under work: run `fiber login fake --as work`"
    );
    let checked = setup.check(&[], &env_of(&[]));
    assert!(
        checked
            .lines
            .iter()
            .any(|line| line.contains("* work") && line.contains("nothing stored or configured")),
        "{:?}",
        checked.lines
    );
}

#[test]
fn a_provider_that_reads_no_stored_credential_shows_its_label_s_source_and_never_blocks() {
    let setup = Setup::new();
    setup.profiled();
    // Keys stored under its labels anyway are never what it reads.
    setup.store("cloud", "work", "not-a-key");
    setup.store("cloud", "other", "not-a-key");
    let block = |lines: Vec<String>| -> Vec<String> {
        let at = lines.iter().position(|line| line == "cloud:").unwrap();
        lines.into_iter().skip(at).take(3).collect()
    };
    setup
        .write_config(&json!({"model": "cloud/m", "providers": {"cloud": {"credential": "work"}}}));
    assert_eq!(
        block(setup.ready(&[], &env_of(&[]))),
        [
            "cloud:",
            "  * work  cloud profile work, read by credential() (not checked)",
            "permissions: auto",
        ]
    );
    setup.write_config(&json!({"model": "cloud/m"}));
    assert_eq!(
        block(setup.ready(&[], &env_of(&[]))),
        [
            "cloud:",
            "  * default  the environment, read by credential() (not checked)",
            "permissions: auto",
        ]
    );
}

#[test]
fn a_blank_stored_key_gives_the_logout_then_login_line() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    setup.store("fake", "default", "   ");
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::CredentialFailed);
    assert_eq!(
        line,
        "the stored key for fake under default is empty: run `fiber logout fake --as default`, then `fiber login fake --as default`."
    );
}

#[test]
fn a_fifo_under_the_selected_label_cannot_be_used() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let dir = setup.home().join("credentials/fake");
    fs::create_dir_all(&dir).unwrap();
    fakes::fifo(dir.join("default"));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::ConfigInvalid);
    assert!(
        line.starts_with("the stored key for fake under default cannot be used:"),
        "{line}"
    );
    assert!(
        line.ends_with("Remove that path, then run `fiber login fake`."),
        "{line}"
    );
}

#[test]
fn a_configured_directory_reads_as_io_never_missing() {
    let setup = Setup::new();
    setup.fake();
    let keys = setup.home().join("keys");
    fs::create_dir_all(&keys).unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credentials": {"default": {"file": keys.to_str().unwrap()}}}},
    }));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(
        line.starts_with("the key for fake under default cannot be read:"),
        "{line}"
    );
    assert!(
        line.ends_with("Check that you can read that file, or run `fiber login fake`."),
        "{line}"
    );
    let session = failure(ErrorCode::IoFailed, "session text");
    let fixed = fix_at(session, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.code, ErrorCode::IoFailed);
    assert_eq!(fixed.message, format!("{line}\nsession text"));
    let missing = failure(ErrorCode::CredentialMissing, "session text");
    let kept = fix_at(missing, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(kept.message, "session text");
}

#[test]
fn an_unreadable_configured_key_file_reads_as_io() {
    let setup = Setup::new();
    setup.fake();
    let key = setup.home().join("keys").join("fake");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, "k").unwrap();
    let mut permissions = fs::metadata(&key).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o0);
    fs::set_permissions(&key, permissions).unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credentials": {"default": {"file": key.to_str().unwrap()}}}},
    }));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(line.contains("cannot be read:"), "{line}");
    let session = failure(ErrorCode::IoFailed, "session text");
    let fixed = fix_at(session, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.message, format!("{line}\nsession text"));
}

#[test]
fn an_invalid_label_gives_the_config_set_fix() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credential": "work.lock"}},
    }));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert_eq!(
        line,
        "the credential label work.lock for fake is not a valid label: set another with `fiber config set providers.\"fake\".credential work.lock`"
    );
    let session = failure(ErrorCode::InvalidArguments, "session text");
    let fixed = fix_at(session, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.code, ErrorCode::InvalidArguments);
    assert_eq!(fixed.message, format!("{line}\nsession text"));
}

#[test]
fn an_unselected_fifo_is_information_not_a_blocker() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    setup.store("fake", "default", "k");
    let dir = setup.home().join("credentials/fake");
    fs::create_dir_all(&dir).unwrap();
    fakes::fifo(dir.join("other"));
    // The label is configured so it lists (stored listing holds regular
    // files only); at `locate` the stored FIFO wins over the configured
    // file, and its plain-metadata check reports without reading a key.
    let key = setup.home().join("keys").join("other");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, "k").unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credentials": {"other": {"file": key.to_str().unwrap()}}}},
    }));
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("other") && line.contains("cannot be read:")),
        "{lines:?}"
    );
}

#[test]
fn an_unselected_unreadable_file_shows_its_origin_only() {
    let setup = Setup::new();
    setup.fake();
    let key = setup.home().join("keys").join("other");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, "k").unwrap();
    let mut permissions = fs::metadata(&key).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o0);
    fs::set_permissions(&key, permissions).unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credentials": {"other": {"file": key.to_str().unwrap()}}}},
    }));
    setup.store("fake", "default", "k");
    let lines = setup.ready(&[], &env_of(&[]));
    let row = lines.iter().find(|line| line.contains("other")).unwrap();
    assert!(row.contains(&key.to_string_lossy().to_string()), "{row}");
    assert!(!row.contains("cannot be read"), "{row}");
    assert!(!row.contains("(no key)"), "{row}");
}

#[test]
fn a_command_source_is_listed_but_never_runs() {
    let setup = Setup::new();
    setup.place(
        "fake",
        &[provider(
            "fake",
            &[model("m", "openai-responses", 1_000, None)],
            Some(json!({"command": ["op", "read", "fake"]})),
            None,
        )],
        manifest("fake"),
    );
    setup.write_config(&json!({"model": "fake/m"}));
    let marker = setup.home().join("marker");
    assert!(!marker.exists());
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("command op (not run)")),
        "{lines:?}"
    );
    assert!(!marker.exists());
}

#[test]
fn a_set_env_source_is_no_blocker() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let lines = setup.ready(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("FIBER_TEST_UNSET_KEY") && !line.contains("(no key)")),
        "{lines:?}"
    );
}

#[test]
fn a_thinking_suffix_names_a_level_the_model_takes() {
    let setup = Setup::new();
    setup.place(
        "fake",
        &[provider(
            "fake",
            &[model("m", "openai-responses", 1_000, Some(vec!["low"]))],
            None,
            None,
        )],
        manifest("fake"),
    );
    setup.write_config(&json!({"model": "fake/m:high"}));
    setup.store("fake", "default", "k");
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::InvalidArguments);
    assert!(
        line.contains("The thinking level `high` is not one model `fake/m` takes"),
        "{line}"
    );
    assert!(
        line.ends_with("Drop the `:<level>` suffix from `model`, or use a level it lists."),
        "{line}"
    );
    setup.write_config(&json!({"model": "fake/m:low"}));
    setup.ready(&[], &env_of(&[]));
}

#[test]
fn a_model_with_no_window_is_unknown() {
    let setup = Setup::new();
    setup.place(
        "fake",
        &[provider(
            "fake",
            &[model("m", "openai-responses", 0, None)],
            None,
            None,
        )],
        manifest("fake"),
    );
    setup.write_config(&json!({"model": "fake/m"}));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::NoModel);
    assert!(line.contains("has no model `m`"), "{line}");
}

#[test]
fn a_windowless_model_blocks_through_the_window_check() {
    use extensions::{Model, Providers};

    // The loader leaves windowless models out, so `startup` never reaches
    // the window refusal through a fixture: pin the shared refusal through
    // the check itself, for a missing window and a zero one.
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let (_, project) = crate::project_of(&setup.home(), &setup.workspace()).unwrap();
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project,
        overrides: Vec::new(),
    })
    .unwrap();
    let (providers, _) = Providers::load(&setup.home()).unwrap();
    let workspace = setup.workspace();
    for window in [None, Some(0)] {
        let provider = ProviderData {
            name: "fake".to_owned(),
            credential: None,
            credential_name: None,
            headers: Default::default(),
            placeholders: Default::default(),
            models: Vec::new(),
            reviewer_model: None,
            login: None,
            credential_label: None,
        };
        let model_data = ModelData {
            id: "m".to_owned(),
            name: None,
            protocol: Protocol::OpenaiResponses,
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            compat: Default::default(),
            deferred_tools: false,
            extra_body: Default::default(),
            context_window: window,
            max_output_tokens: None,
            input: Vec::new(),
            cost: None,
            subscription: false,
            web_search: None,
            thinking_levels: Vec::new(),
            thinking_default: None,
            prompt_addendum: None,
        };
        let model = Model {
            provider: &provider,
            model: &model_data,
            thinking: None,
        };
        let mut checked = Checked {
            lines: Vec::new(),
            blocker: None,
            chosen: None,
            workspace: &workspace,
            config: &config,
            inventory: &providers,
            enabled: &providers,
            env: &|_: &str| None::<String>,
            has_userinfo: &|_: &str| false,
        };
        checked.window(&model);
        let blocker = checked
            .blocker
            .expect("a windowless model blocks with the shared refusal");
        assert_eq!(blocker.code, ErrorCode::NoModel);
        assert!(!blocker.key);
        assert!(
            blocker.line.contains("declares no `context_window`"),
            "{}",
            blocker.line
        );
        assert_eq!(checked.lines, [blocker.line]);
    }
}

#[test]
fn an_invalid_configuration_blocks_every_later_section() {
    let setup = Setup::new();
    setup.fake();
    fs::write(setup.home().join("config.json"), "{oops").unwrap();
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert!(
        blocker.line.starts_with("configuration does not load:"),
        "{}",
        blocker.line
    );
    assert!(
        blocker.line.ends_with("Fix the file and try again."),
        "{}",
        blocker.line
    );
    assert_eq!(
        checked.lines,
        vec![blocker.line.clone()],
        "no model, provider, MCP or secret line follows a configuration blocker"
    );
}

#[test]
fn an_unreadable_configuration_names_reading() {
    let setup = Setup::new();
    setup.fake();
    let path = setup.home().join("config.json");
    fs::write(&path, "{}").unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o0);
    fs::set_permissions(&path, permissions).unwrap();
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert!(
        blocker.line.starts_with("configuration does not load:"),
        "{}",
        blocker.line
    );
    assert!(
        blocker.line.ends_with("Check that you can read that file."),
        "{}",
        blocker.line
    );
}

#[test]
fn a_non_json_manifest_names_removal() {
    let setup = Setup::new();
    let dir = setup.home().join("extensions").join("broken");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("extension.json"), "{oops").unwrap();
    // The manifest is not JSON, so the record helper cannot read it:
    // hand-place a healthy record naming the extension.
    fs::write(
        dir.join(".fiber.json"),
        json!({"name": "broken", "version": "v1.0.0", "requested": true,
               "source": {"path": "/p"}})
        .to_string(),
    )
    .unwrap();
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert!(
        blocker.line.starts_with("extensions do not load:"),
        "{}",
        blocker.line
    );
    assert!(
        blocker.line.ends_with(
            "Remove the extension it names with `fiber extension remove <name>`, then install it again."
        ),
        "{}",
        blocker.line
    );
}

#[test]
fn a_required_server_with_both_shapes_blocks_without_its_url() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {"s": {
            "command": "run",
            "url": "https://example.invalid/mcp",
            "required": true,
        }}},
    }));
    setup.store("fake", "default", "k");
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert_eq!(blocker.code, ErrorCode::ConfigInvalid);
    assert_eq!(
        blocker.line,
        "The required MCP server `s` declares both `command` and `url`, so Fiber cannot start it. Keep the one you mean."
    );
}

#[test]
fn a_required_server_with_userinfo_in_its_url_blocks_without_it() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {"s": {
            "url": "https://user:pw@example.invalid/mcp",
            "required": true,
        }}},
    }));
    setup.store("fake", "default", "k");
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert_eq!(blocker.code, ErrorCode::ConfigInvalid);
    assert_eq!(
        blocker.line,
        "The required MCP server `s` has a user name or password in its `url`, so Fiber cannot start it. Take them out of the URL."
    );
    assert!(
        checked
            .lines
            .iter()
            .all(|line| !line.contains("user") || line.contains("user name")),
        "{:?}",
        checked.lines
    );
    assert!(
        checked
            .lines
            .iter()
            .all(|line| !line.contains("example.invalid") && !line.contains("pw@")),
        "{:?}",
        checked.lines
    );
}

#[test]
fn a_required_server_with_a_misnamed_secret_blocks_without_it() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {"s": {
            "command": "run",
            "required": true,
            "env": {"K": {"secret": "other.KEY"}},
        }}},
    }));
    setup.store("fake", "default", "k");
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert_eq!(blocker.code, ErrorCode::ConfigInvalid);
    assert_eq!(
        blocker.line,
        "The required MCP server `s` names the secret `other.KEY`, which must be `mcp.s.<KEY>`, so Fiber cannot start it. Name the secret after the server."
    );
}

#[test]
fn a_non_required_bad_server_is_ready_and_a_model_blocker_wins() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {"s": {
            "command": "run",
            "url": "https://example.invalid/mcp",
        }}},
    }));
    setup.store("fake", "default", "k");
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines.iter().any(|line| line == "MCP servers: 1"),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("example.invalid")),
        "{lines:?}"
    );
    // A model blocker outranks the MCP shapes: only it blocks.
    setup.write_config(&json!({
        "mcp": {"servers": {"s": {
            "command": "run",
            "url": "https://example.invalid/mcp",
            "required": true,
        }}},
    }));
    let (code, line) = setup.blocked(&[], &env_of(&[]));
    assert_eq!(code, ErrorCode::NoModel);
    assert!(line.contains("No model was chosen"), "{line}");
}

#[test]
fn an_mcp_blocker_is_not_a_session_fix() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({
        "model": "fake/m",
        "mcp": {"servers": {"s": {
            "command": "run",
            "url": "https://example.invalid/mcp",
            "required": true,
        }}},
    }));
    setup.store("fake", "default", "k");
    let session = failure(ErrorCode::ConfigInvalid, "session text");
    let fixed = fix_at(session, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.message, "session text");
}

#[test]
fn a_missing_scripted_file_blocks_as_io() {
    let setup = Setup::new();
    setup.write_config(&json!({"model": "scripted/missing.lua"}));
    let checked = setup.check(&[], &env_of(&[]));
    let blocker = checked.blocker.unwrap();
    assert_eq!(blocker.code, ErrorCode::IoFailed);
    assert!(blocker.line.starts_with("The script `"), "{}", blocker.line);
    assert!(blocker.line.contains("missing.lua"), "{}", blocker.line);
    // The blocker prints under the model line.
    assert!(
        checked.lines[0].starts_with("model: "),
        "{:?}",
        checked.lines
    );
    assert_eq!(checked.lines[1], blocker.line, "{:?}", checked.lines);
    assert!(blocker.line.contains("missing.lua"), "{}", blocker.line);
    assert!(
        blocker.line.ends_with(
            "Check that path, resolved against the workspace, and that you can read the file."
        ),
        "{}",
        blocker.line
    );
}

#[test]
fn a_scripted_directory_or_unreadable_script_blocks_as_io() {
    for name in ["dir.lua", "dark.lua"] {
        let setup = Setup::new();
        if name == "dir.lua" {
            fs::create_dir_all(setup.workspace().join(name)).unwrap();
        } else {
            let path = setup.workspace().join(name);
            fs::write(&path, "return {}").unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o0);
            fs::set_permissions(&path, permissions).unwrap();
        }
        setup.write_config(&json!({"model": format!("scripted/{name}")}));
        let checked = setup.check(&[], &env_of(&[]));
        let blocker = checked.blocker.unwrap();
        assert_eq!(blocker.code, ErrorCode::IoFailed, "{name}");
        assert!(blocker.line.contains(name), "{name}: {}", blocker.line);
    }
}

#[test]
fn a_scripted_fifo_is_not_checked_and_never_waits() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "scripted/pipe.lua"}));
    fakes::fifo(setup.workspace().join("pipe.lua"));
    // The scripted reference reads no credential: the installed
    // provider's missing key blocks nothing.
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines.iter().any(|line| line.contains("scripted/pipe.lua")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("no key")),
        "{lines:?}"
    );
}

#[test]
fn a_malformed_scripted_file_is_not_checked() {
    let setup = Setup::new();
    setup.write_config(&json!({"model": "scripted/bad.lua"}));
    fs::write(setup.workspace().join("bad.lua"), "this is not lua {{").unwrap();
    setup.ready(&[], &env_of(&[]));
}

#[test]
fn labels_come_sorted_with_the_star_only_on_the_selected() {
    let setup = Setup::new();
    setup.fake();
    let other = setup.home().join("keys").join("other");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, "k").unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {
            "fake": {
                "credential": "zebra",
                "credentials": {"other": {"file": other.to_str().unwrap()}},
            },
        },
    }));
    setup.store("fake", "zebra", "k");
    setup.store("fake", "apple", "k");
    let lines = setup.ready(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    let rows: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| line.starts_with("  "))
        .collect();
    assert_eq!(rows.len(), 4, "{lines:?}");
    assert!(rows[0].starts_with("    apple"), "{rows:?}");
    assert!(
        rows[1].starts_with("    default  environment variable FIBER_TEST_UNSET_KEY"),
        "{rows:?}"
    );
    assert!(rows[2].starts_with("    other"), "{rows:?}");
    assert!(rows[3].starts_with("  * zebra"), "{rows:?}");
}

#[test]
fn two_providers_show_two_blocks() {
    let setup = Setup::new();
    setup.place(
        "a",
        &[provider(
            "a",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("a"),
    );
    setup.place(
        "b",
        &[provider(
            "b",
            &[model("n", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("b"),
    );
    setup.write_config(&json!({"model": "a/m"}));
    setup.store("a", "default", "k");
    setup.store("b", "default", "k");
    let lines = setup.ready(&[], &env_of(&[]));
    let blocks: Vec<&str> = lines
        .iter()
        .map(String::as_str)
        .filter(|line| *line == "a:" || *line == "b:")
        .collect();
    assert_eq!(blocks, vec!["a:", "b:"]);
}

#[test]
fn control_characters_in_names_stay_on_their_line() {
    let setup = Setup::new();
    setup.place(
        "bad\nname",
        &[provider(
            "bad\nname",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("bad\nname"),
    );
    setup.write_config(&json!({"model": "bad\nname/m"}));
    let checked = setup.check(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    for line in &checked.lines {
        assert!(!line.contains('\n'), "{line:?}");
        assert!(!line.contains('\u{1b}'), "{line:?}");
    }
    assert!(
        checked.lines.iter().any(|line| line.contains("bad name:")),
        "{:?}",
        checked.lines
    );
}

#[test]
fn a_newline_and_an_escape_in_labels_print_on_one_line() {
    // A label is a file name, and file names may hold a newline: both it
    // and a configured label holding an escape print on one line each,
    // and the line count and order equal the literal's.
    let setup = Setup::new();
    setup.fake();
    let key = setup.home().join("keys").join("esc");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, "k").unwrap();
    setup.write_config(&json!({
        "model": "fake/m",
        "providers": {"fake": {"credentials": {"\u{1b}[2J": {"file": key.to_str().unwrap()}}}},
    }));
    let dir = setup.home().join("credentials/fake");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("work\nhub: running"), "k").unwrap();
    let lines = setup.ready(&[], &env_of(&[("FIBER_TEST_UNSET_KEY", "k")]));
    let stored = "credentials/fake/work hub: running";
    assert_eq!(
        lines,
        vec![
            "model: fake/m (provider fake)".to_owned(),
            "fake:".to_owned(),
            format!("    {:<17}  file {}", " [2J", key.display()),
            "  * default            environment variable FIBER_TEST_UNSET_KEY".to_owned(),
            format!("    work hub: running  stored in {stored}"),
            "permissions: auto".to_owned(),
            "MCP servers: 0".to_owned(),
        ],
        "{lines:?}"
    );
    for line in &lines {
        assert!(!line.contains('\n'), "{line:?}");
        assert!(!line.contains('\u{1b}'), "{line:?}");
    }
}

#[test]
fn a_ready_model_shows_no_other_providers_note() {
    let setup = Setup::new();
    setup.place(
        "a",
        &[provider(
            "a",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("a"),
    );
    setup.place(
        "other",
        &[provider(
            "other",
            &[model("n", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("other"),
    );
    setup.write_config(&json!({"model": "a/m"}));
    setup.store("a", "default", "k");
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("(no key)") && !line.contains("(empty)")),
        "{lines:?}"
    );
}

#[test]
fn another_providers_fifo_under_its_label_never_waits() {
    let setup = Setup::new();
    setup.place(
        "a",
        &[provider(
            "a",
            &[model("m", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("a"),
    );
    setup.place(
        "other",
        &[provider(
            "other",
            &[model("n", "openai-responses", 1_000, None)],
            None,
            None,
        )],
        manifest("other"),
    );
    setup.write_config(&json!({"model": "a/m"}));
    setup.store("a", "default", "k");
    let dir = setup.home().join("credentials/other");
    fs::create_dir_all(&dir).unwrap();
    fakes::fifo(dir.join("default"));
    let lines = setup.ready(&[], &env_of(&[]));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("other:") || line.contains("other")),
        "{lines:?}"
    );
}

#[test]
fn an_enabled_init_marks_not_checked_not_a_blocker() {
    let setup = Setup::new();
    // No provider data file: the provider is absent from data, but the
    // enabled extension declared it with an `init.lua` that could
    // register it. Syntactically invalid Lua proves the load never runs
    // it, and the marker its code would write stays absent.
    let dir = setup.home().join("extensions").join("coded");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "coded", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
               "providers": {"coded": ["http://127.0.0.1:1/v1"]}})
        .to_string(),
    )
    .unwrap();
    fs::write(dir.join("init.lua"), "this is not lua (((").unwrap();
    fakes::write_record(&dir);
    let marker = setup.home().join("marker");
    setup.write_config(&json!({"model": "coded/m"}));
    let checked = setup.check(&[], &env_of(&[]));
    assert!(
        checked.blocker.is_none(),
        "{:?}",
        checked.blocker.map(|b| b.line)
    );
    assert!(
        checked
            .lines
            .iter()
            .any(|line| line == "coded: registered by extension code, not checked"),
        "{:?}",
        checked.lines
    );
    assert!(!marker.exists());
}

#[test]
fn a_provider_script_marks_not_checked_not_a_blocker() {
    let setup = Setup::new();
    let dir = setup.home().join("extensions").join("coded");
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "coded", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
               "providers": {"coded": ["http://127.0.0.1:1/v1"]}})
        .to_string(),
    )
    .unwrap();
    fs::write(dir.join("providers/coded.lua"), "-- never run by the check").unwrap();
    fakes::write_record(&dir);
    setup.write_config(&json!({"model": "coded/m"}));
    let checked = setup.check(&[], &env_of(&[]));
    assert!(
        checked.blocker.is_none(),
        "{:?}",
        checked.blocker.map(|b| b.line)
    );
    assert!(
        checked
            .lines
            .iter()
            .any(|line| line == "coded: registered by extension code, not checked"),
        "{:?}",
        checked.lines
    );
}

#[test]
fn an_init_only_extension_marks_its_provider_not_checked() {
    // An extension with only `init.lua` is still code, so its undeclared
    // provider is a candidate without running anything.
    let setup = Setup::new();
    let dir = setup.home().join("extensions").join("coded");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "coded", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
               "providers": {"coded": ["http://127.0.0.1:1/v1"]}})
        .to_string(),
    )
    .unwrap();
    fs::write(dir.join("init.lua"), "-- never run by the check").unwrap();
    fakes::write_record(&dir);
    setup.write_config(&json!({"model": "coded/m"}));
    let checked = setup.check(&[], &env_of(&[]));
    assert!(
        checked.blocker.is_none(),
        "{:?}",
        checked.blocker.map(|b| b.line)
    );
    assert!(
        checked
            .lines
            .iter()
            .any(|line| line == "coded: registered by extension code, not checked"),
        "{:?}",
        checked.lines
    );
}

#[test]
fn with_fix_annotates_only_a_missing_key() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let missing = failure(ErrorCode::CredentialMissing, "session text");
    let fixed = fix_at(missing, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(fixed.code, ErrorCode::CredentialMissing);
    assert_eq!(
        fixed.message,
        "no key for fake: run `fiber login fake`\nsession text"
    );
    // An override can resolve the model: then the failure is unchanged.
    setup.store("fake", "default", "k");
    let missing = failure(ErrorCode::CredentialMissing, "session text");
    let kept = fix_at(
        missing,
        &["model=fake/m".to_owned()],
        &setup.home(),
        &setup.workspace(),
        &|_| None,
    );
    assert_eq!(kept.message, "session text");
}

#[test]
fn with_fix_leaves_a_failed_key_and_a_missing_provider() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    setup.store("fake", "default", "   ");
    let mut failed = failure(ErrorCode::CredentialFailed, "session text");
    failed.provider = Some(contract::shapes::ProviderFailure {
        name: "fake".to_owned(),
        status: None,
        message: "the extension refused".to_owned(),
    });
    let kept = fix_at(failed, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(kept.message, "session text");
    assert!(kept.provider.is_some());
    let missing = failure(ErrorCode::ExtensionMissing, "session text");
    let kept = fix_at(missing, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(kept.message, "session text");
}

#[test]
fn with_fix_leaves_an_unseen_cause() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    setup.store("fake", "default", "k");
    let seen = failure(ErrorCode::BudgetExceeded, "session text");
    let kept = fix_at(seen, &[], &setup.home(), &setup.workspace(), &|_| None);
    assert_eq!(kept.message, "session text");
}

#[test]
fn with_fix_diagnoses_the_overridden_model() {
    let setup = Setup::new();
    setup.fake();
    setup.write_config(&json!({"model": "fake/m"}));
    let missing = failure(ErrorCode::CredentialMissing, "session text");
    let fixed = fix_at(
        missing,
        &["providers.\"fake\".credential=work".to_owned()],
        &setup.home(),
        &setup.workspace(),
        &|_| None,
    );
    assert_eq!(fixed.code, ErrorCode::CredentialMissing);
    assert_eq!(
        fixed.message,
        "no key for fake under work: run `fiber login fake --as work`\nsession text"
    );
}
