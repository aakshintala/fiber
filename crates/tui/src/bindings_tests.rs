//! Tests for the bindings table.

use super::BINDINGS;

/// The cells of each row of `docs/tui.md`'s "Bindings" table, header and
/// rule left out, with the code marks dropped.
fn doc_rows() -> Vec<Vec<String>> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/tui.md");
    let doc = std::fs::read_to_string(path).unwrap_or_else(|err| panic!("{path}: {err}"));
    let section = doc
        .split("\n### Bindings\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no Bindings section in {path}"));
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
