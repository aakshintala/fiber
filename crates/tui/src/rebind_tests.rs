//! Tests for the `/keys` screen: its fixed controls, capture, the swap
//! prompts, reset, unbind and the failed save (`docs/tui.md`, "Bindings").

use super::{KeysScreen, Outcome, columns, fits_from, heights, place};
use crate::bindings::BINDINGS;
use crate::configure::KeyEdit;
use crate::keyset::Keyset;
use crate::rows::Join;
use crate::stroke::Stroke;
use crate::swapped::Spot;
use serde_json::json;

/// The draw width in columns the unit tests wrap at.
const WIDTH: usize = 80;

/// The view height in rows the unit tests frame at.
const HEIGHT: usize = 24;

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
        outcome = screen.press(&stroke(name), keys, WIDTH, HEIGHT);
    }
    outcome
}

/// The screen's footer over `keys`.
fn footer(screen: &KeysScreen, keys: &Keyset) -> String {
    screen.frame(keys, WIDTH, HEIGHT).footer
}

/// The screen's lines below the rows over `keys`.
fn below(screen: &KeysScreen, keys: &Keyset) -> Vec<String> {
    screen.frame(keys, WIDTH, HEIGHT).below
}

/// The selected action's first display line over `keys`.
fn selected(screen: &KeysScreen, keys: &Keyset) -> usize {
    screen.frame(keys, WIDTH, HEIGHT).list.selected()
}

/// The first visible display line over `keys`.
fn top(screen: &KeysScreen, keys: &Keyset) -> usize {
    screen.frame(keys, WIDTH, HEIGHT).list.top()
}

/// The first display line of action `at` over `keys`.
fn first_line(keys: &Keyset, at: usize) -> usize {
    heights(keys, WIDTH).iter().take(at).sum()
}

/// Selects action `at` through its first display line.
fn select(screen: &mut KeysScreen, keys: &Keyset, at: usize) {
    let outcome = screen.click(Spot::Row(first_line(keys, at)), keys, WIDTH, HEIGHT);
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
    assert_eq!(selected(&screen, &keys), first_line(&keys, 0));
    select(&mut screen, &keys, BINDINGS.len().saturating_sub(1));
    assert!(matches!(
        press(&mut screen, &keys, &["down"]),
        Outcome::Nothing
    ));
    assert_eq!(
        selected(&screen, &keys),
        first_line(&keys, BINDINGS.len().saturating_sub(1))
    );
}

