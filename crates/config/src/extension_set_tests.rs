//! Unit tests for the extension-settings helpers: `host_shape`, the
//! declared-settings readers, `settings_key` and the layered get. No
//! set-routing coverage: the `cli` behaviour tests hold that.

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::fs;

use serde_json::json;

use super::{declared_settings_in, host_shape, installed_dir};
use crate::extension_settings::settings_key;
use crate::{Config, ProjectKey, Source, Sources};

#[test]
fn host_shape_accepts_a_bare_host() {
    assert_eq!(host_shape("example.com"), Some("example.com"));
    assert_eq!(host_shape("a-b.c9"), Some("a-b.c9"));
    assert_eq!(host_shape("Example.COM"), Some("Example.COM"));
}

#[test]
fn host_shape_strips_one_leading_scheme_and_one_trailing_slash() {
    assert_eq!(host_shape("https://example.com"), Some("example.com"));
    assert_eq!(host_shape("https://example.com/"), Some("example.com"));
    assert_eq!(host_shape("example.com/"), Some("example.com"));
    // Only one of each is stripped: what remains is not a host.
    assert_eq!(host_shape("https://https://example.com"), None);
    assert_eq!(host_shape("example.com//"), None);
}

#[test]
fn host_shape_checks_the_port_boundary() {
    assert_eq!(host_shape("host:0"), Some("host:0"));
    assert_eq!(host_shape("host:65534"), Some("host:65534"));
    assert_eq!(host_shape("host:65535"), Some("host:65535"));
    assert_eq!(host_shape("host:65536"), None);
    assert_eq!(host_shape("host:"), None);
    assert_eq!(host_shape("host:abc"), None);
    assert_eq!(host_shape("host:80:80"), None);
    // A leading `+` is digits to `u16` parsing but never a host port.
    assert_eq!(host_shape("host:+80"), None);
}

#[test]
fn host_shape_refuses_what_is_not_a_host() {
    assert_eq!(host_shape(""), None);
    assert_eq!(host_shape("https://"), None);
    assert_eq!(host_shape("not a host"), None);
    assert_eq!(host_shape("under_score"), None);
    assert_eq!(host_shape("host/path"), None);
    assert_eq!(host_shape(":80"), None);
}

/// The fixture `dir` holds an extension with `workspace_url` under
/// `repo_settings` and a `region` placeholder with an environment fallback.
fn fixture(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "repo_settings": ["workspace_url"]})
        .to_string(),
    )
    .unwrap();
    fs::write(
        dir.join("providers").join("acme.json"),
        json!({"name": "acme", "models": [],
               "placeholders": {"region": {"env": "FIBER_TEST_3021_REGION"}}})
        .to_string(),
    )
    .unwrap();
}

#[test]
fn declared_settings_join_repo_settings_with_placeholder_names() {
    let root = fakes::TempDir::new("fiber-extension-set");
    let dir = root.path().join("acme");
    fixture(&dir);
    let declared = declared_settings_in(&dir).unwrap();
    assert_eq!(declared.repo_settings, vec!["workspace_url".to_owned()]);
    assert!(declared.placeholders.contains("region"));
    // The provider data still names the environment fallback.
    let providers = crate::extension::read_providers(&dir).unwrap();
    assert_eq!(
        providers[0].placeholders["region"].env.as_deref(),
        Some("FIBER_TEST_3021_REGION")
    );
}

#[test]
fn declared_settings_fail_without_a_manifest() {
    let root = fakes::TempDir::new("fiber-extension-set");
    let missing = root.path().join("absent");
    assert!(declared_settings_in(&missing).is_err());
}

