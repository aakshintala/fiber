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

fn refer(package: &str, binary: Option<&str>, name: &str) -> TestRef {
    TestRef {
        package: package.to_owned(),
        binary: binary.map(str::to_owned),
        name: name.to_owned(),
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
        vec![refer("log", None, "span::tests::second")]
    );
}

#[test]
fn a_hunk_on_the_closing_brace_line_selects() {
    let diff = span_diff("@@ -20,1 +20,1 @@\n-}\n+}\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, "span::tests::second")]
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
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![
            refer("log", None, "span::tests::first"),
            refer("log", None, "span::tests::second"),
        ]
    );
}

#[test]
fn a_hunk_starting_at_the_span_end_selects() {
    let diff = span_diff("@@ -20,1 +20,6 @@\n-}\n+}\n+a\n+b\n+c\n+d\n+e\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, "span::tests::second")]
    );
}

#[test]
fn a_hunk_header_may_carry_a_section_heading() {
    let diff = span_diff("@@ -10,1 +10,1 @@ fn second\n-/// old\n+/// new\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, "span::tests::second")]
    );
}

#[test]
fn a_zero_count_gap_before_the_span_start_selects() {
    let diff = span_diff("@@ -9,1 +9,0 @@\n-gone\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, "span::tests::second")]
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
        vec![refer("log", None, "span::tests::second")]
    );
    let diff = span_diff("@@ -19,1 +19,0 @@\n-gone\n");
    assert_eq!(
        selected(&diff, &span_sources()),
        vec![refer("log", None, "span::tests::second")]
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
        vec![refer("log", None, "span::tests::second")]
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
        vec![refer("log", None, "span::tests::first")]
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
        vec![refer("log", None, "kept::tests::kept")]
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
        vec![refer("log", None, "omitted::tests::same")]
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
        vec![refer("log", None, "end::tests::sixth")]
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
        vec![refer("log", None, "quoted::tests::quoted")]
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
        vec![refer("log", None, "newline::tests::bare")]
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
    assert_eq!(
        selected(&diff, &[one, two]),
        vec![
            refer("log", None, "one::tests::one"),
            refer("log", None, "two::tests::two"),
        ]
    );
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
    let names: Vec<String> = selected(&diff, &detect_sources())
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert_eq!(
        names,
        vec![
            "detect::tests::plain",
            "detect::tests::async_plain",
            "detect::tests::async_mt",
            "detect::tests::stacked",
            "detect::tests::generic",
            "detect::tests::type",
            "detect::tests::nested::inner",
            "detect::tests::nested::deep::deeper",
        ]
    );
}

#[test]
fn a_test_attribute_stack_starts_at_its_first_attribute() {
    // Line 24 is the `#[cfg(test)]` above `#[test]`: the span starts there.
    let diff = detect_diff(24);
    assert_eq!(
        selected(&diff, &detect_sources()),
        vec![refer("log", None, "detect::tests::stacked")]
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
        vec![refer("log", None, "two::tests::touched")]
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
        vec![refer("log", None, "writer::tests::flushes_on_drop")]
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
        vec![refer("loop", Some("loop::turns"), "resumes_after_pause")]
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
        vec![refer("loop", None, "support::helper")]
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
        vec![refer("loop", Some("loop::support"), "helper")]
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
        vec![refer("main", Some("main::completion"), "bash::completes")]
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
        vec![refer("main", None, "settings::idle_tests::idle")]
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
fn filter_joins_terms_in_diff_order() {
    let tests = vec![
        refer("loop", Some("loop::turns"), "resumes_after_pause"),
        refer("log", None, "writer::tests::flushes_on_drop"),
    ];
    assert_eq!(
        filter(&tests),
        "(binary_id(loop::turns) & test(=resumes_after_pause)) | (package(log) & test(=writer::tests::flushes_on_drop))"
    );
}

#[test]
fn filter_marks_a_shared_support_test_with_its_kind() {
    let tests = vec![refer("log", None, "support::helper")];
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
            Ok(tests) => assert!(tests.is_empty(), "{diff}: got {tests:?}"),
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
            Ok(tests) => assert!(tests.is_empty(), "{header}: got {tests:?}"),
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
        Ok(tests) => assert!(tests.is_empty(), "expected an error, got {tests:?}"),
    }
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
            Ok(tests) => assert!(tests.is_empty(), "{header}: got {tests:?}"),
        }
    }
}