/// PageUp and PageDown move (`docs/tui.md:2077`); the step is the whole
/// actions fitting from the top less one, at least one — the `List::key`
/// "by the height less one" precedent translated from display lines to
/// fitting actions, so a page down lands on the last fitting action's
/// predecessor.
#[test]
fn paging_moves_by_the_fitting_actions_less_one() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    let shown = HEIGHT.saturating_sub(2);
    let fitting = fits_from(0, &grown, shown);
    let mut screen = keys_screen();
    assert!(matches!(
        press(&mut screen, &keys, &["pagedown"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, fitting - 1));
    assert_eq!(top(&screen, &keys), first_line(&keys, 0));
    assert!(matches!(
        press(&mut screen, &keys, &["pageup"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 0));
    // A page up past the first action clamps at the top.
    select(&mut screen, &keys, 2);
    assert!(matches!(
        press(&mut screen, &keys, &["pageup"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 0));
}

/// How many whole actions fit from the top in the shown display lines:
/// a row taller than the window fits none.
#[test]
fn fits_counts_the_whole_actions_fitting_from_the_top() {
    assert_eq!(fits_from(0, &[2, 2, 2], 3), 1);
    assert_eq!(fits_from(1, &[2, 2, 2], 3), 1);
    assert_eq!(fits_from(2, &[2, 2, 2], 3), 1);
    assert_eq!(fits_from(0, &[1, 1, 1], 3), 3);
    assert_eq!(fits_from(0, &[5, 1], 3), 0);
    assert_eq!(fits_from(0, &[1], 0), 0);
}

/// The frame's list lands the selected action's first line and the
/// display-line top independently: a wheel step can rest the top
/// inside a wrapped action while the selection stays above it, and a
/// stale top clamps to the window range before the selected action is
/// revealed (`docs/tui.md`, "Swapped views").
#[test]
fn place_lands_the_exact_selected_and_top_lines() {
    let grown = vec![2, 2, 2];
    let at = place(0, 0, &grown, 3);
    assert!(at.is_gutter());
    assert_eq!((at.selected(), at.top()), (0, 0));
    // The top rests on the second line of the second action while the
    // selection stays on its first.
    let at = place(1, 3, &grown, 3);
    assert_eq!((at.selected(), at.top()), (2, 3));
    // A first line touching the window's end stays put.
    let at = place(1, 0, &grown, 3);
    assert_eq!((at.selected(), at.top()), (2, 0));
    // A first line on the window's end moves to the last shown line.
    let at = place(2, 0, &grown, 3);
    assert_eq!((at.selected(), at.top()), (4, 2));
    // A last line touching the window's top stays put.
    let at = place(2, 3, &grown, 3);
    assert_eq!((at.selected(), at.top()), (4, 3));
    // A taller-than-window action shows its top lines, held.
    let tall = vec![1, 5, 1];
    let at = place(1, 1, &tall, 3);
    assert_eq!((at.selected(), at.top()), (1, 1));
    // A stale top past the end clamps to the fullest window first:
    // the first action is above it, so it is revealed, while the
    // last action already shows and stays on the clamped top.
    let at = place(0, 9, &grown, 3);
    assert_eq!((at.selected(), at.top()), (0, 0));
    let at = place(2, 9, &grown, 3);
    assert_eq!((at.selected(), at.top()), (4, 3));
}

#[test]
fn a_modified_arrow_does_not_move() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 5);
    assert!(matches!(
        press(&mut screen, &keys, &["ctrl+up"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 5));
}

#[test]
fn enter_starts_a_capture_naming_the_action() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 5);
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
    select(&mut screen, &keys, 11);
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
    select(&mut screen, &keys, 5);
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
    select(&mut screen, &keys, 5);
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
    select(&mut screen, &keys, 11);
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
    select(&mut screen, &keys, 11);
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
    select(&mut screen, &keys, 5);
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
    select(&mut screen, &keys, 0);
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
    select(&mut screen, &keys, 0);
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
    select(&mut screen, &keys, 0);
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
    select(&mut screen, &keys, 0);
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
    select(&mut screen, &keys, 11);
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
    select(&mut screen, &keys, 21);
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
    select(&mut screen, &keys, 10);
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
    select(&mut screen, &keys, 17);
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
    select(&mut screen, &keys, 5);
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
    select(&mut screen, &keys, 12);
    assert!(matches!(
        press(&mut screen, &keys, &["delete"]),
        Outcome::Nothing
    ));
}

#[test]
fn clear_then_quit_keeps_ctrl_c_on_every_control() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 3);
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
    select(&mut screen, &keys, 5);
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
    assert!(matches!(
        screen.click(Spot::Close, &keys, WIDTH, HEIGHT),
        Outcome::Close
    ));
    select(&mut screen, &keys, 5);
    press(&mut screen, &keys, &["enter"]);
    assert!(matches!(
        screen.click(Spot::Close, &keys, WIDTH, HEIGHT),
        Outcome::Close
    ));
    let mut screen = keys_screen();
    select(&mut screen, &keys, 0);
    press(&mut screen, &keys, &["enter", "ctrl+o"]);
    assert!(matches!(
        screen.click(Spot::Close, &keys, WIDTH, HEIGHT),
        Outcome::Close
    ));
}

#[test]
fn a_row_click_selects_while_browsing_only() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 5);
    assert_eq!(selected(&screen, &keys), first_line(&keys, 5));
    press(&mut screen, &keys, &["enter"]);
    let outcome = screen.click(Spot::Row(3), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Start a new session · Esc cancel"
    );
}

