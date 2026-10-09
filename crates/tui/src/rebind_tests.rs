//! Tests for the `/keys` screen: its fixed controls, capture, the swap
//! prompts, reset, unbind and the failed save (`docs/tui.md`, "Bindings").

use super::{KeysScreen, Outcome};
use crate::bindings::BINDINGS;
use crate::configure::KeyEdit;
use crate::keyset::Keyset;
use crate::stroke::Stroke;
use crate::swapped::Spot;
use serde_json::json;

/// The keyset with no person entries.
fn keys() -> Keyset {
    Keyset::default()
}

/// The keyset for `entries`.
fn person(entries: &[(&str, serde_json::Value)]) -> Keyset {
    let mut user = serde_json::Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    crate::keyset::load(&user).0
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// A screen over `keys`.
fn keys_screen() -> KeysScreen {
    KeysScreen::default()
}

/// Presses `names` in order; the last answer.
fn press(screen: &mut KeysScreen, keys: &Keyset, names: &[&str]) -> Outcome {
    let mut outcome = Outcome::Nothing;
    for name in names {
        outcome = screen.press(&stroke(name), keys, 24);
    }
    outcome
}

/// The screen's footer over `keys`.
fn footer(screen: &KeysScreen, keys: &Keyset) -> String {
    screen.frame(keys, 24).footer
}

/// The screen's lines below the rows over `keys`.
fn below(screen: &KeysScreen, keys: &Keyset) -> Vec<String> {
    screen.frame(keys, 24).below
}

/// The selected row over `keys`.
fn selected(screen: &KeysScreen, keys: &Keyset) -> usize {
    screen.frame(keys, 24).list.selected()
}

/// Selects row `at`.
fn select(screen: &mut KeysScreen, at: usize) {
    let outcome = screen.click(Spot::Row(at), 24);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
}

#[test]
fn moving_clamps_at_both_ends() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["up"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), 0);
    select(&mut screen, BINDINGS.len().saturating_sub(1));
    assert!(matches!(
        press(&mut screen, &keys, &["down"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), BINDINGS.len().saturating_sub(1));
}

#[test]
fn paging_moves_by_the_shown_rows_less_one() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["pagedown"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), 21);
    assert!(matches!(
        press(&mut screen, &keys, &["pageup"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), 0);
}

#[test]
fn a_modified_arrow_does_not_move() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+up"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), 5);
}

#[test]
fn enter_starts_a_capture_naming_the_action() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    assert!(matches!(
        press(&mut screen, &keys, &["enter"]),
        Outcome::Nothing
    ));
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Start a new session · Esc cancel"
    );
}

#[test]
fn a_variant_capture_names_each_variant_in_order() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 10);
    press(&mut screen, &keys, &["enter"]);
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Move by word (left) · Esc cancel"
    );
    assert!(matches!(
        press(&mut screen, &keys, &["alt+left"]),
        Outcome::Nothing
    ));
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Move by word (right) · Esc cancel"
    );
}

#[test]
fn esc_in_a_capture_returns_to_browse_applying_nothing() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    press(&mut screen, &keys, &["enter"]);
    assert!(matches!(
        press(&mut screen, &keys, &["esc"]),
        Outcome::Nothing
    ));
    assert_eq!(
        footer(&screen, &keys),
        "↑↓ move · Enter rebind · r reset · Delete unbind · Esc close"
    );
}

#[test]
fn a_free_key_applies_with_its_edit() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    press(&mut screen, &keys, &["enter"]);
    match press(&mut screen, &keys, &["ctrl+t"]) {
        Outcome::Apply(next) => assert_eq!(
            next.edits(&keys),
            [KeyEdit {
                id: "new_session".to_owned(),
                keys: Some(vec!["ctrl+t".to_owned()]),
            }]
        ),
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn a_variant_action_takes_one_key_per_variant() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 10);
    press(&mut screen, &keys, &["enter"]);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+b"]),
        Outcome::Nothing
    ));
    match press(&mut screen, &keys, &["x"]) {
        Outcome::Apply(next) => {
            assert_eq!(next.current("move_word"), [stroke("ctrl+b"), stroke("x")])
        }
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn a_repeated_key_is_refused_and_capture_waits() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 10);
    press(&mut screen, &keys, &["enter", "ctrl+b"]);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+b"]),
        Outcome::Nothing
    ));
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+B is already the key for left"]
    );
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Move by word (right) · Esc cancel"
    );
}

