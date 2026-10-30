use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::rules::RustFile;

/// Compiled-in `docs/testing.md` for the selector's compiled-in list
/// (`docs/ci.md`, "Selection"): kept as `include_str!` so the check sees it.
#[allow(dead_code, reason = "compiled-in list needs the include, not a read")]
const DOCS_TESTING: &str = include_str!("../../docs/testing.md");

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
    let floor = std::time::Duration::from_secs(60);
    assert_eq!(known_ms("WAIT"), Some(fakes::deadline::WAIT >= floor));
    assert_eq!(known_ms("WAITS"), Some(fakes::deadline::WAITS >= floor));
    assert_eq!(known_ms("BUDGET"), Some(fakes::deadline::BUDGET >= floor));
    assert_eq!(known_ms("CLEANUP"), Some(fakes::deadline::CLEANUP >= floor));
    assert_eq!(
        known_ms("MUST_SUCCEED_WITHIN"),
        Some(fakes::MUST_SUCCEED_WITHIN >= floor)
    );
}

#[test]
fn malformed_rust_fails_closed() {
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", "fn a( {", &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("does not tokenise as Rust"),
        "{failures:?}"
    );
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
fn a_non_duration_budget_const_is_not_a_timeout_bound() {
    assert!(
        check(
            "crates/main/src/tool_budget_tests.rs",
            "src/tool_budget_tests.rs",
            "const BUDGET: usize = 15815;",
            &[1],
        )
        .is_empty()
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
fn suffixed_decimal_bounds_are_read() {
    let failures = check(
        "crates/x/tests/a.rs",
        "tests/a.rs",
        "fn a() { Deadline::after(Duration::from_secs(60u64)); }",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
    assert!(
        check(
            "crates/x/tests/a.rs",
            "tests/a.rs",
            "fn a() { Deadline::after(Duration::from_secs(59u64)); }",
            &[1],
        )
        .is_empty()
    );
}

#[test]
fn a_shared_bound_name_passes() {
    for after in ["fakes::deadline::WAIT", "::fakes::deadline::WAIT"] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        assert!(
            check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]).is_empty(),
            "{after} passes"
        );
    }
}

#[test]
fn a_short_named_bound_passes() {
    for after in [
        "MUST_SUCCEED_WITHIN",
        "fakes::MUST_SUCCEED_WITHIN",
        "Duration::ZERO",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        assert!(
            check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]).is_empty(),
            "{after} passes"
        );
    }
}

#[test]
fn a_long_named_bound_fails() {
    for (after, name) in [
        ("fakes::deadline::WAITS", "WAITS"),
        ("fakes::deadline::BUDGET", "BUDGET"),
        ("fakes::deadline::CLEANUP", "CLEANUP"),
        ("Duration::MAX", "MAX"),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is 60 s or more"),
            "{name}: {failures:?}"
        );
    }
}

#[test]
fn an_unshown_after_fails_closed() {
    for (after, case) in [
        ("SPAN", "a bare name"),
        ("Duration::from_secs(30 + 30)", "a computed literal"),
        ("Duration::from_secs(SIXTY)", "a named literal"),
        ("WAIT * 100", "a compound symbolic bound"),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{case}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is not shown below 60 s"),
            "{case}: {failures:?}"
        );
    }
}

#[test]
fn radix_integer_bounds_are_read() {
    for after in [
        "Duration::from_secs(0x3c)",
        "Duration::from_secs(0o74)",
        "Duration::from_secs(0b111100)",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].ends_with("is 60 s or more"),
            "{after}: {failures:?}"
        );
    }
}

#[test]
fn imported_duration_aliases_are_read() {
    for (after, fails) in [
        ("Span::from_secs(30)", false),
        ("Span::from_secs(60)", true),
    ] {
        let source =
            format!("use std::time::Duration as Span;\nfn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[2]);
        assert_eq!(failures.len(), usize::from(fails), "{after}: {failures:?}");
    }
}

#[test]
fn unsupported_duration_paths_fail_closed() {
    for after in [
        "Span::from_secs(30)",
        "Duration.:from_secs(30)",
        "Duration:.from_secs(30)",
        "Duration::from_secs[30]",
        "foo.bar.Duration::from_secs(30)",
        ":x std::time::Duration::from_secs(30)",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].ends_with("is not shown below 60 s"),
            "{after}: {failures:?}"
        );
    }
}

#[test]
fn absolute_duration_paths_are_read() {
    let source = "fn a() { Deadline::after(::std::time::Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
}

