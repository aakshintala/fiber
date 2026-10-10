//! Tests for the key map's state: tabs, queries, focus and scroll.

use super::{areas, keys_text};
use crate::bindings::BINDINGS;
use crate::keys::Key;
use crate::keyset::Keyset;

use super::KeyMap;

/// The visible bindings' ids.
fn shown(state: &KeyMap, keys: &Keyset) -> Vec<&'static str> {
    state
        .visible(keys)
        .iter()
        .map(|binding| binding.id)
        .collect()
}

#[test]
fn chrome_gives_way_a_step_at_a_time() {
    use super::chrome;
    // Full chrome while the body holds a binding row past it.
    let full = chrome(24);
    assert!(full.dims && full.blank && full.footer);
    assert_eq!(full.body, 11);
    // Each give-way step's boundary: dims go at 13, the blank at 12,
    // the footer at 11, and the body rows 1 and 0 at 9 and 8.
    let stepped = chrome(14);
    assert!(stepped.dims && stepped.blank && stepped.footer);
    assert_eq!(stepped.body, 1);
    let stepped = chrome(13);
    assert!(!stepped.dims && stepped.blank && stepped.footer);
    assert_eq!(stepped.body, 2);
    let stepped = chrome(12);
    assert!(!stepped.dims && stepped.blank && stepped.footer);
    assert_eq!(stepped.body, 1);
    // Giving up the footer frees its two rows for bindings.
    let stepped = chrome(11);
    assert!(!stepped.dims && !stepped.blank && stepped.footer);
    assert_eq!(stepped.body, 1);
    let stepped = chrome(10);
    assert!(!stepped.dims && !stepped.blank && !stepped.footer);
    assert_eq!(stepped.body, 2);
    assert_eq!(chrome(9).body, 1);
    assert_eq!(chrome(8).body, 0);
}

#[test]
fn columns_keep_the_areas_whole_while_they_fit() {
    use super::columns;
    let keys = Keyset::default();
    let visible: Vec<&crate::bindings::Binding> = BINDINGS.iter().collect();
    // Wide: each column keeps its widest text, the areas whole.
    let cols = columns(&visible, &keys, 200);
    assert_eq!(cols.area, 25);
    // Narrow: the areas take at most half the room, the rest splits
    // between action and keys, one column each at least.
    let cols = columns(&visible, &keys, 40);
    assert_eq!(cols.area, 17);
    assert_eq!(cols.action.saturating_add(cols.keys), 17);
    assert!(cols.action >= 1 && cols.keys >= 1);
    // Nothing fits: every column is one wide, and drawing cuts.
    let cols = columns(&visible, &keys, 4);
    assert_eq!((cols.area, cols.action, cols.keys), (1, 1, 1));
}

#[test]
fn tabs_run_all_then_each_area_in_table_order() {
    assert_eq!(
        areas(),
        [
            "Sessions",
            "The input box",
            "The conversation",
            "Steering",
            "Requests, models and help",
        ]
    );
    let keys = Keyset::default();
    let all = KeyMap::default();
    assert_eq!(all.tab(), 0);
    assert_eq!(all.visible(&keys).len(), BINDINGS.len());
    // Each tab shows its area's rows in table order.
    let mut state = KeyMap::default();
    state.move_tab(1);
    assert_eq!(state.tab(), 1);
    assert!(shown(&state, &keys).iter().all(|id| {
        BINDINGS
            .iter()
            .find(|row| row.id == *id)
            .is_some_and(|row| row.area == "Sessions")
    }));
    assert_eq!(shown(&state, &keys).len(), 8);
}

#[test]
fn tab_movement_stops_at_both_ends() {
    let mut state = KeyMap::default();
    state.move_tab(-1);
    assert_eq!(state.tab(), 0);
    state.move_tab(100);
    assert_eq!(state.tab(), 5);
    state.move_tab(1);
    assert_eq!(state.tab(), 5);
    state.move_tab(-100);
    assert_eq!(state.tab(), 0);
}

