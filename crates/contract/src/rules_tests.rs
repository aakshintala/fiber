use super::*;

#[test]
fn a_rule_round_trips_with_and_without_added_and_session() {
    let full = Rule {
        decision: RuleDecision::Allow,
        tool: "shell".into(),
        prefix: "npm test".into(),
        added: Some(1_700_000_000_000),
        session_id: Some(SessionId("s_0123456789abcdef".into())),
    };
    let line = serde_json::to_string(&full).unwrap();
    assert_eq!(
        line,
        r#"{"decision":"allow","tool":"shell","prefix":"npm test","added":1700000000000,"session_id":"s_0123456789abcdef"}"#
    );
    assert_eq!(serde_json::from_str::<Rule>(&line).unwrap(), full);

    let plain: Rule =
        serde_json::from_str(r#"{"decision":"ask","tool":"shell","prefix":"npm publish"}"#)
            .unwrap();
    assert_eq!(plain.added, None);
    assert_eq!(plain.session_id, None);
    let line = serde_json::to_string(&plain).unwrap();
    assert_eq!(
        line,
        r#"{"decision":"ask","tool":"shell","prefix":"npm publish"}"#
    );
    assert_eq!(serde_json::from_str::<Rule>(&line).unwrap(), plain);
}

#[test]
fn unknown_keys_are_ignored() {
    let rule: Rule =
        serde_json::from_str(r#"{"decision":"deny","tool":"shell","prefix":"rm","later":"x"}"#)
            .unwrap();
    assert_eq!(rule.prefix, "rm");
}

#[test]
fn an_unknown_decision_fails_to_read() {
    assert!(
        serde_json::from_str::<Rule>(r#"{"decision":"maybe","tool":"shell","prefix":"npm"}"#)
            .is_err()
    );
}

#[test]
fn display_writes_the_message_it_was_built_with() {
    let message = "project rules file line 3: not an object";
    assert_eq!(format!("{}", RulesError(message.to_owned())), message);
}
