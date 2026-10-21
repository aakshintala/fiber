//! `fiber config set|get` for an extension's own setting
//! (`docs/configuration.md`, "Extension settings"): `set` writes
//! `config/<extension>.json` in the layer, `get` reads it back with its
//! layer. Red first: every success-path test fails on the base, where the
//! keys are unknown.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::fs;
use std::path::PathBuf;

use config::{Config, ConfigError, Layer, Sources};
use serde_json::json;

use super::{run_get, run_set};
use crate::CliError;
use crate::test_support::{approve_repo, declare_repo, repo_manifest, write_record};

/// The environment variable the `region` placeholder falls back to: unset
/// in every test process, so only the fake `env` closure below provides it.
const REGION_ENV: &str = "FIBER_TEST_3021_REGION";

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("fiber-config-extension"),
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

    fn project_key(&self) -> config::ProjectKey {
        crate::project_of(&self.home(), &self.workspace())
            .unwrap()
            .1
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

    /// Runs `set` with stderr discarded: its result and the file it wrote.
    fn set(&self, layer: Layer, key: &str, value: &str) -> Result<PathBuf, CliError> {
        run_set(
            &self.home(),
            &self.workspace(),
            layer,
            key,
            value,
            &mut std::io::sink(),
            &*fakes::clock::FakeClock::new(),
        )
    }

    /// Writes `value` straight into a settings file, bypassing `set`: what
    /// a person editing the file by hand holds.
    fn write_settings(&self, file: &std::path::Path, value: &serde_json::Value) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, value.to_string()).unwrap();
    }
}

/// Installs the extension `extension` registering the provider `name` whose
/// models fill `{region}` from the extension's `region` setting, falling
/// back to `REGION_ENV`. The manifest lists `workspace_url` under
/// `repo_settings`: the one key a repository's file may set.
fn install(home: &std::path::Path, extension: &str, name: &str) {
    let dir = home.join("extensions").join(extension);
    fs::create_dir_all(dir.join("providers")).unwrap();
    let mut manifest = json!({"name": extension, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = json!({name: ["https://{region}/v1"]});
    manifest["repo_settings"] = json!(["workspace_url"]);
    fs::write(dir.join("extension.json"), manifest.to_string()).unwrap();
    write_record(&dir);
    fs::write(
        dir.join("providers").join(format!("{name}.json")),
        json!({
            "name": name,
            "placeholders": {"region": {"env": REGION_ENV}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{region}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

/// Declares the repository package `extension` with the same shape
/// [`install`] gives the personal install, and approves it for the
/// workspace's project: the approved copy `run_set` resolves `--repo`
/// writes through.
fn declare_and_approve(setup: &Setup, extension: &str, name: &str) {
    let mut manifest = repo_manifest(extension, &json!({name: ["https://{region}/v1"]}));
    manifest["repo_settings"] = json!(["workspace_url"]);
    let data = json!({
        "name": name,
        "placeholders": {"region": {"env": REGION_ENV}},
        "models": [{"id": "m", "protocol": "openai-responses",
                    "base_url": "https://{region}/v1", "context_window": 1000}],
    })
    .to_string();
    declare_repo(
        &setup.workspace(),
        &[(
            extension,
            manifest,
            &[("providers/acme.json", data.as_str())],
        )],
    );
    approve_repo(&setup.home(), &setup.workspace(), extension);
}

/// The `Refused` refusal `set` gave: its file and its static reason.
fn refused(result: Result<PathBuf, CliError>) -> (PathBuf, &'static str) {
    match result {
        Err(CliError::Config(ConfigError::Refused { file, why, .. })) => (file, why),
        other => panic!("a refused set names its file and reason: {other:?}"),
    }
}

#[test]
fn global_set_writes_the_extension_settings_file() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let file = setup
        .set(
            Layer::Global,
            "extensions.\"acme\".settings.region",
            "example.com",
        )
        .unwrap();
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"region": "example.com"}));
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn get_prints_the_set_value_with_its_settings_file() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    setup
        .set(
            Layer::Global,
            "extensions.\"acme\".settings.region",
            "example.com",
        )
        .unwrap();
    let (result, out, err) = setup.get("extensions.\"acme\".settings.region");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!(
            "\"example.com\" from {}\n",
            setup.home().join("config").join("acme.json").display()
        )
    );
}

#[test]
fn project_set_writes_the_project_settings_file() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let file = setup
        .set(
            Layer::Project,
            "extensions.\"acme\".settings.region",
            "example.com",
        )
        .unwrap();
    let expected = setup
        .home()
        .join("projects")
        .join(setup.project_key().as_str())
        .join("config")
        .join("acme.json");
    assert_eq!(file, expected);
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&expected).unwrap()).unwrap();
    assert_eq!(written, json!({"region": "example.com"}));
}