#[test]
fn a_query_matches_the_action_keys_and_other_paths_ignoring_case() {
    let keys = Keyset::default();
    let mut state = KeyMap::default();
    for ch in "steering".chars() {
        state.push(ch);
    }
    assert_eq!(state.query(), "steering");
    // Across every area when the tab is All: a Sessions row and a
    // Steering row both match in their descriptions.
    assert!(shown(&state, &keys).contains(&"send"));
    assert!(shown(&state, &keys).contains(&"select_steering"));
    assert!(
        state.visible(&keys).iter().all(|row| {
            row.description.to_lowercase().contains("steering")
                || keys.shown(row).to_lowercase().contains("steering")
                || row.other_paths.to_lowercase().contains("steering")
        }),
        "{:?}",
        shown(&state, &keys)
    );
    state.pop();
    assert_eq!(state.query(), "steerin");
}

#[test]
fn a_query_matching_only_a_person_set_key_finds_it() {
    let mut user = serde_json::Map::new();
    user.insert("send".to_owned(), serde_json::json!(["ctrl+s", "alt+s"]));
    let (keys, notices) = crate::keyset::load(&user);
    assert!(notices.is_empty(), "{notices:?}");
    let mut rebound = KeyMap::default();
    for ch in "ctrl+s".chars() {
        rebound.push(ch);
    }
    assert!(shown(&rebound, &keys).contains(&"send"));
    // The doc's Enter no longer matches the rebound row.
    let mut doc = KeyMap::default();
    for ch in "Send a prompt, or a steering message during a turn · Enter".chars() {
        doc.push(ch);
    }
    assert!(
        !shown(&doc, &keys).contains(&"send"),
        "{:?}",
        shown(&doc, &keys)
    );
    let mut plain = KeyMap::default();
    for ch in "enter".chars() {
        plain.push(ch);
    }
    assert!(!shown(&plain, &keys).contains(&"send"));
}

#[test]
fn an_unbound_row_matches_unbound() {
    let mut user = serde_json::Map::new();
    user.insert("toggle_ledgers".to_owned(), serde_json::json!([]));
    let (keys, notices) = crate::keyset::load(&user);
    assert!(notices.is_empty(), "{notices:?}");
    let binding = BINDINGS
        .iter()
        .find(|row| row.id == "toggle_ledgers")
        .expect("the row");
    assert_eq!(keys.shown(binding), "unbound");
    let mut state = KeyMap::default();
    for ch in "unbound".chars() {
        state.push(ch);
    }
    assert!(shown(&state, &keys).contains(&"toggle_ledgers"));
}

#[test]
fn the_keys_leave_the_condition_to_the_action() {
    let binding = BINDINGS
        .iter()
        .find(|row| row.id == "recall_prompt")
        .expect("the row");
    assert_eq!(keys_text(binding, binding.keys), "↑");
    assert_eq!(keys_text(binding, "Ctrl+X"), "Ctrl+X");
    let plain = BINDINGS
        .iter()
        .find(|row| row.id == "send")
        .expect("the row");
    assert_eq!(keys_text(plain, plain.keys), "Enter");
}

#[test]
fn editing_the_query_resets_focus_and_scroll() {
    let keys = Keyset::default();
    // Seven bindings fit: moving down seven times scrolls past the first.
    let fits = |_: usize| 7;
    let mut state = KeyMap::default();
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    assert_eq!(state.focus(), 7);
    assert_eq!(state.top(), 1);
    state.push('x');
    assert_eq!((state.focus(), state.top()), (0, 0));
    state.key(&Key::Down, &keys, &fits);
    state.pop();
    assert_eq!((state.focus(), state.top()), (0, 0));
    let mut tabbed = KeyMap::default();
    tabbed.key(&Key::Down, &keys, &fits);
    tabbed.move_tab(1);
    assert_eq!((tabbed.focus(), tabbed.top()), (0, 0));
}

