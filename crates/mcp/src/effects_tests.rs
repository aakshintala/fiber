//! The hint table in `docs/mcp.md`, one row per line, plus the override rule.

use contract::shapes::{DeclaredEffects, Effect};
use serde_json::{Value, json};

use super::Hints;

fn annotations(value: Value) -> Hints {
    Hints::from_annotations(&value)
}

#[test]
fn each_hint_combination_declares_its_effects() {
    let rows: &[(&str, Value, Vec<Effect>, bool)] = &[
        ("empty", json!({}), vec![Effect::Executes, Effect::Network], false),
        (
            "readOnly true",
            json!({"readOnlyHint": true}),
            vec![Effect::Reads, Effect::Network],
            true,
        ),
        (
            "readOnly true with destructive true",
            json!({"readOnlyHint": true, "destructiveHint": true}),
            vec![Effect::Reads, Effect::Network],
            true,
        ),
        (
            "readOnly false",
            json!({"readOnlyHint": false}),
            vec![Effect::Executes, Effect::Network],
            false,
        ),
        (
            "destructive true",
            json!({"destructiveHint": true}),
            vec![Effect::Writes, Effect::Network],
            false,
        ),
        (
            "destructive false",
            json!({"destructiveHint": false}),
            vec![Effect::Writes, Effect::Network],
            true,
        ),
        (
            "openWorld true",
            json!({"openWorldHint": true}),
            vec![Effect::Executes, Effect::Network],
            false,
        ),
        (
            "openWorld false",
            json!({"openWorldHint": false}),
            vec![Effect::Executes],
            false,
        ),
        (
            "readOnly true with openWorld false",
            json!({"readOnlyHint": true, "openWorldHint": false}),
            vec![Effect::Reads],
            true,
        ),
    ];
    for (name, hints, effects, reversible) in rows {
        assert_eq!(
            annotations(hints.clone()).declared(),
            DeclaredEffects {
                effects: effects.clone(),
                reversible: *reversible,
                paths: None,
            },
            "row: {name}",
        );
    }
}

#[test]
fn the_override_replaces_rather_than_merges() {
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
}
