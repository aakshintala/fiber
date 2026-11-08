//! `Config::settings` and `Config::in_layer`: every key `/settings` lists,
//! its value and layer, and what it never shows (`docs/tui.md`, "Swapped
//! views"; `docs/configuration.md`, "Keys", "Layers").

#![allow(clippy::panic, reason = "test helpers; a failure is the test's")]

mod common;

use common::Setup;
use config::{Layer, SettingInfo, SettingValue, Source, WriteScope};
use contract::settings::SettingControl;
use serde_json::json;

/// The row for `key`.
fn row<'a>(rows: &'a [SettingInfo], key: &str) -> &'a SettingInfo {
    rows.iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("no row {key}"))
}

#[test]
fn every_plain_key_is_listed_with_its_default() {
    let setup = Setup::new();
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "handoff.tokens").value,
        SettingValue::Value(json!(400000), Source::Default)
    );
    assert_eq!(row(&rows, "model").value, SettingValue::Unset);
    assert!(rows.iter().any(|row| row.key == "tui.theme"));
    // No row names a `*` pattern, and the rows are sorted by key.
    assert!(rows.iter().all(|row| !row.key.contains('*')));
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
}

#[test]
fn a_star_key_lists_each_instance_a_layer_sets() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"roles": {"fast": "a/b"}, "mcp": {"servers": {"gh": {"command": "gh-mcp"}}}}"#,
    );
    setup.write(&setup.project(), r#"{"roles": {"slow": "c/d"}}"#);
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "roles.fast").value,
        SettingValue::Value(json!("a/b"), Source::Global(setup.global()))
    );
    assert_eq!(
        row(&rows, "roles.slow").value,
        SettingValue::Value(json!("c/d"), Source::Project(setup.project()))
    );
    assert_eq!(
        row(&rows, "mcp.servers.gh.command").value,
        SettingValue::Value(json!("gh-mcp"), Source::Global(setup.global()))
    );
    assert!(!rows.iter().any(|row| row.key == "mcp.servers.gh.args"));
}

#[test]
fn the_layer_is_the_highest_that_sets_it() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"handoff": {"tokens": 1}}"#);
    setup.write(&setup.repository(), r#"{"handoff": {"tokens": 2}}"#);
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "handoff.tokens").value,
        SettingValue::Value(json!(2), Source::Repository(setup.repository()))
    );
    setup.write(&setup.project(), r#"{"handoff": {"tokens": 3}}"#);
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "handoff.tokens").value,
        SettingValue::Value(json!(3), Source::Project(setup.project()))
    );
}

#[test]
fn a_run_override_shows_as_dash_c() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"handoff": {"tokens": 3}}"#);
    let rows = setup.load(&["handoff.tokens=9"]).unwrap().settings();
    assert_eq!(
        row(&rows, "handoff.tokens").value,
        SettingValue::Value(json!(9), Source::Run)
    );
}

#[test]
fn credential_sources_show_kind_and_program_only() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"providers": {"openai": {"credentials": {
            "work": {"command": ["op", "read", "sk-secret-arg"]},
            "home": {"env": "OPENAI_KEY"},
            "box": {"file": "/keys/openai"}}}}}"#,
    );
    let rows = setup.load(&[]).unwrap().settings();
    let global = Source::Global(setup.global());
    for (key, shown) in [
        ("providers.openai.credentials.work", "the command op"),
        (
            "providers.openai.credentials.home",
            "the environment variable OPENAI_KEY",
        ),
        ("providers.openai.credentials.box", "the file /keys/openai"),
    ] {
        assert_eq!(
            row(&rows, key).value,
            SettingValue::Redacted(shown.to_owned(), global.clone()),
            "{key}"
        );
    }
    let all = format!("{rows:?}");
    assert!(!all.contains("sk-secret-arg"), "{all}");
}

#[test]
fn mcp_env_shows_names_only() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"mcp": {"servers": {"gh": {"env": {"TOKEN": "ghp-secret", "HOME": "/h"}}}}}"#,
    );
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "mcp.servers.gh.env").value,
        SettingValue::Redacted(
            "names HOME, TOKEN".to_owned(),
            Source::Global(setup.global())
        )
    );
    assert!(!format!("{rows:?}").contains("ghp-secret"));
}

#[test]
fn skills_disabled_is_the_union_with_each_names_layer() {
    let setup = Setup::new();
    let rows = setup.load(&[]).unwrap().settings();
    // With no layer listing a name, the default's empty list shows.
    assert_eq!(
        row(&rows, "skills.disabled").value,
        SettingValue::Value(json!([]), Source::Default)
    );
    setup.write(&setup.global(), r#"{"skills": {"disabled": ["a"]}}"#);
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["b", "a"]}}"#);
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        row(&config.settings(), "skills.disabled").value,
        SettingValue::Union(vec![
            ("a".to_owned(), Source::Global(setup.global())),
            ("b".to_owned(), Source::Project(setup.project())),
        ])
    );
    // Each layer's own list, never the union.
    assert_eq!(
        config.in_layer("skills.disabled", Layer::Global),
        Some(json!(["a"]))
    );
    assert_eq!(
        config.in_layer("skills.disabled", Layer::Project),
        Some(json!(["b", "a"]))
    );
    assert_eq!(config.in_layer("skills.disabled", Layer::Repository), None);
}

