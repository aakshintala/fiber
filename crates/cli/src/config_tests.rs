//! `fiber config get|set`: `get` prints the effective value and its layer,
//! `set` writes one key in one layer's file, against a temporary Fiber home.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use crate::test_support::{approve_repo, declare_repo, repo_data, repo_manifest, write_record};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use config::Layer;
use contract::ErrorCode;
use fakes::Deadline;
use serde_json::json;

use super::{run_get, run_set};
use crate::CliError;

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
    fn get(&self, key: &str) -> (Result<(), CliError>, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run_get(&self.home(), &self.workspace(), key, &mut out, &mut err);
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn set(&self, layer: Layer, key: &str, value: &str) -> Result<(), CliError> {
        run_set(
            &self.home(),
            &self.workspace(),
            layer,
            key,
            value,
            &mut std::io::sink(),
            &*fakes::clock::FakeClock::new(),
        )
        .map(|_| ())
    }

    /// Runs `set` with stderr captured: its result and what it warned.
    fn set_capturing(
        &self,
        layer: Layer,
        key: &str,
        value: &str,
    ) -> (Result<(), CliError>, String) {
        let mut err = Vec::new();
        let result = run_set(
            &self.home(),
            &self.workspace(),
            layer,
            key,
            value,
            &mut err,
            &*fakes::clock::FakeClock::new(),
        )
        .map(|_| ());
        (result, String::from_utf8(err).unwrap())
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
fn get_of_reviewer_context_prints_both_layers_notes_with_no_layer_line() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join("config.json"),
        &json!({"reviewer": {"context": "Our org is acme."}}),
    );
    let (_, project) = crate::project_of(&setup.home(), &setup.workspace()).unwrap();
    setup.write(
        &setup
            .home()
            .join("projects")
            .join(project.as_str())
            .join("config.json"),
        &json!({"reviewer": {"context": "Never touch infra/prod."}}),
    );
    let (result, out, err) = setup.get("reviewer.context");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        "## Notes that hold everywhere\n\nOur org is acme.\n\n\
         ## Notes for this project\n\nNever touch infra/prod.\n"
    );
}

#[test]
fn get_of_an_unset_reviewer_context_names_it_on_stderr_and_succeeds() {
    let setup = Setup::new();
    let (result, out, err) = setup.get("reviewer.context");
    result.unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "`reviewer.context` is not set.\n");
}

#[test]
fn get_of_an_unknown_key_is_a_usage_error_naming_the_key() {
    let setup = Setup::new();
    let (result, out, err) = setup.get("no.such.key");
    let e = result.unwrap_err();
    assert!(
        matches!(e, CliError::ConfigKeyUnknown { ref key } if key == "no.such.key"),
        "{e:?}"
    );
    assert_eq!(out, "");
    assert_eq!(err, "");
}

#[test]
fn get_with_an_unknown_key_prints_its_notice_as_one_line() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join("config.json"),
        &json!({"model": "a/b", "frobnicate": 1}),
    );
    let (result, out, err) = setup.get("model");
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
    assert_eq!(
        err,
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n",
            setup.home().join("config.json").display()
        )
    );
}

#[test]
fn get_prints_no_provider_notice_when_loading_them_fails() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join("config.json"),
        &json!({"model": "a/b", "frobnicate": 1}),
    );
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    fs::write(
        setup.home().join("extensions/acme/providers/acme.json"),
        "not json",
    )
    .unwrap();
    let (result, out, err) = setup.get("model");
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "\"a/b\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
    assert_eq!(
        err,
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n",
            setup.home().join("config.json").display()
        )
    );
}

#[test]
fn set_of_a_non_model_key_prints_the_config_notice() {
    let setup = Setup::new();
    setup.write(&setup.home().join("config.json"), &json!({"frobnicate": 1}));
    let (result, err) = setup.set_capturing(Layer::Global, "handoff.tokens", "200000");
    result.unwrap();
    assert_eq!(
        err,
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n",
            setup.home().join("config.json").display()
        )
    );
}

#[test]
fn set_model_without_a_cached_list_prints_the_config_notice() {
    let setup = Setup::new();
    setup.write(&setup.home().join("config.json"), &json!({"frobnicate": 1}));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/big");
    result.unwrap();
    assert_eq!(
        err,
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n",
            setup.home().join("config.json").display()
        )
    );
}

