use super::*;

#[test]
fn thinking_key_values_match_the_contract_levels() {
    assert_eq!(
        LEVELS,
        contract::ThinkingLevel::ALL.map(contract::ThinkingLevel::as_str)
    );
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
