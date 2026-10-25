use super::*;
use crate::rules::RustFile;
use crate::select::tests::members;

fn file(krate: &str, path: &str, source: &str) -> RustFile {
    RustFile {
        krate: krate.to_owned(),
        path: path.to_owned(),
        rel: path
            .strip_prefix(&format!("crates/{krate}/"))
            .unwrap_or(path)
            .to_owned(),
        source: source.to_owned(),
    }
}

/// The tests the diff selects, failing with the error instead of passing.
fn selected(diff: &str, sources: &[RustFile]) -> Vec<TestRef> {
    let result = changed_tests(diff, &members(), sources);
    assert!(result.is_ok(), "{result:?}");
    result.unwrap_or_default()
}

fn refer(package: &str, binary: Option<&str>, unit: bool, name: &str) -> TestRef {
    TestRef {
        package: package.to_owned(),
        binary: binary.map(str::to_owned),
        name: name.to_owned(),
        unit,
    }
}

fn diff_file(path: &str, hunks: &str) -> String {
    format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{hunks}")
}

/// A source whose second test spans lines 10 to 20: its `///` line is 10,
/// its `#[test]` line 11, its body closes on line 20.
const SPAN: &str = "//! module doc
#![allow(dead_code)]

// a plain comment
#[test]
fn first() {}

// a plain comment above the stack
// another plain comment
/// doc for second
#[test]
fn second() {
    let x = 1;
    let x = 2;
    let x = 3;
    let x = 4;
    let x = 5;
    let x = 6;
    let x = 7;
}

fn helper() {}
";

fn span_sources() -> Vec<RustFile> {
    vec![file("log", "crates/log/src/span_tests.rs", SPAN)]
}

fn span_diff(hunk: &str) -> String {
    diff_file("crates/log/src/span_tests.rs", hunk)
}

#[test]
fn a_hunk_on_the_line_before_the_span_selects_nothing() {
    let diff = span_diff("@@ -9,1 +9,1 @@\n-// old\n+// new\n");
    assert_eq!(selected(&diff, &span_sources()), vec![]);
}

