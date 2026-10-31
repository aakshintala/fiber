//! Tests for the `/keys` screen: its fixed controls, capture, the swap
//! prompts, reset, unbind and the failed save (`docs/tui.md`, "Bindings").

use super::{KeysScreen, Outcome, columns, heights};
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
    assert_eq!(selected(&screen, &keys), 0);
    select(&mut screen, &keys, BINDINGS.len().saturating_sub(1));
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
    select(&mut screen, &keys, 5);
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
    assert_eq!(selected(&screen, &keys), 5);
    press(&mut screen, &keys, &["enter"]);
    let outcome = screen.click(Spot::Row(3), &keys, WIDTH, HEIGHT);
    assert!(matches!(outcome, Outcome::Nothing), "{outcome:?}");
    assert_eq!(
        footer(&screen, &keys),
        "Press the key for Start a new session · Esc cancel"
    );
}

/// The wheel shifts the visible rows three a step while browsing,
/// keeping a still-visible selection where it was and clearing the
/// transient message as a stroke does.
/// (`docs/tui.md:576`).
#[test]
fn wheel_shifts_three_rows_and_keeps_a_visible_selection_while_browsing() {
    let keys = keys();
    let mut screen = keys_screen();
    screen.failed("boom".to_owned());
    assert_eq!(below(&screen, &keys), ["Not saved: boom"]);
    select(&mut screen, &keys, 5);
    assert!(matches!(
        screen.wheel(false, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 3);
    assert_eq!(selected(&screen, &keys), 5);
    assert!(below(&screen, &keys).is_empty());
    assert!(matches!(
        screen.wheel(true, &keys, WIDTH, HEIGHT),
        Outcome::Nothing
    ));
    assert_eq!(top(&screen, &keys), 0);
    assert_eq!(selected(&screen, &keys), 5);
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
    assert_eq!(selected(&screen, &keys), 5);
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
/// separators: at 80 the keys take 13, the id 20 and the description
/// 43 (`docs/tui.md`, "Bindings").
#[test]
fn columns_share_the_draw_width_past_the_gutter_and_separators() {
    let keys = keys();
    assert_eq!(columns(&keys, 80), (13, 20, 43));
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
/// delete_session row wraps its keys after the comma into exactly two
/// display lines, while a single-line action stays one.
#[test]
fn delete_session_wraps_after_the_comma_at_80() {
    let keys = keys();
    let grown = heights(&keys, WIDTH);
    assert_eq!(grown.len(), BINDINGS.len());
    assert_eq!(grown[7], 2, "delete_session takes two display lines");
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
        &frame.joins[start..start + 2],
        &[Join::Break, Join::WrapSpace]
    );
    let single: usize = grown.iter().take(3).sum();
    assert_eq!(&frame.joins[single..single + 1], &[Join::Break]);
}

/// An overlong word breaks inside where it reaches the column's edge,
/// noting a wrap with no space, at a narrow width: the delete_word
/// keys break inside `⌥Backspace` while its id and description stand
/// whole on the first line.
#[test]
fn an_overlong_word_breaks_inside_at_a_narrow_width() {
    let keys = keys();
    let (label, id, description) = columns(&keys, 44);
    assert_eq!((label, id, description), (5, 20, 15));
    let delete_word = BINDINGS
        .iter()
        .position(|binding| binding.id == "delete_word")
        .expect("delete_word");
    assert_eq!(delete_word, 12);
    let grown = heights(&keys, 44);
    assert_eq!(grown[delete_word], 2, "the keys wrap, nothing else does");
    let screen = keys_screen();
    let frame = screen.frame(&keys, 44, HEIGHT);
    let start: usize = grown.iter().take(delete_word).sum();
    assert_eq!(frame.rows[start][0].0.trim_end(), "⌥Back");
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
    assert_eq!((label, id, description), (13, 20, 43));
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
    // The first description line is exactly 43 columns wide, filling
    // its column with no pad.
    let description_line = format!(
        "Send a prompt, or a steering message during{}",
        " ".repeat(description - 43)
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
            format!("a turn{}", " ".repeat(description - 6))
        };
        assert_eq!(text, format!("{mark}{key}{id}{rest}"), "line {line}");
    }
}
