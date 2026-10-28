//! Tests for the keyset: resolution, validation notices and clash passes.

use serde_json::{Map, Value, json};

use super::{Context, Contexts, Keyset, canonical_key, check_action, load, person_bound_keys};
use crate::bindings::BINDINGS;
use crate::keys::{Edit, Key};
use crate::stroke::Stroke;

/// The keyset for `entries`, with its notices.
fn keyset(entries: &[(&str, Value)]) -> (Keyset, Vec<String>) {
    let mut user = Map::new();
    for (id, value) in entries {
        user.insert((*id).to_owned(), value.clone());
    }
    load(&user)
}

/// The stroke `name` names.
fn stroke(name: &str) -> Stroke {
    Stroke::parse(name).unwrap_or_else(|err| panic!("{name}: {err}"))
}

/// What `name` means in `context` with `entries` set.
fn at(entries: &[(&str, Value)], name: &str, context: Context) -> super::Resolved {
    let (keys, _) = keyset(entries);
    keys.resolve(&stroke(name), context)
}

#[test]
fn the_defaults_never_clash() {
    for (lower, first) in BINDINGS.iter().enumerate() {
        for second in BINDINGS.iter().skip(lower.saturating_add(1)) {
            if !first.contexts.overlaps(second.contexts) {
                continue;
            }
            for one in first.defaults.iter() {
                for other in second.defaults.iter() {
                    assert!(
                        one != other,
                        "{} and {} share {one} in overlapping contexts",
                        first.id,
                        second.id
                    );
                }
            }
        }
    }
}

#[test]
fn ctrl_n_starts_a_new_session_outside_search_and_overlays() {
    for context in [Context::Input, Context::Steering, Context::Conversation] {
        assert_eq!(
            at(&[], "ctrl+n", context),
            super::Resolved::Action("new_session"),
            "{context:?}"
        );
    }
    for context in [Context::Search, Context::Overlay] {
        assert_eq!(at(&[], "ctrl+n", context), super::Resolved::Nothing);
    }
}

#[test]
fn space_types_where_no_action_binds_it() {
    assert_eq!(
        at(&[], "space", Context::Input),
        super::Resolved::Key(Key::Char(' '))
    );
    assert_eq!(
        at(&[], "space", Context::Overlay),
        super::Resolved::Key(Key::Char(' '))
    );
}