/// The wheel steps three display lines a step while browsing, keeping
/// a still-visible selection where it was and clearing the transient
/// message as a stroke does: the first two actions each wrap to two
/// lines, so one step down from the top rests on display line 3,
/// inside the second action.
/// (`docs/tui.md:576`).
#[test]
fn wheel_steps_three_display_lines_and_keeps_a_visible_selection_while_browsing() {
    let keys = keys();
    let mut screen = keys_screen();
    screen.failed("boom".to_owned());
    assert_eq!(below(&screen, &keys), ["Not saved: boom"]);
    let grown = heights(&keys, WIDTH);
    assert_eq!(&grown[..2], &[2, 2], "wrapped actions sit in view");
    select(&mut screen, &keys, 5);
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 3);
    assert_eq!(selected(&screen, &keys), first_line(&keys, 5));
    assert!(below(&screen, &keys).is_empty());
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), first_line(&keys, 0));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 5));
}

/// The wheel pulls a selection that leaves the window to the nearest
/// shown action: down from the top the selection rides the action at
/// the first visible line, and back up it stays while shown. The step
/// is three display lines, so from the top the window rests inside
/// the second action.
#[test]
fn wheel_pulls_a_leaving_selection_to_the_nearest_shown_action() {
    let keys = keys();
    let mut screen = keys_screen();
    let grown = heights(&keys, WIDTH);
    assert_eq!(&grown[..2], &[2, 2], "wrapped actions sit in view");
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 3);
    assert_eq!(selected(&screen, &keys), first_line(&keys, 1));
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), first_line(&keys, 0));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 1));
}

/// Wheeling steps exactly three display lines a step away from the
/// ends, resting inside wrapped actions instead of snapping to their
/// starts: down from the top the window lands on lines 3, 6 and 9,
/// and back up it returns by the same lines.
/// (`docs/tui.md:584`).
#[test]
fn wheel_up_into_a_wrapped_action_moves_at_most_three_display_lines() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    assert_eq!(
        &grown[..6],
        &[2, 2, 2, 1, 1, 1],
        "wrapped actions sit in view: {grown:?}"
    );
    let mut screen = keys_screen();
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 6);
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 9);
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 6);
    let before = top(&screen, &keys);
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    let after = top(&screen, &keys);
    assert_eq!(
        before - after,
        3,
        "one upward step moves three display lines"
    );
    assert_eq!(selected(&screen, &keys), first_line(&keys, 6));
}

/// A click on a continuation line selects its action, a click on the
/// next action's first line selects that one, and a click past the
/// last line selects the last action (`docs/tui.md`, "Bindings").
#[test]
fn a_click_on_a_continuation_line_selects_its_action() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    let mut screen = keys_screen();
    // The delete_session action wraps into three display lines.
    let start: usize = grown.iter().take(7).sum();
    assert_eq!(grown[7], 3);
    let outcome = screen.click(Spot::Row(start + 1), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(selected(&screen, &keys), first_line(&keys, 7));
    // The first line of the next action belongs to it, not to the
    // wrapped lines above.
    let outcome = screen.click(Spot::Row(start + grown[7]), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(selected(&screen, &keys), first_line(&keys, 8));
    let total: usize = grown.iter().sum();
    let outcome = screen.click(Spot::Row(total + 5), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(
        selected(&screen, &keys),
        first_line(&keys, BINDINGS.len().saturating_sub(1))
    );
}

/// A click above the window pulls the scroll up, so the next page
/// counts from the shown top.
#[test]
fn a_click_above_the_window_resets_the_scroll_for_the_next_page() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, BINDINGS.len().saturating_sub(1));
    let outcome = screen.click(Spot::Row(0), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(selected(&screen, &keys), first_line(&keys, 0));
    assert_eq!(top(&screen, &keys), first_line(&keys, 0));
    assert!(matches!(
        press(&mut screen, &keys, &["pagedown"]),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 11));
}

/// A wheel step clamps a selection sitting outside the window to the
/// action at the nearer edge: the last shown line's action keeps it.
/// The step is three display lines, so from the top the window rests
/// inside the second action and the window of six lines holds lines 3
/// through 8.
#[test]
fn wheel_clamps_a_selection_sitting_on_the_window_edge() {
    let keys = keys();
    let mut screen = keys_screen();
    let grown = heights(&keys, WIDTH);
    assert_eq!(&grown[..5], &[2, 2, 2, 1, 1], "wrapped actions sit in view");
    select(&mut screen, &keys, 7);
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, 8),
        Outcome::Nothing
    ));
    let frame = screen.frame(&keys, WIDTH, 8);
    assert_eq!(frame.list.top(), 3);
    assert_eq!(frame.list.selected(), first_line(&keys, 5));
}

