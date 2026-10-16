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

#[test]
fn store_hook_trials_take_discard_and_dead_writes() {
    let registry = StateRegistry::new(StateSeed::from([(
        "goal".into(),
        [("k".into(), (json!("Z"), OnFork::Latest))].into(),
    )]));
    let store = registry.store("goal");
    store.begin(7);
    store
        .set(Scope::Hook(7), "k", json!("A"), OnFork::AtPoint)
        .unwrap();
    assert_eq!(store.get("k"), Some(json!("A")));
    assert!(registry.take_outbox().is_empty());
    store
        .set(Scope::Other, "k", json!("B"), OnFork::AtPoint)
        .unwrap();
    store
        .set(Scope::Hook(7), "k", json!("C"), OnFork::AtPoint)
        .unwrap();
    store
        .set(Scope::Hook(7), "new", json!(1), OnFork::AtPoint)
        .unwrap();
    store.discard_call(7);
    assert_eq!(store.get("k"), Some(json!("B")));
    assert_eq!(store.keys(), ["k"]);
    assert_eq!(registry.take_outbox().len(), 1);
    store
        .set(Scope::Hook(7), "dead", json!(1), OnFork::AtPoint)
        .unwrap();
    store
        .set(Scope::Hook(9), "unknown", json!(1), OnFork::AtPoint)
        .unwrap();
    assert_eq!(store.keys(), ["k"]);
    assert!(registry.take_outbox().is_empty());
    assert!(store.take_call(7).is_empty());
    store.begin(8);
    store.unset(Scope::Hook(8), "k").unwrap();
    assert_eq!(store.get("k"), None);
    store.discard_call(8);
    assert_eq!(store.get("k"), Some(json!("B")));
    store.begin(10);
    store
        .set(Scope::Hook(10), "k", json!("taken"), OnFork::Fresh)
        .unwrap();
    let lines = store.take_call(10);
    assert_eq!(lines.len(), 1);
    assert!(store.take_call(10).is_empty());
    assert_eq!(store.get("k"), Some(json!("taken")));
    store
        .set(Scope::Hook(10), "k", json!("dead"), OnFork::AtPoint)
        .unwrap();
    assert_eq!(store.get("k"), Some(json!("taken")));
    assert!(registry.take_outbox().is_empty());
    store.begin(11);
    registry.close();
    assert_eq!(
        store.set(Scope::Hook(99), "k", json!(0), OnFork::AtPoint),
        Err(StateError::Closing)
    );
    assert!(store.take_call(11).is_empty());
    store.discard_call(11);
    assert_eq!(registry.admit(lines).len(), 1);
}

#[test]
fn store_generations_share_values_but_never_trials() {
    let registry = registry();
    let a = registry.store("goal");
    let b = registry.store("goal");
    a.begin(7);
    b.set(Scope::Hook(7), "k", json!("dead"), OnFork::AtPoint)
        .unwrap();
    b.discard_call(7);
    assert_eq!(a.get("k"), None);
    a.set(Scope::Hook(7), "k", json!("live"), OnFork::Latest)
        .unwrap();
    assert_eq!(b.get("k"), Some(json!("live")));
    assert_eq!(a.take_call(7).len(), 1);
    assert!(registry.take_outbox().is_empty());
}

#[test]
fn store_vm_scope_is_local_and_leave_restores_other_writes() {
    let registry = registry();
    let store = registry.store("goal");
    let hook = super::state::VmState {
        store: store.clone(),
        scope: std::cell::Cell::new(Scope::Other),
    };
    let provider = super::state::VmState {
        store: registry.store("goal"),
        scope: std::cell::Cell::new(Scope::Other),
    };
    store.begin(1);
    hook.enter(Scope::Hook(1));
    hook.set("hook", json!(1), OnFork::AtPoint).unwrap();
    provider.set("provider", json!(2), OnFork::Latest).unwrap();
    assert_eq!(hook.get("provider"), Some(json!(2)));
    assert_eq!(provider.get("hook"), Some(json!(1)));
    assert_eq!(hook.keys(), ["hook", "provider"]);
    assert_eq!(registry.take_outbox().len(), 1);
    hook.leave();
    hook.unset("provider").unwrap();
    assert_eq!(provider.get("provider"), None);
    assert_eq!(registry.take_outbox().len(), 1);
    let carried = store.take_call(1);
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].key, "hook");
    assert_eq!(carried[0].landing, 1);
    assert_eq!(registry.admit(carried).len(), 1);
    assert!(registry.take_outbox().is_empty());
}