#[test]
fn each_keys_write_scope() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"repository_extensions": [{"path": "ext"}]}"#,
    );
    let rows = setup.load(&[]).unwrap().settings();
    for (key, scope) in [
        ("handoff.tokens", WriteScope::Any { repo: true }),
        ("tui.hover", WriteScope::Any { repo: false }),
        ("diagnostics.level", WriteScope::GlobalOnly),
        ("repository_extensions", WriteScope::RepoOnly),
        ("reviewer.context", WriteScope::PersonFiles),
    ] {
        assert_eq!(row(&rows, key).scope, scope, "{key}");
    }
}

#[test]
fn an_mcp_server_named_credentials_shows_its_env_names() {
    // The `providers.*.credentials.*` arm needs both its names to match;
    // a server called `credentials` matches the second alone, and its
    // env still shows names only.
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"mcp": {"servers": {"credentials": {"env": {"TOKEN": "ghp-secret"}}}}}"#,
    );
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "mcp.servers.credentials.env").value,
        SettingValue::Redacted("names TOKEN".to_owned(), Source::Global(setup.global()))
    );
}

#[test]
fn every_row_carries_the_schemas_presentation_and_control() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"roles": {"fast": "a/b"}}"#);
    let rows = setup.load(&[]).unwrap().settings();
    for row in &rows {
        let schema = config::SETTINGS_SCHEMA
            .iter()
            .find(|key| {
                key.path.split('.').count() == row.key.split('.').count()
                    && key
                        .path
                        .split('.')
                        .zip(row.key.split('.'))
                        .all(|(pattern, name)| pattern == "*" || pattern == name)
            })
            .unwrap_or_else(|| panic!("no schema key for {}", row.key));
        assert_eq!(row.presentation.label, schema.metadata.label, "{}", row.key);
        assert_eq!(
            row.presentation.description, schema.metadata.description,
            "{}",
            row.key
        );
        assert_eq!(row.presentation.tab, schema.metadata.tab, "{}", row.key);
    }
    assert_eq!(row(&rows, "tui.theme").control, SettingControl::Theme);
    assert_eq!(row(&rows, "reviewer.model").control, SettingControl::Model);
    assert_eq!(row(&rows, "model").control, SettingControl::Model);
    assert_eq!(
        row(&rows, "sandbox.enabled").control,
        SettingControl::Boolean
    );
    assert_eq!(
        row(&rows, "repository_extensions").control,
        SettingControl::RepositoryExtensions
    );
    assert_eq!(row(&rows, "roles.fast").control, SettingControl::Json);
    assert_eq!(row(&rows, "roles.fast").presentation.tab, None);
}

#[test]
fn the_last_run_override_wins_and_has_no_file() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"thinking": "minimal"}"#);
    let rows = setup
        .load(&["thinking=high", "thinking=low"])
        .unwrap()
        .settings();
    assert_eq!(
        row(&rows, "thinking").value,
        SettingValue::Value(json!("low"), Source::Run)
    );
    // A named-model override first, then `-c` on another key: each is
    // `-c`, and a later one for the same key replaces the earlier.
    let rows = setup
        .load(&["model=a/b", "model=c/d", "thinking=medium"])
        .unwrap()
        .settings();
    assert_eq!(
        row(&rows, "model").value,
        SettingValue::Value(json!("c/d"), Source::Run)
    );
    assert_eq!(
        row(&rows, "thinking").value,
        SettingValue::Value(json!("medium"), Source::Run)
    );
}

/// `reset` over the setup's layers with `key` and `expected`.
fn reset(
    setup: &Setup,
    layer: config::Layer,
    key: &str,
    expected: serde_json::Value,
) -> Result<config::Reset, config::ConfigError> {
    config::reset(
        &setup.home(),
        &setup.workspace(),
        &common::key(),
        layer,
        key,
        &expected,
    )
}

fn file_text(file: &std::path::Path) -> String {
    std::fs::read_to_string(file).unwrap_or_else(|err| panic!("{}: {err}", file.display()))
}

