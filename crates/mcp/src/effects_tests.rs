//! One test per input class, each asserting the exact effects vector.

use contract::shapes::{DeclaredEffects, Effect};
use serde_json::{Map, Value, json};

use super::Hints;

fn declared(hints: &Hints) -> DeclaredEffects {
    hints.declared()
}

fn annotations(value: Value) -> Hints {
    Hints::from_annotations(&value)
}

#[test]
fn read_only_true_is_reads_reversible_with_network() {
    let effects = declared(&annotations(json!({"readOnlyHint": true})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Reads, Effect::Network],
            reversible: true,
            paths: None,
        }
    );
}

#[test]
fn read_only_true_wins_over_destructive_true() {
    let effects = declared(&annotations(
        json!({"readOnlyHint": true, "destructiveHint": true}),
    ));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Reads, Effect::Network],
            reversible: true,
            paths: None,
        }
    );
}

#[test]
fn destructive_true_is_writes_irreversible_with_network() {
    let effects = declared(&annotations(json!({"destructiveHint": true})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Writes, Effect::Network],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn destructive_false_is_writes_reversible_with_network() {
    let effects = declared(&annotations(json!({"destructiveHint": false})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Writes, Effect::Network],
            reversible: true,
            paths: None,
        }
    );
}

#[test]
fn read_only_false_alone_is_executes() {
    let effects = declared(&annotations(json!({"readOnlyHint": false})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Executes, Effect::Network],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn no_hints_is_executes_and_network_irreversible() {
    let effects = declared(&annotations(json!({})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Executes, Effect::Network],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn missing_annotations_is_executes_and_network() {
    let effects = declared(&annotations(Value::Null));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Executes, Effect::Network],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn open_world_true_adds_network() {
    let effects = declared(&annotations(
        json!({"readOnlyHint": true, "openWorldHint": true}),
    ));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Reads, Effect::Network],
            reversible: true,
            paths: None,
        }
    );
}

#[test]
fn open_world_false_removes_network() {
    let effects = declared(&annotations(
        json!({"readOnlyHint": true, "openWorldHint": false}),
    ));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Reads],
            reversible: true,
            paths: None,
        }
    );
}

#[test]
fn open_world_false_alone_is_executes_without_network() {
    let effects = declared(&annotations(json!({"openWorldHint": false})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn a_non_boolean_hint_is_absent() {
    let effects = declared(&annotations(json!({"readOnlyHint": "yes"})));
    assert_eq!(
        effects,
        DeclaredEffects {
            effects: vec![Effect::Executes, Effect::Network],
            reversible: false,
            paths: None,
        }
    );
}

#[test]
fn the_override_replaces_rather_than_merges() {
    let server =
        annotations(json!({"readOnlyHint": true, "destructiveHint": true, "openWorldHint": false}));
    let over = Hints::from_override(
        json!({"destructiveHint": false})
            .as_object()
            .expect("object literal"),
    );
    assert_eq!(
        over,
        Hints {
            read_only: None,
            destructive: Some(false),
            open_world: None,
        }
    );
    assert_eq!(
        over.declared(),
        DeclaredEffects {
            effects: vec![Effect::Writes, Effect::Network],
            reversible: true,
            paths: None,
        }
    );
    let _ = server;
}

#[test]
fn an_empty_override_clears_every_hint() {
    let over = Hints::from_override(&Map::new());
    assert_eq!(
        over.declared().effects,
        vec![Effect::Executes, Effect::Network]
    );
}
