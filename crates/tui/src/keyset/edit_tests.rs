//! Tests for the `/keys` transactions: reading keys, one swap at a time,
//! and the edits one transaction saves.

use serde_json::{Map, Value, json};

use super::Refused;
use crate::bindings::BINDINGS;
use crate::configure::KeyEdit;
use crate::keyset::Keyset;
use crate::stroke::Stroke;

/// The keyset for `entries`, with its notices dropped.
fn keyset(entries: &[(&str, Value)]) -> Keyset {
    let mut user = Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    super::super::load(&user).0
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// Whether no two actions share a key in overlapping contexts.
fn clash_free(keys: &Keyset) -> bool {
    for (at, first) in BINDINGS.iter().enumerate() {
        for second in BINDINGS.iter().skip(at.saturating_add(1)) {
            if !first.contexts.overlaps(second.contexts) {
                continue;
            }
            if keys
                .current(first.id)
                .iter()
                .any(|key| keys.current(second.id).contains(key))
            {
                return false;
            }
        }
    }
    true
}

/// The default keyset with `id` holding `names`, bypassing validation:
/// a clash the loader would revert, for the give-back's refusal branch.
fn holding(id: &str, names: &[&str]) -> Keyset {
    let Keyset { mut rows } = keyset(&[]);
    let at = BINDINGS
        .iter()
        .position(|binding| binding.id == id)
        .unwrap_or_else(|| panic!("no action {id}"));
    if let Some(row) = rows.get_mut(at) {
        row.keys = names.iter().map(|name| stroke(name)).collect();
    }
    Keyset { rows }
}

#[test]
fn current_follows_the_person_while_defaults_do_not() {
    let plain = keyset(&[]);
    assert_eq!(plain.current("new_session"), [stroke("ctrl+n")]);
    assert_eq!(plain.defaults_of("new_session"), [stroke("ctrl+n")]);
    let person = keyset(&[("new_session", json!(["ctrl+t"]))]);
    assert_eq!(person.current("new_session"), [stroke("ctrl+t")]);
    assert_eq!(person.defaults_of("new_session"), [stroke("ctrl+n")]);
    assert!(plain.current("no_such_action").is_empty());
    assert!(plain.defaults_of("no_such_action").is_empty());
}

#[test]
fn labels_show_the_current_keys_grouped() {
    let keys = keyset(&[]);
    assert_eq!(keys.labels("new_session"), "Ctrl+N");
    assert_eq!(keys.labels("move_word"), "⌥← ⌥→, Ctrl+← Ctrl+→, ⌥B ⌥F");
    let unbound = keyset(&[("delete_word", json!([]))]);
    assert_eq!(unbound.labels("delete_word"), "unbound");
}

#[test]
fn variant_names_each_variant_and_none_for_one_variant() {
    assert_eq!(Keyset::variant("move_word", 0), Some("left".to_owned()));
    assert_eq!(Keyset::variant("move_word", 1), Some("right".to_owned()));
    assert_eq!(Keyset::variant("new_session", 0), None);
}

#[test]
fn a_free_key_binds_with_nothing_else_changed() {
    let before = keyset(&[]);
    let next = before
        .set("new_session", vec![stroke("ctrl+t")], &[])
        .unwrap_or_else(|refused| panic!("free key refused: {refused:?}"));
    assert_eq!(next.current("new_session"), [stroke("ctrl+t")]);
    assert_eq!(
        next.edits(&before),
        [KeyEdit {
            id: "new_session".to_owned(),
            keys: Some(vec!["ctrl+t".to_owned()]),
        }]
    );
}

#[test]
fn binding_the_defaults_clears_a_person_entry() {
    let before = keyset(&[("copy_focused", json!(["c"]))]);
    let next = before
        .set("copy_focused", vec![stroke("y")], &[])
        .unwrap_or_else(|refused| panic!("defaults refused: {refused:?}"));
    assert_eq!(
        next.edits(&before),
        [KeyEdit {
            id: "copy_focused".to_owned(),
            keys: None,
        }]
    );
}

#[test]
fn ctrl_c_and_clear_then_quit_are_fixed() {
    let keys = keyset(&[]);
    assert_eq!(
        keys.set("send", vec![stroke("ctrl+c")], &[]),
        Err(Refused::Fixed)
    );
    assert_eq!(
        keys.set("clear_then_quit", vec![stroke("ctrl+t")], &[]),
        Err(Refused::Fixed)
    );
    assert_eq!(
        keys.set("no_such_action", vec![stroke("x")], &[]),
        Err(Refused::Fixed)
    );
}

#[test]
fn a_key_at_two_places_is_a_repeat() {
    let keys = keyset(&[]);
    assert_eq!(
        keys.set("move_word", vec![stroke("ctrl+b"), stroke("ctrl+b")], &[]),
        Err(Refused::Repeat(stroke("ctrl+b")))
    );
}

#[test]
fn a_key_held_in_a_shared_context_clashes() {
    let keys = keyset(&[]);
    assert_eq!(
        keys.set("send", vec![stroke("ctrl+o")], &[]),
        Err(Refused::Clash {
            other: "toggle_ledgers",
            stroke: stroke("ctrl+o"),
        })
    );
}

#[test]
fn a_key_held_only_in_a_disjoint_context_binds() {
    let before = keyset(&[("send", json!(["ctrl+s"]))]);
    for id in ["open_focused", "search_results"] {
        let next = before
            .set(id, vec![stroke("ctrl+s")], &[])
            .unwrap_or_else(|refused| panic!("{id} refused: {refused:?}"));
        assert_eq!(next.current(id), [stroke("ctrl+s")]);
        assert!(clash_free(&next), "{id} shares ctrl+s");
    }
}

#[test]
fn unbinding_never_clashes() {
    let before = keyset(&[]);
    let next = before
        .set("send", Vec::new(), &[])
        .unwrap_or_else(|refused| panic!("unbind refused: {refused:?}"));
    assert!(next.current("send").is_empty());
    assert!(clash_free(&next));
}

#[test]
fn a_swap_moves_the_key_and_hands_one_back() {
    let before = keyset(&[]);
    let next = before
        .set("copy_focused", vec![stroke("enter")], &["open_focused"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    assert_eq!(next.current("copy_focused"), [stroke("enter")]);
    assert_eq!(next.current("open_focused"), [stroke("y")]);
    assert!(clash_free(&next));
}

#[test]
fn swapping_with_itself_is_skipped() {
    let keys = keyset(&[]);
    assert_eq!(
        keys.set("copy_focused", vec![stroke("enter")], &["copy_focused"]),
        Err(Refused::Clash {
            other: "open_focused",
            stroke: stroke("enter"),
        })
    );
}

#[test]
fn swapping_with_an_action_holding_none_of_the_keys_changes_nothing() {
    let before = keyset(&[]);
    let next = before
        .set("new_session", vec![stroke("ctrl+t")], &["toggle_ledgers"])
        .unwrap_or_else(|refused| panic!("free key refused: {refused:?}"));
    assert_eq!(next.current("toggle_ledgers"), [stroke("ctrl+o")]);
}

#[test]
fn naming_only_one_of_two_clashing_actions_still_refuses() {
    let keys = keyset(&[]);
    assert_eq!(
        keys.set(
            "move_word",
            vec![stroke("ctrl+o"), stroke("ctrl+r")],
            &["toggle_ledgers"]
        ),
        Err(Refused::Clash {
            other: "search_prompts",
            stroke: stroke("ctrl+r"),
        })
    );
}

#[test]
fn the_give_back_is_the_action_key_for_the_captured_variant() {
    let before = keyset(&[]);
    let next = before
        .set("search_prompts", vec![stroke("alt+left")], &["move_word"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    assert_eq!(
        next.current("move_word"),
        [
            stroke("ctrl+r"),
            stroke("alt+right"),
            stroke("ctrl+left"),
            stroke("ctrl+right"),
            stroke("alt+b"),
            stroke("alt+f"),
        ]
    );
    assert!(clash_free(&next));
}

#[test]
fn a_swap_hands_back_for_every_repeated_key() {
    let before = holding("move_word", &["alt+left", "alt+right", "alt+left", "f5"]);
    let next = before
        .set("search_prompts", vec![stroke("alt+left")], &["move_word"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    assert_eq!(
        next.current("move_word"),
        [
            stroke("ctrl+r"),
            stroke("alt+right"),
            stroke("ctrl+r"),
            stroke("f5"),
        ]
    );
    assert!(clash_free(&next));
}

#[test]
fn without_a_give_back_a_repeated_key_loses_every_group() {
    let Keyset { mut rows } = keyset(&[("delete_word", json!([]))]);
    let at = BINDINGS
        .iter()
        .position(|binding| binding.id == "move_word")
        .unwrap_or_else(|| panic!("no action move_word"));
    if let Some(row) = rows.get_mut(at) {
        row.keys = ["alt+left", "alt+right", "alt+left", "f5"]
            .iter()
            .map(|name| stroke(name))
            .collect();
    }
    let before = Keyset { rows };
    let next = before
        .set("delete_word", vec![stroke("alt+left")], &["move_word"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    assert!(next.current("move_word").is_empty());
    assert!(clash_free(&next));
}

#[test]
fn without_a_give_back_the_other_action_loses_the_key_group() {
    let before = keyset(&[]);
    let next = before
        .set("send", vec![stroke("ctrl+o")], &["toggle_ledgers"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    // The give-back is Enter, which `open_focused` holds in the
    // conversation, a context `toggle_ledgers` shares, so no key goes
    // back and the whole group goes.
    assert!(next.current("toggle_ledgers").is_empty());
    assert!(clash_free(&next));
}

#[test]
fn a_variant_action_loses_only_the_captured_group() {
    let before = keyset(&[("delete_word", json!([]))]);
    let next = before
        .set("delete_word", vec![stroke("ctrl+left")], &["move_word"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    // `delete_word` is unbound, so there is no give-back: `move_word`
    // loses its second group and keeps the other two.
    assert_eq!(
        next.current("move_word"),
        [
            stroke("alt+left"),
            stroke("alt+right"),
            stroke("alt+b"),
            stroke("alt+f"),
        ]
    );
    assert!(clash_free(&next));
}

#[test]
fn a_give_back_the_other_action_holds_is_not_given() {
    let before = holding("open_focused", &["enter", "y"]);
    let next = before
        .set("copy_focused", vec![stroke("enter")], &["open_focused"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    // The give-back is `y`, already one of `open_focused`'s keys, so the
    // group holding Enter drops instead.
    assert_eq!(next.current("open_focused"), [stroke("y")]);
    assert_eq!(next.current("copy_focused"), [stroke("enter")]);
}

#[test]
fn a_give_back_among_the_captured_keys_is_not_given() {
    let before = keyset(&[]);
    let next = before
        .set(
            "move_word",
            vec![stroke("ctrl+r"), stroke("alt+left")],
            &["search_prompts"],
        )
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    // The give-back for Ctrl+R is Alt+Left, one of the captured keys, so
    // `search_prompts` loses its group holding Ctrl+R instead.
    assert!(next.current("search_prompts").is_empty());
    assert_eq!(
        next.current("move_word"),
        [stroke("ctrl+r"), stroke("alt+left")]
    );
    assert!(clash_free(&next));
}

#[test]
fn edits_name_every_change_in_table_order() {
    let before = keyset(&[]);
    assert!(before.edits(&before).is_empty());
    let bound = before
        .set("new_session", vec![stroke("ctrl+t")], &[])
        .unwrap_or_else(|refused| panic!("free key refused: {refused:?}"));
    assert_eq!(
        bound.edits(&before),
        [KeyEdit {
            id: "new_session".to_owned(),
            keys: Some(vec!["ctrl+t".to_owned()]),
        }]
    );
    let reset = keyset(&[("copy_focused", json!(["c"]))]);
    let cleared = reset
        .set("copy_focused", vec![stroke("y")], &[])
        .unwrap_or_else(|refused| panic!("defaults refused: {refused:?}"));
    assert_eq!(
        cleared.edits(&reset),
        [KeyEdit {
            id: "copy_focused".to_owned(),
            keys: None,
        }]
    );
    let unbound = before
        .set("copy_focused", Vec::new(), &[])
        .unwrap_or_else(|refused| panic!("unbind refused: {refused:?}"));
    assert_eq!(
        unbound.edits(&before),
        [KeyEdit {
            id: "copy_focused".to_owned(),
            keys: Some(Vec::new()),
        }]
    );
    let swapped = before
        .set("copy_focused", vec![stroke("enter")], &["open_focused"])
        .unwrap_or_else(|refused| panic!("swap refused: {refused:?}"));
    assert_eq!(
        swapped.edits(&before),
        [
            KeyEdit {
                id: "open_focused".to_owned(),
                keys: Some(vec!["y".to_owned()]),
            },
            KeyEdit {
                id: "copy_focused".to_owned(),
                keys: Some(vec!["enter".to_owned()]),
            },
        ]
    );
}

#[test]
fn every_capture_flow_ends_clash_free() {
    let mut strokes: Vec<Stroke> = BINDINGS
        .iter()
        .flat_map(|binding| binding.defaults.iter())
        .filter_map(|name| Stroke::parse(name).ok())
        .collect();
    for name in ["ctrl+t", "ctrl+s", "alt+s", "f5", "x"] {
        strokes.push(stroke(name));
    }
    strokes.sort_by_key(Stroke::name);
    strokes.dedup();
    strokes.retain(|stroke| *stroke != super::super::ctrl_c());
    for binding in BINDINGS {
        if binding.id == "clear_then_quit" {
            continue;
        }
        for key in &strokes {
            let mut keys = keyset(&[]);
            let mut agreed: Vec<&'static str> = Vec::new();
            for _ in 0..BINDINGS.len().saturating_add(1) {
                match keys.set(binding.id, vec![*key], &agreed) {
                    Ok(next) => {
                        assert!(clash_free(&next), "{} takes {}", binding.id, key.name());
                        keys = next;
                        break;
                    }
                    Err(Refused::Clash { other, .. }) => agreed.push(other),
                    Err(Refused::Fixed | Refused::Repeat(_)) => break,
                }
            }
            assert!(
                agreed.len() <= BINDINGS.len(),
                "{} takes {} without end",
                binding.id,
                key.name()
            );
        }
    }
}