/// The wheel clamps at the last action: wheeling down to the bottom
/// holds on the fullest window, whose top is the total lines less the
/// shown ones, and up steps back by exact lines with the selection
/// following the visible window. Each step moves three display lines,
/// so reaching the tail takes more steps than the old three-actions
/// step; the three-line target back falls inside the third-to-last
/// action, so the selection rides the window instead of staying.
#[test]
fn wheel_clamps_at_the_last_action() {
    let keys = keys();
    let mut screen = keys_screen();
    let grown = heights(&keys, WIDTH);
    assert_eq!(
        &grown[grown.len().saturating_sub(3)..],
        &[2, 2, 1],
        "the tail takes one line back inside the third-to-last action"
    );
    let total: usize = grown.iter().sum();
    let shown = HEIGHT.saturating_sub(2);
    let end = total.saturating_sub(shown);
    // The selected line always starts its action: that action's span
    // meets the window, so the selection shows even when the window
    // cuts through the action's first lines.
    let shows = |line: usize, top: usize| {
        let mut start: usize = 0;
        for height in &grown {
            let end_line = start.saturating_add(*height);
            if start == line {
                return start < top.saturating_add(shown) && end_line > top;
            }
            start = end_line;
        }
        false
    };
    // Enough steps of three display lines to cover the whole list:
    // each step advances the top by at least one display line.
    for _ in 0..total.saturating_add(2) {
        assert!(matches!(
            screen.wheel(false, &keys, WIDTH, HEIGHT),
            Outcome::Nothing
        ));
    }
    assert_eq!(top(&screen, &keys), end);
    let bottom = selected(&screen, &keys);
    assert!(
        shows(bottom, end),
        "the selection stays on a visible action: {bottom}"
    );
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), end.saturating_sub(3));
    let back = selected(&screen, &keys);
    assert!(
        shows(back, end.saturating_sub(3)),
        "the selection follows the visible window: {back}"
    );
}

/// Wheel steps near the ends move only the lines left, and steps at
/// the ends hold: every step off an end moves one to three display
/// lines, never more and never zero, in both directions
/// (`docs/tui.md:584`).
#[test]
fn wheel_moves_short_steps_at_the_ends_and_holds_at_them() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    let total: usize = grown.iter().sum();
    let shown = HEIGHT.saturating_sub(2);
    let end = total.saturating_sub(shown);
    let mut screen = keys_screen();
    let mut top = 0;
    for _ in 0..total.saturating_add(2) {
        if top == end {
            break;
        }
        assert!(matches!(
            screen.wheel(false, &keys, WIDTH, HEIGHT),
            Outcome::Nothing
        ));
        let next = screen.frame(&keys, WIDTH, HEIGHT).list.top();
        assert!(
            (1..=3).contains(&next.saturating_sub(top)),
            "down stalls or leaps: {top} to {next}"
        );
        top = next;
    }
    assert_eq!(top, end, "the descent reaches the fullest window");
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(
        screen.frame(&keys, WIDTH, HEIGHT).list.top(),
        end,
        "a step at the end holds"
    );
    for _ in 0..total.saturating_add(2) {
        if top == 0 {
            break;
        }
        assert!(matches!(
            screen.wheel(true, &keys, WIDTH, HEIGHT),
            Outcome::Nothing
        ));
        let next = screen.frame(&keys, WIDTH, HEIGHT).list.top();
        assert!(
            (1..=3).contains(&top.saturating_sub(next)),
            "up stalls or leaps: {top} to {next}"
        );
        top = next;
    }
    assert_eq!(top, 0, "the ascent reaches the top");
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(
        screen.frame(&keys, WIDTH, HEIGHT).list.top(),
        0,
        "a step at the top holds"
    );
}

