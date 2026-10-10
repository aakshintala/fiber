//! Tests for the bindings table.

use super::BINDINGS;
use crate::stroke::Stroke;

/// The ids of actions that edit the draft in the input box, and search: the rule in
/// `docs/tui.md`, "Rules" exempts them from having a mouse target or a
/// slash command.
const EDITING: [&str; 10] = [
    "send",
    "line_break",
    "recall_prompt",
    "search_prompts",
    "move_word",
    "delete_word",
    "line_start_end",
    "paste_image",
    "search",
    "search_next_prev",
];

/// Substrings marking an other path as a slash command or a mouse path:
/// the rule's "a mouse target or a slash command" in lowercase.
const PATHS: [&str; 5] = ["/", "click", "drag", "select", "mouse target"];

/// Actions with no other path, by the rule's exception: choosing in the
/// model picker for this session only is a key alone
/// (`docs/tui.md`, "Keys" › "Rules").
const NO_OTHER_PATH: [&str; 1] = ["session_only"];

/// The cells of each row of `docs/tui.md`'s "Bindings" table, header and
/// rule left out, with the code marks dropped.
fn doc_rows() -> Vec<Vec<String>> {
    let doc = include_str!("../../../docs/tui.md");
    let section = doc
        .split("\n### Bindings\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no Bindings section in docs/tui.md"));
    let table: Vec<&str> = section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert_eq!(
        table.first().copied(),
        Some("| Action | Id | Key | Other paths |")
    );
    table
        .iter()
        .skip(2)
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().replace('`', ""))
                .collect()
        })
        .collect()
}

#[test]
fn the_table_matches_the_doc_row_for_row() {
    let doc = doc_rows();
    let ours: Vec<Vec<String>> = BINDINGS
        .iter()
        .map(|binding| {
            [
                binding.description,
                binding.id,
                binding.keys,
                binding.other_paths,
            ]
            .map(str::to_owned)
            .to_vec()
        })
        .collect();
    assert_eq!(ours, doc);
}

#[test]
fn areas_run_in_order_from_their_first_ids() {
    let mut areas: Vec<(&str, &str)> = Vec::new();
    for binding in BINDINGS {
        if areas.last().map(|(area, _)| *area) != Some(binding.area) {
            areas.push((binding.area, binding.id));
        }
    }
    assert_eq!(
        areas,
        [
            ("Sessions", "send"),
            ("The input box", "recall_prompt"),
            ("The conversation", "toggle_ledgers"),
            ("Steering", "select_steering"),
            ("Requests, models and help", "next_request"),
        ]
    );
    assert_eq!(BINDINGS.last().map(|binding| binding.id), Some("key_map"));
}

#[test]
fn every_action_has_a_key_and_a_mouse_target_or_a_slash_command() {
    for binding in BINDINGS {
        assert!(!binding.keys.is_empty(), "{} has no key", binding.id);
        if EDITING.contains(&binding.id) || NO_OTHER_PATH.contains(&binding.id) {
            continue;
        }
        assert!(
            PATHS.iter().any(|path| binding.other_paths.contains(path)),
            "{} has no mouse target or slash command",
            binding.id
        );
    }
}

#[test]
fn the_editing_exemptions_are_bindings() {
    for id in EDITING {
        assert!(
            BINDINGS.iter().any(|binding| binding.id == id),
            "{id} is not a binding"
        );
    }
}

#[test]
fn every_default_key_parses() {
    for binding in BINDINGS {
        for name in binding.defaults {
            assert!(
                Stroke::parse(name).is_ok(),
                "{}: {name} does not parse",
                binding.id
            );
        }
    }
}

#[test]
fn every_row_lists_whole_variant_groups() {
    for binding in BINDINGS {
        assert!(
            !binding.events.is_empty(),
            "{} has no canonical event",
            binding.id
        );
        assert_eq!(
            binding.defaults.len() % binding.events.len(),
            0,
            "{}: {} defaults for {} variants",
            binding.id,
            binding.defaults.len(),
            binding.events.len()
        );
    }
}

/// Default keys whose label the doc's row does not spell out: the legacy
/// ⌥← ⌥→ forms, the Cmd+F alias the search rows write as `Cmd+F`, and
/// the middle rail rows the cell covers with a range.
const UNLISTED: [&str; 10] = [
    "alt+b", "alt+f", "super+f", "alt+2", "alt+3", "alt+4", "alt+5", "alt+6", "alt+7", "alt+8",
];

#[test]
fn every_listed_default_key_shows_in_its_row() {
    for binding in BINDINGS {
        for name in binding.defaults {
            if UNLISTED.contains(name) {
                continue;
            }
            let label = Stroke::parse(name)
                .unwrap_or_else(|err| panic!("{}: {name}: {err}", binding.id))
                .label();
            assert!(
                binding.keys.contains(&label) || binding.other_paths.contains(&label),
                "{}: {label} is neither in {:?} nor {:?}",
                binding.id,
                binding.keys,
                binding.other_paths
            );
        }
    }
}

#[test]
fn session_only_has_no_other_path() {
    let binding = BINDINGS
        .iter()
        .find(|binding| binding.id == "session_only")
        .expect("session_only is a binding");
    assert_eq!(binding.other_paths, "");
    assert!(NO_OTHER_PATH.contains(&binding.id));
}