#[test]
fn ctrl_c_passes_through_in_every_mode_and_capture_continues() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+c"]),
        Outcome::Pass
    ));
    select(&mut screen, 5);
    press(&mut screen, &keys, &["enter"]);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+c"]),
        Outcome::Pass
    ));
    match press(&mut screen, &keys, &["ctrl+t"]) {
        Outcome::Apply(_) => {}
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("capture did not continue"),
    }
    let mut screen = keys_screen();
    select(&mut screen, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+c"]),
        Outcome::Pass
    ));
    match press(&mut screen, &keys, &["enter"]) {
        Outcome::Apply(_) => {}
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("the prompt did not wait"),
    }
}

#[test]
fn esc_in_browse_closes() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["esc"]),
        Outcome::Close
    ));
}

#[test]
fn a_bound_key_prompts_with_the_swap_or_cancel() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+O is bound to Open or close the ledgers"]
    );
    assert_eq!(footer(&screen, &keys), "Enter swap · Esc cancel");
}

#[test]
fn swapping_applies_both_sides() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    match press(&mut screen, &keys, &["enter"]) {
        Outcome::Apply(next) => {
            assert_eq!(next.current("send"), [stroke("ctrl+o")]);
            assert!(next.current("toggle_ledgers").is_empty());
        }
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn esc_at_a_prompt_cancels_everything() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    assert!(matches!(
        press(&mut screen, &keys, &["esc"]),
        Outcome::Nothing
    ));
    assert_eq!(
        footer(&screen, &keys),
        "↑↓ move · Enter rebind · r reset · Delete unbind · Esc close"
    );
    assert!(below(&screen, &keys).is_empty());
}