/// A wheel at zero shown height changes nothing: with no body rows the
/// top, selection and message stand, and a taller viewport shows the
/// kept top (`docs/tui.md`, "Swapped views").
#[test]
fn wheel_at_zero_shown_height_changes_nothing() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    assert_eq!(&grown[..2], &[2, 2], "wrapped actions sit in view");
    let mut screen = keys_screen();
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    let top = screen.frame(&keys, WIDTH, HEIGHT).list.top();
    assert_eq!(top, 3, "one step rests inside the second action");
    let selected = screen.frame(&keys, WIDTH, HEIGHT).list.selected();
    screen.failed("boom".to_owned());
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, 2),
        Outcome::Nothing
    ));
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, 2),
        Outcome::Nothing
    ));
    let frame = screen.frame(&keys, WIDTH, HEIGHT);
    assert_eq!(frame.list.top(), top);
    assert_eq!(frame.list.selected(), selected);
    assert_eq!(frame.below, ["Not saved: boom"]);
}

/// Paging counts whole actions from the action holding the line top:
/// after a wheel rests the top inside the second action, a page down
/// moves one action onto the third, and a page up returns to the
/// second. Passing the line itself as the action would count from the
/// fourth instead (`docs/tui.md`, "Swapped views").
#[test]
fn paging_counts_from_the_action_holding_the_line_top() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    assert_eq!(
        &grown[..7],
        &[2, 2, 2, 1, 1, 1, 2],
        "wrapped actions sit in view: {grown:?}"
    );
    let height: usize = 5;
    let mut screen = keys_screen();
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, height),
        Outcome::Nothing
    ));
    assert_eq!(screen.frame(&keys, WIDTH, height).list.top(), 3);
    assert!(matches!(
        screen.press(&stroke("pagedown"), &keys, WIDTH, height),
        Outcome::Nothing
    ));
    assert_eq!(
        screen.frame(&keys, WIDTH, height).list.selected(),
        first_line(&keys, 2)
    );
    assert!(matches!(
        screen.press(&stroke("pageup"), &keys, WIDTH, height),
        Outcome::Nothing
    ));
    assert_eq!(
        screen.frame(&keys, WIDTH, height).list.selected(),
        first_line(&keys, 1)
    );
}

/// Reflowing wider keeps the wheel's line top: the stored top clamps
/// to the new range, and with the selected action's first line already
/// showing no reveal moves it. A reset to that first line would land
/// on it instead of the clamped top (`docs/tui.md`, "Swapped views").
#[test]
fn reflow_keeps_the_wheel_top_and_shows_the_selected_first_line() {
    let keys = keys();
    let grown_80 = heights(&keys, WIDTH);
    assert_eq!(&grown_80[..2], &[2, 2], "wrapped actions sit in view");
    let grown_160 = heights(&keys, 160);
    let shown: usize = grown_160.iter().sum();
    let height = shown.saturating_add(2);
    let total_80: usize = grown_80.iter().sum();
    assert!(
        total_80.saturating_sub(shown) >= 3,
        "room to wheel at 80 and clamp to the full view at 160: {grown_80:?} {grown_160:?}"
    );
    let mut screen = keys_screen();
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, height),
        Outcome::Nothing
    ));
    assert_eq!(screen.frame(&keys, WIDTH, height).list.top(), 3);
    assert_eq!(
        screen.frame(&keys, WIDTH, height).list.selected(),
        first_line(&keys, 1)
    );
    let frame = screen.frame(&keys, 160, height);
    let first: usize = grown_160.iter().take(1).sum();
    assert_eq!(frame.list.selected(), first);
    assert!(first < shown, "the selected first line shows: {first}");
    assert_eq!(frame.list.top(), 0, "the clamped top already shows it");
    assert_ne!(
        frame.list.top(),
        frame.list.selected(),
        "the top is not reset to the selected first line"
    );
}