#[test]
fn other_after_methods_are_not_deadline_bounds() {
    for source in [
        "fn a() { Other::after(Duration::from_secs(60)); }",
        "fn a() { Deadline.:after(Duration::from_secs(60)); }",
    ] {
        assert!(
            check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn changed_diff_parsing_keeps_added_rust_lines_and_skips_deletions() {
    // In-process replacement for the removed nested-probe: parses a
    // synthetic `git diff -U0` without spawning a child or touching tracked
    // files. Deleting the `/dev/null` guard would include the deletion hunk;
    // deleting the `.rs` guard would include the markdown hunk.
    let diff = "diff --git a/xtask/src/budget_bounds_tests.rs b/xtask/src/budget_bounds_tests.rs\n+++ b/xtask/src/budget_bounds_tests.rs\n@@ -0,0 +5,1 @@\n+fn budget_bounds_probe() { Deadline::after(Duration::from_secs(60)); }\n\
        diff --git a/docs/testing.md b/docs/testing.md\n+++ b/docs/testing.md\n@@ -1,1 +1,1 @@\n-old\n+new\n\
        diff --git a/old.rs b/old.rs\n+++ b//dev/null\n@@ -1,1 +1,1 @@\n-old\n";
    let changed = parse_changed_diff(diff);
    assert_eq!(
        changed.get("xtask/src/budget_bounds_tests.rs"),
        Some(&BTreeSet::from([5])),
        "{changed:?}"
    );
    assert!(!changed.contains_key("docs/testing.md"), "{changed:?}");
    assert!(!changed.contains_key("/dev/null"), "{changed:?}");
    // The parsed added line is a long `after` on a changed line, so the
    // check rejects it in-process without git.
    let files = [file(
        "xtask/src/budget_bounds_tests.rs",
        "src/budget_bounds_tests.rs",
        "fn budget_bounds_probe() { Deadline::after(Duration::from_secs(60)); }",
    )];
    let mut map = BTreeMap::new();
    map.insert(
        "xtask/src/budget_bounds_tests.rs".to_owned(),
        BTreeSet::from([1]),
    );
    let failures = check_files(&files, &map);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("Deadline::after("), "{failures:?}");
}

#[test]
fn untracked_files_count_whole_and_skip_non_rust() {
    // In-process coverage for the untracked branch: no git, no filesystem.
    // Deleting the `.rs` guard would include the text file; replacing
    // `max(1)` handling would empty the zero-line file.
    let mut changed = BTreeMap::new();
    add_untracked_file(&mut changed, "xtask/src/new_test.rs".to_owned(), 3);
    add_untracked_file(&mut changed, "notes.txt".to_owned(), 3);
    add_untracked_file(&mut changed, "xtask/src/empty.rs".to_owned(), 0);
    assert_eq!(
        changed.get("xtask/src/new_test.rs"),
        Some(&BTreeSet::from([1, 2, 3])),
        "{changed:?}"
    );
    assert!(!changed.contains_key("notes.txt"), "{changed:?}");
    assert_eq!(
        changed.get("xtask/src/empty.rs"),
        Some(&BTreeSet::from([1])),
        "{changed:?}"
    );
}

#[test]
fn invalid_bases_fail_instead_of_reporting_empty() {
    // The git-backed wrappers fail closed on an unknown base: a mutant that
    // returns an empty map or list would report no failure and pass silently.
    // No tracked files are touched and no test binary is spawned.
    assert!(changed_lines("definitely-not-a-fiber-base").is_err());
    assert!(budget_bounds("definitely-not-a-fiber-base").is_err());
}

#[test]
fn show_formats_seconds_and_milliseconds() {
    // Direct pin for `show`: the numeric formatter's exact output.
    // Returning `String::new()` or `"xyzzy"` fails each assertion below.
    assert_eq!(show(60_000), "60 s");
    assert_eq!(show(101_000), "101 s");
    assert_eq!(show(90_001), "90001 ms");
    assert_eq!(show(0), "0 s");
}

#[test]
fn an_unshown_guard_const_fails_closed() {
    let source = "pub const WAITS: Duration = Duration::from_secs(SIXTY);";
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        source,
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown at or below 90 s"),
        "{failures:?}"
    );
}

#[test]
fn a_bracketed_after_call_is_not_a_deadline_bound() {
    // `site_at` only fires on a parenthesised call: a bracketed `after`
    // is not a call, so a long bracketed bound passes while the
    // parenthesised twin fails. Flipping the delimiter check to `==`
    // inverts both, failing each assertion.
    let paren = "fn a() { Deadline::after(Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", paren, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
    let bracket = "fn a() { Deadline::after[Duration::from_secs(60)]; }";
    assert!(
        check("crates/x/tests/a.rs", "tests/a.rs", bracket, &[1]).is_empty(),
        "{bracket}"
    );
}

#[test]
fn a_raised_budget_const_fails() {
    // Deleting the `BUDGET` match arm silences this bound: without the
    // arm the raised `BUDGET` const reports nothing.
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const BUDGET: Duration = Duration::from_secs(111);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0]
            .starts_with("crates/support/src/deadline.rs:1: const BUDGET is 111 s, above 110 s"),
        "{failures:?}"
    );
}