#[test]
fn installed_dir_probes_the_manifest_without_reading() {
    let root = fakes::TempDir::new("fiber-extension-set");
    let home = root.path().join("home");
    assert_eq!(installed_dir(&home, "acme"), None);
    let dir = home.join("extensions").join("acme");
    fixture(&dir);
    assert_eq!(installed_dir(&home, "acme"), Some(dir));
    // A short name resolves to the same slug as the full name.
    let short = home.join("extensions").join("opencode");
    fs::create_dir_all(&short).unwrap();
    fs::write(
        short.join("extension.json"),
        json!({"name": "github.com/aakshintala/fiber/providers/opencode",
               "version": "v0.0.0", "fiber": "0.0.0", "api": 1})
        .to_string(),
    )
    .unwrap();
    assert_eq!(installed_dir(&home, "opencode"), Some(short));
}

#[test]
fn settings_key_splits_name_from_relative_segments() {
    assert_eq!(
        settings_key("extensions.\"acme\".settings.region"),
        Some(("acme".to_owned(), vec!["region".to_owned()]))
    );
    assert_eq!(
        settings_key("extensions.acme.settings"),
        Some(("acme".to_owned(), Vec::new()))
    );
    assert_eq!(
        settings_key("extensions.\"acme\".settings.region.sub"),
        Some((
            "acme".to_owned(),
            vec!["region".to_owned(), "sub".to_owned()]
        ))
    );
}

#[test]
fn settings_key_spells_short_names_full_and_keeps_quoted_dots() {
    assert_eq!(
        settings_key("extensions.opencode.settings.region"),
        Some((
            "github.com/aakshintala/fiber/providers/opencode".to_owned(),
            vec!["region".to_owned()]
        ))
    );
    assert_eq!(
        settings_key("extensions.\"github.com-acme-x\".settings.region"),
        Some(("github.com-acme-x".to_owned(), vec!["region".to_owned()]))
    );
}

#[test]
fn settings_key_rejects_other_shapes_and_unparseable_keys() {
    assert_eq!(settings_key("model"), None);
    assert_eq!(settings_key("extensions.acme"), None);
    assert_eq!(settings_key("extensions.acme.other.region"), None);
    assert_eq!(settings_key("extensions.\"acme.settings.region"), None);
    assert_eq!(settings_key(""), None);
}

/// A configuration whose settings files the test writes by hand: global
/// holds `region` and an `obj` with `a`, the repository holds a
/// `workspace_url` and a `region` the filter drops, the project holds an
/// `obj` with `b`.
struct Layered {
    root: fakes::TempDir,
    project: ProjectKey,
}

impl Layered {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extension-layered");
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let project = ProjectKey::new("proj").unwrap();
        let write = |file: std::path::PathBuf, value: serde_json::Value| {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, value.to_string()).unwrap();
        };
        write(
            home.join("config").join("acme.json"),
            json!({"region": "global.example", "obj": {"a": 1}}),
        );
        write(
            workspace.join(".fiber").join("config").join("acme.json"),
            json!({"workspace_url": "repo-wire", "region": "repo.example"}),
        );
        write(
            home.join("projects")
                .join("proj")
                .join("config")
                .join("acme.json"),
            json!({"obj": {"b": 2}}),
        );
        Self { root, project }
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().join("workspace")
    }

    fn load(&self, overrides: Vec<String>) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: self.project.clone(),
            overrides,
        })
        .unwrap()
    }
}

#[test]
fn layered_get_reads_a_scalar_from_the_highest_holding_layer() {
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    let (value, source) = config
        .extensions()
        .get_layered("acme", &["workspace_url"], "region")
        .unwrap()
        .0
        .unwrap();
    // The repository's `region` is filtered out: only `workspace_url` is
    // declared there, so the global value reads.
    assert_eq!(value, json!("global.example"));
    assert_eq!(
        source,
        Source::Global(setup.home().join("config").join("acme.json"))
    );
}

#[test]
fn layered_get_reads_a_repo_setting_from_the_repository_file() {
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    let (value, source) = config
        .extensions()
        .get_layered("acme", &["workspace_url"], "workspace_url")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(value, json!("repo-wire"));
    assert_eq!(
        source,
        Source::Repository(
            setup
                .workspace()
                .join(".fiber")
                .join("config")
                .join("acme.json")
        )
    );
}

