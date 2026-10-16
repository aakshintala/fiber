use contract::events::{Event, ExtensionStateSet, ExtensionStateUnset, OnFork};
use contract::{Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::{Fold, Ruled, fold, fold_boundary, fold_from, seed};

fn line(seq: u64, event: Event) -> Envelope {
    Envelope {
        kind: event.kind().into(),
        session_id: SessionId("state".into()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    }
}
fn set(seq: u64, extension: &str, key: &str, value: Value, rule: OnFork) -> Envelope {
    line(
        seq,
        Event::ExtensionStateSet(ExtensionStateSet {
            extension: extension.into(),
            key: key.into(),
            value,
            on_fork: rule,
        }),
    )
}
fn unset(seq: u64, key: &str) -> Envelope {
    line(
        seq,
        Event::ExtensionStateUnset(ExtensionStateUnset {
            extension: "goal".into(),
            key: key.into(),
        }),
    )
}

#[test]
fn state_fold_resume_replaces_unsets_restores_and_isolates_extensions() {
    let lines = vec![
        set(0, "goal", "k", json!(1), OnFork::AtPoint),
        set(1, "other", "k", json!(2), OnFork::Fresh),
        unset(2, "k"),
        set(3, "goal", "k", json!(3), OnFork::Latest),
    ];
    assert_eq!(
        fold(&lines, Fold::Resume)["goal"]["k"],
        (json!(3), OnFork::Latest)
    );
    assert_eq!(fold(&lines[..3], Fold::Resume).get("goal"), None);
    assert_eq!(
        fold(&lines, Fold::Resume)["other"]["k"],
        (json!(2), OnFork::Fresh)
    );
    assert!(fold(&[unset(0, "absent")], Fold::Resume).is_empty());
}

#[test]
fn state_fold_point_edges_obey_the_last_rule_at_the_point() {
    for rule in [OnFork::AtPoint, OnFork::Latest, OnFork::Fresh] {
        let lines = vec![
            set(3, "goal", "k", json!("point"), rule),
            set(5, "goal", "k", json!("end"), OnFork::Fresh),
        ];
        for point in [2, 3, 4] {
            let result = fold(&lines, Fold::At { point });
            let expected = match (rule, point) {
                (OnFork::AtPoint, 3 | 4) => Some(json!("point")),
                (OnFork::Latest, 3 | 4) => Some(json!("end")),
                _ => None,
            };
            assert_eq!(
                result
                    .get("goal")
                    .and_then(|keys| keys.get("k"))
                    .map(|(value, _)| value.clone()),
                expected,
                "{rule:?}/{point}"
            );
            assert_eq!(
                result,
                seed(fold_boundary(Ruled::new(), &lines, &lines, point))
            );
        }
    }
}

#[test]
fn state_fold_chain_preserves_unset_rules_and_folds_child_writes_last() {
    let parent = vec![
        set(0, "goal", "a", json!(1), OnFork::AtPoint),
        set(1, "goal", "l", json!(1), OnFork::Latest),
        set(2, "goal", "f", json!(1), OnFork::Fresh),
        unset(3, "l"),
        set(4, "goal", "a", json!(2), OnFork::AtPoint),
        set(5, "goal", "l", json!(2), OnFork::Latest),
        set(6, "goal", "f", json!(2), OnFork::Fresh),
    ];
    let carry = fold_boundary(Ruled::new(), &parent[..4], &parent, 3);
    let first = seed(carry.clone());
    assert_eq!(first["goal"]["a"].0, json!(1));
    assert_eq!(first["goal"]["l"].0, json!(2));
    assert!(!first["goal"].contains_key("f"));
    assert_eq!(first, fold(&parent, Fold::At { point: 3 }));
    let child = vec![
        set(0, "goal", "a", json!(3), OnFork::AtPoint),
        unset(1, "l"),
    ];
    let child_view = fold_from(carry.clone(), &child, Fold::Resume);
    assert_eq!(seed(child_view.clone())["goal"]["a"].0, json!(3));
    assert!(!seed(child_view.clone())["goal"].contains_key("l"));
    let grandchild = fold_boundary(carry, &child, &child, 1);
    assert_eq!(seed(grandchild.clone())["goal"]["a"].0, json!(3));
    assert_eq!(grandchild["goal"]["l"], (None, OnFork::Latest));
    let end = vec![set(0, "goal", "l", json!(4), OnFork::AtPoint)];
    let inherited = fold_boundary(child_view, &[], &end, 0);
    assert_eq!(seed(inherited)["goal"]["l"], (json!(4), OnFork::Latest));
    let mut unset_end = parent.clone();
    unset_end.push(unset(7, "l"));
    assert!(!fold(&unset_end, Fold::At { point: 3 })["goal"].contains_key("l"));
    let at = vec![
        set(0, "goal", "k", json!(1), OnFork::AtPoint),
        unset(1, "k"),
    ];
    assert!(fold(&at, Fold::At { point: 1 }).is_empty());
}

#[test]
fn state_fold_skips_bad_payloads_and_round_trips_the_maximum_value() {
    let value = json!("x".repeat(65_534));
    let mut malformed = set(0, "goal", "bad", json!(1), OnFork::AtPoint);
    malformed.payload.remove("on_fork");
    let mut unrelated = malformed.clone();
    unrelated.kind = "notice".into();
    let lines = vec![
        malformed,
        unrelated,
        set(2, "goal", "good", value.clone(), OnFork::AtPoint),
    ];
    let result = fold(&lines, Fold::Resume);
    assert_eq!(result["goal"].len(), 1);
    assert_eq!(result["goal"]["good"].0, value);
    assert_eq!(
        serde_json::to_vec(&result["goal"]["good"].0).unwrap().len(),
        65_536
    );
}