#[test]
fn exact_budget_cap_passes_and_one_over_fails() {
    // Pins the `<=` guard in `const_site`: at-cap passes, one over fails.
    // Flipping `<=` to `>` inverts both.
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            "pub const BUDGET: Duration = Duration::from_secs(110);",
            &[1],
        )
        .is_empty()
    );
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const BUDGET: Duration = Duration::from_secs(111);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
}

#[test]
fn a_two_arg_after_is_not_a_bound() {
    // `split_top_level` must see exactly one argument: a two-argument
    // `after` passes while the single-argument long twin fails. Replacing
    // the split with `vec![]` empties both, silencing the single-arg failure.
    let single = "fn a() { Deadline::after(Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", single, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    let two = "fn a() { Deadline::after(Duration::from_secs(60), 1); }";
    assert!(
        check("crates/x/tests/a.rs", "tests/a.rs", two, &[1]).is_empty(),
        "{two}"
    );
}

#[test]
fn a_bracketed_duration_fails_closed() {
    // The `||` chain in `duration` fails closed on a non-parenthesised
    // duration: a bracketed `from_secs` reports "not shown below 60 s".
    // Tightening the last `||` to `&&` parses the bracket as 30 s, silencing it.
    let source = "fn a() { Deadline::after(Duration::from_secs[30]); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown below 60 s"),
        "{failures:?}"
    );
}

#[test]
fn a_single_colon_root_fails_closed() {
    // The `&&` guard in `duration` requires a doubled colon before
    // stripping a leading `::`: a single-colon root stays unreadable and
    // fails closed. Loosening it to `||` strips the root and reads 30 s.
    let source = "fn a() { Deadline::after(:x std::time::Duration::from_secs(30)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown below 60 s"),
        "{failures:?}"
    );
}

#[test]
fn a_hex_sixty_second_after_is_read_as_sixty() {
    // Deleting the `0x` arm misreads `0x3c` (60) as unshown: the message
    // must say "is 60 s or more", not "not shown below 60 s".
    let source = "fn a() { Deadline::after(Duration::from_secs(0x3c)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
}

#[test]
fn deadline_and_other_after_are_distinguished_in_one_file() {
    // `deadline_path` must accept `Deadline::after` and reject
    // `Other::after`: one file holding both reports exactly the Deadline
    // site. Loosening the middle `&&` to `||` also accepts `Other::after`,
    // reporting two sites instead of one.
    let source = "fn a() { Deadline::after(Duration::from_secs(60)); Other::after(Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("Deadline::after("), "{failures:?}");
}

#[test]
fn a_half_colon_path_with_a_short_tail_fails_closed() {
    // Pins the `||` in `simple_path_ident`: both colons of each `::`
    // separator are required. `A B:WAIT` tokenises to
    // `[A, B, ':', WAIT]`: the first expected colon is an ident, the
    // second is a colon, so the compound sequence must fail closed
    // ("not shown below 60 s"). With `||` loosened to `&&`, the single
    // present colon suffices and the mutant reads the trailing `WAIT`
    // as a short known bound, reporting no failure.
    let source = "fn a() { Deadline::after(A B:WAIT); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown below 60 s"),
        "{failures:?}"
    );
}

#[test]
fn a_colon_then_ident_path_with_a_short_tail_fails_closed() {
    // Pins the `i + 1` second-colon index in `simple_path_ident`:
    // `A:B WAIT` tokenises to `[A, ':', B, WAIT]`: the first expected
    // colon is present but the second is an ident, so the compound
    // sequence must fail closed. With `i + 1` changed to `i * 1`
    // (that is, `i`), the check reads the first colon twice, reaches
    // the trailing `WAIT`, and treats it as a short known bound.
    let source = "fn a() { Deadline::after(A:B WAIT); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown below 60 s"),
        "{failures:?}"
    );
}

#[test]
fn a_shifted_deadline_after_still_counts_as_a_deadline_bound() {
    // Pins the `i - 2` predecessor index in `deadline_path`: the middle
    // colon is two tokens before `after`, not `i / 2`. `Deadline` here
    // sits at index 5 (`let _ = 1;` precedes it), so `after` is at
    // index 8: `i - 2` is 6 (a colon) while `i / 2` is 4 (`;`, not a
    // colon). The long `after` must still fail; with `-` changed to `/`
    // the mutant misses it and reports no failure.
    let source = "fn a() { let _ = 1; Deadline::after(Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
}

#[test]
fn failure_messages_name_the_numeric_bounds() {
    // `show` renders both sides of a raised-const message: the raised
    // value and its cap. Emptying `show` blanks both, failing each
    // assertion below.
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const CLEANUP: Duration = Duration::from_secs(101);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("101 s") && failures[0].contains("100 s"),
        "{failures:?}"
    );
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const WAITS: Duration = Duration::from_millis(90001);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("90001 ms") && failures[0].contains("90 s"),
        "{failures:?}"
    );
}