#[test]
fn layered_get_merges_objects_lowest_first_from_the_project() {
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    let (value, source) = config
        .extensions()
        .get_layered("acme", &["workspace_url"], "obj")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(value, json!({"a": 1, "b": 2}));
    assert_eq!(
        source,
        Source::Project(
            setup
                .home()
                .join("projects")
                .join("proj")
                .join("config")
                .join("acme.json")
        )
    );
}

#[test]
fn layered_get_reads_run_settings_first() {
    let setup = Layered::new();
    let config = setup.load(vec![
        "extensions.\"acme\".settings.region=run.example".to_owned(),
    ]);
    let (value, source) = config
        .extensions()
        .get_layered("acme", &["workspace_url"], "region")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(value, json!("run.example"));
    assert_eq!(source, Source::Run);
}

#[test]
fn layered_get_hides_below_a_scalar_over_the_path() {
    let root = fakes::TempDir::new("fiber-extension-layered");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let project = ProjectKey::new("proj").unwrap();
    let write = |file: std::path::PathBuf, value: serde_json::Value| {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, value.to_string()).unwrap();
    };
    write(
        home.join("config").join("acme.json"),
        json!({"a": {"b": 1}}),
    );
    write(
        home.join("projects")
            .join("proj")
            .join("config")
            .join("acme.json"),
        json!({"a": 5}),
    );
    let config = Config::load(Sources {
        home,
        workspace,
        project,
        overrides: Vec::new(),
    })
    .unwrap();
    // The project's scalar `a` reads whole, hiding the global object.
    let (value, _) = config
        .extensions()
        .get_layered("acme", &[], "a")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(value, json!(5));
    // Below it, the global `a.b` is hidden: no layer holds the key.
    assert_eq!(
        config
            .extensions()
            .get_layered("acme", &[], "a.b")
            .unwrap()
            .0,
        None
    );
}

#[test]
fn layered_get_reports_nothing_unset_and_fails_unparseable() {
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    assert_eq!(
        config
            .extensions()
            .get_layered("acme", &["workspace_url"], "missing")
            .unwrap()
            .0,
        None
    );
    assert!(
        config
            .extensions()
            .get_layered("acme", &["workspace_url"], "a.\"b")
            .is_err()
    );
}

#[test]
fn layered_get_fails_on_corrupt_and_non_object_files() {
    let root = fakes::TempDir::new("fiber-extension-layered");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    fs::create_dir_all(home.join("config")).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let project = ProjectKey::new("proj").unwrap();
    let load = || {
        Config::load(Sources {
            home: home.clone(),
            workspace: workspace.clone(),
            project: project.clone(),
            overrides: Vec::new(),
        })
        .unwrap()
    };
    fs::write(home.join("config").join("acme.json"), "{not json").unwrap();
    assert!(
        load()
            .extensions()
            .get_layered("acme", &[], "region")
            .is_err()
    );
    fs::write(home.join("config").join("acme.json"), "[]").unwrap();
    assert!(
        load()
            .extensions()
            .get_layered("acme", &[], "region")
            .is_err()
    );
}

#[test]
fn layered_get_ignores_a_missing_repo_key_filter() {
    // An empty `repo_settings` list drops every repository key: the
    // global value still reads.
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    let (value, source) = config
        .extensions()
        .get_layered("acme", &[], "region")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(value, json!("global.example"));
    assert_eq!(
        source,
        Source::Global(setup.home().join("config").join("acme.json"))
    );
}

#[test]
fn declared_settings_keep_a_placeholder_out_of_repo_settings() {
    // Even a manifest listing a placeholder under `repo_settings` keeps
    // it out: a placeholder's setting is never a repository key, so the
    // write gate refuses it there and the read filter drops it.
    let root = fakes::TempDir::new("fiber-extension-set");
    let dir = root.path().join("acme");
    fixture(&dir);
    fs::write(
        dir.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
               "repo_settings": ["workspace_url", "region"]})
        .to_string(),
    )
    .unwrap();
    let declared = declared_settings_in(&dir).unwrap();
    assert_eq!(declared.repo_settings, vec!["workspace_url".to_owned()]);
    assert!(declared.placeholders.contains("region"));
}