#[test]
fn set_model_without_a_cached_list_ignores_an_unreadable_project_file() {
    let setup = Setup::new();
    let (_, project) = crate::project_of(&setup.home(), &setup.workspace()).unwrap();
    let file = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config.json");
    setup.write(&file, &json!({"frobnicate": 1}));
    fs::write(&file, "not json").unwrap();
    // The notices load fails, so nothing prints, and the set still succeeds.
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/big");
    result.unwrap();
    assert_eq!(err, "");
}

#[test]
fn set_model_with_an_unknown_key_prints_its_notice_first() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    setup.write(&setup.home().join("config.json"), &json!({"frobnicate": 1}));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/big");
    result.unwrap();
    assert_eq!(
        err,
        format!(
            "{}: ignored `frobnicate`, which this Fiber does not know.\n",
            setup.home().join("config.json").display()
        )
    );
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
    assert!(matches!(e, CliError::Config(_)), "{e:?}");
    assert_eq!(e.code(), ErrorCode::Usage);
    assert!(e.to_string().contains("`session.idle_exit_ms`"), "{e}");
    assert!(!setup.workspace().join(".fiber/config.json").exists());
}

/// The child's marker: set, the test runs one wrapper and exits with its code.
const CHILD: &str = "FIBER_CLI_TEST_CHILD";

/// How long the re-run child may run before the test kills its group and
/// fails: half of nextest's per-test timeout less the cleanup bound.
const CHILD_DEADLINE: Duration = Duration::from_secs(50);

/// Runs this test binary filtered to `test`, with `FIBER_HOME` and the
/// working directory set, and waits for it under a deadline.
#[track_caller]
fn spawn_child(name: &str, test: &str, setup: &Setup) -> std::process::Output {
    fakes::rerun_prepared(
        &format!("{name}::{test}"),
        &[(CHILD, "1")],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command
                .env("FIBER_HOME", setup.home())
                .current_dir(setup.workspace());
        },
    )
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
        std::process::exit(super::config_set(
            Layer::Global,
            "handoff.tokens",
            "200000",
            &fakes::clock::SystemClock,
        ));
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
    let mut urls: Vec<&str> = models
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("base_url").and_then(|u| u.as_str()))
        .collect();
    urls.sort();
    urls.dedup();
    let mut manifest = json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] =
        serde_json::Value::Object([(name.to_owned(), json!(urls))].into_iter().collect());
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join(format!("{name}.json")),
        json!({"name": name, "models": models}).to_string(),
    )
    .unwrap();
}

fn model(id: &str) -> serde_json::Value {
    json!({"id": id, "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000})
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
    assert!(matches!(e, CliError::ModelAmbiguous(_)), "{e:?}");
    assert!(e.to_string().contains("acme/big"), "{e}");
    assert!(e.to_string().contains("other/big"), "{e}");
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn an_installed_provider_missing_the_model_is_no_model_with_the_closest() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["gpt-5", "gpt-4"]));
    let e = setup.set(Layer::Global, "model", "acme/gtp-5").unwrap_err();
    assert!(matches!(e, CliError::ModelNone { .. }), "{e:?}");
    assert_eq!(
        e.to_string(),
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
    assert!(matches!(e, CliError::ModelNone { .. }), "{e:?}");
    let refs = e.to_string().matches("`acme/").count();
    assert_eq!(refs, 3, "{e}");
}

#[test]
fn a_bare_id_no_provider_has_is_no_model() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    let e = setup.set(Layer::Global, "model", "nope").unwrap_err();
    assert!(matches!(e, CliError::ModelNone { .. }), "{e:?}");
    assert!(e.to_string().contains("`nope`"), "{e}");
    assert!(e.to_string().contains("Run `fiber models`"), "{e}");
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
    assert!(matches!(e, CliError::ModelNone { .. }), "{e:?}");
}

#[test]
fn a_non_model_key_skips_the_model_check() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    setup.set(Layer::Global, "roles.fast", "zz/top").unwrap();
}

#[test]
fn distance_pins_known_edit_counts() {
    assert_eq!(super::distance("", "ab"), 2);
    assert_eq!(super::distance("abc", ""), 3);
    assert_eq!(super::distance("kitten", "sitting"), 3);
    assert_eq!(super::distance("ab", "ba"), 2);
}

#[test]
fn distance_ranks_a_shared_prefix_closer_than_a_single_letter() {
    assert_eq!(super::distance("abc", "ab"), 1);
    assert_eq!(super::distance("abc", "a"), 2);
    assert!(super::distance("abc", "ab") < super::distance("abc", "a"));
}

