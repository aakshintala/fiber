//! Tests for a tool group's drawing.

#[test]
fn an_empty_group_has_no_ledger() {
    assert!(!super::Group::default().has_ledger());
}
