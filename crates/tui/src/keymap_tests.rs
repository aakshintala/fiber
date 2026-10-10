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

#[test]
fn columns_grow_by_a_single_step() {
    use super::{Columns, columns, row_text};
    use crate::bindings::Binding;
    use crate::format::width;
    let keys = Keyset::default();
    let binding =
        |area: &'static str, description: &'static str, shown: &'static str, id: &'static str| {
            Binding {
                area,
                id,
                description,
                keys: shown,
                when: "",
                other_paths: "",
                contexts: crate::keyset::Contexts::ALL,
                defaults: &[],
                events: &[],
            }
        };
    // Widest (7, 2, 2): at inner 16 the room is 10, the area takes 5
    // and the floored shares are (2, 2), both already at their
    // widest with one column unused: no step widens past either.
    let short = binding("AAAAAAA", "aa", "bb", "t_short");
    assert_eq!(
        columns(&[&short], &keys, 16),
        Columns {
            area: 5,
            action: 2,
            keys: 2
        }
    );
    // Widest (7, 6, 2): the same room shares (3, 1), one short of
    // the rest with the action below its widest: one step grows the
    // action to (4, 1).
    let medium = binding("AAAAAAA", "aaaaaa", "bb", "t_medium");
    assert_eq!(
        columns(&[&medium], &keys, 16),
        Columns {
            area: 5,
            action: 4,
            keys: 1
        }
    );
    // Widest (20, 4, 4): at inner 18 the room is 12, the area takes
    // 6 and the shares are exactly (3, 3): no step runs at an exact
    // fit.
    let even = binding("AAAAAAAAAAAAAAAAAAAA", "aaaa", "bbbb", "t_even");
    assert_eq!(
        columns(&[&even], &keys, 18),
        Columns {
            area: 6,
            action: 3,
            keys: 3
        }
    );
    // The short row fits whole at inner 21: each column keeps its
    // widest text.
    assert_eq!(
        columns(&[&short], &keys, 21),
        Columns {
            area: 7,
            action: 2,
            keys: 2
        }
    );
    // Across widths the floored shares leave at most one column
    // unused: the single step fills it, so past the early return the
    // rest holds at most one more than the two columns take.
    let visible: Vec<&Binding> = BINDINGS.iter().collect();
    let mut widest = (1u16, 1u16, 1u16);
    for row in &visible {
        let (area, action, paths) = row_text(row, &keys.shown(row));
        widest.0 = widest
            .0
            .max(u16::try_from(width(&area)).unwrap_or(u16::MAX));
        widest.1 = widest
            .1
            .max(u16::try_from(width(&action)).unwrap_or(u16::MAX));
        widest.2 = widest
            .2
            .max(u16::try_from(width(&paths)).unwrap_or(u16::MAX));
    }
    for inner in 0..200u16 {
        let cols = columns(&visible, &keys, inner);
        assert!(cols.area >= 1 && cols.action >= 1 && cols.keys >= 1);
        let room = inner.saturating_sub(6);
        if widest.0.saturating_add(widest.1).saturating_add(widest.2) <= room {
            assert_eq!((cols.area, cols.action, cols.keys), widest);
        } else {
            assert_eq!(cols.area, widest.0.min(room / 2).max(1));
            let rest = room.saturating_sub(cols.area);
            if rest >= 2 {
                let sum = cols.action.saturating_add(cols.keys);
                assert!(sum <= rest, "inner {inner}: {cols:?} past {rest}");
                assert!(
                    rest.saturating_sub(sum) <= 1,
                    "inner {inner}: {cols:?} leaves more than one of {rest}"
                );
            }
        }
    }
}

#[test]
fn columns_spare_step_grows_action_or_stays_unused() {
    use super::{Columns, columns};
    use crate::bindings::Binding;
    let keys = Keyset::default();
    let binding =
        |area: &'static str, description: &'static str, shown: &'static str, id: &'static str| {
            Binding {
                area,
                id,
                description,
                keys: shown,
                when: "",
                other_paths: "",
                contexts: crate::keyset::Contexts::ALL,
                defaults: &[],
                events: &[],
            }
        };
    // Widest (7, 6, 2): at inner 16 the room is 10, the area takes 5
    // and the floored shares are (3, 1), one short of the rest with
    // the action below its widest: the spare column goes to the
    // action. Under the `action > widest.1` mutation the step never
    // runs, giving (3, 1), so this fails mutated.
    let medium = binding("AAAAAAA", "aaaaaa", "bb", "t_spare_action");
    assert_eq!(
        columns(&[&medium], &keys, 16),
        Columns {
            area: 5,
            action: 4,
            keys: 1
        }
    );
    // Widest (7, 2, 2): the same room shares (2, 2), both already at
    // their widest with one column spare: the step stays unused
    // rather than widen past either. No input sends the spare to the
    // keys: the action at or past its widest leaves the keys share at
    // or past theirs whenever a column is spare (for widest.1 > 1 a
    // kept action means rest >= total, and for widest.1 == 1 a spare
    // means rest >= total), so the keys-takes-spare branch was dead
    // code and is deleted rather than tested.
    let short = binding("AAAAAAA", "aa", "bb", "t_spare_unused");
    assert_eq!(
        columns(&[&short], &keys, 16),
        Columns {
            area: 5,
            action: 2,
            keys: 2
        }
    );
}