#[test]
fn up_and_down_stop_at_the_ends() {
    let keys = Keyset::default();
    let fits = |_: usize| 40;
    let mut state = KeyMap::default();
    state.key(&Key::Up, &keys, &fits);
    assert_eq!((state.focus(), state.top()), (0, 0));
    state.key(&Key::Down, &keys, &fits);
    assert_eq!(state.focus(), 1);
    for _ in 0..100 {
        state.key(&Key::Down, &keys, &fits);
    }
    assert_eq!(state.focus(), BINDINGS.len() - 1);
    state.key(&Key::Down, &keys, &fits);
    assert_eq!(state.focus(), BINDINGS.len() - 1);
    for _ in 0..100 {
        state.key(&Key::Up, &keys, &fits);
    }
    assert_eq!((state.focus(), state.top()), (0, 0));
}

#[test]
fn pages_move_by_what_fits_less_one_at_least_one() {
    let keys = Keyset::default();
    // Seven fit: a page is six.
    let fits = |_: usize| 7;
    let mut state = KeyMap::default();
    state.key(&Key::PageDown, &keys, &fits);
    assert_eq!(state.focus(), 6);
    state.key(&Key::PageUp, &keys, &fits);
    assert_eq!(state.focus(), 0);
    // One fits: a page is still one.
    let fits = |_: usize| 1;
    let mut state = KeyMap::default();
    state.key(&Key::PageDown, &keys, &fits);
    assert_eq!(state.focus(), 1);
    // Nothing fits: a page is still one.
    let fits = |_: usize| 0;
    let mut state = KeyMap::default();
    state.key(&Key::PageDown, &keys, &fits);
    assert_eq!(state.focus(), 1);
}

#[test]
fn the_window_keeps_the_focused_binding_whole() {
    let keys = Keyset::default();
    // Rows alternate one and two wrapped rows: from an even top three
    // bindings fit, from an odd one two.
    let fits = |top: usize| if top.is_multiple_of(2) { 3 } else { 2 };
    let mut state = KeyMap::default();
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    state.key(&Key::Down, &keys, &fits);
    assert_eq!(state.focus(), 3);
    // From 0 three fit (0, 1, 2): the focus leaves the window, so the
    // scroll moves to the first top holding it whole: from 1 two fit
    // (1, 2), so it moves on to 2.
    assert_eq!(state.top(), 2);
    assert!(state.focus() < state.top() + fits(state.top()));
    // Back up: the scroll follows the focus.
    state.key(&Key::Up, &keys, &fits);
    state.key(&Key::Up, &keys, &fits);
    state.key(&Key::Up, &keys, &fits);
    assert_eq!((state.focus(), state.top()), (0, 0));
}

#[test]
fn the_counts_are_bindings_above_and_below() {
    let keys = Keyset::default();
    let fits = |_: usize| 7;
    let mut state = KeyMap::default();
    for _ in 0..10 {
        state.key(&Key::Down, &keys, &fits);
    }
    let total = state.visible(&keys).len();
    let above = state.top();
    let below = total.saturating_sub(state.top() + fits(state.top()));
    assert_eq!(above, state.top());
    assert!(above > 0);
    assert_eq!(above + fits(state.top()) + below, total);
    // A query restart clears both counts.
    state.push('z');
    state.push('z');
    state.push('z');
    let total = state.visible(&keys).len();
    assert_eq!((state.top(), total.saturating_sub(fits(0))), (0, 0));
}

#[test]
fn fits_from_leaves_the_above_count_row_once_scrolled() {
    use super::fits_from;
    // From the top the whole body holds the rows. Once bindings hide
    // above, the count line takes the body's last row, so one row less
    // is left for bindings.
    assert_eq!(fits_from(0, &[1, 1, 1], 2), 2);
    assert_eq!(fits_from(1, &[1, 1, 1], 2), 1);
}
