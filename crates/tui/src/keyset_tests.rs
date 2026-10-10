//! Tests for the keyset: resolution, validation notices and clash passes.

use serde_json::{Map, Value, json};

use super::{Context, Contexts, Keyset, load, resolved};
use crate::bindings::BINDINGS;
use crate::keys::{Edit, Key, default_event};
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

/// Every context a binding acts in.
fn acting(binding: &crate::bindings::Binding) -> Vec<Context> {
    [
        Context::Input,
        Context::Steering,
        Context::Conversation,
        Context::Search,
        Context::Overlay,
        Context::Picker,
    ]
    .into_iter()
    .filter(|context| binding.contexts.contains(*context))
    .collect()
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
fn without_person_entries_every_default_key_gives_its_default_event() {
    let (keys, notices) = keyset(&[]);
    assert!(notices.is_empty());
    for binding in BINDINGS {
        assert!(
            !binding.defaults.is_empty(),
            "{} has no defaults",
            binding.id
        );
        for name in binding.defaults {
            let pressed = stroke(name);
            for context in acting(binding) {
                let got = keys.resolve(&pressed, context);
                let want = if matches!(binding.events, [super::Canon::Action]) {
                    super::Resolved::Action(binding.id)
                } else {
                    resolved(default_event(&pressed))
                };
                assert_eq!(got, want, "{} {name} in {context:?}", binding.id);
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
    assert_eq!(at(entries, "ctrl+s", Context::Picker), super::Resolved::Nothing);
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
