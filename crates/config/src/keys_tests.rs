use super::*;

#[test]
fn thinking_key_values_match_the_contract_levels() {
    assert_eq!(
        LEVELS,
        contract::ThinkingLevel::ALL.map(contract::ThinkingLevel::as_str)
    );
}