/// An action taller than the window shows its top lines with the
/// selection held: the scroll starts on its first line.
#[test]
fn an_action_taller_than_the_window_shows_its_top_lines() {
    let keys = keys();
    let width: usize = 44;
    let height: usize = 6;
    let grown = heights(&keys, width);
    assert!(
        grown[6] > height.saturating_sub(2),
        "rail_row_n overflows the window"
    );
    let mut screen = keys_screen();
    let line: usize = grown.iter().take(6).sum();
    let outcome = screen.click(Spot::Row(line), &keys, width, height);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    let frame = screen.frame(&keys, width, height);
    assert_eq!(frame.list.selected(), line);
    assert_eq!(frame.list.top(), line);
}

/// The wheel moves three display lines a step even at an action
/// taller than the window: down from its first line and up from the
/// first line after it each move the top, never stalling on the
/// action's start (`docs/tui.md:584`).
#[test]
fn wheel_moves_three_display_lines_at_a_taller_than_window_action() {
    let keys = keys();
    let width: usize = 44;
    let height: usize = 6;
    let grown = heights(&keys, width);
    assert!(
        grown[6] > height.saturating_sub(2),
        "the tall action overflows the window: {grown:?}"
    );
    let first = |at: usize| grown.iter().take(at).sum::<usize>();
    // Down from the tall action's first line: three lines down stays
    // inside it, so the top advances three display lines.
    let mut down = keys_screen();
    let outcome = down.click(Spot::Row(first(6)), &keys, width, height);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(down.frame(&keys, width, height).list.top(), first(6));
    assert!(matches!(
        down.wheel(false, &keys, width, height),
        Outcome::Nothing
    ));
    let down_top = down.frame(&keys, width, height).list.top();
    // Up from the first line after it: three lines up stays inside
    // the tall action, so the top retreats three display lines. Both
    // directions run before either asserts, so one failure names both
    // tops.
    let mut up = keys_screen();
    let outcome = up.click(Spot::Row(first(7)), &keys, width, height);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(up.frame(&keys, width, height).list.top(), first(7));
    assert!(matches!(
        up.wheel(true, &keys, width, height),
        Outcome::Nothing
    ));
    let up_top = up.frame(&keys, width, height).list.top();
    assert!(
        down_top == first(6).saturating_add(3) && up_top == first(7).saturating_sub(3),
        "the wheel stalls at the tall action: down left {down_top}, want {}; up left {up_top}, want {}",
        first(6).saturating_add(3),
        first(7).saturating_sub(3),
    );
}

/// The wheel in a capture is swallowed, never a captured key: the
/// capture waits on.
/// (`docs/tui.md:576`).
#[test]
fn wheel_in_a_capture_waits_on() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 5);
    press(&mut screen, &keys, &["enter"]);
    let prompt = footer(&screen, &keys);
    assert_eq!(prompt, "Press the key for Start a new session · Esc cancel");
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(selected(&screen, &keys), first_line(&keys, 5));
    assert_eq!(footer(&screen, &keys), prompt);
}

/// The wheel in a clash is swallowed: the clash prompt stands.
/// (`docs/tui.md:576`).
#[test]
fn wheel_in_a_clash_keeps_the_prompt() {
    let keys = keys();
    let mut screen = keys_screen();
    select(&mut screen, &keys, 11);
    press(&mut screen, &keys, &["enter", "ctrl+o", "ctrl+r"]);
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+O is bound to Open or close the ledgers"]
    );
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(
        below(&screen, &keys),
        ["Ctrl+O is bound to Open or close the ledgers"]
    );
}