fn apply_event(value: &mut Option<Value>, event: &Event) {
    match event {
        Event::ExtensionStateSet(set) => *value = Some(set.value.clone()),
        Event::ExtensionStateUnset(_) => *value = None,
        _ => panic!("expected state event"),
    }
}

#[test]
fn store_exhaustive_trial_admission_matches_surviving_writes() {
    // The model keeps accepted writes, not attempted unsets of absent keys.
    for seeded in [false, true] {
        for length in 0..=4_u32 {
            for encoded in 0..4_usize.pow(length) {
                for fate in 0..4 {
                    for outbox_first in [false, true] {
                        let initial = seeded.then(|| json!("Z"));
                        let seed = initial
                            .clone()
                            .map(|value| {
                                StateSeed::from([(
                                    "goal".into(),
                                    [("k".into(), (value, OnFork::Latest))].into(),
                                )])
                            })
                            .unwrap_or_default();
                        let registry = StateRegistry::new(seed);
                        let store = registry.store("goal");
                        store.begin(1);
                        let mut writes = Vec::new();
                        let mut view = initial.clone();
                        let mut code = encoded;
                        for index in 0..length {
                            let op = code % 4;
                            code /= 4;
                            let hook = op < 2;
                            let set = op % 2 == 0;
                            let scope = if hook { Scope::Hook(1) } else { Scope::Other };
                            if set {
                                let value = json!(index);
                                store
                                    .set(scope, "k", value.clone(), OnFork::AtPoint)
                                    .unwrap();
                                view = Some(value.clone());
                                writes.push((hook, Some(value)));
                            } else {
                                store.unset(scope, "k").unwrap();
                                if view.take().is_some() {
                                    writes.push((hook, None));
                                }
                            }
                            assert_eq!(store.get("k"), view);
                        }
                        let mut carried = if fate == 2 {
                            store.discard_call(1);
                            Vec::new()
                        } else {
                            store.take_call(1)
                        };
                        if fate == 3 {
                            registry.queue(std::mem::take(&mut carried));
                        }
                        if fate == 1 {
                            carried.clear();
                        }
                        let mut events = Vec::new();
                        if outbox_first {
                            events.extend(registry.take_outbox());
                        }
                        events.extend(registry.admit(carried));
                        events.extend(registry.take_outbox());
                        let mut replay = initial.clone();
                        for event in &events {
                            apply_event(&mut replay, event);
                        }
                        let expected_replay = writes
                            .iter()
                            .rev()
                            .find(|(hook, _)| !hook || (fate != 1 && fate != 2))
                            .map(|(_, value)| value.clone())
                            .unwrap_or_else(|| initial.clone());
                        let expected_view = if fate == 2 {
                            writes
                                .iter()
                                .rev()
                                .find(|(hook, _)| !hook)
                                .map(|(_, value)| value.clone())
                                .unwrap_or(initial)
                        } else {
                            view
                        };
                        assert_eq!(
                            replay, expected_replay,
                            "seed={seeded}, length={length}, ops={encoded}, fate={fate}, outbox_first={outbox_first}"
                        );
                        assert_eq!(store.get("k"), expected_view);
                        assert_eq!(store.keys().len(), usize::from(expected_view.is_some()));
                        assert!(store.take_call(1).is_empty());
                        assert!(registry.take_outbox().is_empty());
                    }
                }
            }
        }
    }
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