/// A home/workspace pair with the `acme` fixture installed, for the
/// `set` write-path tests below: every gate `set_extension_setting`
/// holds gets a test that flips it, so CI's mutants die in this crate
/// (mutation testing runs this crate's tests alone).
struct SetSetup {
    root: fakes::TempDir,
    project: ProjectKey,
}

impl SetSetup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-extension-set-write");
        let setup = Self {
            root,
            project: ProjectKey::new("proj").unwrap(),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.workspace()).unwrap();
        fixture(&setup.home().join("extensions").join("acme"));
        setup
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().join("workspace")
    }

    /// The workspace ready for a `--repo` write: `.fiber/` and
    /// `.fiber/config/` exist as plain directories, and `dir` holds an
    /// approved copy of the extension.
    fn repo_dirs(&self) -> std::path::PathBuf {
        fs::create_dir_all(self.workspace().join(".fiber").join("config")).unwrap();
        let dir = self.root.path().join("approved").join("acme");
        fixture(&dir);
        dir
    }

    fn refused_why(result: Result<std::path::PathBuf, crate::ConfigError>) -> String {
        match result {
            Err(crate::ConfigError::Refused { why, .. }) => why.to_owned(),
            other => panic!("a refused set names its reason: {other:?}"),
        }
    }
}

#[test]
fn set_global_placeholder_write_returns_its_settings_file() {
    // Kills: whole-`set_extension_setting` to `Ok(Default)`,
    // `repository && dir.is_none()` to `||`, the repository gate's `&&`
    // to `||`, `!contains` to `contains`, the host gate's second `&&` to
    // `||` and its `!` deletion, `settings_layer_file` to `Default`, and
    // `set_settings_file` to `Ok(())` (the file content is read back).
    let setup = SetSetup::new();
    let file = crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.region",
        json!("example.com"),
    )
    .unwrap();
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"region": "example.com"}));
}

#[test]
fn set_global_unknown_setting_is_refused() {
    // Kills the `!declared_names.contains` `!` deletion (an unknown key
    // must be refused, and the refusal below the repository gate keeps
    // its unknown-setting reason).
    let setup = SetSetup::new();
    let why = SetSetup::refused_why(crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.bogus",
        json!("example.com"),
    ));
    assert!(why.contains("no such setting"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn set_repo_valid_key_writes_the_repository_file() {
    // Kills: `set_repo_extension_setting` to `Ok(Default)`, the `!any`
    // `!` deletion and the `==` to `!=` in the repository gate (a listed
    // `repo_settings` key must write), and the repository arm of
    // `settings_layer_file` to `Default`.
    let setup = SetSetup::new();
    let dir = setup.repo_dirs();
    let file = crate::set_repo_extension_setting(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        &dir,
        "extensions.\"acme\".settings.workspace_url",
        json!("repo-wire"),
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
fn set_repo_undeclared_key_is_refused_as_not_a_repo_setting() {
    // Kills the repository gate's `||` to `&&`: an undeclared key must
    // still be refused there, with the repo-settings reason rather than
    // falling through to the unknown-setting gate.
    let setup = SetSetup::new();
    let dir = setup.repo_dirs();
    let why = SetSetup::refused_why(crate::set_repo_extension_setting(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        &dir,
        "extensions.\"acme\".settings.bogus",
        json!("example.com"),
    ));
    assert!(why.contains("repo_settings"), "{why:?}");
}

#[test]
fn set_placeholder_descendant_is_refused() {
    // Kills `placeholders.contains && len > 1`'s `&&` to `||` in one
    // direction (a placeholder descendant must be refused) and the `>`
    // to `<` (a longer rest must still be refused).
    let setup = SetSetup::new();
    let why = SetSetup::refused_why(crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.region.sub",
        json!("example.com"),
    ));
    assert!(why.contains("no object below"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn set_plain_descendant_below_a_repo_setting_writes() {
    // Kills the descendant gate's `&&` to `||`: a descendant of a
    // non-placeholder key is no placeholder descendant and must write.
    // Also pins the `>` to `==` and `>=` kills beside the valid
    // single-segment write above: a two-segment non-placeholder rest
    // writes, so `len > 1` alone never refuses.
    let setup = SetSetup::new();
    let file = crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.workspace_url.sub",
        json!("v"),
    )
    .unwrap();
    assert_eq!(file, setup.home().join("config").join("acme.json"));
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"workspace_url": {"sub": "v"}}));
}

#[test]
fn set_plain_single_key_with_a_non_host_value_writes() {
    // Kills the host gate's first `&&` to `||`: a non-placeholder key
    // holds any value, even one that is no host.
    let setup = SetSetup::new();
    let file = crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.workspace_url",
        json!("not a host!!"),
    )
    .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, json!({"workspace_url": "not a host!!"}));
}