#[test]
fn a_hunk_on_the_first_attribute_line_selects() {
    let diff = span_diff("@@ -10,1 +10,1 @@\n-/// old\n+/// new\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_hunk_on_the_closing_brace_line_selects() {
    let diff = span_diff("@@ -20,1 +20,1 @@\n-}\n+}\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_hunk_on_the_line_after_the_span_selects_nothing() {
    let diff = span_diff("@@ -21,1 +21,1 @@\n-\n+// new\n");
    assert_eq!(selected(&diff, &span_sources()), vec![]);
}

#[test]
fn a_hunk_ending_at_the_span_start_selects() {
    let diff = span_diff("@@ -5,6 +5,6 @@\n-a\n+b\n-a\n+b\n-a\n+b\n-a\n+b\n-a\n+b\n-a\n+b\n");
    // Selection order is not a documented contract, so compare as a set.
    let got = selected(&diff, &span_sources());
    assert_eq!(got.len(), 2);
    assert!(got.contains(&refer("log", None, true, "span::tests::first")));
    assert!(got.contains(&refer("log", None, true, "span::tests::second")));
}

#[test]
fn a_hunk_starting_at_the_span_end_selects() {
    let diff = span_diff("@@ -20,1 +20,6 @@\n-}\n+}\n+a\n+b\n+c\n+d\n+e\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_hunk_header_may_carry_a_section_heading() {
    let diff = span_diff("@@ -10,1 +10,1 @@ fn second\n-/// old\n+/// new\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_zero_count_gap_before_the_span_start_selects() {
    let diff = span_diff("@@ -9,1 +9,0 @@\n-gone\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_zero_count_gap_outside_the_span_selects_nothing() {
    let diff = span_diff("@@ -8,1 +8,0 @@\n-gone\n");
    assert_eq!(selected(&diff, &span_sources()), vec![]);
}

#[test]
fn a_zero_count_gap_inside_the_span_selects() {
    let diff = span_diff("@@ -10,1 +10,0 @@\n-gone\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
    let diff = span_diff("@@ -19,1 +19,0 @@\n-gone\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_zero_count_gap_after_the_closing_brace_selects_nothing() {
    let diff = span_diff("@@ -20,1 +20,0 @@\n-gone\n");
    assert_eq!(selected(&diff, &span_sources()), vec![]);
}

#[test]
fn a_hunk_on_a_doc_comment_above_the_test_selects() {
    // The `///` line tokenises as a `#[doc]` attribute, so it joins the
    // attribute stack and the span starts there.
    let diff = span_diff("@@ -10,1 +10,1 @@\n-/// old\n+/// new\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_hunk_on_a_plain_comment_above_the_stack_selects_nothing() {
    // A `//` comment leaves no token, so it is outside every span.
    for line in [1, 4, 8, 9] {
        let diff = span_diff(&format!("@@ -{line},1 +{line},1 @@\n-old\n+new\n"));
        assert_eq!(selected(&diff, &span_sources()), vec![], "line {line}");
    }
}

#[test]
fn a_hunk_on_inner_attributes_selects_nothing() {
    // `#![..]` and `//!` never join a function's attribute stack.
    for line in [1, 2] {
        let diff = span_diff(&format!("@@ -{line},1 +{line},1 @@\n-old\n+new\n"));
        assert_eq!(selected(&diff, &span_sources()), vec![], "line {line}");
    }
}

#[test]
fn a_test_after_inner_attributes_is_selected_only_by_its_own_span() {
    let diff = span_diff("@@ -5,1 +5,1 @@\n-#[test]\n+#[test]\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::first")]
    );
}

#[test]
fn a_deleted_function_above_a_test_selects_that_test() {
    // The source is the working tree's new state: the deleted function is
    // gone, so the kept test's span starts at line 1.
    let source = "#[test]
fn kept() {}
";
    let sources = vec![file("log", "crates/log/src/kept_tests.rs", source)];
    let diff = diff_file(
        "crates/log/src/kept_tests.rs",
        "@@ -1,3 +1,0 @@\n-#[test]\n-fn old() {}\n-\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "kept::tests::kept")]
    );
}

#[test]
fn a_hunk_with_an_omitted_new_count_holds_one_line() {
    let source = "#[test] fn same() {}\n#[test] fn next() {}\n";
    let sources = vec![file("log", "crates/log/src/omitted_tests.rs", source)];
    // `+1` without a count is one line: it touches `same` on line 1 only.
    // A zero count would be the gap before line 2 and select `next`.
    let diff = diff_file(
        "crates/log/src/omitted_tests.rs",
        "@@ -1 +1 @@\n-old\n+new\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "omitted::tests::same")]
    );
}

#[test]
fn a_hunk_end_is_start_plus_count_minus_one() {
    let source = "x\nx\nx\nx\nx\n#[test] fn sixth() {}\n";
    let sources = vec![file("log", "crates/log/src/end_tests.rs", source)];
    let wide = diff_file(
        "crates/log/src/end_tests.rs",
        "@@ -3,4 +3,4 @@\n-a\n+b\n-a\n+b\n-a\n+b\n-a\n+b\n",
    );
    assert_eq!(
        selected(&wide, &sources),
        vec![refer("log", None, true, "end::tests::sixth")]
    );
    let narrow = diff_file(
        "crates/log/src/end_tests.rs",
        "@@ -3,3 +3,3 @@\n-a\n+b\n-a\n+b\n-a\n+b\n",
    );
    assert_eq!(selected(&narrow, &sources), vec![]);
}

#[test]
fn a_hunk_body_is_consumed_by_its_counts() {
    // The added line's text is `++ "b/x"`, shown as `+++ "b/x"`: it is
    // body, not a quoted-path header, so there is no error.
    let source = "#[test] fn quoted() {}\n";
    let sources = vec![file("log", "crates/log/src/quoted_tests.rs", source)];
    let diff = diff_file(
        "crates/log/src/quoted_tests.rs",
        "@@ -1,0 +1 @@\n+++ \"b/x\"\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "quoted::tests::quoted")]
    );
}

#[test]
fn a_missing_trailing_newline_is_not_a_hunk_line() {
    let source = "#[test]\nfn bare() {}\n";
    let sources = vec![file("log", "crates/log/src/newline_tests.rs", source)];
    let diff = diff_file(
        "crates/log/src/newline_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn bare() {}\n+fn bare() {}\n\\ No newline at end of file\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "newline::tests::bare")]
    );
}

#[test]
fn two_files_in_one_diff_each_select() {
    let one = file(
        "log",
        "crates/log/src/one_tests.rs",
        "#[test]\nfn one() {}\n",
    );
    let two = file(
        "log",
        "crates/log/src/two_tests.rs",
        "#[test]\nfn two() {}\n",
    );
    let diff = format!(
        "{}\n{}",
        diff_file(
            "crates/log/src/one_tests.rs",
            "@@ -2,1 +2,1 @@\n-fn one() {}\n+fn one() {}\n"
        ),
        diff_file(
            "crates/log/src/two_tests.rs",
            "@@ -2,1 +2,1 @@\n-fn two() {}\n+fn two() {}\n"
        ),
    );
    // Selection order is not a documented contract, so compare as a set.
    let got = selected(&diff, &[one, two]);
    assert_eq!(got.len(), 2);
    assert!(got.contains(&refer("log", None, true, "one::tests::one")));
    assert!(got.contains(&refer("log", None, true, "two::tests::two")));
}

#[test]
fn a_deleted_file_selects_nothing() {
    let sources = vec![file(
        "log",
        "crates/log/src/gone_tests.rs",
        "#[test]\nfn gone() {}\n",
    )];
    let diff = "diff --git a/crates/log/src/gone_tests.rs b/crates/log/src/gone_tests.rs\n--- a/crates/log/src/gone_tests.rs\n+++ /dev/null\n@@ -1,2 +1,0 @@\n-#[test]\n-fn gone() {}\n";
    assert_eq!(selected(diff, &sources), vec![]);
}

#[test]
fn a_non_rust_path_selects_nothing() {
    let diff = "diff --git a/docs/notes.md b/docs/notes.md\n--- a/docs/notes.md\n+++ b/docs/notes.md\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    assert_eq!(selected(diff, &span_sources()), vec![]);
}

/// Every attribute form beside a real `#[test]`, with the lines each
/// function covers.
const DETECT: &str = "#[test]
fn plain() {}

#[cfg(test)]
fn cfg_only() {}

#[ignore]
fn ignored_alone() {}

#[should_panic]
fn panics() {}

fn bare() {}

#[test_case(1)]
fn cases() {}

#[tokio::test]
async fn async_plain() {}

#[tokio::test(flavor = \"multi_thread\")]
async fn async_mt() {}

#[cfg(test)]
#[test]
#[ignore]
fn stacked() {}

pub fn pubbed() {}

#[test]
pub async fn generic() -> Result<Vec<u8>, String>
where
    String: From<u8>,
{
    Ok(vec![])
}

#[test]
fn r#type() {}

mod nested {
    #[test]
    fn inner() {}
    mod deep {
        #[test]
        fn deeper() {}
    }
}
";

fn detect_sources() -> Vec<RustFile> {
    vec![file("log", "crates/log/src/detect_tests.rs", DETECT)]
}

fn detect_diff(line: u32) -> String {
    diff_file(
        "crates/log/src/detect_tests.rs",
        &format!("@@ -{line},1 +{line},1 @@\n-old\n+new\n"),
    )
}

#[test]
fn only_test_attributes_select() {
    let mut diff = String::new();
    for line in [1, 4, 7, 10, 13, 15, 18, 21, 24, 29, 32, 39, 43, 46] {
        diff.push_str(&detect_diff(line));
    }
    // The filter joins one term per test, and `docs/ci.md` promises which
    // tests run but not the order of the terms, so the names compare as a
    // set.
    let mut names: Vec<String> = selected(&diff, &detect_sources())
        .iter()
        .map(|t| t.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "detect::tests::async_mt",
            "detect::tests::async_plain",
            "detect::tests::generic",
            "detect::tests::nested::deep::deeper",
            "detect::tests::nested::inner",
            "detect::tests::plain",
            "detect::tests::stacked",
            "detect::tests::type",
        ]
    );
}

#[test]
fn a_test_attribute_stack_starts_at_its_first_attribute() {
    // Line 24 is the `#[cfg(test)]` above `#[test]`: the span starts there.
    let diff = detect_diff(24);
    assert_eq!(
        selected(&diff, &detect_sources()),
        vec![refer("log", None, true, "detect::tests::stacked")]
    );
    let before = detect_diff(23);
    assert_eq!(selected(&before, &detect_sources()), vec![]);
}

#[test]
fn of_two_tests_only_the_touched_one_selects() {
    let source = "#[test]
fn touched() {}

#[test]
fn untouched() {}
";
    let sources = vec![file("log", "crates/log/src/two_tests.rs", source)];
    let diff = diff_file(
        "crates/log/src/two_tests.rs",
        "@@ -1,2 +1,2 @@\n-#[test]\n+#[test]\n-fn touched() {}\n+fn touched() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "two::tests::touched")]
    );
}

#[test]
fn a_unit_test_file_names_its_module_path() {
    let sources = vec![file(
        "log",
        "crates/log/src/writer_tests.rs",
        "#[test]\nfn flushes_on_drop() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/writer_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn flushes_on_drop() {}\n+fn flushes_on_drop() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "writer::tests::flushes_on_drop")]
    );
}

#[test]
fn a_tests_binary_names_the_bare_function() {
    let sources = vec![file(
        "loop",
        "crates/loop/tests/turns.rs",
        "#[test]\nfn resumes_after_pause() {}\n",
    )];
    let diff = diff_file(
        "crates/loop/tests/turns.rs",
        "@@ -2,1 +2,1 @@\n-fn resumes_after_pause() {}\n+fn resumes_after_pause() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer(
            "loop",
            Some("loop::turns"),
            false,
            "resumes_after_pause"
        )]
    );
}

#[test]
fn a_shared_tests_subdirectory_without_a_root_file_names_no_binary() {
    let sources = vec![file(
        "loop",
        "crates/loop/tests/support/mod.rs",
        "#[test]\nfn helper() {}\n",
    )];
    let diff = diff_file(
        "crates/loop/tests/support/mod.rs",
        "@@ -2,1 +2,1 @@\n-fn helper() {}\n+fn helper() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("loop", None, false, "support::helper")]
    );
}

#[test]
fn a_shared_tests_subdirectory_with_a_root_file_names_that_binary() {
    let sources = vec![
        file("loop", "crates/loop/tests/support.rs", ""),
        file(
            "loop",
            "crates/loop/tests/support/mod.rs",
            "#[test]\nfn helper() {}\n",
        ),
    ];
    let diff = diff_file(
        "crates/loop/tests/support/mod.rs",
        "@@ -2,1 +2,1 @@\n-fn helper() {}\n+fn helper() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("loop", Some("loop::support"), false, "helper")]
    );
}

#[test]
fn a_tests_subdirectory_with_a_root_file_prefixes_below_the_directory() {
    let sources = vec![
        file("main", "crates/main/tests/completion.rs", ""),
        file(
            "main",
            "crates/main/tests/completion/bash.rs",
            "#[test]\nfn completes() {}\n",
        ),
    ];
    let diff = diff_file(
        "crates/main/tests/completion/bash.rs",
        "@@ -2,1 +2,1 @@\n-fn completes() {}\n+fn completes() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer(
            "main",
            Some("main::completion"),
            false,
            "bash::completes"
        )]
    );
}

#[test]
fn an_example_test_file_names_its_example_binary() {
    let sources = vec![file(
        "bench",
        "crates/bench/examples/bench/run_tests.rs",
        "#[test]\nfn runs() {}\n",
    )];
    let diff = diff_file(
        "crates/bench/examples/bench/run_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn runs() {}\n+fn runs() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer(
            "bench",
            Some("bench::example/bench"),
            false,
            "run::tests::runs"
        )]
    );
}

