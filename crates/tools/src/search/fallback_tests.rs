//! Tests beside [`super::exec`]: what a failed replacement reports.

use std::ffi::OsString;

use super::exec;

#[test]
fn a_missing_program_reports_and_returns_two() {
    let mut stderr = Vec::new();
    let code = exec(
        "fiber-search-no-such-tool",
        &[OsString::from("x")],
        &mut stderr,
    );
    assert_eq!(code, 2);
    let message = String::from_utf8(stderr).unwrap();
    assert!(
        message.starts_with("fiber-search-no-such-tool: cannot run the system tool: "),
        "{message}"
    );
    assert!(message.contains("No such file"), "{message}");
}
