use super::*;

use contract::ThinkingLevel;
use contract::settings::{SettingControl, SettingsTab};

#[test]
fn cache_diagnostics_defaults_to_off() {
    // Opt-in: with the setting unset, no prefix hash or cache-use field
    // is written.
    assert_eq!(
        defaults().pointer("/cache/diagnostics"),
        Some(&serde_json::json!(false)),
        "the built-in default is off"
    );
}

#[test]
fn a_repository_may_set_cache_diagnostics() {
    let layer: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(r#"{"cache": {"diagnostics": true}}"#).expect("the test layer parses");
    let mut notices = Vec::new();
    let kept = check(
        layer,
        &crate::Source::Repository("repo/config.json".into()),
        &mut notices,
    )
    .expect("a repository may set the setting");
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        kept.get("cache").and_then(|cache| cache.get("diagnostics")),
        Some(&serde_json::json!(true)),
    );
}

#[test]
fn thinking_key_values_match_the_contract_levels() {
    let names = contract::ThinkingLevel::ALL.map(contract::ThinkingLevel::as_str);
    // Both thinking keys accept exactly the contract's names: nothing
    // outside the contract validates.
    for path in ["thinking", "models.*.thinking"] {
        let key = KEYS
            .iter()
            .find(|key| key.path == path)
            .unwrap_or_else(|| panic!("the `{path}` key exists"));
        let Kind::OneOf(allowed) = key.kind else {
            panic!("the `{path}` key accepts one of a fixed set");
        };
        // Order is contractual (`docs/model-routing.md`, "Thinking"):
        // the doc lists the levels `off`, `minimal`, `low`, `medium`,
        // `high`, `xhigh`, `max`, and the refusal/expected messages render
        // them in that order.
        assert_eq!(
            allowed, names,
            "`{path}` accepts exactly the contract levels"
        );
        for name in names {
            assert!(
                key.kind.accepts(&serde_json::json!(name)),
                "`{path}` accepts `{name}`"
            );
        }
        assert!(
            !key.kind.accepts(&serde_json::json!("turbo")),
            "`{path}` rejects a name outside the contract"
        );
    }
}

#[test]
fn each_write_scope_answers_whether_a_repository_sets_it() {
    let source = crate::Source::Repository("repo/config.json".into());
    let checked = |text: &str| {
        let layer: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(text).expect("the test layer parses");
        let mut notices = Vec::new();
        let kept = check(layer, &source, &mut notices).expect("the test layer checks");
        (kept, notices)
    };
    // `Any { repo: true }` stays.
    let (kept, notices) = checked(r#"{"model": "a/b"}"#);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(kept.get("model"), Some(&serde_json::json!("a/b")));
    // `Any { repo: false }` is ignored.
    let (kept, notices) = checked(r#"{"hub": {"port": 4040}}"#);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0]
            .message
            .ends_with("`hub.port`, which a repository may not set."),
        "{}",
        notices[0].message
    );
    assert!(kept.get("hub").unwrap().as_object().unwrap().is_empty());
    // `RepoOnly` stays.
    let (kept, notices) = checked(r#"{"repository_extensions": [{"path": "pkg"}]}"#);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        kept.get("repository_extensions"),
        Some(&serde_json::json!([{"path": "pkg"}]))
    );
    // `GlobalOnly` and `PersonFiles` are ignored: a repository may not set them either.
    for (text, key) in [
        (
            r#"{"diagnostics": {"level": "debug"}}"#,
            "diagnostics.level",
        ),
        (r#"{"reviewer": {"context": "x"}}"#, "reviewer.context"),
    ] {
        let (_, notices) = checked(text);
        assert_eq!(notices.len(), 1, "{key}: {notices:?}");
        assert!(
            notices[0]
                .message
                .ends_with(&format!("`{key}`, which a repository may not set.")),
            "{}",
            notices[0].message
        );
    }
}

#[test]
fn mcp_server_env_accepts_a_secret_reference() {
    let layer: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r#"{"mcp": {"servers": {"x": {"env": {"K": {"secret": "mcp.x.K"}}}}}}"#,
    )
    .expect("the test layer parses");
    let mut notices = Vec::new();
    let kept = check(
        layer,
        &crate::Source::Global("global/config.json".into()),
        &mut notices,
    )
    .expect("a secret env reference loads");
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        kept.get("mcp"),
        Some(&serde_json::json!({"servers": {"x": {"env": {"K": {"secret": "mcp.x.K"}}}}}))
    );
}