#[test]
fn a_path_declared_test_file_maps_to_its_declaring_module() {
    let sources = vec![
        file(
            "main",
            "crates/main/src/settings.rs",
            "#[cfg(test)]\n#[path = \"idle_tests.rs\"]\nmod idle_tests;\n",
        ),
        file(
            "main",
            "crates/main/src/idle_tests.rs",
            "#[test]\nfn idle() {}\n",
        ),
    ];
    let diff = diff_file(
        "crates/main/src/idle_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn idle() {}\n+fn idle() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("main", None, true, "settings::idle_tests::idle")]
    );
}

#[test]
fn a_unit_test_without_a_tests_segment_selects_with_the_unit_filter() {
    // A `*_tests` file a `#[path]` declaration names from a plain module
    // (as `crates/tui/src/app/commands.rs` names `aliases_tests.rs` from
    // `mod aliases`): its tests compile into the package's unit tests, so
    // the filter is the package term even though the module path holds no
    // `tests::` segment.
    let sources = vec![
        file(
            "log",
            "crates/log/src/writer.rs",
            "#[path = \"writer_alias_tests.rs\"]\nmod writer_alias;\n",
        ),
        file(
            "log",
            "crates/log/src/writer_alias_tests.rs",
            "#[test]\nfn flushes() {}\n",
        ),
    ];
    let diff = diff_file(
        "crates/log/src/writer_alias_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn flushes() {}\n+fn flushes() {}\n",
    );
    let touched = selected(&diff, &sources);
    assert_eq!(
        touched,
        vec![refer("log", None, true, "writer::writer_alias::flushes")]
    );
    assert_eq!(
        filter(&touched),
        "(package(log) & test(=writer::writer_alias::flushes))"
    );
}

