//! Deliberately failing probe for #2225: shows that a backtrace in CI still
//! names file and line under `debug = "line-tables-only"`. Removed in the
//! next commit.

#[inline(never)]
fn capture() -> String {
    std::backtrace::Backtrace::force_capture().to_string()
}

#[test]
fn a_backtrace_names_file_and_line() {
    assert_eq!(capture(), "");
}
