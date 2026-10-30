use super::*;
use std::collections::BTreeMap;

// Rule sources: `docs/testing/screens.md` ("Screens") requires each kept
// read to name its signal in a `// settled:` comment and defines the
// ratchet's line rule (plain `.screen()` text, full-line `//` exclusion,
// literal marker on the call or previous line) as a conservative upper
// bound; every count asserted below cites that section, and the
// `screen_ratchet` module doc enforces the same rule.

fn file(path: &str, source: &str) -> RustFile {
    RustFile {
        krate: "main".to_owned(),
        path: path.to_owned(),
        rel: path.strip_prefix("crates/main/").unwrap_or(path).to_owned(),
        source: source.to_owned(),
    }
}

fn maxima(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|(path, maximum)| ((*path).to_owned(), *maximum))
        .collect()
}

#[test]
fn the_line_rule_counts_plain_calls() {
    let cases: &[(&str, usize)] = &[
        ("    let grid = run.screen();\n", 1),
        ("    let a = run.screen();\n    let b = run.screen();\n", 2),
    ];
    for (source, expected) in cases {
        assert_eq!(count_unmarked(source), *expected, "source: {source:?}");
    }
}

#[test]
fn a_field_access_without_parens_does_not_count() {
    let cases: &[(&str, usize)] = &[
        ("    run.screen.wait_for(\"Key: \");\n", 0),
        ("    let shown = &run.screen.seen;\n", 0),
    ];
    for (source, expected) in cases {
        assert_eq!(count_unmarked(source), *expected, "source: {source:?}");
    }
}

#[test]
fn a_full_line_comment_call_does_not_count() {
    let cases: &[(&str, usize)] = &[
        ("    // let grid = run.screen();\n", 0),
        ("// run.screen();\n", 0),
    ];
    for (source, expected) in cases {
        assert_eq!(count_unmarked(source), *expected, "source: {source:?}");
    }
}

#[test]
fn a_call_inside_a_string_counts() {
    assert_eq!(count_unmarked("    let marker = \"run.screen()\";\n"), 1);
}

#[test]
fn a_call_inside_a_block_comment_counts() {
    let cases: &[(&str, usize)] = &[
        ("    /* run.screen() */\n", 1),
        ("    /* open\n    run.screen();\n    close */\n", 1),
    ];
    for (source, expected) in cases {
        assert_eq!(count_unmarked(source), *expected, "source: {source:?}");
    }
}

#[test]
fn a_settled_marker_on_the_call_line_exempts() {
    assert_eq!(
        count_unmarked("    let grid = run.screen(); // settled: wait checked tint\n"),
        0
    );
}

#[test]
fn a_settled_marker_on_the_previous_line_exempts() {
    assert_eq!(
        count_unmarked("    // settled: wait checked tint\n    let grid = run.screen();\n"),
        0
    );
}

#[test]
fn marker_text_inside_a_string_still_exempts() {
    assert_eq!(
        count_unmarked("    let note = \"// settled: done\";\n    let grid = run.screen();\n"),
        0
    );
    assert_eq!(
        count_unmarked("    let grid = run.screen(); let note = \"// settled: x\";\n"),
        0
    );
}

#[test]
fn an_unrelated_previous_line_does_not_exempt() {
    assert_eq!(
        count_unmarked("    let other = 1;\n    let grid = run.screen();\n"),
        1
    );
}

#[test]
fn a_marker_two_lines_above_does_not_exempt() {
    assert_eq!(
        count_unmarked(
            "    // settled: wait checked tint\n    let other = 1;\n    let grid = run.screen();\n"
        ),
        1
    );
}

#[test]
fn support_files_are_outside_the_rule() {
    let files = [file(
        "crates/main/tests/support/pty.rs",
        "    let grid = self.screen();\n    let other = run.screen();\n",
    )];
    assert!(check(&files, &maxima(&[])).is_empty());
}

#[test]
fn per_file_maxima_are_independent() {
    let files = [
        file("crates/main/tests/a.rs", "    let a = run.screen();\n"),
        file(
            "crates/main/tests/b.rs",
            "    let b = run.screen();\n    let c = run.screen();\n",
        ),
    ];
    assert_eq!(
        check(&files, &maxima(&[("crates/main/tests/a.rs", 1)])),
        ["crates/main/tests/b.rs holds 2 unmarked .screen() call lines (maximum 0)"]
    );
}

#[test]
fn an_absent_file_has_maximum_zero() {
    let files = [file(
        "crates/main/tests/fresh.rs",
        "    let grid = run.screen();\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[])),
        ["crates/main/tests/fresh.rs holds 1 unmarked .screen() call lines (maximum 0)"]
    );
}

#[test]
fn a_file_at_its_maximum_passes() {
    let files = [file(
        "crates/main/tests/a.rs",
        "    let grid = run.screen();\n",
    )];
    assert!(check(&files, &maxima(&[("crates/main/tests/a.rs", 1)])).is_empty());
}

#[test]
fn a_failure_names_the_file_the_count_and_the_maximum() {
    let files = [file(
        "crates/main/tests/a.rs",
        "    let a = run.screen();\n    let b = run.screen();\n",
    )];
    assert_eq!(
        check(&files, &maxima(&[("crates/main/tests/a.rs", 1)])),
        ["crates/main/tests/a.rs holds 2 unmarked .screen() call lines (maximum 1)"]
    );
}

#[test]
fn the_maxima_table_parses_paths_and_counts() {
    let cases: &[(&str, &[(&str, usize)])] = &[
        (
            "crates/main/tests/a.rs 1\ncrates/main/tests/b.rs 7\n",
            &[("crates/main/tests/a.rs", 1), ("crates/main/tests/b.rs", 7)],
        ),
        ("", &[]),
        ("\n   \n# a comment\n", &[]),
        (
            "# leading comment\ncrates/main/tests/a.rs 1\n",
            &[("crates/main/tests/a.rs", 1)],
        ),
        (
            "  crates/main/tests/a.rs   1  \n",
            &[("crates/main/tests/a.rs", 1)],
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse_maxima(input),
            Ok(expected
                .iter()
                .map(|(path, maximum)| ((*path).to_owned(), *maximum))
                .collect()),
            "input: {input:?}"
        );
    }
}

#[test]
fn the_maxima_table_rejects_malformed_lines_with_exact_errors() {
    let cases: &[(&str, &str)] = &[
        (
            "crates/main/tests/a.rs\n",
            "line 1: expected `<path> <maximum>`, got `crates/main/tests/a.rs`",
        ),
        (
            "crates/main/tests/a.rs 1 extra\n",
            "line 1: expected `<path> <maximum>`, got `crates/main/tests/a.rs 1 extra`",
        ),
        (
            "crates/main/tests/a.rs many\n",
            "line 1: maximum for crates/main/tests/a.rs: invalid digit found in string",
        ),
        (
            "crates/main/tests/a.rs 1\ncrates/main/tests/a.rs 2\n",
            "line 2: duplicate maximum for crates/main/tests/a.rs",
        ),
        (
            "crates/main/tests/a.rs 1\nbadline\n",
            "line 2: expected `<path> <maximum>`, got `badline`",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse_maxima(input),
            Err((*expected).to_owned()),
            "input: {input:?}"
        );
    }
}
