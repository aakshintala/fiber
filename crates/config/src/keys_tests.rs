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
        (r#"{"diagnostics": {"level": "debug"}}"#, "diagnostics.level"),
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