#[test]
fn a_reset_reveals_the_value_below_in_each_layer() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"handoff": {"tokens": 1}}"#);
    setup.write(&setup.project(), r#"{"handoff": {"tokens": 2}}"#);
    setup.write(&setup.repository(), r#"{"handoff": {"tokens": 3}}"#);
    for (layer, file, was, then) in [
        (Layer::Repository, setup.repository(), 3, 2),
        (Layer::Project, setup.project(), 2, 1),
    ] {
        assert_eq!(
            reset(&setup, layer, "handoff.tokens", json!(was)).unwrap(),
            config::Reset::Removed(file.clone())
        );
        assert!(!file_text(&file).contains("tokens"));
        let rows = setup.load(&[]).unwrap().settings();
        assert_eq!(
            row(&rows, "handoff.tokens").value,
            SettingValue::Value(json!(then), then_source(&setup, then))
        );
    }
    assert_eq!(
        reset(&setup, Layer::Global, "handoff.tokens", json!(1)).unwrap(),
        config::Reset::Removed(setup.global())
    );
    let rows = setup.load(&[]).unwrap().settings();
    assert_eq!(
        row(&rows, "handoff.tokens").value,
        SettingValue::Value(json!(400_000), Source::Default)
    );
}

fn then_source(setup: &Setup, then: i64) -> Source {
    if then == 2 {
        Source::Project(setup.project())
    } else {
        Source::Global(setup.global())
    }
}

#[test]
fn resetting_an_absent_override_writes_and_creates_nothing() {
    let setup = Setup::new();
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        let got = reset(&setup, layer, "handoff.tokens", json!(5)).unwrap();
        let config::Reset::Absent(file) = got else {
            panic!("an absent override is reported as absent, not {got:?}");
        };
        assert!(!file.exists(), "{} was created", file.display());
    }
    // A file that exists but lacks the key is left byte for byte.
    setup.write(&setup.global(), "{\"model\":   \"a/b\"}\n");
    assert_eq!(
        reset(&setup, Layer::Global, "handoff.tokens", json!(5)).unwrap(),
        config::Reset::Absent(setup.global())
    );
    assert_eq!(file_text(&setup.global()), "{\"model\":   \"a/b\"}\n");
}

#[test]
fn the_expected_value_must_equal_the_stored_one() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"handoff": {"window_fraction": 0.5}}"#);
    let before = file_text(&setup.global());
    // Just below and just above: nothing is removed.
    for other in [json!(0.499), json!(0.501)] {
        assert_eq!(
            reset(&setup, Layer::Global, "handoff.window_fraction", other).unwrap(),
            config::Reset::Changed(setup.global())
        );
        assert_eq!(file_text(&setup.global()), before);
    }
    assert_eq!(
        reset(&setup, Layer::Global, "handoff.window_fraction", json!(0.5)).unwrap(),
        config::Reset::Removed(setup.global())
    );
}

#[test]
fn a_reset_keeps_unrelated_keys_and_a_quoted_path() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"model": "a/b", "mcp": {"servers": {"a.b": {"command": "x", "args": ["1"]}}}}"#,
    );
    let key = r#"mcp.servers."a.b".command"#;
    assert_eq!(
        reset(&setup, Layer::Global, key, json!("x")).unwrap(),
        config::Reset::Removed(setup.global())
    );
    let kept: serde_json::Value = serde_json::from_str(&file_text(&setup.global())).unwrap();
    assert_eq!(
        kept,
        json!({"model": "a/b", "mcp": {"servers": {"a.b": {"args": ["1"]}}}})
    );
}

#[test]
fn a_reset_refuses_what_a_set_refuses_and_touches_nothing() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"diagnostics": {"level": "debug"}}"#);
    let before = file_text(&setup.global());
    // Unknown key, a key the layer may not hold, a path that does not
    // parse and an extension's own setting.
    for (layer, key) in [
        (Layer::Global, "no.such.key"),
        (Layer::Repository, "hub.port"),
        (Layer::Project, "diagnostics.level"),
        (Layer::Repository, "diagnostics.level"),
        (Layer::Repository, "reviewer.context"),
        (Layer::Global, "handoff..tokens"),
        (Layer::Global, "extensions.x.settings.y"),
    ] {
        assert!(
            reset(&setup, layer, key, json!("debug")).is_err(),
            "{key} in {layer:?}"
        );
    }
    assert_eq!(file_text(&setup.global()), before);
    assert!(!setup.project().exists());
    assert!(!setup.repository().exists());
    // A repository-only key is refused in the person's files.
    assert!(reset(&setup, Layer::Global, "repository_extensions", json!([])).is_err());
}

#[test]
fn a_reset_refuses_a_file_that_is_not_a_configuration_object() {
    let setup = Setup::new();
    for text in ["[]", "{not json", r#"{"handoff": {"tokens": "x"}}"#] {
        setup.write(&setup.global(), text);
        assert!(
            reset(&setup, Layer::Global, "handoff.tokens", json!("x")).is_err(),
            "{text}"
        );
        assert_eq!(file_text(&setup.global()), text);
    }
}

#[test]
fn a_reset_does_not_follow_a_repository_link() {
    let setup = Setup::new();
    let outside = setup.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("config.json"), r#"{"handoff": {"tokens": 4}}"#).unwrap();
    std::os::unix::fs::symlink(&outside, setup.workspace().join(".fiber")).unwrap();
    assert!(reset(&setup, Layer::Repository, "handoff.tokens", json!(4)).is_err());
    assert_eq!(
        file_text(&outside.join("config.json")),
        r#"{"handoff": {"tokens": 4}}"#
    );
}