/// The columns share the draw width past the gutter and the two
/// separators: at 80 the keys take 16, the id 20 and the description
/// 40 (`docs/tui.md`, "Bindings").
#[test]
fn columns_share_the_draw_width_past_the_gutter_and_separators() {
    let keys = keys();
    assert_eq!(columns(&keys, 80), (16, 20, 40));
    // Where the share runs, the three columns with their two
    // single-space separators and the two gutter columns fill the draw
    // width exactly, so no key is cut and no column overlaps.
    for width in [40, 80, 102] {
        let (label, id, description) = columns(&keys, width);
        assert_eq!(
            label + 1 + id + 1 + description + 2,
            width,
            "columns at {width}"
        );
    }
    // No overlap at any supported width: below three 1-wide columns
    // plus separators the layout is out of scope.
    for width in 7..=200 {
        let (label, id, description) = columns(&keys, width);
        assert!(
            label + 1 + id + 1 + description + 2 <= width,
            "columns at {width}"
        );
    }
    // Below the room for three 1-wide columns the widths clamp at one
    // column each, never panicking, with no overlap promise.
    assert_eq!(columns(&keys, 4), (1, 1, 1));
    let screen = keys_screen();
    let _ = screen.frame(&keys, 4, HEIGHT);
    let _ = heights(&keys, 4);
}

/// A wrapped action takes as many lines as its tallest column: the
/// delete_session row wraps its keys after the comma into exactly
/// three display lines, while a single-line action stays one.
#[test]
fn delete_session_wraps_after_the_comma_at_80() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    assert_eq!(grown.len(), BINDINGS.len());
    assert_eq!(grown[7], 3, "delete_session takes three display lines");
    assert_eq!(grown[3], 1, "clear_then_quit takes one display line");
    let screen = keys_screen();
    let frame = screen.frame(&keys, WIDTH, HEIGHT);
    let start: usize = grown.iter().take(7).sum();
    assert_eq!(frame.rows.len(), grown.iter().sum::<usize>());
    // The boundary: the first line is exactly `Delete,` and the second
    // starts with `Backspace`, so the wrap fell after the comma.
    assert_eq!(frame.rows[start][0].0.trim_end(), "Delete,");
    assert!(
        frame.rows[start + 1][0]
            .0
            .trim_start()
            .starts_with("Backspace"),
        "{:?}",
        frame.rows[start + 1][0].0
    );
    assert_eq!(
        &frame.joins[start..start + 3],
        &[Join::Break, Join::WrapSpace, Join::WrapSpace]
    );
    let single: usize = grown.iter().take(3).sum();
    assert_eq!(&frame.joins[single..single + 1], &[Join::Break]);
}

/// An overlong word breaks inside where it reaches the column's edge,
/// noting a wrap with no space, at a narrow width: the delete_word
/// keys break inside `⌥Backspace` on the first boundary.
#[test]
fn an_overlong_word_breaks_inside_at_a_narrow_width() {
    let keys = keys();
    let (label, id, description) = columns(&keys, 44);
    assert_eq!((label, id, description), (6, 20, 14));
    let delete_word = BINDINGS
        .iter()
        .position(|binding| binding.id == "delete_word")
        .expect("delete_word");
    assert_eq!(delete_word, 12);
    let grown = heights(&keys, 44);
    assert_eq!(grown[delete_word], 6);
    let screen = keys_screen();
    let frame = screen.frame(&keys, 44, HEIGHT);
    let start: usize = grown.iter().take(delete_word).sum();
    assert_eq!(frame.rows[start][0].0.trim_end(), "⌥Backs");
    assert_eq!(&frame.joins[start..start + 2], &[Join::Break, Join::Wrap]);
}