#[test]
fn get_reads_the_project_layer_with_its_settings_file() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    setup
        .set(
            Layer::Project,
            "extensions.\"acme\".settings.region",
            "example.com",
        )
        .unwrap();
    let (result, out, err) = setup.get("extensions.\"acme\".settings.region");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!(
            "\"example.com\" from {}\n",
            setup
                .home()
                .join("projects")
                .join(setup.project_key().as_str())
                .join("config")
                .join("acme.json")
                .display()
        )
    );
}

#[test]
fn unknown_setting_of_a_known_extension_is_refused_naming_the_extension() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let (file, why) = refused(setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.bogus",
        "example.com",
    ));
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    assert!(why.contains("acme"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn set_for_an_uninstalled_extension_is_refused_naming_its_settings_file() {
    let setup = Setup::new();
    let (file, _) = refused(setup.set(
        Layer::Global,
        "extensions.\"nobody\".settings.region",
        "example.com",
    ));
    assert_eq!(file, setup.home().join("config").join("nobody.json"));
    assert!(!setup.home().join("config").join("nobody.json").exists());
    assert!(!setup.home().join("config.json").exists());
}

#[test]
fn non_host_region_is_refused_and_names_a_host() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let (file, why) = refused(setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.region",
        "not a host!!",
    ));
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    assert!(why.contains("host"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn placeholder_descendant_is_refused_leaving_the_file_unchanged() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let (file, why) = refused(setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.region.sub",
        "example.com",
    ));
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    assert!(why.contains("region"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn refused_descendant_leaves_the_environment_fallback_in_play() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let before = setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.region.sub",
        "example.com",
    );
    assert!(matches!(
        before,
        Err(CliError::Config(ConfigError::Refused { .. }))
    ));
    // No object sits at the placeholder name, so the fill reads the
    // environment through the fake closure: no process environment is
    // touched.
    let project = setup.project_key();
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: project.clone(),
        overrides: Vec::new(),
    })
    .unwrap();
    let set = extensions::RepositorySet::load_pinned(
        &setup.home(),
        &project,
        &setup.workspace(),
        fakes::clock::FakeClock::new(),
    );
    let (mut providers, _) =
        extensions::Providers::load_all_repository(&setup.home(), &set).unwrap();
    let env = |name: &str| (name == REGION_ENV).then(|| "env-host.example".to_owned());
    providers.fill_placeholders(&config, &env).unwrap();
    let data = providers.get("acme").unwrap();
    assert_eq!(data.models.len(), 1);
    assert_eq!(data.models[0].base_url, "https://env-host.example/v1");
}

#[test]
fn repo_set_of_a_repo_setting_writes_the_repository_settings_file() {
    let setup = Setup::new();
    declare_and_approve(&setup, "acme", "acme");
    let file = setup
        .set(
            Layer::Repository,
            "extensions.\"acme\".settings.workspace_url",
            "repo-wire",
        )
        .unwrap();
    let expected = setup
        .workspace()
        .join(".fiber")
        .join("config")
        .join("acme.json");
    assert_eq!(file, expected);
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&expected).unwrap()).unwrap();
    assert_eq!(written, json!({"workspace_url": "repo-wire"}));
}

