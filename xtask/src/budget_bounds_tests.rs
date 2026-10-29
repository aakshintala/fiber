use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::rules::RustFile;

fn file(path: &str, rel: &str, source: &str) -> RustFile {
    RustFile {
        krate: "x".to_owned(),
        path: path.to_owned(),
        rel: rel.to_owned(),
        source: source.to_owned(),
    }
}

/// The failures for one file whose changed lines are `changed`.
fn check(path: &str, rel: &str, source: &str, changed: &[usize]) -> Vec<String> {
    let files = [file(path, rel, source)];
    let mut lines = BTreeSet::new();
    lines.extend(changed.iter().copied());
    let mut map = BTreeMap::new();
    map.insert(path.to_owned(), lines);
    check_files(&files, &map)
}

#[test]
fn the_bounds_match_the_shared_consts() {
    assert_eq!(WAITS, fakes::deadline::WAITS);
    assert_eq!(BUDGET, fakes::deadline::BUDGET);
    assert_eq!(CLEANUP, fakes::deadline::CLEANUP);
}

#[test]
fn a_raised_guard_const_fails() {
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const WAITS: Duration = Duration::from_secs(91);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("crates/support/src/deadline.rs:1: const WAITS is 91 s, above"),
        "{failures:?}"
    );
}

#[test]
fn each_named_guard_const_is_pinned_at_its_edge() {
    for (name, at, over) in [
        ("WAITS", "90", "91"),
        ("BUDGET", "110", "111"),
        ("CLEANUP", "100", "101"),
    ] {
        let source = format!("pub const {name}: Duration = Duration::from_secs({at});");
        assert!(
            check("crates/x/src/a.rs", "src/a.rs", &source, &[1]).is_empty(),
            "{name} at {at} passes"
        );
        let source = format!("pub const {name}: Duration = Duration::from_secs({over});");
        assert_eq!(
            check("crates/x/src/a.rs", "src/a.rs", &source, &[1]).len(),
            1,
            "{name} at {over} fails"
        );
    }
}

#[test]
fn a_lowered_guard_const_passes() {
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            "pub const WAITS: Duration = Duration::from_secs(89);",
            &[1],
        )
        .is_empty()
    );
}

#[test]
fn an_unruled_sixty_second_after_in_a_test_fails() {
    for (after, case) in [
        ("Duration::from_secs(60)", "an unruled 60 s after"),
        ("Duration::from_secs(61)", "an unruled 61 s after"),
        (
            "Duration::from_millis(60_000)",
            "an unruled 60_000 ms after",
        ),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{case}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is 60 s or more"),
            "{case}: {failures:?}"
        );
    }
}

#[test]
fn a_fifty_nine_second_after_in_a_test_passes() {
    let source = "fn a() { Deadline::after(Duration::from_secs(59)); }";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_long_after_outside_a_test_file_passes() {
    let source = "fn a() { Deadline::after(Duration::from_secs(60)); }";
    assert!(check("crates/x/src/a.rs", "src/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_ruled_line_passes() {
    let source = "pub const WAITS: Duration = Duration::from_secs(90); // ruling: #3662";
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            source,
            &[1]
        )
        .is_empty()
    );
    let source = "fn a() { Deadline::after(Duration::from_secs(60)); // ruling: #3662\n}";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn an_unchanged_long_after_outside_the_diff_passes() {
    let source = "fn a() {\n    Deadline::after(Duration::from_secs(60));\n}";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_const_bound_passes_outside_the_diff() {
    let source = "pub const WAITS: Duration = Duration::from_secs(91);";
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            source,
            &[]
        )
        .is_empty()
    );
}

#[test]
fn a_shared_bound_name_passes() {
    let source = "fn a() { Deadline::after(fakes::deadline::WAIT); }";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}
