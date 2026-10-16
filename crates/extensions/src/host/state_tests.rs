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
        let expected_event = line.event.clone();
        assert_eq!(registry.admit(vec![line]), vec![expected_event]);
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
    if let Event::ExtensionStateSet(set) = event {
        *value = Some(set.value.clone());
    } else {
        assert!(
            matches!(event, Event::ExtensionStateUnset(_)),
            "expected state event"
        );
        *value = None;
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

#[test]
fn lua_state_accepts_each_explicit_fork_rule_and_empty_options() {
    for (options, expected) in [
        ("{}", OnFork::AtPoint),
        ("{on_fork='at_point'}", OnFork::AtPoint),
        ("{on_fork='latest'}", OnFork::Latest),
        ("{on_fork='fresh'}", OnFork::Fresh),
    ] {
        let (value, events) = lua_api(
            &format!("state.set('k',true,{options}); return json.encode(state.get('k'))"),
            StateSeed::new(),
        );
        assert_eq!(value, true);
        let [Event::ExtensionStateSet(set)] = events.as_slice() else {
            panic!("one set")
        };
        assert_eq!(set.on_fork, expected);
    }
}

fn lua_api(script: &str, seed: StateSeed) -> (Value, Vec<Event>) {
    let dir = fakes::TempDir::new("fiber-state-api");
    std::fs::write(
        dir.path().join("init.lua"),
        format!("fiber.command('test', {{timeout=5000, run=function() {script} end}})"),
    )
    .unwrap();
    let registry = StateRegistry::new(seed);
    let state = registry.clone();
    let value = crate::test_support::bounded(move || {
        let ext = crate::LuaExtension::new(
            "goal",
            dir.path(),
            dir.path(),
            fakes::clock::FakeClock::new(),
        )
        .with_state(state);
        let result = ext.command("test", "").unwrap();
        serde_json::from_str(&result).unwrap()
    });
    (value, registry.take_outbox())
}

#[test]
fn lua_state_errors_are_coded_tables_with_messages() {
    for call in [
        "state.set('k', nil)",
        "state.set(1, 'v')",
        "state.set('k', function() end)",
        "state.set('k', 0/0)",
        "state.set('k', math.huge)",
        "state.set('k', {[true]=1})",
        "state.set('k', coroutine.create(function() end))",
        "state.set('k', 'v', {on_fork='unknown'})",
        "state.set('k', 'v', false)",
        "state.set('k', 'v', {on_fork=false})",
        "state.get(1)",
        "state.unset(1)",
    ] {
        let (value, events) = lua_api(
            &format!(
                "local ok, err = pcall(function() {call} end); return json.encode({{ok=ok, code=err.code, message=err.message}})"
            ),
            StateSeed::new(),
        );
        assert_eq!(value["ok"], false, "{call}");
        assert_eq!(value["code"], "invalid_arguments", "{call}");
        assert!(!value["message"].as_str().unwrap().is_empty(), "{call}");
        if call == "state.set('k', nil)" {
            assert!(value["message"].as_str().unwrap().contains("state.unset"));
        }
        assert!(events.is_empty(), "{call}");
    }
}

#[test]
fn lua_state_size_edges_and_default_rule() {
    for (bytes, code) in [
        (MAX_BYTES - 1, "ok"),
        (MAX_BYTES, "ok"),
        (MAX_BYTES + 1, "state_too_large"),
    ] {
        let (value, events) = lua_api(
            &format!(
                "local ok, err = pcall(state.set, 'k', string.rep('x', {})); return json.encode({{code=ok and 'ok' or err.code, message=ok and '' or err.message}})",
                bytes - 2
            ),
            StateSeed::new(),
        );
        assert_eq!(value["code"], code);
        match events.as_slice() {
            [Event::ExtensionStateSet(set)] => {
                assert_eq!(code, "ok");
                assert_eq!(set.on_fork, OnFork::AtPoint);
                assert_eq!(serde_json::to_vec(&set.value).unwrap().len(), bytes);
            }
            [] => {
                assert_eq!(code, "state_too_large");
                assert!(!value["message"].as_str().unwrap().is_empty());
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }
}

#[test]
fn lua_state_uses_host_json_conversion_and_reads_sorted_keys() {
    let (value, events) = lua_api(
        r#"
        local inputs = { {'a','b'}, {a=1}, {}, {a=1,[1]=2}, {[1]='a',[3]='c'} }
        for i, t in ipairs(inputs) do state.set(tostring(i), t, {on_fork='latest'}) end
        state.set('z', 1); state.unset('z'); state.unset('absent')
        return json.encode({values={state.get('1'),state.get('2'),state.get('3'),state.get('4'),state.get('5')}, keys=state.keys(), absent=state.get('absent') == nil})
    "#,
        StateSeed::new(),
    );
    assert_eq!(
        value,
        json!({"values":[["a","b"],{"a":1},[],{"a":1,"1":2},{"1":"a","3":"c"}],"keys":["1","2","3","4","5"],"absent":true})
    );
    assert_eq!(events.len(), 7);
    for event in events.iter().take(5) {
        let Event::ExtensionStateSet(set) = event else {
            panic!("expected set")
        };
        assert_eq!(set.on_fork, OnFork::Latest);
    }
}

#[test]
fn lua_state_is_seeded_before_init_shared_by_vms_and_closes() {
    let dir = fakes::TempDir::new("fiber-state-seed");
    std::fs::write(dir.path().join("init.lua"), "local initial = state.get('k'); fiber.command('read', {timeout=5000,run=function() return json.encode({initial=initial, current=state.get('k')}) end}); fiber.command('write', {timeout=5000,run=function() local ok,e=pcall(state.set,'k',3); return ok and 'ok' or e.code end})").unwrap();
    let registry = StateRegistry::new(StateSeed::from([(
        "goal".into(),
        [("k".into(), (json!(1), OnFork::AtPoint))].into(),
    )]));
    crate::test_support::bounded(move || {
        let make = || {
            crate::LuaExtension::new(
                "goal",
                dir.path(),
                dir.path(),
                fakes::clock::FakeClock::new(),
            )
            .with_state(registry.clone())
        };
        let a = make();
        let b = make();
        assert_eq!(
            a.command("read", "").unwrap(),
            "{\"current\":1,\"initial\":1}"
        );
        assert!(registry.take_outbox().is_empty());
        assert_eq!(b.command("write", "").unwrap(), "ok");
        assert_eq!(
            a.command("read", "").unwrap(),
            "{\"current\":3,\"initial\":1}"
        );
        registry.close();
        assert_eq!(a.command("write", "").unwrap(), "closing");
        assert_eq!(
            a.command("read", "").unwrap(),
            "{\"current\":3,\"initial\":1}"
        );
        assert_eq!(registry.take_outbox().len(), 1);
    });
}

#[test]
fn lua_state_depth_limit_round_trips_inside_the_log_envelope() {
    for depth in [super::MAX_DEPTH - 1, super::MAX_DEPTH, super::MAX_DEPTH + 1] {
        let (value, events) = lua_api(
            &format!(
                "local t={{}}; for _=2,{depth} do t={{t}} end; local ok,e=pcall(state.set,'deep',t); return json.encode({{ok=ok, code=ok and 'ok' or e.code}})"
            ),
            StateSeed::new(),
        );
        if depth <= super::MAX_DEPTH {
            assert_eq!(value["ok"], true);
            let [event] = events.as_slice() else {
                panic!("one accepted write")
            };
            let envelope = contract::Envelope {
                kind: "extension_state_set".into(),
                session_id: contract::SessionId("state".into()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: Some(contract::Seq(1)),
                payload: event.payload().unwrap(),
            };
            let bytes = serde_json::to_vec(&envelope).unwrap();
            let read: contract::Envelope = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                contract::events::Event::from_envelope(&read)
                    .unwrap()
                    .as_ref(),
                Some(event)
            );
        } else {
            assert_eq!(value["ok"], false);
            assert_eq!(value["code"], "invalid_arguments");
            assert!(events.is_empty());
        }
    }
}

#[test]
fn lua_state_and_emit_share_conversion_and_timer_writes_are_unscoped() {
    struct Messages(std::sync::mpsc::Sender<Value>);
    impl contract::emit::Emit for Messages {
        fn emit(&self, event: &Event) {
            if let Event::ExtensionMessage(message) = event {
                let _sent = self.0.send(message.data.clone());
            }
        }
    }
    let dir = fakes::TempDir::new("fiber-state-timer");
    std::fs::write(dir.path().join("init.lua"), "fiber.command('test', {timeout=5000,run=function() local t={a=1,[1]=2}; state.set('mixed',t); host.emit(t); host.after(0,function() state.set('timer',true); host.emit({timer=true}) end,{timeout=5000}); return 'armed' end})").unwrap();
    let registry = registry();
    let held = registry.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    crate::test_support::bounded(move || {
        let ext = crate::LuaExtension::new(
            "goal",
            dir.path(),
            dir.path(),
            fakes::clock::FakeClock::new(),
        )
        .with_state(held);
        ext.set_emit(std::sync::Arc::new(Messages(tx)));
        assert_eq!(ext.command("test", "").unwrap(), "armed");
        let deadline = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        assert_eq!(
            deadline.recv_or_fail(&rx, "mixed value emitted"),
            json!({"a":1,"1":2})
        );
        assert_eq!(
            deadline.recv_or_fail(&rx, "timer state write completed"),
            json!({"timer":true})
        );
    });
    let events = registry.take_outbox();
    let [
        Event::ExtensionStateSet(mixed),
        Event::ExtensionStateSet(timer),
    ] = events.as_slice()
    else {
        panic!("one command and one timer write: {events:?}")
    };
    assert_eq!(mixed.value, json!({"a":1,"1":2}));
    assert_eq!(timer.key, "timer");
    assert_eq!(timer.value, true);
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
        .map(|event| {
            let Event::ExtensionStateSet(set) = event else {
                panic!("expected a set")
            };
            set.key.as_str()
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
        .map(|event| {
            let Event::ExtensionStateSet(set) = event else {
                panic!("expected a set")
            };
            set.on_fork
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