#[test]
fn clashing_with_two_actions_prompts_twice_then_applies() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 10);
    press(&mut screen, &keys, &["enter", "ctrl+o", "ctrl+r"]);
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+O is bound to Open or close the ledgers"]
    );
    press(&mut screen, &keys, &["enter"]);
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+R is bound to Search those prompts"]
    );
    match press(&mut screen, &keys, &["enter"]) {
        Outcome::Apply(next) => {
            assert_eq!(
                next.current("move_word"),
                [stroke("ctrl+o"), stroke("ctrl+r")]
            );
            assert_eq!(
                next.edits(&keys),
                [
                    KeyEdit {
                        id: "search_prompts".to_owned(),
                        keys: Some(vec!["alt+right".to_owned()]),
                    },
                    KeyEdit {
                        id: "move_word".to_owned(),
                        keys: Some(vec!["ctrl+o".to_owned(), "ctrl+r".to_owned()]),
                    },
                    KeyEdit {
                        id: "toggle_ledgers".to_owned(),
                        keys: Some(vec!["alt+left".to_owned()]),
                    },
                ]
            );
        }
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn reset_applies_the_defaults_removing_the_entry() {
    let keys = person(&[("copy_focused", json!(["c"]))]);
    let mut screen = keys_screen();
    select(&mut screen, 19);
    match press(&mut screen, &keys, &["r"]) {
        Outcome::Apply(next) => assert_eq!(
            next.edits(&keys),
            [KeyEdit {
                id: "copy_focused".to_owned(),
                keys: None,
            }]
        ),
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn reset_through_a_clash_prompts_and_swaps() {
    let keys = person(&[
        ("search_prompts", json!(["f5"])),
        (
            "move_word",
            json!([
                "ctrl+r",
                "ctrl+b",
                "ctrl+left",
                "ctrl+right",
                "alt+b",
                "alt+f"
            ]),
        ),
    ]);
    let mut screen = keys_screen();
    select(&mut screen, 9);
    press(&mut screen, &keys, &["r"]);
    assert_eq!(below(&screen, &keys), ["Ctrl+R is bound to Move by word"]);
    match press(&mut screen, &keys, &["enter"]) {
        Outcome::Apply(next) => {
            assert_eq!(next.current("search_prompts"), [stroke("ctrl+r")]);
            assert_eq!(
                next.edits(&keys),
                [
                    KeyEdit {
                        id: "search_prompts".to_owned(),
                        keys: None,
                    },
                    KeyEdit {
                        id: "move_word".to_owned(),
                        keys: Some(vec![
                            "f5".to_owned(),
                            "ctrl+b".to_owned(),
                            "ctrl+left".to_owned(),
                            "ctrl+right".to_owned(),
                            "alt+b".to_owned(),
                            "alt+f".to_owned(),
                        ]),
                    },
                ]
            );
        }
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn delete_and_backspace_unbind() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 15);
    match press(&mut screen, &keys, &["delete"]) {
        Outcome::Apply(next) => assert_eq!(
            next.edits(&keys),
            [KeyEdit {
                id: "toggle_ledgers".to_owned(),
                keys: Some(Vec::new()),
            }]
        ),
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
    let mut screen = keys_screen();
    select(&mut screen, 5);
    match press(&mut screen, &keys, &["backspace"]) {
        Outcome::Apply(next) => assert_eq!(
            next.edits(&keys),
            [KeyEdit {
                id: "new_session".to_owned(),
                keys: Some(Vec::new()),
            }]
        ),
        Outcome::Nothing | Outcome::Close | Outcome::Pass => panic!("no apply"),
    }
}

#[test]
fn delete_on_an_unbound_action_saves_nothing() {
    let keys = person(&[("delete_word", json!([]))]);
    let mut screen = keys_screen();
    select(&mut screen, 11);
    assert!(matches!(
        press(&mut screen, &keys, &["delete"]),
        Outcome::Nothing
    ));
}

#[test]
fn clear_then_quit_keeps_ctrl_c_on_every_control() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 3);
    for name in ["enter", "r", "delete"] {
        assert!(matches!(
            press(&mut screen, &keys, &[name]),
            Outcome::Nothing
        ));
        assert_eq!(below(&screen, &keys), ["Ctrl+C always clears, then quits."]);
    }
}

#[test]
fn any_other_key_in_browse_does_nothing() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["x"]),
        Outcome::Nothing
    ));
    assert!(below(&screen, &keys).is_empty());
}

#[test]
fn the_message_clears_on_the_next_key() {
    let keys = keys();
    let mut screen = keys_screen();
    screen.failed("disk full".to_owned());
    assert_eq!(below(&screen, &keys), ["Not saved: disk full"]);
    assert!(matches!(
        press(&mut screen, &keys, &["x"]),
        Outcome::Nothing
    ));
    assert!(below(&screen, &keys).is_empty());
}

#[test]
fn a_failed_save_shows_its_message_and_browses() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    press(&mut screen, &keys, &["enter"]);
    screen.failed("disk full".to_owned());
    assert_eq!(below(&screen, &keys), ["Not saved: disk full"]);
    assert_eq!(
        footer(&screen, &keys),
        "↑↓ move · Enter rebind · r reset · Delete unbind · Esc close"
    );
}

#[test]
fn the_cross_closes_in_every_mode() {
    let keys = keys();
    let mut screen = keys_screen();
    assert!(matches!(screen.click(Spot::Close, 24), Outcome::Close));
    select(&mut screen, 5);
    press(&mut screen, &keys, &["enter"]);
    assert!(matches!(screen.click(Spot::Close, 24), Outcome::Close));
    let mut screen = keys_screen();
    select(&mut screen, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    assert!(matches!(screen.click(Spot::Close, 24), Outcome::Close));
}

#[test]
fn a_row_click_selects_while_browsing_only() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, 5);
    assert_eq!(selected(&screen, &keys), 5);
    press(&mut screen, &keys, &["enter"]);
    let outcome = screen.click(Spot::Row(3), 24);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Start a new session · Esc cancel"
    );
}