/// The focused wrapped row fills the draw width whole: the columns
/// were budgeted inside the gutter, so the last description cell draws
/// in full instead of cut at the edge.
#[test]
fn the_focused_wrapped_row_fills_the_draw_width_whole() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let keys = keys();
    let (label, id, description) = columns(&keys, WIDTH);
    assert_eq!((label, id, description), (16, 20, 40));
    let screen = keys_screen();
    let frame = screen.frame(&keys, WIDTH, HEIGHT);
    let grown = heights(&keys, WIDTH);
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    crate::swapped::render(&frame, area, &mut buf, &mut Vec::new());
    // The focused action is send: its keys, id and description lines,
    // padded to their columns, with the gutter mark on the first only.
    let send = BINDINGS
        .iter()
        .position(|binding| binding.id == "send")
        .expect("send");
    assert_eq!(send, 0);
    let keys_line = format!("Enter{}", " ".repeat(label - 5));
    // The id cell keeps its single-space separators around the padded
    // id, as the frame draws the cells with no gap between them.
    let id_line = format!(" send{} ", " ".repeat(id - 4));
    // The first description line is 36 columns wide, padded to its
    // column.
    let description_line = format!(
        "Send a prompt, or a steering message{}",
        " ".repeat(description - 36)
    );
    for line in 0..grown[send] {
        let y = u16::try_from(1 + line).expect("a row");
        let text: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        let mark = if line == 0 { "› " } else { "  " };
        let key = if line == 0 {
            keys_line.clone()
        } else {
            " ".repeat(label)
        };
        let id = if line == 0 {
            id_line.clone()
        } else {
            format!(" {} ", " ".repeat(id))
        };
        let rest = if line == 0 {
            description_line.clone()
        } else {
            format!("during a turn{}", " ".repeat(description - 13))
        };
        assert_eq!(text, format!("{mark}{key}{id}{rest}"), "line {line}");
    }
}

/// The settle scrolls until the selected action fits whole from the
/// top: each action takes two display lines and the window shows
/// three, so the second action already overflows a window on the
/// first and the third moves the top onto its own first line. A
/// flipped comparison stops at the wrong action: equality holds too
/// early, and greater-than holds at once.
#[test]
fn settle_moves_the_window_until_the_selection_fits_whole() {
    let grown = vec![2, 2, 2];
    let mut screen = keys_screen();
    screen.selected = 0;
    screen.top = 0;
    screen.settle(&grown, 3);
    assert_eq!((screen.selected, screen.top), (0, 0));
    let mut screen = keys_screen();
    screen.selected = 1;
    screen.top = 0;
    screen.settle(&grown, 3);
    assert_eq!((screen.selected, screen.top), (1, 2));
    let mut screen = keys_screen();
    screen.selected = 2;
    screen.top = 0;
    screen.settle(&grown, 3);
    assert_eq!((screen.selected, screen.top), (2, 4));
}

/// A selection past the last action clamps to it: the stored index
/// cannot name an action, so the scroll ends on a line the table
/// holds. Either flipped operator keeps the past-the-end index.
#[test]
fn settle_clamps_a_past_the_end_selection_to_the_last_action() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    let shown = HEIGHT.saturating_sub(2);
    let mut screen = keys_screen();
    screen.selected = BINDINGS.len();
    screen.top = 0;
    screen.settle(&grown, shown);
    assert_eq!(screen.selected, BINDINGS.len().saturating_sub(1));
    // The window moves down to show the last action's first line.
    assert_eq!(screen.top, 38);
}

/// A window of nothing shown keeps the clamped top: with no rows to
/// reveal the selection the scroll stands. Treating the empty window
/// as showing would move the top onto the selection's line.
#[test]
fn place_with_nothing_shown_keeps_the_clamped_top() {
    let grown = vec![2, 2, 2];
    let at = place(0, 0, &grown, 0);
    assert_eq!((at.selected(), at.top()), (0, 0));
    let at = place(2, 0, &grown, 0);
    assert_eq!((at.selected(), at.top()), (4, 0));
}

/// The shares leave no spare column at the exact boundary: at width
/// 52 the room past the gutter is 48, the id keeps 20, and the keys
/// and description shares of the rest are exactly 8 and 20, so no
/// column is left for the keys to take. Allowing the boundary would
/// hand the keys a 29th column the room does not hold.
#[test]
fn columns_leave_no_spare_column_at_the_exact_share_boundary() {
    let keys = keys();
    assert_eq!(columns(&keys, 52), (8, 20, 20));
}
