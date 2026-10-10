//! The hint table in `docs/mcp.md`, one row per line, plus the override rule.

use contract::shapes::{DeclaredEffects, Effect};
use serde_json::json;

use super::Hints;

fn hints(read_only: Option<bool>, destructive: Option<bool>, open_world: Option<bool>) -> Hints {
    Hints {
        read_only,
        destructive,
        open_world,
    }
}

#[test]
fn each_hint_combination_declares_its_effects() {
    let rows: &[(&str, Hints, Vec<Effect>, bool)] = &[
        (
            "empty",
            hints(None, None, None),
            vec![Effect::Executes, Effect::Network],
            false,
        ),
        (
            "readOnly true",
            hints(Some(true), None, None),
            vec![Effect::Reads, Effect::Network],
            true,
        ),
        (
            "readOnly true with destructive true",
            hints(Some(true), Some(true), None),
            vec![Effect::Reads, Effect::Network],
            true,
        ),
        (
            "readOnly false",
            hints(Some(false), None, None),
            vec![Effect::Executes, Effect::Network],
            false,
        ),
        (
            "destructive true",
            hints(None, Some(true), None),
            vec![Effect::Writes, Effect::Network],
            false,
        ),
        (
            "destructive false",
            hints(None, Some(false), None),
            vec![Effect::Writes, Effect::Network],
            true,
        ),
        (
            "openWorld true",
            hints(None, None, Some(true)),
            vec![Effect::Executes, Effect::Network],
            false,
        ),
        (
            "openWorld false",
            hints(None, None, Some(false)),
            vec![Effect::Executes],
            false,
        ),
        (
            "readOnly true with openWorld false",
            hints(Some(true), None, Some(false)),
            vec![Effect::Reads],
            true,
        ),
    ];
    for (name, hints, effects, reversible) in rows {
        assert_eq!(
            hints.declared(),
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
