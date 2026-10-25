//! The public repository overlay for one extension's settings
//! (`docs/configuration.md`, "Layers"): a replacement for the repository
//! layer in a cloned configuration.

mod common;

use common::{PROJECT, Setup};
use contract::ErrorCode;
use serde_json::json;

const ACME: &str = "github.com/acme/fiber-acme";
const FILE: &str = "config/github.com-acme-fiber-acme.json";

#[test]
fn overlay_replaces_only_the_repository_contribution() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join(FILE),
        r#"{"a": "global", "b": "global"}"#,
    );
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"a": "repo", "b": "repo"}"#,
    );
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"b": "project"}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let (merged, notices) = config.extensions().merged(ACME, &["a", "b"]).unwrap();
    assert_eq!(merged, json!({"a": "pinned", "b": "project"}));
    assert!(notices.is_empty(), "{notices:?}");
}

#[test]
fn overlay_keeps_global_to_run_order() {
    let setup = Setup::new();
    setup.write(&setup.home().join(FILE), r#"{"a": "global"}"#);
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"a": "repo"}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    // Project beats the overlay.
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"a": "project"}"#,
    );
    let config = setup.load(&[]).unwrap();
    let mut config = config;
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let (merged, _) = config.extensions().merged(ACME, &["a"]).unwrap();
    assert_eq!(merged, json!({"a": "project"}));
    // Run beats everything.
    let mut config = setup
        .load(&[r#"extensions."github.com/acme/fiber-acme".settings.a="run""#])
        .unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let (merged, _) = config.extensions().merged(ACME, &["a"]).unwrap();
    assert_eq!(merged, json!({"a": "run"}));
    // Without project or run, the overlay wins over global.
    let setup2 = Setup::new();
    setup2.write(&setup2.home().join(FILE), r#"{"a": "global"}"#);
    let mut config2 = setup2.load(&[]).unwrap();
    config2
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let (merged, _) = config2.extensions().merged(ACME, &["a"]).unwrap();
    assert_eq!(merged, json!({"a": "pinned"}));
}

#[test]
fn overlay_get_and_layered_agree_on_values_and_sources() {
    let setup = Setup::new();
    setup.write(&setup.home().join(FILE), r#"{"a": "global"}"#);
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"a": "repo"}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let repo_file = setup.workspace().join(".fiber").join(FILE);
    assert_eq!(
        config.extensions().get(ACME, &["a"], "a").unwrap(),
        Some(json!("pinned"))
    );
    assert_eq!(
        config
            .extensions()
            .get_layered(ACME, &["a"], "a")
            .unwrap()
            .0,
        Some((
            json!("pinned"),
            config::Source::Repository(repo_file.clone())
        ))
    );
    // A project value keeps project attribution under an overlay.
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"a": "project"}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let project_file = setup.home().join("projects").join(PROJECT).join(FILE);
    assert_eq!(
        config.extensions().get(ACME, &["a"], "a").unwrap(),
        Some(json!("project"))
    );
    assert_eq!(
        config
            .extensions()
            .get_layered(ACME, &["a"], "a")
            .unwrap()
            .0,
        Some((json!("project"), config::Source::Project(project_file)))
    );
}

#[test]
fn overlay_does_not_mask_host_config_set_read_after_write() {
    let setup = Setup::new();
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    config
        .extensions_mut()
        .set(ACME, config::Scope::Machine, "b", json!("mine"))
        .unwrap();
    let (merged, _) = config.extensions().merged(ACME, &["a", "b"]).unwrap();
    assert_eq!(merged, json!({"a": "pinned", "b": "mine"}));
    assert_eq!(
        config.extensions().get(ACME, &["a", "b"], "b").unwrap(),
        Some(json!("mine"))
    );
}

#[test]
fn overlay_leaves_other_extensions_alone() {
    let setup = Setup::new();
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"a": "repo"}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "pinned"}));
    let (other, _) = config.extensions().merged("other", &["a"]).unwrap();
    assert_eq!(other, json!({}));
}

#[test]
fn overlay_debug_hides_values() {
    let setup = Setup::new();
    let mut config = setup.load(&[]).unwrap();
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"a": "secret-value"}));
    let debug = format!("{:?}", config.extensions());
    assert!(!debug.contains("secret-value"), "{debug}");
}

#[test]
fn withheld_name_leaves_the_person_s_entry_unchanged() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join(FILE),
        r#"{"hooks": {"fmt": {"point": "after_tool", "command": "mine"}}}"#,
    );
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"hooks": {"fmt": {"point": "after_tool", "command": "repo"}}}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    // No approved entry: the overlay withholds the repository name.
    config
        .extensions_mut()
        .set_overlay(ACME, json!({"hooks": {}}));
    let (merged, _) = config.extensions().merged(ACME, &["hooks"]).unwrap();
    assert_eq!(
        merged,
        json!({"hooks": {"fmt": {"point": "after_tool", "command": "mine"}}})
    );
}

#[test]
fn approved_name_merges_key_by_key_under_person_layers() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"hooks": {"fmt": {"command": "mine"}}}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config.extensions_mut().set_overlay(
        ACME,
        json!({"hooks": {"fmt": {"point": "after_tool", "command": "pinned", "timeout": 1000}}}),
    );
    let (merged, _) = config.extensions().merged(ACME, &["hooks"]).unwrap();
    assert_eq!(
        merged,
        json!({"hooks": {"fmt": {"point": "after_tool", "command": "mine", "timeout": 1000}}})
    );
    // Source of a merged object names the highest layer, as `Config::get` does.
    let project_file = setup.home().join("projects").join(PROJECT).join(FILE);
    let (_, source) = config
        .extensions()
        .get_layered(ACME, &["hooks"], "hooks.fmt.command")
        .unwrap()
        .0
        .unwrap();
    assert_eq!(source, config::Source::Project(project_file));
}

#[test]
fn repository_cannot_set_hooks_order_or_hook_timeout() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"hooks": {"order": {"after_tool": ["b"]}}, "extensions": {"hooks": {"hook_timeout_ms": 5}}}"#,
    );
    setup.write(
        &setup.global(),
        r#"{"hooks": {"order": {"after_tool": ["a"]}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let mut ignored: Vec<String> = config
        .notices()
        .iter()
        .filter(|n| n.code == ErrorCode::ConfigKeyIgnored)
        .map(|n| n.message.clone())
        .collect();
    ignored.sort();
    assert_eq!(ignored.len(), 2, "{:?}", config.notices());
    assert!(
        ignored.iter().any(|m| m.contains("hooks.order")),
        "{ignored:?}"
    );
    assert!(
        ignored.iter().any(|m| m.contains("hook_timeout_ms")),
        "{ignored:?}"
    );
    // The person's value wins; the repository's is ignored.
    let (order, _) = config.get("hooks.order.after_tool", None).unwrap();
    assert_eq!(order, json!(["a"]));
}