#[test]
fn repo_set_of_a_placeholder_is_refused_naming_repo_settings() {
    let setup = Setup::new();
    declare_and_approve(&setup, "acme", "acme");
    let (file, why) = refused(setup.set(
        Layer::Repository,
        "extensions.\"acme\".settings.region",
        "example.com",
    ));
    assert_eq!(
        file,
        setup
            .workspace()
            .join(".fiber")
            .join("config")
            .join("acme.json")
    );
    assert!(why.contains("repo_settings"), "{why:?}");
    assert!(
        !setup
            .workspace()
            .join(".fiber")
            .join("config")
            .join("acme.json")
            .exists()
    );
}

#[test]
fn repo_set_of_an_undeclared_key_is_refused_naming_the_extension() {
    let setup = Setup::new();
    declare_and_approve(&setup, "acme", "acme");
    let (_, why) = refused(setup.set(
        Layer::Repository,
        "extensions.\"acme\".settings.bogus",
        "example.com",
    ));
    assert!(why.contains("acme"), "{why:?}");
}

#[test]
fn corrupt_settings_file_is_refused_leaving_the_file_unchanged() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let file = setup.home().join("config").join("acme.json");
    setup.write_settings(&file, &json!({"region": "example.com"}));
    fs::write(&file, "{not json").unwrap();
    let before = fs::read(&file).unwrap();
    let result = setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.region",
        "other.example",
    );
    assert!(matches!(result, Err(CliError::Config(_))), "{result:?}");
    assert_eq!(fs::read(&file).unwrap(), before);
}

#[test]
fn non_object_settings_file_is_refused_leaving_the_file_unchanged() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let file = setup.home().join("config").join("acme.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "[]").unwrap();
    let result = setup.set(
        Layer::Global,
        "extensions.\"acme\".settings.region",
        "example.com",
    );
    assert!(matches!(result, Err(CliError::Config(_))), "{result:?}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "[]");
}

#[test]
fn symlinked_repository_config_dir_is_refused_writing_nothing_outside() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let outside = setup.root.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    std::os::unix::fs::symlink(&outside, setup.workspace().join(".fiber").join("config")).unwrap();
    let result = setup.set(
        Layer::Repository,
        "extensions.\"acme\".settings.workspace_url",
        "repo-wire",
    );
    assert!(
        matches!(result, Err(CliError::Config(ConfigError::NotPlain { .. }))),
        "{result:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn empty_settings_rest_is_a_usage_error() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let result = setup.set(Layer::Global, "extensions.\"acme\".settings", "example.com");
    assert!(
        matches!(result, Err(CliError::Config(ConfigError::Override { .. }))),
        "{result:?}"
    );
}

#[test]
fn object_get_merges_the_global_and_project_settings_files() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    setup.write_settings(
        &setup.home().join("config").join("acme.json"),
        &json!({"x": 1}),
    );
    let project_file = setup
        .home()
        .join("projects")
        .join(setup.project_key().as_str())
        .join("config")
        .join("acme.json");
    setup.write_settings(&project_file, &json!({"y": 2}));
    let (result, out, err) = setup.get("extensions.\"acme\".settings");
    result.unwrap();
    assert_eq!(err, "");
    assert_eq!(
        out,
        format!("{{\"x\":1,\"y\":2}} from {}\n", project_file.display())
    );
}

#[test]
fn get_of_an_unset_setting_names_it_on_stderr() {
    let setup = Setup::new();
    install(&setup.home(), "acme", "acme");
    let (result, out, err) = setup.get("extensions.\"acme\".settings.region");
    result.unwrap();
    assert_eq!(out, "");
    assert_eq!(err, "`extensions.\"acme\".settings.region` is not set.\n");
}

#[test]
fn get_for_an_uninstalled_extension_is_not_a_configuration_key() {
    let setup = Setup::new();
    let (result, _, _) = setup.get("extensions.\"nobody\".settings.region");
    assert!(
        matches!(
            result,
            Err(CliError::ConfigKeyUnknown { ref key }) if key == "extensions.\"nobody\".settings.region"
        ),
        "{result:?}"
    );
}
