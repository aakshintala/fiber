//! `fiber config get|set`: `get` prints the effective value and its layer,
//! `set` writes one key in one layer's file, against a temporary Fiber home.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use config::Layer;
use contract::ErrorCode;
use contract::shapes::Failure;
use serde_json::json;

use super::{run_get, run_set};

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-config-cli"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.workspace()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn write(&self, file: &Path, value: &serde_json::Value) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, value.to_string()).unwrap();
    }

    /// Runs `get` with in-memory writers.
    fn get(&self, key: &str) -> (Result<(), Failure>, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run_get(&self.home(), &self.workspace(), key, &mut out, &mut err);
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn set(&self, layer: Layer, key: &str, value: &str) -> Result<(), Failure> {
        run_set(&self.home(), &self.workspace(), layer, key, value)
    }
}

#[test]
fn get_of_a_set_key_prints_compact_json_and_the_file() {
    let setup = Setup::new();
    setup.write(&setup.home().join("config.json"), &json!({"model": "a/b"}));
    let (result, out, err) = setup.get("model");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
}

#[test]
fn get_of_a_default_prints_the_built_in_defaults() {
    let setup = Setup::new();
    let (result, out, err) = setup.get("handoff.enabled");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(out, "true from the built-in defaults\n");
}

#[test]
fn a_project_value_beats_a_repository_value_and_names_the_project_file() {
    let setup = Setup::new();
    setup.write(
        &setup.workspace().join(".fiber/config.json"),
        &json!({"handoff": {"tokens": 50}}),
    );
    let (_, project) = crate::project_of(&setup.home(), &setup.workspace()).unwrap();
    let file = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config.json");
    setup.write(&file, &json!({"handoff": {"tokens": 100}}));
    let (result, out, err) = setup.get("handoff.tokens");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(out, format!("100 from {}\n", file.display()));
}

#[test]
fn get_of_an_unknown_key_is_a_usage_error_naming_the_key() {
    let setup = Setup::new();
    let (result, out, err) = setup.get("no.such.key");
    let e = result.unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(e.message.contains("`no.such.key`"), "{}", e.message);
    assert_eq!(out, "");
    assert_eq!(err, "");
}

#[test]
fn get_of_an_unset_key_without_a_default_names_it_on_stderr_and_succeeds() {
    let setup = Setup::new();
    let (result, out, err) = setup.get("reviewer.model");
    result.unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "`reviewer.model` is not set.\n");
}

#[test]
fn set_parses_a_number_and_a_bare_string() {
    let setup = Setup::new();
    setup
        .set(Layer::Global, "handoff.tokens", "200000")
        .unwrap();
    setup.set(Layer::Global, "model", "openai/gpt-5.6").unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        written,
        json!({"handoff": {"tokens": 200000}, "model": "openai/gpt-5.6"})
    );
}

#[test]
fn set_of_a_person_only_key_at_the_repository_layer_is_a_usage_error() {
    let setup = Setup::new();
    let e = setup
        .set(Layer::Repository, "session.idle_exit_ms", "60000")
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(
        e.message.contains("`session.idle_exit_ms`"),
        "{}",
        e.message
    );
    assert!(!setup.workspace().join(".fiber/config.json").exists());
}

/// The child's marker: set, the test runs one wrapper and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

/// Spawns this test binary filtered to `test`, with `FIBER_HOME` and the
/// working directory set, and waits for it under a deadline.
fn spawn_child(name: &str, test: &str, setup: &Setup) -> std::process::Output {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{name}::{test}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FIBER_HOME", setup.home())
        .env(CHILD, "1")
        .current_dir(setup.workspace())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let Ok(output) = rx.recv_timeout(CHILD_DEADLINE) else {
        fakes::kill_pid(pid, "KILL").unwrap();
        panic!("waited {CHILD_DEADLINE:?} for the config child to exit");
    };
    output
}

fn child_name() -> String {
    module_path!().split_once("::").unwrap().1.to_owned()
}

#[test]
fn config_get_exits_zero_and_prints_the_value_and_its_layer() {
    // Runs in a child with a Fiber home holding a model, so the exit code is
    // `config_get`'s own. The parent checks the code and the line: a code
    // alone would not catch a `get` that returns 0 without printing.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::config_get("model"));
    }
    let setup = Setup::new();
    setup.write(&setup.home().join("config.json"), &json!({"model": "a/b"}));
    let output = spawn_child(
        &child_name(),
        "config_get_exits_zero_and_prints_the_value_and_its_layer",
        &setup,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )),
        "{stdout:?}"
    );
}