#[test]
fn mcp_server_env_values_are_strings_or_single_secret_objects() {
    let source = crate::Source::Global("global/config.json".into());
    let check_env = |env: &serde_json::Value| {
        let text = serde_json::json!({"mcp": {"servers": {"x": {"env": env}}}}).to_string();
        let layer: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&text).expect("the test layer parses");
        let mut notices = Vec::new();
        check(layer, &source, &mut notices)
    };
    // Strings pass, as does one `{ "secret" }` object per value.
    for env in [
        serde_json::json!({}),
        serde_json::json!({"K": "v"}),
        serde_json::json!({"K": ""}),
        serde_json::json!({"K": {"secret": "mcp.x.K"}}),
        serde_json::json!({"A": "v", "B": {"secret": "mcp.x.B"}}),
    ] {
        assert!(check_env(&env).is_ok(), "{env}");
    }
    // Anything else fails `WrongType` naming the key: a non-string, a
    // secret object with a non-string name, with another key beside
    // `secret`, or empty, and a value that is no object at all.
    for env in [
        serde_json::json!({"K": 1}),
        serde_json::json!({"K": {"secret": 1}}),
        serde_json::json!({"K": {"secret": "a", "x": "b"}}),
        serde_json::json!({"K": {}}),
        serde_json::json!({"K": ["v"]}),
        serde_json::json!({"K": null}),
        serde_json::json!([]),
        serde_json::json!(1),
        serde_json::json!("v"),
    ] {
        let error = check_env(&env).expect_err(&format!("{env} fails"));
        let ConfigError::WrongType { key, .. } = error else {
            panic!("{env} is {error:?}, not WrongType");
        };
        assert_eq!(key, "mcp.servers.x.env", "{env}");
    }
}

#[test]
fn only_a_server_env_accepts_a_secret_reference() {
    let layer: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r#"{"mcp": {"servers": {"x": {"tools": {"t": {"hints": {"K": {"secret": "mcp.x.K"}}}}}}}}"#,
    )
    .expect("the test layer parses");
    let mut notices = Vec::new();
    let error = check(
        layer,
        &crate::Source::Global("global/config.json".into()),
        &mut notices,
    )
    .expect_err("a secret reference outside env fails");
    let ConfigError::WrongType { key, .. } = error else {
        panic!("a secret hint is {error:?}, not WrongType");
    };
    assert_eq!(key, "mcp.servers.x.tools.t.hints");
}

/// A tab's plain keys, in the order `docs/tui.md`, "Swapped views",
/// `/settings` draws them.
const TABBED: &[(SettingsTab, &[&str])] = &[
    (
        SettingsTab::Appearance,
        &[
            "tui.panel.cards",
            "tui.rail.shown",
            "tui.rail.width",
            "tui.panel.width",
            "tui.theme",
            "tui.reduced_motion",
            "tui.screen_reader",
            "tui.hover",
            "tui.inline_images",
            "tui.logo_glyph",
        ],
    ),
    (
        SettingsTab::Notifications,
        &[
            "tui.attention.notification",
            "tui.attention.bell",
            "tui.attention.title",
            "quota.notice_at",
        ],
    ),
    (
        SettingsTab::Models,
        &[
            "model_lists.refresh_after",
            "model",
            "scoped_models",
            "thinking",
            "web_search.backend",
            "providers.databricks.request_tags",
        ],
    ),
    (
        SettingsTab::Permissions,
        &[
            "reviewer.model",
            "reviewer.block_limits.consecutive",
            "reviewer.block_limits.session",
            "reviewer.context",
            "sandbox.enabled",
            "sandbox.allow_write",
            "sandbox.deny_read",
        ],
    ),
    (
        SettingsTab::Sessions,
        &[
            "compaction.enabled",
            "compaction.tokens",
            "compaction.window_fraction",
            "compaction.nudge",
            "budget.usd",
            "repository_extensions",
        ],
    ),
    (
        SettingsTab::Advanced,
        &[
            "hub.idle_exit_ms",
            "hub.port",
            "hub.default",
            "hub.allowed_origins",
            "session.idle_exit_ms",
            "cache.lifetime",
            "cache.warm_idle",
            "cache.warm_cap",
            "cache.diagnostics",
            "retry.attempts",
            "retry.initial_delay_ms",
            "retry.max_delay_ms",
            "diagnostics.level",
        ],
    ),
];

/// The one plain key a screen row covers (`/skills`): it has no tab so it
/// is drawn once.
const COVERED_PLAIN: &str = "skills.disabled";

#[test]
fn settings_metadata_text_is_complete() {
    for key in KEYS {
        for (name, text) in [
            ("label", key.metadata.label),
            ("description", key.metadata.description),
        ] {
            assert!(!text.trim().is_empty(), "`{}` has no {name}", key.path);
            assert_eq!(
                text.trim(),
                text,
                "`{}` {name} has spaces around it",
                key.path
            );
            assert!(
                !text.contains('\n'),
                "`{}` {name} is more than one line",
                key.path
            );
        }
    }
}

