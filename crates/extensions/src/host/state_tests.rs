use contract::events::{Event, OnFork};
use contract::hook::StateSeed;
use serde_json::{Value, json};

use super::state::{MAX_BYTES, Scope, StateError, StateRegistry};

#[test]
fn store_landings_count_only_accepted_writes_and_admission_is_monotone() {
    for (landing, expected) in [(2, 2), (3, 1), (4, 0)] {
        let registry = registry();
        let store = registry.store("goal");
        store
            .set(Scope::Other, "k", json!(1), OnFork::AtPoint)
            .unwrap();
        store.unset(Scope::Other, "absent").unwrap();
        store
            .set(Scope::Other, "k", json!(2), OnFork::AtPoint)
            .unwrap();
        store.unset(Scope::Other, "k").unwrap();
        let line = contract::hook::StateLine {
            event: Event::ExtensionStateUnset(contract::events::ExtensionStateUnset {
                extension: "goal".into(),
                key: "k".into(),
            }),
            extension: "goal".into(),
            key: "k".into(),
            landing,
            context: None,
        };
        assert_eq!(registry.admit(vec![line.clone()]), [line.event.clone()]);
        assert_eq!(registry.take_outbox().len(), expected);
        assert!(registry.take_outbox().is_empty());
    }
}

#[test]
fn store_close_preserves_reads_and_outbox_but_refuses_every_write() {
    let registry = registry();
    let store = registry.store("goal");
    store
        .set(Scope::Other, "k", json!(1), OnFork::AtPoint)
        .unwrap();
    let oversize = Value::String("x".repeat(MAX_BYTES - 1));
    assert!(matches!(
        store.set(Scope::Other, "k", oversize.clone(), OnFork::Latest),
        Err(StateError::TooLarge { .. })
    ));
    registry.close();
    registry.close();
    assert_eq!(
        store.set(Scope::Other, "k", json!(2), OnFork::Fresh),
        Err(StateError::Closing)
    );
    assert_eq!(store.unset(Scope::Other, "k"), Err(StateError::Closing));
    assert_eq!(
        store.unset(Scope::Other, "missing"),
        Err(StateError::Closing)
    );
    assert_eq!(
        store.set(Scope::Other, "k", oversize, OnFork::Latest),
        Err(StateError::Closing)
    );
    assert_eq!(store.get("k"), Some(json!(1)));
    assert_eq!(store.keys(), ["k"]);
    assert_eq!(registry.take_outbox().len(), 1);
    assert!(registry.take_outbox().is_empty());
    let other = registry.store("other");
    assert_eq!(
        other.set(Scope::Other, "k", json!(1), OnFork::AtPoint),
        Err(StateError::Closing)
    );
    assert_eq!(other.get("k"), None);
    assert!(registry.take_outbox().is_empty());
}

fn registry() -> std::sync::Arc<StateRegistry> {
    StateRegistry::new(StateSeed::new())
}

#[test]
fn store_sets_gets_and_sorts_keys() {
    let registry = registry();
    let store = registry.store("goal");
    assert_eq!(store.get("missing"), None);
    assert!(store.keys().is_empty());
    for key in ["z", "a", "A"] {
        store
            .set(Scope::Other, key, json!({"n": 3}), OnFork::AtPoint)
            .unwrap();
    }
    assert_eq!(store.get("a"), Some(json!({"n": 3})));
    assert_eq!(store.keys(), ["A", "a", "z"]);
    let events = registry.take_outbox();
    let keys: Vec<_> = events
        .iter()
        .map(|event| match event {
            Event::ExtensionStateSet(set) => set.key.as_str(),
            _ => panic!("expected a set"),
        })
        .collect();
    assert_eq!(keys, ["z", "a", "A"]);
    assert!(registry.take_outbox().is_empty());
}

#[test]
fn store_unsets_present_keys_only() {
    let registry = registry();
    let store = registry.store("goal");
    store.unset(Scope::Other, "absent").unwrap();
    assert!(registry.take_outbox().is_empty());
    store
        .set(Scope::Other, "k", json!(1), OnFork::AtPoint)
        .unwrap();
    store.unset(Scope::Other, "k").unwrap();
    assert_eq!(store.get("k"), None);
    assert!(store.keys().is_empty());
    let events = registry.take_outbox();
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[1], Event::ExtensionStateUnset(unset) if unset.extension == "goal" && unset.key == "k")
    );
    store.unset(Scope::Other, "k").unwrap();
    assert!(registry.take_outbox().is_empty());
}

#[test]
fn store_records_every_fork_rule_and_repeated_value() {
    let registry = registry();
    let store = registry.store("goal");
    for rule in [
        OnFork::AtPoint,
        OnFork::Latest,
        OnFork::Fresh,
        OnFork::Fresh,
    ] {
        store.set(Scope::Other, "k", json!(1), rule).unwrap();
    }
    let rules: Vec<_> = registry
        .take_outbox()
        .into_iter()
        .map(|event| match event {
            Event::ExtensionStateSet(set) => set.on_fork,
            _ => panic!("expected a set"),
        })
        .collect();
    assert_eq!(
        rules,
        [
            OnFork::AtPoint,
            OnFork::Latest,
            OnFork::Fresh,
            OnFork::Fresh
        ]
    );
}

#[test]
fn store_caps_serialized_value_bytes_without_changing_the_old_value() {
    let registry = registry();
    let store = registry.store("goal");
    for bytes in [MAX_BYTES - 1, MAX_BYTES] {
        let value = Value::String("x".repeat(bytes - 2));
        assert_eq!(serde_json::to_vec(&value).unwrap().len(), bytes);
        store
            .set(Scope::Other, "k", value.clone(), OnFork::AtPoint)
            .unwrap();
        assert_eq!(store.get("k"), Some(value));
        assert_eq!(registry.take_outbox().len(), 1);
    }
    let old = store.get("k");
    let too_large = Value::String("x".repeat(MAX_BYTES - 1));
    assert!(
        matches!(store.set(Scope::Other, "k", too_large, OnFork::Latest), Err(StateError::TooLarge { bytes }) if bytes == MAX_BYTES + 1)
    );
    assert_eq!(store.get("k"), old);
    assert!(registry.take_outbox().is_empty());
    assert!(matches!(
        store.set(
            Scope::Other,
            "k",
            json!({"nested": "x".repeat(MAX_BYTES)}),
            OnFork::AtPoint
        ),
        Err(StateError::TooLarge { .. })
    ));
    assert_eq!(store.get("k"), old);
    assert!(registry.take_outbox().is_empty());
}

#[test]
fn store_seeds_without_logging_and_isolates_extensions() {
    let seed = StateSeed::from([(
        "goal".into(),
        [("k".into(), (json!("seed"), OnFork::Latest))].into(),
    )]);
    let registry = StateRegistry::new(seed);
    let store = registry.store("goal");
    assert_eq!(store.get("k"), Some(json!("seed")));
    assert_eq!(store.keys(), ["k"]);
    assert_eq!(registry.store("other").get("k"), None);
    assert!(registry.take_outbox().is_empty());
    store
        .set(Scope::Other, "k", json!("seed"), OnFork::Latest)
        .unwrap();
    assert_eq!(registry.take_outbox().len(), 1);
}