#[test]
fn config_get_of_an_unknown_key_exits_two() {
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::config_get("no.such.key"));
    }
    let setup = Setup::new();
    let output = spawn_child(
        &child_name(),
        "config_get_of_an_unknown_key_exits_two",
        &setup,
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn config_set_exits_zero_and_writes_the_file() {
    // Runs in a child with a temporary Fiber home, so the exit code is
    // `config_set`'s own and the write lands in the temporary home.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::config_set(Layer::Global, "handoff.tokens", "200000"));
    }
    let setup = Setup::new();
    let output = spawn_child(
        &child_name(),
        "config_set_exits_zero_and_writes_the_file",
        &setup,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"handoff": {"tokens": 200000}}));
}

/// The model check's fixture: installs the extension `extension` registering
/// the provider `name` serving `models`, as `models_tests` builds them.
fn install(home: &Path, extension: &str, name: &str, models: &serde_json::Value) {
    let dir = home.join("extensions").join(extension);
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join(format!("{name}.json")),
        json!({"name": name, "models": models}).to_string(),
    )
    .unwrap();
}

fn model(id: &str) -> serde_json::Value {
    json!({"id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1"})
}

fn models(ids: &[&str]) -> serde_json::Value {
    serde_json::Value::Array(ids.iter().map(|id| model(id)).collect())
}

#[test]
fn an_exact_model_match_is_accepted() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big", "small"]));
    setup.set(Layer::Global, "model", "acme/big").unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/big"}));
}

#[test]
fn a_thinking_level_suffix_is_accepted_and_written_as_typed() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    setup.set(Layer::Global, "model", "acme/big:high").unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/big:high"}));
}

#[test]
fn a_bare_id_with_one_provider_is_accepted() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    install(&setup.home(), "other", "other", &models(&["tiny"]));
    setup.set(Layer::Global, "model", "big").unwrap();
}

#[test]
fn a_bare_id_in_two_providers_is_model_ambiguous_listing_both() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    install(&setup.home(), "other", "other", &models(&["big"]));
    let e = setup.set(Layer::Global, "model", "big").unwrap_err();
    assert_eq!(e.code, ErrorCode::ModelAmbiguous);
    assert!(e.message.contains("acme/big"), "{}", e.message);
    assert!(e.message.contains("other/big"), "{}", e.message);
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn an_installed_provider_missing_the_model_is_no_model_with_the_closest() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["gpt-5", "gpt-4"]));
    let e = setup.set(Layer::Global, "model", "acme/gtp-5").unwrap_err();
    assert_eq!(e.code, ErrorCode::NoModel);
    assert_eq!(
        e.message,
        "No installed model matches `acme/gtp-5`. \
         Closest: `acme/gpt-5`, `acme/gpt-4`. Run `fiber models` to list them."
    );
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn the_closest_names_up_to_three_references() {
    let setup = Setup::new();
    install(
        &setup.home(),
        "acme",
        "acme",
        &models(&["aaa", "aab", "abb", "bbb", "ccc"]),
    );
    let e = setup.set(Layer::Global, "model", "zzz").unwrap_err();
    assert_eq!(e.code, ErrorCode::NoModel);
    let refs = e.message.matches("`acme/").count();
    assert_eq!(refs, 3, "{}", e.message);
}

#[test]
fn a_bare_id_no_provider_has_is_no_model() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    let e = setup.set(Layer::Global, "model", "nope").unwrap_err();
    assert_eq!(e.code, ErrorCode::NoModel);
    assert!(e.message.contains("`nope`"), "{}", e.message);
    assert!(e.message.contains("Run `fiber models`"), "{}", e.message);
}

#[test]
fn a_model_id_holding_a_colon_matches_exactly_before_any_strip() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["x:high"]));
    setup.set(Layer::Global, "model", "acme/x:high").unwrap();
}

#[test]
fn a_reference_naming_no_installed_provider_is_accepted() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    setup.set(Layer::Global, "model", "zz/top").unwrap();
    setup.set(Layer::Global, "model", "zz/top:high").unwrap();
}

#[test]
fn with_no_provider_any_reference_is_accepted() {
    let setup = Setup::new();
    setup.set(Layer::Global, "model", "zz/top").unwrap();
    setup.set(Layer::Global, "model", "bare").unwrap();
}

#[test]
fn a_provider_with_an_empty_list_accepts_its_own_references_and_any_bare_id() {
    let setup = Setup::new();
    install(
        &setup.home(),
        "acme",
        "acme",
        &serde_json::Value::Array(Vec::new()),
    );
    setup.set(Layer::Global, "model", "acme/anything").unwrap();
    setup.set(Layer::Global, "model", "whatever").unwrap();
}

#[test]
fn a_suffixed_reference_to_a_missing_model_is_no_model() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    let e = setup
        .set(Layer::Global, "model", "acme/nope:high")
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NoModel);
}

#[test]
fn a_non_model_key_skips_the_model_check() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    setup.set(Layer::Global, "roles.fast", "zz/top").unwrap();
}