#[test]
fn settings_metadata_tabs_follow_key_classification() {
    let tab_of = |path: &str| {
        TABBED
            .iter()
            .find(|(_, paths)| paths.contains(&path))
            .map(|(tab, _)| *tab)
    };
    // A typo in the table above would otherwise leave a key unchecked.
    for (_, paths) in TABBED {
        for path in *paths {
            let key = KEYS.iter().find(|key| key.path == *path);
            assert!(key.is_some(), "`{path}` is not a schema key");
            assert!(!path.contains('*'), "`{path}` has a `*`, so no tab");
        }
    }
    for key in KEYS {
        let want = tab_of(key.path);
        assert_eq!(
            key.metadata.tab, want,
            "the tab of `{}` (a key with a `*`, or the key a screen row covers, has none)",
            key.path
        );
        if want.is_none() {
            // No blanket fallback: a new plain key must be classified
            // above, or named as the covered one.
            assert!(
                key.path.contains('*') || key.path == COVERED_PLAIN,
                "`{}` has no tab: add it to a tab, or have a screen row cover it",
                key.path
            );
        }
    }
}

/// The control a key's schema type gives it, for each key a person edits.
fn control_of(path: &str) -> SettingControl {
    KEYS.iter()
        .find(|key| key.path == path)
        .unwrap_or_else(|| panic!("`{path}` is a schema key"))
        .kind
        .control()
}

#[test]
fn every_kind_has_the_control_its_type_calls_for() {
    // One value per Kind variant: a new variant fails to compile in
    // `control`, and the table below pins what each maps to.
    let cases: [(Kind, SettingControl); 16] = [
        (Kind::Str, SettingControl::Text),
        (Kind::Model, SettingControl::Model),
        (Kind::Theme, SettingControl::Theme),
        (Kind::Bool, SettingControl::Boolean),
        (Kind::Count, SettingControl::Number),
        (Kind::CountBelow(12), SettingControl::Number),
        (Kind::Number, SettingControl::Number),
        (
            Kind::OneOf(&["b", "a"]),
            SettingControl::Fixed(vec!["b".into(), "a".into()]),
        ),
        (Kind::StrList, SettingControl::StringList),
        (Kind::StrOrStrList, SettingControl::Json),
        (Kind::Role, SettingControl::Json),
        (Kind::EnvMap, SettingControl::Json),
        (Kind::BoolMap, SettingControl::Json),
        (Kind::Credential, SettingControl::Json),
        (
            Kind::RepositoryExtensions,
            SettingControl::RepositoryExtensions,
        ),
        (Kind::Duration, SettingControl::Text),
    ];
    for (kind, want) in cases {
        assert_eq!(kind.control(), want);
    }
}

#[test]
fn fixed_choices_keep_the_schemas_order() {
    // `ThinkingLevel::NAMES` is the doc's order; a sorted list would put
    // `high` before `low`.
    let want: Vec<String> = ThinkingLevel::NAMES
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    assert_ne!(
        {
            let mut sorted = want.clone();
            sorted.sort();
            sorted
        },
        want,
        "the levels are not alphabetical, so this test tells order from sorting"
    );
    assert_eq!(control_of("thinking"), SettingControl::Fixed(want));
    assert_eq!(
        control_of("cache.lifetime"),
        SettingControl::Fixed(vec!["5m".into(), "1h".into()])
    );
    assert_eq!(
        control_of("diagnostics.level"),
        SettingControl::Fixed(vec!["info".into(), "debug".into()])
    );
}

#[test]
fn model_and_theme_keys_are_not_plain_text() {
    assert_eq!(control_of("model"), SettingControl::Model);
    assert_eq!(control_of("reviewer.model"), SettingControl::Model);
    assert_eq!(control_of("tui.theme"), SettingControl::Theme);
    // A free-text key stays text.
    assert_eq!(control_of("hub.default"), SettingControl::Text);
}

#[test]
fn model_and_theme_check_a_value_as_a_string_does() {
    for kind in [Kind::Model, Kind::Theme] {
        // A theme file's name, or a reference with a credential label,
        // is any string.
        assert!(kind.accepts(&serde_json::json!("anything/at-all:label")));
        assert!(kind.accepts(&serde_json::json!("")));
        assert!(!kind.accepts(&serde_json::json!(1)));
        assert!(!kind.accepts(&serde_json::json!(["a"])));
        assert_eq!(kind.expected(), Kind::Str.expected());
    }
}

#[test]
fn string_lists_and_object_lists_have_different_controls() {
    assert_eq!(control_of("scoped_models"), SettingControl::StringList);
    assert_eq!(control_of("skills.disabled"), SettingControl::StringList);
    assert_eq!(
        control_of("repository_extensions"),
        SettingControl::RepositoryExtensions
    );
}

#[test]
fn scalar_keys_get_boolean_number_and_text_controls() {
    assert_eq!(control_of("sandbox.enabled"), SettingControl::Boolean);
    assert_eq!(
        control_of("compaction.window_fraction"),
        SettingControl::Number
    );
    assert_eq!(control_of("hub.port"), SettingControl::Number);
    assert_eq!(
        control_of("model_lists.refresh_after"),
        SettingControl::Text
    );
}
