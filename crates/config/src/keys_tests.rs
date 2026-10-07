use super::*;

#[test]
fn thinking_key_values_match_the_contract_levels() {
    assert_eq!(
        LEVELS,
        contract::ThinkingLevel::ALL.map(contract::ThinkingLevel::as_str)
    );
}

#[test]
fn hub_port_is_a_port_number_a_repository_may_not_set() {
    let port = leaf(&["hub".to_owned(), "port".to_owned()]).expect("hub.port is a key");
    assert!(!port.repo);
    assert!(port.default.is_none());
    for good in [serde_json::json!(4040), serde_json::json!(65535)] {
        assert!(port.kind.accepts(&good), "{good}");
    }
    for bad in [
        serde_json::json!("x"),
        serde_json::json!(65536),
        serde_json::json!(-1),
    ] {
        assert!(!port.kind.accepts(&bad), "{bad}");
    }
}
