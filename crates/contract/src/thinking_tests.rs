use super::*;

#[test]
fn round_trips_each_level_through_serde() {
    for level in ThinkingLevel::ALL {
        let name = level.as_str();
        let parsed: ThinkingLevel = serde_json::from_value(serde_json::json!(name)).expect(name);
        assert_eq!(parsed, level);
        assert_eq!(serde_json::json!(level), serde_json::json!(name));
        assert_eq!(name.parse::<ThinkingLevel>().expect(name), level);
    }
}

#[test]
fn rejects_unknown_and_wrong_case_names() {
    assert!("on".parse::<ThinkingLevel>().is_err());
    assert!("High".parse::<ThinkingLevel>().is_err());
    assert!("".parse::<ThinkingLevel>().is_err());
    assert!(serde_json::from_value::<ThinkingLevel>(serde_json::json!("High")).is_err());
}

#[test]
fn displays_each_level_by_its_name() {
    let shown: Vec<String> = ThinkingLevel::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        ["off", "minimal", "low", "medium", "high", "xhigh", "max"]
    );
}