#[test]
fn a_shared_support_test_selects_with_the_kind_filter() {
    // A module under `tests/<dir>/` with no root file compiles into the
    // integration binaries, so its term keeps `kind(test)`.
    let sources = vec![file(
        "loop",
        "crates/loop/tests/support/mod.rs",
        "#[test]\nfn helper() {}\n",
    )];
    let diff = diff_file(
        "crates/loop/tests/support/mod.rs",
        "@@ -2,1 +2,1 @@\n-fn helper() {}\n+fn helper() {}\n",
    );
    let touched = selected(&diff, &sources);
    assert_eq!(touched, vec![refer("loop", None, false, "support::helper")]);
    assert_eq!(
        filter(&touched),
        "(package(loop) & kind(test) & test(=support::helper))"
    );
}

#[test]
fn a_non_test_file_with_a_touched_test_selects_nothing() {
    let sources = vec![file(
        "log",
        "crates/log/src/writer.rs",
        "#[test]\nfn hidden() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/writer.rs",
        "@@ -2,1 +2,1 @@\n-fn hidden() {}\n+fn hidden() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn filter_joins_one_term_per_touched_test() {
    // `docs/ci.md` promises which tests run but not the order of the
    // terms, so the terms compare as a set.
    let tests = vec![
        refer("loop", Some("loop::turns"), false, "resumes_after_pause"),
        refer("log", None, true, "writer::tests::flushes_on_drop"),
    ];
    let joined = filter(&tests);
    let mut terms: Vec<&str> = joined.split(" | ").collect();
    terms.sort_unstable();
    assert_eq!(
        terms,
        vec![
            "(binary_id(loop::turns) & test(=resumes_after_pause))",
            "(package(log) & test(=writer::tests::flushes_on_drop))",
        ]
    );
}

#[test]
fn filter_marks_a_shared_support_test_with_its_kind() {
    let tests = vec![refer("log", None, false, "support::helper")];
    assert_eq!(
        filter(&tests),
        "(package(log) & kind(test) & test(=support::helper))"
    );
}

#[test]
fn filter_deduplicates_a_test_two_hunks_touch() {
    let sources = vec![file(
        "log",
        "crates/log/src/again_tests.rs",
        "#[test]\nfn again() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/again_tests.rs",
        "@@ -1,1 +1,1 @@\n-#[test]\n+#[test]\n@@ -2,1 +2,1 @@\n-fn again() {}\n+fn again() {}\n",
    );
    assert_eq!(
        filter(&selected(&diff, &sources)),
        "(package(log) & test(=again::tests::again))"
    );
}

#[test]
fn an_empty_selection_filters_to_nothing() {
    assert_eq!(filter(&[]), "");
}

#[test]
fn a_quoted_diff_path_is_an_error_naming_the_path() {
    for (diff, path) in [
        (
            "diff --git \"a/weird name.rs\" \"b/weird name.rs\"\n--- \"a/weird name.rs\"\n+++ \"b/weird name.rs\"\n@@ -1,1 +1,1 @@\n-old\n+new\n",
            "\"a/weird name.rs\"",
        ),
        (
            "diff --git a/plain.rs b/plain.rs\n--- a/plain.rs\n+++ \"b/plain.rs\"\n@@ -1,1 +1,1 @@\n-old\n+new\n",
            "\"b/plain.rs\"",
        ),
    ] {
        match changed_tests(diff, &members(), &span_sources()) {
            Err(error) => assert!(error.contains(path), "{diff}: {error}"),
            Ok(tests) => panic!("{diff}: expected an error naming the path, got {tests:?}"),
        }
    }
}

#[test]
fn a_malformed_hunk_header_is_an_error_naming_the_path() {
    for header in ["@@ nonsense @@\n", "@@ -a +b @@\n", "@@ -1,2 @@\n"] {
        let diff = diff_file("crates/log/src/span_tests.rs", header);
        match changed_tests(&diff, &members(), &span_sources()) {
            Err(error) => assert!(
                error.contains("crates/log/src/span_tests.rs"),
                "{header}: {error}"
            ),
            Ok(tests) => {
                panic!("{header}: expected an error naming the path, got {tests:?}")
            }
        }
    }
}

#[test]
fn an_unparsable_test_file_is_an_error_naming_the_path() {
    let sources = vec![file(
        "log",
        "crates/log/src/broken_tests.rs",
        "fn broken( {\n",
    )];
    let diff = diff_file(
        "crates/log/src/broken_tests.rs",
        "@@ -1,1 +1,1 @@\n-fn broken( {\n+fn broken( {\n",
    );
    match changed_tests(&diff, &members(), &sources) {
        Err(error) => assert!(error.contains("crates/log/src/broken_tests.rs"), "{error}"),
        Ok(tests) => panic!("expected an error naming the path, got {tests:?}"),
    }
}

#[test]
fn a_single_colon_in_a_test_path_selects_nothing() {
    // `#[test : foo]` has only the first colon, so the path walk stops
    // after `test` and the trailing tokens reject it: `&&` needs both
    // colons, while `||` would walk past `test` and accept the empty rest.
    let sources = vec![file(
        "log",
        "crates/log/src/attr_tests.rs",
        "#[test : foo]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/attr_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_two_level_test_path_selects() {
    // `a::b::test` walks two `::` pairs before its answer: the walk must
    // finish and accept the path. Stepping an index past each pair stalls
    // here instead (`0 * 3` stays 0 under its `*=` mutant), hanging the
    // run rather than answering.
    let sources = vec![file(
        "log",
        "crates/log/src/nested_attr_tests.rs",
        "#[a::b::test]\nfn deep() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/nested_attr_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn deep() {}\n+fn deep() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "nested_attr::tests::deep")]
    );
}

#[test]
fn a_stray_semicolon_does_not_eat_the_next_attribute() {
    // The `;` is not `#`, so it leaves the following `#[test]` for the
    // attribute arm; matching any punctuation there would swallow the `#`
    // and the test would go unselected.
    let sources = vec![file(
        "log",
        "crates/log/src/semi_tests.rs",
        "fn helper() {};\n#[test]\nfn real() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/semi_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn real() {}\n+fn real() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "semi::tests::real")]
    );
}

#[test]
fn a_hash_with_a_paren_group_is_not_an_attribute() {
    // Only `#[` joins a stack: `#(test)` is not an attribute even though
    // its group holds a test path, so treating any delimiter as `[` would
    // wrongly select `paren`.
    let sources = vec![file(
        "log",
        "crates/log/src/paren_tests.rs",
        "#(test)\nfn paren() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/paren_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn paren() {}\n+fn paren() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn an_inner_attribute_keeps_a_pending_test_stack() {
    // `#![..]` after `#[test]` skips its group without clearing the
    // stack; falling through to the clearing arm (as `!=` on the `!`
    // check, or `false` for its guard, would) drops `kept`.
    let sources = vec![file(
        "log",
        "crates/log/src/inner_tests.rs",
        "#[test]\n#![allow(dead_code)]\nfn kept() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/inner_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn kept() {}\n+fn kept() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "inner::tests::kept")]
    );
}

#[test]
fn a_hash_before_other_punctuation_clears_the_stack() {
    // After `#[test]`, `#;` is neither an attribute nor an inner
    // attribute, so the stack clears. Keeping it on any punctuation
    // (as `true` for the `!` guard, or `!=` on the `!` check, would)
    // wrongly selects `cleared`.
    let sources = vec![file(
        "log",
        "crates/log/src/cleared_tests.rs",
        "#[test]\n#;\nfn cleared() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/cleared_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn cleared() {}\n+fn cleared() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_function_declaration_stops_at_its_semicolon() {
    // `fn declared();` has no body: the scan stops at `;` instead of
    // running on into `real`, so the hunk touches only `real`. Skipping
    // the `;` (as `false` for its guard would) merges both into one span.
    let sources = vec![file(
        "log",
        "crates/log/src/decl_tests.rs",
        "fn declared();\n#[test]\nfn real() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/decl_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn real() {}\n+fn real() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "decl::tests::real")]
    );
}

#[test]
fn an_unsafe_qualifier_keeps_the_stack() {
    // `unsafe` between the attribute and the `fn` is a qualifier like
    // `pub`: only it flips the `||` arm that drops it (and the `||`-to-`&&`
    // arm, which drops every qualifier).
    let sources = vec![file(
        "log",
        "crates/log/src/unsafe_tests.rs",
        "#[test]\nunsafe fn guarded() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/unsafe_tests.rs",
        "@@ -2,1 +2,1 @@\n-unsafe fn guarded() {}\n+unsafe fn guarded() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "unsafe::tests::guarded")]
    );
}

#[test]
fn an_extern_string_keeps_the_stack() {
    // `extern "C"` is a qualifier plus a literal: the qualifier arm keeps
    // the stack across `extern`, and the group-or-literal arm keeps it
    // across `"C"`. Dropping either (the `extern` or `const` arm, the
    // `||`-to-`&&` arm, or the group arm) loses `linked`.
    let sources = vec![file(
        "log",
        "crates/log/src/linked_tests.rs",
        "#[test]\nextern \"C\"\nfn linked() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/linked_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn linked() {}\n+fn linked() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "linked::tests::linked")]
    );
}

#[test]
fn a_const_qualifier_keeps_the_stack() {
    // Only `const` flips the `||` arm that drops it (and the `||`-to-`&&`
    // arm, which drops every qualifier).
    let sources = vec![file(
        "log",
        "crates/log/src/const_tests.rs",
        "#[test]\nconst fn evaluated() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/const_tests.rs",
        "@@ -2,1 +2,1 @@\n-const fn evaluated() {}\n+const fn evaluated() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "const::tests::evaluated")]
    );
}

#[test]
fn a_pub_restriction_keeps_the_stack() {
    // `pub(crate)` is a qualifier plus a group: the qualifier arm keeps
    // the stack across `pub`, and the group-or-literal arm keeps it
    // across `(crate)`. Clearing the stack on a group would lose `scoped`.
    let sources = vec![file(
        "log",
        "crates/log/src/scoped_tests.rs",
        "#[test]\npub(crate)\nfn scoped() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/scoped_tests.rs",
        "@@ -3,1 +3,1 @@\n-fn scoped() {}\n+fn scoped() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "scoped::tests::scoped")]
    );
}

#[test]
fn a_non_qualifier_word_clears_the_stack() {
    // Only `pub`, `unsafe`, `async`, `extern` and `const` keep a pending
    // `#[test]` stack across to the `fn`: any other word clears it, so the
    // following `fn` is not a test and neither hunk selects anything.
    // Replacing the guard with `true` keeps the stack across `bogus` too,
    // making `cleared` a test spanning lines 1 to 3, so both hunks would
    // wrongly select it. (`bogus` is a bare word on purpose: a line like
    // `let x = 1;` reaches `=`/`;` punctuation, which clears the stack even
    // with the mutated guard, so it cannot tell the two apart.)
    for word in ["pub", "unsafe", "async", "extern", "const"] {
        let sources = vec![file(
            "log",
            "crates/log/src/qual_tests.rs",
            &format!("#[test]\n{word} fn kept() {{}}\n"),
        )];
        let diff = diff_file(
            "crates/log/src/qual_tests.rs",
            "@@ -2,1 +2,1 @@\n-fn kept() {}\n+fn kept() {}\n",
        );
        assert_eq!(
            selected(&diff, &sources),
            vec![refer("log", None, true, "qual::tests::kept")],
            "word {word}"
        );
    }
    let sources = vec![file(
        "log",
        "crates/log/src/qual_tests.rs",
        "#[test]\nbogus\nfn cleared() {}\n",
    )];
    for line in [2, 3] {
        let diff = diff_file(
            "crates/log/src/qual_tests.rs",
            &format!("@@ -{line},1 +{line},1 @@\n-old\n+new\n"),
        );
        assert_eq!(selected(&diff, &sources), vec![], "line {line}");
    }
}

#[test]
fn an_added_line_in_a_zero_new_count_hunk_is_an_error_naming_the_path() {
    // The hunk holds two old lines and no new ones, yet its body adds one:
    // the `+` line finds no remaining new line and errors. Without the
    // `remaining_old > 0` check the hunk closes early and the error is lost.
    let diff = diff_file(
        "crates/log/src/span_tests.rs",
        "@@ -1,2 +1,0 @@\n-foo\n+bar\n",
    );
    match changed_tests(&diff, &members(), &span_sources()) {
        Err(error) => assert!(error.contains("crates/log/src/span_tests.rs"), "{error}"),
        Ok(tests) => panic!("expected an error naming the path, got {tests:?}"),
    }
}

#[test]
fn a_context_line_counts_and_selects() {
    // A context line consumes one old and one new line and selects like
    // any held line: it starts with a space but is not empty (and the
    // blank context line is empty but starts with nothing), so exactly one
    // side of the `||` holds in each case. Requiring both would ignore
    // context lines and misalign every line after them.
    let spaced = diff_file(
        "crates/log/src/span_tests.rs",
        "@@ -10,2 +10,2 @@\n /// doc for second\n-#[test]\n+#[test]\n",
    );
    assert_eq!(
        selected(&spaced, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
    let gappy = vec![file(
        "log",
        "crates/log/src/gappy_tests.rs",
        "#[test]\nfn gappy() {\n\n}\n",
    )];
    let blank = diff_file("crates/log/src/gappy_tests.rs", "@@ -3,1 +3,1 @@\n\n");
    assert_eq!(
        selected(&blank, &gappy),
        vec![refer("log", None, true, "gappy::tests::gappy")]
    );
}

#[test]
fn a_context_line_on_the_spans_last_line_selects() {
    // The context line is `}` on line 20: selecting `next_new - 1` keeps
    // the span, while adding to or dividing the first term moves off it.
    let diff = diff_file("crates/log/src/span_tests.rs", "@@ -20,1 +20,1 @@\n }\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, true, "span::tests::second")]
    );
}

#[test]
fn a_context_line_before_the_span_selects_nothing() {
    // The context line is the plain comment on line 9, just above the
    // span: selecting `next_new - 1` misses, while adding to or dividing
    // the second term reaches into the span. Unlike the `-`/`+` version
    // above, this hunk runs through the context-line arithmetic.
    let diff = diff_file(
        "crates/log/src/span_tests.rs",
        "@@ -9,1 +9,1 @@\n // another plain comment\n",
    );
    assert_eq!(selected(&diff, &span_sources()), vec![]);
}

#[test]
fn a_colon_before_a_non_colon_in_a_test_path_selects_nothing() {
    // `#[test:,]` holds `:` then `,`, so only the first colon check
    // holds and the `&&` guard fails: the walk stops after `test`,
    // leaving two trailing tokens that reject the path. Replacing `&&`
    // with `||` (or the guard with `true`) walks past the pair and
    // accepts the empty rest, wrongly selecting `odd`.
    let sources = vec![file(
        "log",
        "crates/log/src/colon_tests.rs",
        "#[test:,]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/colon_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_non_colon_before_a_colon_in_a_test_path_selects_nothing() {
    // `#[test,:]` holds `,` then `:`, so only the second colon check
    // holds and the `&&` guard fails: the walk stops after `test`,
    // leaving two trailing tokens that reject the path. Replacing `&&`
    // with `||` (or the guard with `true`) walks past the pair and
    // accepts the empty rest, wrongly selecting `odd`.
    let sources = vec![file(
        "log",
        "crates/log/src/comma_tests.rs",
        "#[test,:]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/comma_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_double_non_colon_in_a_test_path_selects_nothing() {
    // `#[test!!]` holds no colon at all, so both colon checks fail and
    // the `&&` guard fails: the walk stops after `test`, leaving two
    // trailing tokens that reject the path. Replacing the guard with
    // `true` walks past the pair and accepts the empty rest, wrongly
    // selecting `odd`. (Replacing `&&` with `||` still fails here, so
    // this pins the guard itself with both comparisons false.)
    let sources = vec![file(
        "log",
        "crates/log/src/bang_tests.rs",
        "#[test!!]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/bang_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_cfg_prefixed_test_path_selects_nothing() {
    // `#[cfg::test]` walks one `::` pair to segments `[cfg, test]`
    // with an empty rest: the last segment is `test`, but the first is
    // `cfg`, so the `||` rejects it. Replacing `||` with `&&` (or the
    // `cfg` check's `==` with `!=`) drops the rejection and wrongly
    // selects `prefixed`.
    let sources = vec![file(
        "log",
        "crates/log/src/cfg_path_tests.rs",
        "#[cfg::test]\nfn prefixed() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/cfg_path_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn prefixed() {}\n+fn prefixed() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_test_prefixed_non_test_path_selects_nothing() {
    // `#[test::foo]` walks one `::` pair to segments `[test, foo]` with
    // an empty rest: the last segment is not `test`, so the `!=`
    // rejects it. Replacing `!=` with `==` accepts the empty rest
    // instead and wrongly selects `odd`.
    let sources = vec![file(
        "log",
        "crates/log/src/suffix_tests.rs",
        "#[test::foo]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/suffix_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn an_attribute_without_a_path_selects_nothing() {
    // `#[]` holds no tokens and `#[::test]` starts with a punctuator,
    // so neither yields a path segment and both hit the empty-path
    // early return. Returning `true` there (as that arm's mutant would)
    // wrongly selects both functions.
    let sources = vec![file(
        "log",
        "crates/log/src/empty_tests.rs",
        "#[]\nfn empty() {}\n\n#[::test]\nfn leading() {}\n",
    )];
    let mut diff = String::new();
    diff.push_str(&diff_file(
        "crates/log/src/empty_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn empty() {}\n+fn empty() {}\n",
    ));
    diff.push_str(&diff_file(
        "crates/log/src/empty_tests.rs",
        "@@ -5,1 +5,1 @@\n-fn leading() {}\n+fn leading() {}\n",
    ));
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_test_path_with_a_trailing_argument_list_selects() {
    // `#[test(foo)]` leaves a single group after `test`: the trailing
    // argument list still selects. Denying the group arm (as `false`
    // for the whole trailing-token check would) loses `with_args`.
    let sources = vec![file(
        "log",
        "crates/log/src/args_tests.rs",
        "#[test(foo)]\nfn with_args() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/args_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn with_args() {}\n+fn with_args() {}\n",
    );
    assert_eq!(
        selected(&diff, &sources),
        vec![refer("log", None, true, "args::tests::with_args")]
    );
}

#[test]
fn a_test_path_followed_by_a_bare_word_selects_nothing() {
    // `#[test foo]` leaves a single bare word after `test`: with no
    // `::` and no argument list the path is rejected. Accepting any
    // single trailing token (as `true` for the trailing-token check
    // would) wrongly selects `odd`.
    let sources = vec![file(
        "log",
        "crates/log/src/bare_tests.rs",
        "#[test foo]\nfn odd() {}\n",
    )];
    let diff = diff_file(
        "crates/log/src/bare_tests.rs",
        "@@ -2,1 +2,1 @@\n-fn odd() {}\n+fn odd() {}\n",
    );
    assert_eq!(selected(&diff, &sources), vec![]);
}

#[test]
fn a_hunk_count_past_u32_is_an_error_naming_the_path() {
    for header in [
        "@@ -1 +18446744073709551615,2 @@\n-old\n+new\n+new\n",
        "@@ -1,2 +4294967295,2 @@\n-old\n-old\n+new\n+new\n",
    ] {
        let diff = diff_file("crates/log/src/span_tests.rs", header);
        match changed_tests(&diff, &members(), &span_sources()) {
            Err(error) => assert!(
                error.contains("crates/log/src/span_tests.rs"),
                "{header}: {error}"
            ),
            Ok(tests) => {
                panic!("{header}: expected an error naming the path, got {tests:?}")
            }
        }
    }
}