#[test]
fn a_person_key_gives_its_variant_canonical_event() {
    let entries = &[("move_word", json!(["ctrl+b", "alt+g"]))];
    let (_, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        at(entries, "ctrl+b", Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
    assert_eq!(
        at(entries, "alt+g", Context::Input),
        super::Resolved::Edit(Edit::WordRight)
    );
    // `move_word` does not act in the conversation: search keeps Ctrl+F.
    assert_eq!(
        at(entries, "ctrl+f", Context::Conversation),
        super::Resolved::Key(Key::CtrlF)
    );
}

#[test]
fn a_default_key_in_its_default_variant_gives_its_own_default_event() {
    let entries = &[("delete_session", json!(["backspace"]))];
    assert_eq!(
        at(entries, "backspace", Context::Conversation),
        super::Resolved::Key(Key::Backspace)
    );
}

#[test]
fn a_default_key_in_another_variant_gives_the_canonical_event() {
    let entries = &[("move_word", json!(["alt+right", "alt+left"]))];
    let (_, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        at(entries, "alt+right", Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn a_default_key_in_a_later_group_gives_the_canonical_event_of_its_slot() {
    // `alt+right` is move_word's default at place 1 (right). Here it sits at
    // index 2 of the person's list, the left variant's slot in the second
    // group, so its answer is the canonical left event, not its own right.
    let entries = &[(
        "move_word",
        json!(["ctrl+b", "alt+g", "alt+right", "alt+j"]),
    )];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        keys.resolve(&stroke("alt+right"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn a_moved_off_default_gives_nothing_in_its_contexts_and_its_event_outside() {
    let entries = &[("copy_focused", json!("c"))];
    assert_eq!(
        at(entries, "y", Context::Conversation),
        super::Resolved::Nothing
    );
    assert_eq!(
        at(entries, "y", Context::Input),
        super::Resolved::Key(Key::Char('y'))
    );
    assert_eq!(
        at(entries, "c", Context::Conversation),
        super::Resolved::Key(Key::Char('y'))
    );
}

#[test]
fn an_empty_list_unbinds_in_every_context() {
    let entries = &[("toggle_ledgers", json!([]))];
    for context in [
        Context::Input,
        Context::Steering,
        Context::Conversation,
        Context::Search,
        Context::Overlay,
        Context::Picker,
    ] {
        assert_eq!(at(entries, "ctrl+o", context), super::Resolved::Nothing);
    }
}

#[test]
fn session_only_acts_only_in_the_picker() {
    // At its default Ctrl+S resolves to the action in the picker, and
    // to nothing where the picker is not open.
    assert_eq!(
        at(&[], "ctrl+s", Context::Picker),
        super::Resolved::Action("session_only")
    );
    for context in [Context::Input, Context::Overlay, Context::Conversation] {
        assert_eq!(
            at(&[], "ctrl+s", context),
            super::Resolved::Nothing,
            "{context:?}"
        );
    }
    // `s` is an ordinary key in the picker, reaching the filter.
    assert_eq!(
        at(&[], "s", Context::Picker),
        super::Resolved::Key(Key::Char('s'))
    );
    let entries = &[("session_only", json!("x"))];
    assert_eq!(
        at(entries, "x", Context::Picker),
        super::Resolved::Action("session_only")
    );
    assert_eq!(
        at(entries, "ctrl+s", Context::Picker),
        super::Resolved::Nothing
    );
}

#[test]
fn picker_tab_and_refresh_resolve_to_their_keys() {
    assert_eq!(
        at(&[], "tab", Context::Picker),
        super::Resolved::Key(Key::Tab)
    );
    assert_eq!(
        at(&[], "ctrl+r", Context::Picker),
        super::Resolved::Key(Key::CtrlR)
    );
    // Ctrl+R in the input box still searches the prompts.
    assert_eq!(
        at(&[], "ctrl+r", Context::Input),
        super::Resolved::Key(Key::CtrlR)
    );
}

#[test]
fn show_all_rebinds_to_another_key() {
    let entries = &[("show_all", json!("ctrl+t"))];
    assert_eq!(
        at(entries, "ctrl+t", Context::Picker),
        super::Resolved::Key(Key::Tab)
    );
    assert_eq!(
        at(entries, "tab", Context::Picker),
        super::Resolved::Nothing
    );
}

#[test]
fn refresh_lists_rebinds_to_another_key() {
    let entries = &[("refresh_lists", json!("ctrl+t"))];
    assert_eq!(
        at(entries, "ctrl+t", Context::Picker),
        super::Resolved::Key(Key::CtrlR)
    );
}

#[test]
fn model_picker_rebinds_to_another_key() {
    let entries = &[("model_picker", json!("ctrl+t"))];
    assert_eq!(
        at(entries, "ctrl+t", Context::Input),
        super::Resolved::Key(Key::CtrlL)
    );
    assert_eq!(
        at(entries, "ctrl+l", Context::Input),
        super::Resolved::Nothing
    );
}

#[test]
fn a_person_key_for_paste_image_gives_ctrl_v() {
    let entries = &[("paste_image", json!("alt+v"))];
    for context in [Context::Input, Context::Steering] {
        assert_eq!(
            at(entries, "alt+v", context),
            super::Resolved::Key(Key::CtrlV),
            "{context:?}"
        );
        assert_eq!(
            at(entries, "ctrl+v", context),
            super::Resolved::Nothing,
            "{context:?}"
        );
    }
}

#[test]
fn unbinding_search_results_leaves_no_alias_live_in_search() {
    let entries = &[("search_results", json!([]))];
    for name in ["ctrl+f", "super+f"] {
        assert_eq!(at(entries, name, Context::Search), super::Resolved::Nothing);
        assert_eq!(
            at(entries, name, Context::Input),
            super::Resolved::Key(Key::CtrlF)
        );
    }
}

#[test]
fn unbinding_steering_leaves_its_pass_through_keys_dead() {
    let entries = &[("drop_steering", json!([])), ("select_steering", json!([]))];
    for name in ["alt+x", "alt+up", "alt+down"] {
        assert_eq!(
            at(entries, name, Context::Conversation),
            super::Resolved::Nothing,
            "{name}"
        );
        assert_eq!(
            at(entries, name, Context::Input),
            super::Resolved::Nothing,
            "{name}"
        );
    }
}

#[test]
fn an_unknown_id_gives_one_notice_and_is_ignored() {
    let (keys, notices) = keyset(&[("new_sesion", json!("ctrl+t"))]);
    assert_eq!(
        notices,
        ["keys.new_sesion: Fiber has no action new_sesion; it is ignored."]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+t"), Context::Input),
        super::Resolved::Nothing
    );
}

#[test]
fn a_non_string_value_gives_one_notice_and_keeps_the_default() {
    let (keys, notices) = keyset(&[("send", json!(5))]);
    assert_eq!(
        notices,
        ["keys.send: 5 is not a key or a list of keys; send keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("enter"), Context::Input),
        super::Resolved::Key(Key::Enter)
    );
}

#[test]
fn a_bad_key_name_gives_one_notice_and_keeps_the_default() {
    let (keys, notices) = keyset(&[("new_session", json!("ctrl+tt"))]);
    assert_eq!(
        notices,
        ["keys.new_session: \"ctrl+tt\" is not a key name; new_session keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+n"), Context::Input),
        super::Resolved::Action("new_session")
    );
}

#[test]
fn a_variant_action_list_of_the_wrong_length_gives_one_notice() {
    let (keys, notices) = keyset(&[("move_word", json!(["a", "b", "c"]))]);
    assert_eq!(
        notices,
        ["keys.move_word: takes keys in twos (left, right); move_word keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn a_key_in_two_variants_gives_one_notice_and_keeps_the_default() {
    let (keys, notices) = keyset(&[("move_word", json!(["ctrl+b", "ctrl+b"]))]);
    assert_eq!(
        notices,
        ["keys.move_word: ctrl+b is both left and right; move_word keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
    let (keys, notices) = keyset(&[("move_word", json!(["a", "b", "b", "c"]))]);
    assert_eq!(
        notices,
        ["keys.move_word: b is both right and left; move_word keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn clear_then_quit_cannot_be_set() {
    let (keys, notices) = keyset(&[("clear_then_quit", json!("ctrl+q"))]);
    assert_eq!(
        notices,
        [
            "keys.clear_then_quit: Ctrl+C always clears, then quits, and cannot be rebound; it is ignored."
        ]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+c"), Context::Input),
        super::Resolved::Key(Key::CtrlC)
    );
}

#[test]
fn no_action_may_take_ctrl_c() {
    let (keys, notices) = keyset(&[("copy_focused", json!("ctrl+c"))]);
    assert_eq!(
        notices,
        ["keys.copy_focused: Ctrl+C always clears, then quits; copy_focused keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("y"), Context::Conversation),
        super::Resolved::Key(Key::Char('y'))
    );
}

#[test]
fn variant_lists_of_two_and_four_keys_load() {
    let (keys, notices) = keyset(&[("move_word", json!(["a", "b", "c", "d"]))]);
    assert!(notices.is_empty(), "{notices:?}");
    for name in ["a", "c"] {
        assert_eq!(
            keys.resolve(&stroke(name), Context::Input),
            super::Resolved::Edit(Edit::WordLeft),
            "{name}"
        );
    }
    for name in ["b", "d"] {
        assert_eq!(
            keys.resolve(&stroke(name), Context::Input),
            super::Resolved::Edit(Edit::WordRight),
            "{name}"
        );
    }
}

#[test]
fn a_group_identical_to_an_earlier_group_is_dropped_whole() {
    let entries = &[("move_word", json!(["a", "b", "a", "b"]))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    let binding = BINDINGS
        .iter()
        .find(|binding| binding.id == "move_word")
        .expect("move_word is a binding");
    assert_eq!(keys.shown(binding), "a b");
}

#[test]
fn groups_keep_one_key_per_variant_in_order() {
    let entries = &[("move_word", json!(["a", "b", "c", "d", "a", "b"]))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        keys.resolve(&stroke("a"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
    assert_eq!(
        keys.resolve(&stroke("c"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
    assert_eq!(
        keys.resolve(&stroke("b"), Context::Input),
        super::Resolved::Edit(Edit::WordRight)
    );
    assert_eq!(
        keys.resolve(&stroke("d"), Context::Input),
        super::Resolved::Edit(Edit::WordRight)
    );
}

#[test]
fn a_repeated_key_counts_once_for_a_single_variant_action() {
    let entries = &[("send", json!(["ctrl+s", "ctrl+s"]))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    let binding = BINDINGS
        .iter()
        .find(|binding| binding.id == "send")
        .expect("send is a binding");
    assert_eq!(keys.shown(binding), "Ctrl+S");
    assert_eq!(
        keys.resolve(&stroke("ctrl+s"), Context::Input),
        super::Resolved::Key(Key::Enter)
    );
}

#[test]
fn a_clash_with_a_default_reverts_the_person_side() {
    let entries = &[("search", json!("ctrl+o"))];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        [
            "keys.search: Ctrl+O is also Open or close the ledgers (toggle_ledgers); search keeps its default."
        ]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+o"), Context::Conversation),
        super::Resolved::Key(Key::CtrlO)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+f"), Context::Conversation),
        super::Resolved::Key(Key::CtrlF)
    );
}

#[test]
fn two_person_entries_clashing_revert_both() {
    let entries = &[
        ("search", json!("ctrl+o")),
        ("toggle_ledgers", json!(["ctrl+o", "ctrl+q"])),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        ["keys.search and keys.toggle_ledgers both bind Ctrl+O; both keep their defaults."]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+o"), Context::Conversation),
        super::Resolved::Key(Key::CtrlO)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+f"), Context::Conversation),
        super::Resolved::Key(Key::CtrlF)
    );
}

#[test]
fn two_person_entries_sharing_a_key_at_different_slots_revert_both() {
    // Ctrl+T sits at slot 1 of `move_word`'s pair and at slot 0 of
    // `search`'s single key: different slots, still a clash in Input.
    let entries = &[
        ("move_word", json!(["ctrl+b", "ctrl+t"])),
        ("search", json!("ctrl+t")),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        ["keys.move_word and keys.search both bind Ctrl+T; both keep their defaults."]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+t"), Context::Input),
        super::Resolved::Nothing
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+f"), Context::Input),
        super::Resolved::Key(Key::CtrlF)
    );
}

#[test]
fn a_person_variant_key_at_another_slot_clashing_with_a_default_reverts() {
    // Ctrl+F sits at slot 1 of the person's `move_word` pair and at slot
    // 0 of `search`'s defaults: the person's side reverts on its own.
    let entries = &[("move_word", json!(["ctrl+b", "ctrl+f"]))];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        ["keys.move_word: Ctrl+F is also Search (search); move_word keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+f"), Context::Input),
        super::Resolved::Key(Key::CtrlF)
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn two_person_variant_entries_sharing_a_key_at_the_same_slot_revert_both() {
    // Ctrl+B sits at slot 0 of both pairs: the same-slot clash still
    // reverts both sides.
    let entries = &[
        ("move_word", json!(["ctrl+b", "alt+g"])),
        ("line_start_end", json!(["ctrl+b", "alt+h"])),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        ["keys.line_start_end and keys.move_word both bind Ctrl+B; both keep their defaults."]
    );
    assert_eq!(
        keys.resolve(&stroke("alt+left"), Context::Input),
        super::Resolved::Edit(Edit::WordLeft)
    );
}

#[test]
fn one_pass_with_two_clashes_gives_two_notices_in_table_order() {
    let entries = &[
        ("search", json!("ctrl+o")),
        ("toggle_panel", json!("ctrl+o")),
        ("toggle_ledgers", json!("ctrl+l")),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        [
            "keys.toggle_ledgers: Ctrl+L is also Open the model picker (model_picker); toggle_ledgers keeps its default.",
            "keys.search and keys.toggle_panel both bind Ctrl+O; both keep their defaults.",
        ]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+l"), Context::Input),
        super::Resolved::Key(Key::CtrlL)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+o"), Context::Input),
        super::Resolved::Key(Key::CtrlO)
    );
}

#[test]
fn a_cascade_settles_over_two_passes() {
    let entries = &[
        ("search", json!("ctrl+o")),
        ("toggle_ledgers", json!("alt+p")),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        [
            "keys.toggle_ledgers: ⌥P is also Show or hide the panel (toggle_panel); toggle_ledgers keeps its default.",
            "keys.search: Ctrl+O is also Open or close the ledgers (toggle_ledgers); search keeps its default.",
        ]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+o"), Context::Input),
        super::Resolved::Key(Key::CtrlO)
    );
}

#[test]
fn an_entry_reverted_by_two_pairs_in_one_pass_stays_reverted() {
    let entries = &[
        ("search", json!("ctrl+o")),
        ("toggle_ledgers", json!(["ctrl+o", "ctrl+l"])),
    ];
    let (keys, notices) = keyset(entries);
    assert_eq!(
        notices,
        [
            "keys.search and keys.toggle_ledgers both bind Ctrl+O; both keep their defaults.",
            "keys.toggle_ledgers: Ctrl+L is also Open the model picker (model_picker); toggle_ledgers keeps its default.",
        ]
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+o"), Context::Input),
        super::Resolved::Key(Key::CtrlO)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+l"), Context::Input),
        super::Resolved::Key(Key::CtrlL)
    );
}

#[test]
fn variant_actions_name_their_variants() {
    let (keys, notices) = keyset(&[("rail_row_n", json!(["alt+1", "alt+2"]))]);
    assert_eq!(
        notices,
        ["keys.rail_row_n: takes keys in nines (1 to 9); rail_row_n keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("alt+1"), Context::Overlay),
        super::Resolved::Key(Key::AltDigit(1))
    );
    let (_, notices) = keyset(&[("select_steering", json!("alt+up"))]);
    assert_eq!(
        notices,
        ["keys.select_steering: takes keys in twos (up, down); select_steering keeps its default."]
    );
    let (_, notices) = keyset(&[("line_start_end", json!("super+left"))]);
    assert_eq!(
        notices,
        ["keys.line_start_end: takes keys in twos (start, end); line_start_end keeps its default."]
    );
    let (_, notices) = keyset(&[("focus_next_prev", json!("down"))]);
    assert_eq!(
        notices,
        [
            "keys.focus_next_prev: takes keys in twos (next, prev); focus_next_prev keeps its default."
        ]
    );
    let (_, notices) = keyset(&[("select_steering", json!(["alt+up", "alt+up"]))]);
    assert_eq!(
        notices,
        ["keys.select_steering: alt+up is both up and down; select_steering keeps its default."]
    );
    let (_, notices) = keyset(&[("line_start_end", json!(["super+left", "super+left"]))]);
    assert_eq!(
        notices,
        [
            "keys.line_start_end: super+left is both start and end; line_start_end keeps its default."
        ]
    );
    let (_, notices) = keyset(&[("focus_next_prev", json!(["down", "down"]))]);
    assert_eq!(
        notices,
        ["keys.focus_next_prev: down is both next and prev; focus_next_prev keeps its default."]
    );
    let (_, notices) = keyset(&[(
        "rail_row_n",
        json!([
            "alt+1", "alt+1", "alt+3", "alt+4", "alt+5", "alt+6", "alt+7", "alt+8", "alt+9",
        ]),
    )]);
    assert_eq!(
        notices,
        ["keys.rail_row_n: alt+1 is both 1 and 2; rail_row_n keeps its default."]
    );
}

#[test]
fn a_mixed_list_gives_one_notice_and_keeps_the_default() {
    let (keys, notices) = keyset(&[("send", json!(["a", 5]))]);
    assert_eq!(
        notices,
        ["keys.send: [\"a\",5] is not a key or a list of keys; send keeps its default."]
    );
    assert_eq!(
        keys.resolve(&stroke("enter"), Context::Input),
        super::Resolved::Key(Key::Enter)
    );
}

#[test]
fn actions_in_disjoint_contexts_share_a_key() {
    let entries = &[("open_focused", json!("ctrl+s")), ("send", json!("ctrl+s"))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        keys.resolve(&stroke("ctrl+s"), Context::Conversation),
        super::Resolved::Key(Key::Enter)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+s"), Context::Input),
        super::Resolved::Key(Key::Enter)
    );
}

#[test]
fn a_search_only_key_does_not_clash_with_an_input_key() {
    let entries = &[
        ("search_results", json!("ctrl+s")),
        ("send", json!("ctrl+s")),
    ];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(
        keys.resolve(&stroke("ctrl+s"), Context::Search),
        super::Resolved::Key(Key::CtrlF)
    );
    assert_eq!(
        keys.resolve(&stroke("ctrl+s"), Context::Input),
        super::Resolved::Key(Key::Enter)
    );
}

#[test]
fn an_entry_equal_to_the_defaults_counts_as_not_set() {
    let entries = &[(
        "move_word",
        json!([
            "alt+left",
            "alt+right",
            "ctrl+left",
            "ctrl+right",
            "alt+b",
            "alt+f",
        ]),
    )];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    let binding = BINDINGS
        .iter()
        .find(|binding| binding.id == "move_word")
        .expect("move_word is a binding");
    assert_eq!(keys.shown(binding), "⌥← ⌥→, Ctrl+← Ctrl+→");
}

#[test]
fn shown_gives_the_doc_cell_the_labels_or_unbound() {
    let (keys, _) = keyset(&[]);
    let send = BINDINGS
        .iter()
        .find(|binding| binding.id == "send")
        .expect("send is a binding");
    assert_eq!(keys.shown(send), "Enter");
    let entries = &[("send", json!(["ctrl+s", "alt+s"]))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    assert_eq!(keys.shown(send), "Ctrl+S, ⌥S");
    let entries = &[("move_word", json!(["ctrl+b", "alt+g"]))];
    let (keys, _) = keyset(entries);
    let move_word = BINDINGS
        .iter()
        .find(|binding| binding.id == "move_word")
        .expect("move_word is a binding");
    assert_eq!(keys.shown(move_word), "Ctrl+B ⌥G");
    let entries = &[("toggle_ledgers", json!([]))];
    let (keys, _) = keyset(entries);
    let toggle_ledgers = BINDINGS
        .iter()
        .find(|binding| binding.id == "toggle_ledgers")
        .expect("toggle_ledgers is a binding");
    assert_eq!(keys.shown(toggle_ledgers), "unbound");
}

#[test]
fn contexts_contain_single_contexts() {
    assert!(Contexts::INPUT.contains(Context::Input));
    assert!(!Contexts::INPUT.contains(Context::Steering));
    assert!(Contexts::ALL.contains(Context::Overlay));
    assert!(Contexts::ALL.contains(Context::Picker));
    assert!(Contexts::PICKER.contains(Context::Picker));
    assert!(!Contexts::PICKER.contains(Context::Overlay));
    assert!(!Contexts::OVERLAY.contains(Context::Picker));
}

#[test]
fn contexts_overlap_on_a_shared_context() {
    assert!(Contexts::INPUT.overlaps(Contexts::INPUT_STEERING));
    assert!(!Contexts::INPUT.overlaps(Contexts::CONVERSATION));
    assert!(Contexts::ALL.overlaps(Contexts::SEARCH));
    assert!(Contexts::ALL.overlaps(Contexts::PICKER));
    assert!(!Contexts::PICKER.overlaps(Contexts::INPUT));
    assert!(!Contexts::PICKER.overlaps(Contexts::OVERLAY));
}

/// Whether the person bound a stroke themselves: their own row, in
/// this context, holding this key. Defaults do not count
/// (`docs/tui.md`, "Selection and copy": their own Home binding wins
/// over the input editor's line edits).
#[test]
fn person_bound_needs_a_person_row_in_this_context_with_this_key() {
    let entries = &[("delete_to_line_start", json!("home"))];
    let (keys, notices) = keyset(entries);
    assert!(notices.is_empty(), "{notices:?}");
    let home = stroke("home");
    let enter = stroke("enter");
    // Their own Home binding in the input box.
    assert!(keys.person_bound(&home, Context::Input));
    // A default key is not their binding, even in the input box.
    assert!(!keys.person_bound(&enter, Context::Input));
    // Their Home binding is not bound over the overlay.
    assert!(!keys.person_bound(&home, Context::Overlay));
}

#[test]
fn canonical_key_reads_any_case_and_every_alias() {
    for (name, canonical) in [
        ("ctrl+o", "ctrl+o"),
        // An uppercase letter reads as shift with that letter.
        ("Ctrl+O", "ctrl+shift+o"),
        ("CTRL+Q", "ctrl+shift+q"),
        ("CTRL+ENTER", "ctrl+enter"),
        ("control+k", "ctrl+k"),
        ("escape", "esc"),
        ("ESCAPE", "esc"),
        ("opt+x", "alt+x"),
        ("option+x", "alt+x"),
        ("meta+x", "alt+x"),
        ("cmd+z", "super+z"),
        ("command+z", "super+z"),
        ("return", "enter"),
        ("ctrl++", "ctrl++"),
        ("+", "+"),
        ("A", "shift+a"),
        ("space", "space"),
        ("shift+space", "shift+space"),
        ("F5", "f5"),
        ("f12", "f12"),
    ] {
        assert_eq!(canonical_key(name).as_deref(), Some(canonical), "{name}");
    }
}

#[test]
fn canonical_key_rejects_names_that_do_not_parse() {
    for name in ["", "ctrl+", "bogus", "ctrl+shift", "f13", "f0"] {
        assert_eq!(canonical_key(name), None, "{name}");
    }
}

#[test]
fn check_action_accepts_plain_actions_without_a_variant() {
    for id in [
        "send",
        "new_session",
        "go_home",
        "toggle_ledgers",
        "model_picker",
    ] {
        assert_eq!(check_action(id, None), None, "{id}");
    }
}

#[test]
fn check_action_accepts_every_variant_of_the_six_variant_actions() {
    for (id, variants) in [
        ("move_word", &["left", "right"][..]),
        ("line_start_end", &["start", "end"][..]),
        ("focus_next_prev", &["next", "prev"][..]),
        ("search_next_prev", &["next", "prev"][..]),
        ("select_steering", &["up", "down"][..]),
        (
            "rail_row_n",
            &["1", "2", "3", "4", "5", "6", "7", "8", "9"][..],
        ),
    ] {
        for variant in variants {
            assert_eq!(check_action(id, Some(variant)), None, "{id}/{variant}");
        }
    }
}

#[test]
fn check_action_rejects_a_missing_or_unknown_variant() {
    for (id, bad) in [
        ("move_word", None),
        ("move_word", Some("up")),
        ("line_start_end", None),
        ("line_start_end", Some("left")),
        ("focus_next_prev", None),
        ("focus_next_prev", Some("start")),
        ("search_next_prev", None),
        ("search_next_prev", Some("end")),
        ("select_steering", None),
        ("select_steering", Some("left")),
        ("rail_row_n", None),
        ("rail_row_n", Some("0")),
        ("rail_row_n", Some("10")),
        ("rail_row_n", Some("left")),
    ] {
        let reason = check_action(id, bad);
        assert!(reason.is_some(), "{id}/{bad:?}");
        let reason = reason.unwrap();
        assert!(reason.contains(id), "{id}/{bad:?}: {reason}");
    }
}

#[test]
fn check_action_rejects_a_variant_for_a_one_variant_action() {
    for id in ["send", "new_session", "toggle_ledgers"] {
        let reason = check_action(id, Some("left"));
        assert!(reason.is_some(), "{id}");
        assert!(reason.unwrap().contains(id), "{id}");
    }
}

#[test]
fn check_action_rejects_an_unknown_action_and_clear_then_quit() {
    for id in ["no_such_action", "clear_then_quit", "Clear_Then_Quit"] {
        let reason = check_action(id, None);
        assert!(reason.is_some(), "{id}");
        assert!(reason.unwrap().contains(id), "{id}");
    }
}

#[test]
fn person_bound_keys_lists_the_person_set_canonicals() {
    let mut user = Map::new();
    user.insert("send".to_owned(), json!("ctrl+q"));
    user.insert("new_session".to_owned(), json!("ctrl+n"));
    let (keys, notices) = keyset(&[("send", json!("ctrl+q"))]);
    assert!(notices.is_empty(), "{notices:?}");
    assert!(keys.person_bound(&stroke("ctrl+q"), Context::Input));
    let owned = person_bound_keys(&user);
    assert!(owned.contains("ctrl+q"), "{owned:?}");
    // The default-equal entry counts as unset.
    assert!(!owned.contains("ctrl+n"), "{owned:?}");
}

#[test]
fn person_bound_keys_ignores_default_equal_unbound_and_invalid_entries() {
    let entries = [
        ("send", json!("enter")),
        ("new_session", json!([])),
        ("toggle_ledgers", json!("bogus")),
    ];
    let mut user = Map::new();
    for (id, value) in &entries {
        user.insert((*id).to_owned(), value.clone());
    }
    let (_, notices) = keyset(&entries);
    // docs/tui.md:1941-1954: each clash reverts with one notice; an invalid entry gives one notice and keeps its defaults.
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(person_bound_keys(&user).is_empty());
}

#[test]
fn person_bound_keys_reverts_a_person_plus_person_clash() {
    let entries = [("send", json!("ctrl+q")), ("new_session", json!("ctrl+q"))];
    let mut user = Map::new();
    for (id, value) in &entries {
        user.insert((*id).to_owned(), value.clone());
    }
    let (_, notices) = keyset(&entries);
    // docs/tui.md:1941-1954: each clash reverts with one notice; an invalid entry gives one notice and keeps its defaults.
    assert!(!notices.is_empty());
    // Both sides revert to their defaults, so neither owns the key.
    assert!(person_bound_keys(&user).is_empty());
}

#[test]
fn action_event_returns_the_checked_canonical_event_for_every_variant() {
    use super::{Keyset, Resolved};
    for (id, variant, expected) in [
        ("move_word", "left", Resolved::Edit(Edit::WordLeft)),
        ("move_word", "right", Resolved::Edit(Edit::WordRight)),
        ("line_start_end", "start", Resolved::Edit(Edit::LineStart)),
        ("line_start_end", "end", Resolved::Edit(Edit::LineEnd)),
        ("focus_next_prev", "next", Resolved::Key(Key::Down)),
        ("focus_next_prev", "prev", Resolved::Key(Key::Up)),
        ("search_next_prev", "next", Resolved::Key(Key::Down)),
        ("search_next_prev", "prev", Resolved::Key(Key::Up)),
        ("select_steering", "up", Resolved::Key(Key::AltUp)),
        ("select_steering", "down", Resolved::Key(Key::AltDown)),
        ("rail_row_n", "1", Resolved::Key(Key::AltDigit(1))),
        ("rail_row_n", "5", Resolved::Key(Key::AltDigit(5))),
        ("rail_row_n", "9", Resolved::Key(Key::AltDigit(9))),
    ] {
        assert_eq!(
            Keyset::action_event(id, Some(variant)),
            Some(expected),
            "{id}/{variant}"
        );
    }
    for (id, expected) in [
        ("send", Resolved::Key(Key::Enter)),
        ("line_break", Resolved::Edit(Edit::ShiftEnter)),
        ("go_home", Resolved::Action("go_home")),
        ("session_only", Resolved::Action("session_only")),
    ] {
        assert_eq!(Keyset::action_event(id, None), Some(expected), "{id}");
    }
}

#[test]
fn action_event_returns_none_for_an_invalid_id_variant_or_slot() {
    use super::Keyset;
    for (id, variant) in [
        ("no_such_action", None),
        ("clear_then_quit", None),
        ("move_word", None),
        ("move_word", Some("start")),
        ("rail_row_n", Some("0")),
        ("rail_row_n", Some("10")),
        ("send", Some("left")),
        ("new_session", Some("next")),
    ] {
        assert_eq!(Keyset::action_event(id, variant), None, "{id}/{variant:?}");
    }
}

#[test]
fn person_bound_any_covers_every_context() {
    let contexts = [
        Context::Input,
        Context::Steering,
        Context::Conversation,
        Context::Search,
        Context::Overlay,
        Context::Picker,
    ];
    let (keys, notices) = keyset(&[("send", json!("ctrl+q"))]);
    assert!(notices.is_empty(), "{notices:?}");
    let owned = stroke("ctrl+q");
    // The scoped check fires only where the action acts.
    assert!(keys.person_bound(&owned, Context::Input));
    assert!(!keys.person_bound(&owned, Context::Overlay));
    // The live check ignores contexts: owned everywhere, even over the
    // overlay and picker.
    assert!(keys.person_bound_any(&owned));
    // A default key is the person's nowhere.
    let plain = stroke("enter");
    assert!(!keys.person_bound_any(&plain));
    for context in contexts {
        assert!(!keys.person_bound(&plain, context), "{context:?}");
    }
}

#[test]
fn person_bound_any_ignores_default_equal_unbound_and_invalid_entries() {
    let entries = [
        ("send", json!("enter")),
        ("new_session", json!([])),
        ("toggle_ledgers", json!("bogus")),
    ];
    let (keys, _) = keyset(&entries);
    for name in ["enter", "ctrl+n", "ctrl+o", "ctrl+q"] {
        assert!(!keys.person_bound_any(&stroke(name)), "{name}");
    }
}

#[test]
fn person_bound_any_reverts_clashes_and_follows_live_edits() {
    let (clashed, _) = keyset(&[("send", json!("ctrl+q")), ("new_session", json!("ctrl+q"))]);
    assert!(!clashed.person_bound_any(&stroke("ctrl+q")));
    // A live `/keys` change reloads the keyset: the freed key leaves the
    // person set at once.
    let (freed, _) = keyset(&[("send", json!("ctrl+q"))]);
    assert!(freed.person_bound_any(&stroke("ctrl+q")));
}
