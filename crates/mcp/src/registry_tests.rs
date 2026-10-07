//! Tests for process-wide MCP server registration and safe signalling.

#[test]
fn a_pid_of_one_or_less_is_refused() {
    assert!(super::refused(0));
    assert!(super::refused(1));
    assert!(!super::refused(2));
}
