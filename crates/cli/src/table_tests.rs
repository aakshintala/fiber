//! Tests for the text table: padding to the widest cell, two spaces
//! between columns, trailing spaces trimmed.

use super::pad;

fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|cell| (*cell).to_owned()).collect()
}

#[test]
fn columns_pad_to_their_widest_cell_two_spaces_apart_with_no_trailing_space() {
    let rows = [row(&["id", "state", "name"]), row(&["s_1", "é", ""])];
    assert_eq!(pad(&rows), ["id   state  name", "s_1  é"]);
}

#[test]
fn a_wider_later_cell_widens_its_whole_column() {
    let rows = [row(&["a", "b"]), row(&["longer", "c"])];
    assert_eq!(pad(&rows), ["a       b", "longer  c"]);
}

#[test]
fn no_rows_is_no_lines() {
    assert!(pad(&[]).is_empty());
}