/// As [`install`], with model `m` at `https://{workspace}/v1` read through
/// `placeholders`: no `env` naming means the process environment can never
/// leak into a test using it.
fn install_host(home: &Path, extension: &str, name: &str, placeholders: serde_json::Value) {
    let dir = home.join("extensions").join(extension);
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = serde_json::Value::Object(
        [(name.to_owned(), json!(["https://{workspace}/v1"]))]
            .into_iter()
            .collect(),
    );
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join(format!("{name}.json")),
        json!({
            "name": name,
            "placeholders": placeholders,
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

/// As [`install_host`], with `FIBER_TEST_1128_UNSET_HOST` read when the
/// setting is unset.
fn install_placeholder(home: &Path, extension: &str, name: &str) {
    install_host(
        home,
        extension,
        name,
        json!({"workspace": {"env": "FIBER_TEST_1128_UNSET_HOST"}}),
    );
}

#[test]
fn config_set_of_an_unconfigured_model_warns_and_exits_zero() {
    // Runs in a child with a Fiber home holding an unconfigured model, so
    // the exit code is `config_set`'s own and the warning lands on the
    // child's stderr. The parent checks the code, the line and the write:
    // a code alone would not catch a `set` that returns 0 without warning.
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(super::config_set(
            Layer::Global,
            "model",
            "acme/m",
            &fakes::clock::SystemClock,
        ));
    }
    let setup = Setup::new();
    install_placeholder(&setup.home(), "acme", "acme");
    let output = spawn_child(
        &child_name(),
        "config_set_of_an_unconfigured_model_warns_and_exits_zero",
        &setup,
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let line = "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
                which has no value, and `FIBER_TEST_1128_UNSET_HOST` is not set in the hub's \
                environment (your login shell). Run `fiber login acme`, \
                or set `workspace` in `config/acme.json` in Fiber home, \
                or export it in your shell profile and run `fiber hub refresh`.";
    assert_eq!(stderr.matches(line).count(), 1, "{stderr:?}");
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/m"}));
}

#[test]
fn config_set_text_writes_and_returns_the_warning_lines() {
    let setup = Setup::new();
    install_host(&setup.home(), "acme", "acme", json!({"workspace": {}}));
    let (lines, file) = super::config_set_text(
        &setup.home(),
        &setup.workspace(),
        Layer::Global,
        "model",
        "acme/m",
        &fakes::clock::SystemClock,
    )
    .unwrap();
    assert_eq!(
        lines,
        [
            "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
          which has no value. Run `fiber login acme`, \
          or set `workspace` in `config/acme.json` in Fiber home."
        ]
    );
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/m"}));
    assert_eq!(file, setup.home().join("config.json"));
}

#[test]
fn config_set_text_returns_the_layers_file_for_project_and_repository() {
    let setup = Setup::new();
    let (_, project) = crate::project_of(&setup.home(), &setup.workspace()).unwrap();
    let project_file = setup
        .home()
        .join("projects")
        .join(project.as_str())
        .join("config.json");
    let (_, project_file_returned) = super::config_set_text(
        &setup.home(),
        &setup.workspace(),
        Layer::Project,
        "handoff.tokens",
        "100",
        &fakes::clock::SystemClock,
    )
    .unwrap();
    assert_eq!(project_file_returned, project_file);
    let repository_file = setup.workspace().join(".fiber/config.json");
    let (_, repository_file_returned) = super::config_set_text(
        &setup.home(),
        &setup.workspace(),
        Layer::Repository,
        "handoff.tokens",
        "50",
        &fakes::clock::SystemClock,
    )
    .unwrap();
    assert_eq!(repository_file_returned, repository_file);
}

#[test]
fn config_set_text_refuses_a_repository_forbidden_key_without_writing() {
    let setup = Setup::new();
    let e = super::config_set_text(
        &setup.home(),
        &setup.workspace(),
        Layer::Repository,
        "tui.hover",
        "false",
        &fakes::clock::SystemClock,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
    assert!(!setup.workspace().join(".fiber/config.json").exists());
}

#[test]
fn set_of_an_unconfigured_model_warns_and_writes() {
    for typed in ["acme/m", "acme/m:high", "m"] {
        let setup = Setup::new();
        install_host(&setup.home(), "acme", "acme", json!({"workspace": {}}));
        let (result, err) = setup.set_capturing(Layer::Global, "model", typed);
        result.unwrap();
        assert_eq!(
            err,
            "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
             which has no value. Run `fiber login acme`, \
             or set `workspace` in `config/acme.json` in Fiber home.\n",
            "{typed}"
        );
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(written, json!({"model": typed}), "{typed}");
    }
}

#[test]
fn set_of_a_configured_model_is_silent() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/big");
    result.unwrap();
    assert_eq!(err, "");
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/big"}));
}

#[test]
fn set_with_a_setting_that_is_not_a_host_names_the_setting_not_the_value() {
    let setup = Setup::new();
    install_host(&setup.home(), "acme", "acme", json!({"workspace": {}}));
    setup.write(
        &setup.home().join("config/acme.json"),
        &json!({"workspace": "evil@x"}),
    );
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/m");
    result.unwrap();
    assert_eq!(
        err,
        "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
         whose value is not a host.\n"
    );
    assert!(!err.contains("evil@x"), "{err:?}");
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/m"}));
}

#[test]
fn set_ignores_a_repository_host_setting() {
    // A repository never supplies the host, so its valid value still warns.
    let setup = Setup::new();
    install_host(&setup.home(), "acme", "acme", json!({"workspace": {}}));
    setup.write(
        &setup.workspace().join(".fiber/config/acme.json"),
        &json!({"workspace": "adb-1.example"}),
    );
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/m");
    result.unwrap();
    assert_eq!(
        err,
        "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value. Run `fiber login acme`, \
         or set `workspace` in `config/acme.json` in Fiber home.\n"
    );
}

#[test]
fn set_of_a_bare_id_shared_with_a_configured_provider_is_ambiguous() {
    let setup = Setup::new();
    install(&setup.home(), "a", "a", &models(&["m"]));
    install_host(&setup.home(), "b", "b", json!({"workspace": {}}));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "m");
    let e = result.unwrap_err();
    assert!(matches!(e, CliError::ModelAmbiguous(_)), "{e:?}");
    assert!(e.to_string().contains("a/m"), "{e}");
    assert!(e.to_string().contains("b/m"), "{e}");
    assert_eq!(err, "");
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn set_of_a_model_in_an_all_unconfigured_provider_warns() {
    // The uncached check ran before the fill: the provider's list is
    // non-empty there, so its only model being unconfigured still warns
    // instead of accepting the reference unchecked.
    let setup = Setup::new();
    install_host(&setup.home(), "acme", "acme", json!({"workspace": {}}));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/m");
    result.unwrap();
    assert_eq!(
        err,
        "fiber: The model `acme/m` needs the setting `workspace` for its base URL, \
         which has no value. Run `fiber login acme`, \
         or set `workspace` in `config/acme.json` in Fiber home.\n"
    );
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/m"}));
}

impl Setup {
    /// Runs `get` in `workspace` with in-memory writers.
    fn get_in(&self, workspace: &Path, key: &str) -> (Result<(), CliError>, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = run_get(&self.home(), workspace, key, &mut out, &mut err);
        (
            result,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

#[test]
fn set_model_checks_against_repository_models() {
    let setup = Setup::new();
    let manifest = repo_manifest(
        "fiber.test/acme",
        &json!({"acme": ["http://127.0.0.1:1/v1"]}),
    );
    let data = repo_data("acme", "http://127.0.0.1:1/v1", &["repo-model"]);
    declare_repo(
        &setup.workspace(),
        &[("repo", manifest, &[("providers/acme.json", data.as_str())])],
    );
    approve_repo(&setup.home(), &setup.workspace(), "fiber.test/acme");
    // A repository-only model is accepted, as an installed one is.
    setup
        .set(Layer::Global, "model", "acme/repo-model")
        .unwrap();
    // A typo is rejected with the repository reference as the closest
    // match: the check runs against the merged providers.
    let result = setup.set(Layer::Global, "model", "acme/typo-model");
    match result {
        Err(CliError::ModelNone { closest, .. }) => {
            assert_eq!(closest, vec!["acme/repo-model".to_owned()]);
        }
        other => panic!("a typo model checks clean: {other:?}"),
    }
}

#[test]
fn set_model_rejects_a_model_only_the_shadowed_install_serves() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["personal-model"]));
    let manifest = repo_manifest("acme", &json!({"acme": ["http://127.0.0.1:1/v1"]}));
    let data = repo_data("acme", "http://127.0.0.1:1/v1", &["repo-model"]);
    declare_repo(
        &setup.workspace(),
        &[("repo", manifest, &[("providers/acme.json", data.as_str())])],
    );
    approve_repo(&setup.home(), &setup.workspace(), "acme");
    // The repository copy wins: its model sets, the shadowed install's
    // model no longer names anything installed.
    setup
        .set(Layer::Global, "model", "acme/repo-model")
        .unwrap();
    assert!(matches!(
        setup.set(Layer::Global, "model", "acme/personal-model"),
        Err(CliError::ModelNone { .. })
    ));
    // The shadow notice prints once on the accepting run.
    let (_, err) = setup.set_capturing(Layer::Global, "model", "acme/repo-model");
    assert_eq!(
        err.lines()
            .filter(|line| line.contains("in place of your install"))
            .count(),
        1,
        "{err:?}"
    );
}

#[test]
fn outside_the_project_config_reads_only_the_installed_providers() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme", &models(&["big"]));
    // A damaged directory: its notice pins the home-only load's notices.
    let broken = setup.home().join("extensions").join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(
        broken.join("extension.json"),
        json!({"name": "broken", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    setup.write(
        &setup.home().join("config.json"),
        &json!({"model": "acme/big"}),
    );
    let elsewhere = setup.root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let (result, out, err) = setup.get_in(&elsewhere, "model");
    result.unwrap();
    assert_eq!(
        out,
        format!(
            "\"acme/big\" from {}\n",
            setup.home().join("config.json").display()
        )
    );
    // Outside the project the notices are the installed load's, as at
    // Base: no repository set merges in.
    let (_, loading) = extensions::Providers::load(&setup.home()).unwrap();
    assert_eq!(
        err.lines().collect::<Vec<_>>(),
        loading
            .iter()
            .map(|notice| notice.message.as_str())
            .collect::<Vec<_>>()
    );
}

/// A placeholder variable name no test process sets: each hub test reads
/// its absence first and sets nothing.
const HUB_ENV_HOST: &str = "FIBER_TEST_CONFIG_HUB_ENV_HOST";

/// A hub on `home`'s `run/hub` for one client: it says `hub_hello`, reads
/// the one command line and sends it on, then answers the requested names
/// from `values` and holds the connection until the client closes it.
fn fake_hub_env(home: &Path, values: serde_json::Value) -> std::sync::mpsc::Receiver<String> {
    use std::io::{BufRead, Write};
    let listener = std::os::unix::net::UnixListener::bind(home.join("run/hub")).unwrap();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(
                format!(
                    "{}\n",
                    json!({"kind": "hub_hello", "ts": 1,
                        "schema_version": contract::SCHEMA_VERSION, "payload": {}})
                )
                .as_bytes(),
            )
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap();
        sent.send(line).unwrap_or(());
        stream
            .write_all(
                format!(
                    "{}\n",
                    json!({"kind": "command_accepted", "ts": 1,
                        "schema_version": contract::SCHEMA_VERSION,
                        "payload": {"command_id": "c_env",
                            "result": {"values": values}}})
                )
                .as_bytes(),
            )
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut rest = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut rest)
            .unwrap_or(0);
    });
    got
}

#[test]
fn config_set_accepts_a_model_the_hub_holds_though_the_client_lacks_it() {
    assert!(
        std::env::var_os(HUB_ENV_HOST).is_none(),
        "the test process lacks {HUB_ENV_HOST}"
    );
    let setup = Setup::new();
    install_host(
        &setup.home(),
        "acme",
        "acme",
        json!({"workspace": {"env": HUB_ENV_HOST}}),
    );
    fs::create_dir_all(setup.home().join("run")).unwrap();
    let got = fake_hub_env(&setup.home(), json!({HUB_ENV_HOST: "adb-1.example.test"}));
    let (result, err) = setup.set_capturing(Layer::Global, "model", "acme/m");
    result.unwrap();
    assert_eq!(err, "", "the hub-held model checks without warning");
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(written, json!({"model": "acme/m"}));
    let request: serde_json::Value = serde_json::from_str(
        &fakes::Deadline::after(Duration::from_secs(10))
            .recv(&got)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        request,
        json!({"id": "c_env", "command": "placeholder_env",
            "args": {"names": [HUB_ENV_HOST]}}),
    );
}