#[test]
fn set_placeholder_single_key_with_a_non_host_value_is_refused() {
    // Kills the host gate's `== 1` to `!=` (a single-segment placeholder
    // with a non-host value must be refused) and its `!matches` `!`
    // deletion in the refusal direction.
    let setup = SetSetup::new();
    let why = SetSetup::refused_why(crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings.region",
        json!("not a host!!"),
    ));
    assert!(why.contains("host"), "{why:?}");
    assert!(!setup.home().join("config").join("acme.json").exists());
}

#[test]
fn set_direct_repository_key_without_a_directory_is_refused_uninstalled() {
    // Pins the `repository && dir.is_none()` gate's repository direction:
    // a settings-shaped repository key reaching `set` directly names no
    // directory this crate may use.
    let setup = SetSetup::new();
    let dir = setup.repo_dirs();
    let _ = &dir;
    let why = SetSetup::refused_why(crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Repository,
        "extensions.\"acme\".settings.workspace_url",
        json!("repo-wire"),
    ));
    assert!(why.contains("not installed"), "{why:?}");
}

#[test]
fn set_uninstalled_extension_is_refused_uninstalled() {
    // Pins the `installed_dir` `None` arm: neither an installed nor a
    // caller-resolved directory means uninstalled.
    let setup = SetSetup::new();
    let why = SetSetup::refused_why(crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"nobody\".settings.region",
        json!("example.com"),
    ));
    assert!(why.contains("not installed"), "{why:?}");
}

#[test]
fn set_empty_rest_is_a_usage_error() {
    let setup = SetSetup::new();
    match crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Global,
        "extensions.\"acme\".settings",
        json!("example.com"),
    ) {
        Err(crate::ConfigError::Override { .. }) => {}
        other => panic!("an empty rest is usage: {other:?}"),
    }
}

#[test]
fn set_project_placeholder_write_returns_the_project_file() {
    // Pins the project arm of `settings_layer_file`: the same key on the
    // project layer writes under `projects/<key>/`.
    let setup = SetSetup::new();
    let file = crate::set(
        &setup.home(),
        &setup.workspace(),
        &setup.project,
        crate::Layer::Project,
        "extensions.\"acme\".settings.region",
        json!("example.com"),
    )
    .unwrap();
    assert_eq!(
        file,
        setup
            .home()
            .join("projects")
            .join("proj")
            .join("config")
            .join("acme.json")
    );
}

#[test]
fn layered_get_returns_the_notice_for_an_ignored_repository_key() {
    // The repository holds `region`, which no filter lists: the merged
    // value ignores it with a notice, and the layered read returns that
    // notice beside the global value.
    let setup = Layered::new();
    let config = setup.load(Vec::new());
    let (found, notices) = config
        .extensions()
        .get_layered("acme", &["workspace_url"], "region")
        .unwrap();
    let (value, _) = found.unwrap();
    assert_eq!(value, json!("global.example"));
    assert!(
        notices
            .iter()
            .any(|notice| notice.message.contains("region")),
        "the ignored repository key gets a notice: {notices:?}"
    );
}
