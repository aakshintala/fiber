use super::*;
use crate::rules::RustFile;
use crate::select::{Member, Members, TestRef};
use crate::test_index::{TestBinary, TestIndex};

fn member(unrun_tests: &[&str]) -> Members {
    std::collections::BTreeMap::from([(
        "log".to_owned(),
        Member {
            dir: "crates/log".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            library: true,
            unrun_tests: unrun_tests.iter().map(|path| (*path).to_owned()).collect(),
        },
    )])
}

fn source_file(path: &str, source: &str) -> RustFile {
    RustFile {
        krate: "log".to_owned(),
        path: path.to_owned(),
        rel: path.strip_prefix("crates/log/").unwrap().to_owned(),
        source: source.to_owned(),
    }
}

fn binary_test() -> TestRef {
    TestRef {
        package: "main".to_owned(),
        binary: Some("main::completion".to_owned()),
        name: "bash::registers".to_owned(),
        unit: false,
    }
}

fn unit_test() -> TestRef {
    TestRef {
        package: "log".to_owned(),
        binary: None,
        name: "writer::tests::flushes_on_drop".to_owned(),
        unit: true,
    }
}

#[test]
fn empty_test_file_error_requires_a_test_function_and_an_empty_compiled_index() {
    let path = "crates/log/src/utility_tests.rs".to_owned();
    let files = [path.clone()];
    let sources = [source_file(&path, "#[test] fn catches() {}")];
    let members = member(&[]);
    let empty = TestIndex::from_fixtures(vec![(path.clone(), TestBinary { tests: Vec::new() })]);

    assert_eq!(
        empty_test_file_error(&files, &sources, &members, &empty),
        Some(format!("{path}: changed test file selected no tests"))
    );
    assert_eq!(
        empty_test_file_error(&files, &sources, &members, &TestIndex::default()),
        None,
        "an uncompiled file has no index entry"
    );
    let listed = TestIndex::from_fixtures(vec![(
        path.clone(),
        TestBinary {
            tests: vec![unit_test()],
        },
    )]);
    assert_eq!(
        empty_test_file_error(&files, &sources, &members, &listed),
        None,
        "a binary that lists tests is not an empty-index miss"
    );
}

#[test]
fn empty_test_file_error_ignores_files_without_test_functions() {
    let path = "crates/log/src/utility_tests.rs".to_owned();
    let files = [path.clone()];
    let sources = [source_file(&path, "const BUDGET: u32 = 1;")];
    let index = TestIndex::from_fixtures(vec![(path.clone(), TestBinary { tests: Vec::new() })]);

    assert_eq!(
        empty_test_file_error(&files, &sources, &member(&[]), &index),
        None
    );
}

#[test]
fn empty_test_file_error_skips_cfg_inner_attributes() {
    let path = "crates/log/src/utility_tests.rs".to_owned();
    let files = [path.clone()];
    let sources = [source_file(
        &path,
        "#![cfg(feature = \"x\")] #[test] fn catches() {}",
    )];
    let index = TestIndex::from_fixtures(vec![(path.clone(), TestBinary { tests: Vec::new() })]);

    assert_eq!(
        empty_test_file_error(&files, &sources, &member(&[]), &index),
        None
    );
}

#[test]
fn has_cfg_inner_attr_ignores_non_cfg_attributes_with_arguments() {
    assert_eq!(has_cfg_inner_attr("#![allow(dead_code)]"), Ok(false));
}

#[test]
fn empty_test_file_error_skips_unrun_test_files_but_not_run_files() {
    let normal = "crates/log/src/utility_tests.rs";
    let unrun = "crates/log/tests/main_journeys.rs";
    for (path, expected) in [
        (
            normal,
            Some(format!("{normal}: changed test file selected no tests")),
        ),
        (unrun, None),
    ] {
        let files = [path.to_owned()];
        let sources = [source_file(path, "#[test] fn catches() {}")];
        let index =
            TestIndex::from_fixtures(vec![(path.to_owned(), TestBinary { tests: Vec::new() })]);

        assert_eq!(
            empty_test_file_error(&files, &sources, &member(&[unrun]), &index),
            expected,
            "path {path}"
        );
    }
}

#[test]
fn empty_test_file_error_names_the_first_file_with_an_empty_index() {
    let first = "crates/log/src/first_tests.rs".to_owned();
    let second = "crates/log/src/second_tests.rs".to_owned();
    let files = [first.clone(), second.clone()];
    let sources = [
        source_file(&first, "#[test] fn first() {}"),
        source_file(&second, "#[test] fn second() {}"),
    ];
    let index = TestIndex::from_fixtures(vec![
        (first.clone(), TestBinary { tests: Vec::new() }),
        (
            second,
            TestBinary {
                tests: vec![unit_test()],
            },
        ),
    ]);

    assert_eq!(
        empty_test_file_error(&files, &sources, &member(&[]), &index),
        Some(format!("{first}: changed test file selected no tests"))
    );
}

#[test]
fn empty_test_file_error_requires_every_binary_for_a_file_to_list_no_tests() {
    let path = "crates/log/src/utility_tests.rs".to_owned();
    let files = [path.clone()];
    let sources = [source_file(&path, "#[test] fn catches() {}")];
    let index = TestIndex::from_fixtures(vec![
        (path.clone(), TestBinary { tests: Vec::new() }),
        (
            path.clone(),
            TestBinary {
                tests: vec![unit_test()],
            },
        ),
    ]);

    assert_eq!(
        empty_test_file_error(&files, &sources, &member(&[]), &index),
        None
    );
}

#[test]
fn an_empty_label_list_runs_the_pilot_default() {
    assert_eq!(stress_count(""), 30);
}

#[test]
fn a_label_list_without_test_only_runs_the_pilot_default() {
    assert_eq!(stress_count("bug"), STRESS_COUNT);
    assert_eq!(stress_count("bug\nneeds-triage"), 30);
}

#[test]
fn a_test_only_label_runs_the_flaky_fix_count() {
    assert_eq!(stress_count("test-only"), 100);
    assert_eq!(stress_count("bug\ntest-only"), 100);
}

#[test]
fn a_near_test_only_label_runs_the_pilot_default() {
    for labels in [
        "test-only-x",
        "xtest-only",
        "bug test-only",
        "test-only bug",
    ] {
        assert_eq!(stress_count(labels), 30, "{labels:?}");
    }
}

#[test]
fn a_padded_test_only_label_runs_the_flaky_fix_count() {
    assert_eq!(stress_count("  test-only  "), 100);
    assert_eq!(stress_count("bug\n\ttest-only "), 100);
}

#[test]
fn a_zero_length_run_costs_nothing() {
    assert_eq!(
        cost(0, 0),
        Cost {
            wall_seconds: 0,
            runner_minutes: 0,
        }
    );
}

#[test]
fn a_run_under_a_minute_bills_one_minute() {
    assert_eq!(
        cost(0, 1),
        Cost {
            wall_seconds: 1,
            runner_minutes: 1,
        }
    );
    assert_eq!(cost(0, 59).runner_minutes, 1);
    assert_eq!(cost(0, 59).wall_seconds, 59);
}

#[test]
fn a_whole_minute_bills_one_minute_and_the_next_second_bills_two() {
    assert_eq!(cost(0, 60).runner_minutes, 1);
    assert_eq!(cost(0, 61).runner_minutes, 2);
    assert_eq!(cost(0, 61).wall_seconds, 61);
}

#[test]
fn an_end_before_the_start_measures_nothing() {
    assert_eq!(
        cost(100, 40),
        Cost {
            wall_seconds: 0,
            runner_minutes: 0,
        }
    );
}

#[test]
fn the_summary_names_the_wall_time_and_the_billed_minutes() {
    assert_eq!(
        cost_summary(&cost(0, 432), 30, 5, "success"),
        "### Stress job cost\nwall time: 7m 12s (432 s)\nrunner minutes: 8\nstress-cost: wall_seconds=432 runner_minutes=8 count=30 tests=5 outcome=success"
    );
}

#[test]
fn the_summary_splits_minutes_and_seconds() {
    let minute = cost_summary(&cost(0, 60), 30, 1, "success");
    assert!(minute.contains("wall time: 1m 0s (60 s)"), "{minute}");
    let seconds = cost_summary(&cost(0, 59), 30, 1, "failure");
    assert!(seconds.contains("wall time: 0m 59s (59 s)"), "{seconds}");
    assert!(seconds.contains("runner minutes: 1"), "{seconds}");
    assert!(
        seconds.ends_with("count=30 tests=1 outcome=failure"),
        "{seconds}"
    );
}

#[test]
fn a_retry_list_line_matching_a_binary_scoped_test_fails() {
    let touched = [binary_test()];
    assert_eq!(
        touched_retries("main::completion bash::registers\tfirst failure", &touched),
        vec![
            "touched-retries: main::completion bash::registers was added or changed by this pull request and passed only on retry"
        ]
    );
}

#[test]
fn a_retry_list_line_with_another_binary_or_name_passes() {
    let touched = [binary_test()];
    assert_eq!(
        touched_retries("main::other bash::registers\tfirst failure", &touched),
        Vec::<String>::new()
    );
    assert_eq!(
        touched_retries("main::completion bash::other\tfirst failure", &touched),
        Vec::<String>::new()
    );
}

#[test]
fn a_retry_list_line_matching_a_package_scoped_test_fails() {
    let touched = [unit_test()];
    assert_eq!(
        touched_retries(
            "log writer::tests::flushes_on_drop\tfirst failure",
            &touched
        ),
        vec![
            "touched-retries: log writer::tests::flushes_on_drop was added or changed by this pull request and passed only on retry"
        ]
    );
}

#[test]
fn a_retry_list_line_with_the_same_name_in_another_package_passes() {
    let touched = [unit_test()];
    assert_eq!(
        touched_retries(
            "other writer::tests::flushes_on_drop\tfirst failure",
            &touched
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        touched_retries("other::log writer::tests::flushes_on_drop", &touched),
        Vec::<String>::new()
    );
}

#[test]
fn an_empty_retry_list_has_no_failure() {
    assert_eq!(touched_retries("", &[binary_test()]), Vec::<String>::new());
    assert_eq!(
        touched_retries("\n", &[binary_test(), unit_test()]),
        Vec::<String>::new()
    );
}

#[test]
fn a_retry_list_line_without_a_space_is_skipped() {
    assert_eq!(
        touched_retries("main::completion\tfirst failure", &[binary_test()]),
        Vec::<String>::new()
    );
    assert_eq!(
        touched_retries("garbage", &[binary_test(), unit_test()]),
        Vec::<String>::new()
    );
}

#[test]
fn a_retry_list_head_matches_despite_detail_text() {
    let touched = [binary_test(), unit_test()];
    let list =
        "main::completion bash::registers\tfirst failure with spaces\\nand an escaped newline";
    assert_eq!(
        touched_retries(list, &touched),
        vec![
            "touched-retries: main::completion bash::registers was added or changed by this pull request and passed only on retry"
        ]
    );
}

#[test]
fn stress_plan_emits_one_test_line_per_distinct_test() {
    let touched = [
        TestRef {
            package: "main".to_owned(),
            binary: None,
            name: "hub_start::tests::starts".to_owned(),
            unit: false,
        },
        TestRef {
            package: "main".to_owned(),
            binary: None,
            name: "hub_start::tests::offers".to_owned(),
            unit: false,
        },
        TestRef {
            package: "main".to_owned(),
            binary: None,
            name: "hub_start::tests::offers".to_owned(),
            unit: true,
        },
    ];
    assert_eq!(
        plan_test_lines(&touched),
        vec![
            "test\tmain hub_start::tests::offers",
            "test\tmain hub_start::tests::starts",
        ]
    );
}

#[test]
fn a_summary_with_no_tests_names_the_empty_selection() {
    assert_eq!(
        plan_summary(30, &[]),
        "### Stress job: no test added or changed\n"
    );
}

#[test]
fn a_summary_with_one_test_uses_the_singular() {
    assert_eq!(
        plan_summary(100, &["main::completion bash::registers".to_owned()]),
        "### Stress job: 1 test, 100 runs each\n```\nmain::completion bash::registers\n```\n"
    );
}

#[test]
fn a_summary_with_two_tests_uses_the_plural() {
    assert_eq!(
        plan_summary(
            30,
            &[
                "loop::turns resumes_after_pause".to_owned(),
                "log writer::tests::flushes_on_drop".to_owned(),
            ]
        ),
        "### Stress job: 2 tests, 30 runs each\n```\nloop::turns resumes_after_pause\nlog writer::tests::flushes_on_drop\n```\n"
    );
}

#[test]
fn a_named_group_keeps_the_flaky_fix_count_and_rest_runs_the_default() {
    assert_eq!(group_count(Some(Only::Named), "test-only"), 100);
    assert_eq!(group_count(Some(Only::Named), "bug"), 30);
    assert_eq!(group_count(Some(Only::Rest), "test-only"), 30);
    assert_eq!(group_count(Some(Only::Rest), ""), 30);
    assert_eq!(group_count(None, "test-only"), 100);
    assert_eq!(group_count(None, ""), 30);
}

#[test]
fn a_named_summary_names_the_group_and_its_count() {
    assert_eq!(
        group_summary(
            100,
            &["test\tb fold::tests::second".to_owned()],
            Only::Named
        ),
        "### Stress job: 1 test, 100 runs each (named by the linked flake issue)\n```\ntest\tb fold::tests::second\n```\n"
    );
}

#[test]
fn a_rest_summary_names_the_group_and_its_count() {
    assert_eq!(
        group_summary(
            30,
            &[
                "test\ta::t new_test".to_owned(),
                "test\tb fold::tests::second".to_owned(),
            ],
            Only::Rest
        ),
        "### Stress job: 2 tests, 30 runs each (rest)\n```\ntest\ta::t new_test\ntest\tb fold::tests::second\n```\n"
    );
}

#[test]
fn an_empty_group_summary_names_the_group_with_zero_tests() {
    assert_eq!(
        group_summary(100, &[], Only::Named),
        "### Stress job: 0 tests, 100 runs each (named by the linked flake issue)\n```\n```\n"
    );
    assert_eq!(
        group_summary(30, &[], Only::Rest),
        "### Stress job: 0 tests, 30 runs each (rest)\n```\n```\n"
    );
}

#[test]
fn only_parses_the_two_group_names() {
    assert_eq!(parse_only(None), Ok(None));
    assert_eq!(parse_only(Some("named".to_owned())), Ok(Some(Only::Named)));
    assert_eq!(parse_only(Some("rest".to_owned())), Ok(Some(Only::Rest)));
    assert_eq!(
        parse_only(Some("bogus".to_owned())),
        Err("--only \"bogus\" must be named or rest".to_owned())
    );
}

#[test]
fn unmatched_names_lists_names_matching_no_touched_test() {
    let touched = [binary_test(), unit_test()];
    assert_eq!(
        unmatched_names(
            &["bash::registers".to_owned(), "missing::ghost".to_owned()],
            &touched
        ),
        vec!["missing::ghost".to_owned()]
    );
    assert_eq!(
        unmatched_names(&["bash::registers".to_owned()], &touched),
        Vec::<String>::new()
    );
    assert_eq!(unmatched_names(&[], &touched), Vec::<String>::new());
}

#[test]
fn issue_text_names_are_extracted_by_issue_test_names() {
    let temp = fakes::TempDir::new("xtask-stress-issue-text");
    temp.write(
        "issue.txt",
        "The test `main bash::registers` passed after a retry on linux.\n\
         tested deadbeef\n\
         ```\n\
         failure text quoting `other::t`\n\
         ```\n\
         ```\n\
         The test `sneaky::inside` passed after a retry on linux.\n",
    );
    let path = temp.path().join("issue.txt").to_string_lossy().into_owned();
    assert_eq!(
        names_from_issue_text(&path),
        Ok(vec!["bash::registers".to_owned()])
    );
    let missing = temp
        .path()
        .join("missing.txt")
        .to_string_lossy()
        .into_owned();
    assert!(
        names_from_issue_text(&missing).is_err_and(|error| error.contains("missing.txt")),
        "{missing}"
    );
}

#[test]
fn an_empty_issue_text_file_yields_no_names() {
    let temp = fakes::TempDir::new("xtask-stress-issue-text-empty");
    temp.write("issue.txt", "");
    let path = temp.path().join("issue.txt").to_string_lossy().into_owned();
    assert_eq!(names_from_issue_text(&path), Ok(Vec::<String>::new()));
}

const CI_STRESS_OUTPUT_RAW: &str = include_str!("stress_tests/nextest-ci-30.raw");
const CI_STRESS_OUTPUT_STRIPPED: &str = include_str!("stress_tests/nextest-ci-30.txt");

#[test]
fn the_ci_30_iteration_stress_output_passes_raw_and_ansi_stripped() {
    assert!(CI_STRESS_OUTPUT_RAW.contains('\u{1b}'));
    assert_eq!(check_stress_output(CI_STRESS_OUTPUT_RAW, 30, 8), Ok(()));
    assert_eq!(
        check_stress_output(CI_STRESS_OUTPUT_STRIPPED, 30, 8),
        Ok(())
    );
}

#[test]
fn zero_iteration_summaries_fail_even_when_the_run_summary_passes() {
    let summary = CI_STRESS_OUTPUT_STRIPPED
        .lines()
        .find(|line| line.contains("stress run iterations:"))
        .unwrap();
    assert_eq!(
        check_stress_output(summary, 30, 8),
        Err("expected 30 iteration summaries, found 0".to_owned())
    );
}

#[test]
fn a_short_iteration_count_fails() {
    let short = CI_STRESS_OUTPUT_STRIPPED
        .lines()
        .filter(|line| !line.contains("iteration 30/30:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        check_stress_output(&short, 30, 8),
        Err("expected 30 iteration summaries, found 29".to_owned())
    );
}

#[test]
fn an_iteration_that_runs_fewer_tests_than_planned_fails() {
    let fewer = CI_STRESS_OUTPUT_STRIPPED.replacen("8 tests run:", "7 tests run:", 1);
    assert_eq!(
        check_stress_output(&fewer, 30, 8),
        Err("iteration 1/30 ran 7 tests; plan names 8".to_owned())
    );
}

#[test]
fn an_iteration_that_runs_more_tests_than_planned_fails() {
    let more = CI_STRESS_OUTPUT_STRIPPED.replacen("8 tests run:", "9 tests run:", 1);
    assert_eq!(
        check_stress_output(&more, 30, 8),
        Err("iteration 1/30 ran 9 tests; plan names 8".to_owned())
    );
}

#[test]
fn an_iteration_with_a_different_planned_total_fails() {
    let wrong_total = CI_STRESS_OUTPUT_STRIPPED.replacen("iteration 1/30:", "iteration 1/29:", 1);
    assert_eq!(
        check_stress_output(&wrong_total, 30, 8),
        Err("iteration 1/29 does not match the planned count 30".to_owned())
    );
}

#[test]
fn a_missing_run_summary_fails() {
    let without_summary = CI_STRESS_OUTPUT_STRIPPED
        .lines()
        .filter(|line| !line.contains("stress run iterations:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        check_stress_output(&without_summary, 30, 8),
        Err("expected one stress run summary, found 0".to_owned())
    );
}

#[test]
fn run_summary_counts_must_all_match_the_plan() {
    for (summary, expected) in [
        (
            "29/30 stress run iterations: 30 passed",
            "stress run summary says 29/30 iterations and 30 passed; expected 30/30 and 30 passed",
        ),
        (
            "30/29 stress run iterations: 30 passed",
            "stress run summary says 30/29 iterations and 30 passed; expected 30/30 and 30 passed",
        ),
        (
            "30/30 stress run iterations: 29 passed",
            "stress run summary says 30/30 iterations and 29 passed; expected 30/30 and 30 passed",
        ),
    ] {
        let log =
            CI_STRESS_OUTPUT_STRIPPED.replace("30/30 stress run iterations: 30 passed", summary);
        assert_eq!(check_stress_output(&log, 30, 8), Err(expected.to_owned()));
    }
}

#[test]
fn a_failed_or_duplicate_run_summary_fails() {
    let failed = CI_STRESS_OUTPUT_STRIPPED.replace(
        "30/30 stress run iterations: 30 passed",
        "30/30 stress run iterations: 30 failed",
    );
    assert_eq!(
        check_stress_output(&failed, 30, 8),
        Err("expected one stress run summary, found 0".to_owned())
    );

    let summary = CI_STRESS_OUTPUT_STRIPPED
        .lines()
        .find(|line| line.contains("stress run iterations:"))
        .unwrap();
    let duplicate = format!("{CI_STRESS_OUTPUT_STRIPPED}{summary}\n");
    assert_eq!(
        check_stress_output(&duplicate, 30, 8),
        Err("expected one stress run summary, found 2".to_owned())
    );
}

#[test]
fn a_singular_iteration_run_is_counted() {
    let output = " Stress test [   0.001s] iteration 1/1: 1 test run: 1 passed\n     Summary [   0.001s] 1/1 stress run iterations: 1 passed\n";
    assert_eq!(check_stress_output(output, 1, 1), Ok(()));
}

#[test]
fn stress_check_command_returns_its_parser_result() {
    let temp = fakes::TempDir::new("xtask-stress-check");
    temp.write("valid.log", CI_STRESS_OUTPUT_STRIPPED);
    temp.write("invalid.log", "not nextest output");

    let args = |path: &str| {
        vec![
            "--log".to_owned(),
            temp.path().join(path).to_string_lossy().into_owned(),
            "--count".to_owned(),
            "30".to_owned(),
            "--tests".to_owned(),
            "8".to_owned(),
        ]
    };
    assert_eq!(stress_check_command(&args("valid.log")), Ok(true));
    assert_eq!(
        stress_check_command(&args("invalid.log")),
        Err("stress output: expected 30 iteration summaries, found 0".to_owned())
    );
}

fn large_stress_filter() -> String {
    let tests = (0..2754)
        .map(|index| TestRef {
            package: "xtask".to_owned(),
            binary: Some("xtask::stress".to_owned()),
            name: format!("stress_case_{index:04}"),
            unit: false,
        })
        .collect::<Vec<_>>();
    filter(&tests)
}

fn assert_same_without_dumping_large_values(actual: &str, expected: &str) {
    let first_difference = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(actual, expected)| actual != expected)
        .unwrap_or(actual.len().min(expected.len()));
    assert!(
        actual == expected,
        "config differs at byte {first_difference} (actual {} bytes, expected {} bytes)",
        actual.len(),
        expected.len()
    );
}

#[test]
fn stress_config_escapes_toml_basic_string_characters() {
    let actual = stress_config(r#"test(=a\"b\\c)"#);
    let expected = "[profile.stress]\ndefault-filter = \"test(=a\\\\\\\"b\\\\\\\\c)\"\n";
    assert_same_without_dumping_large_values(&actual, expected);
}

#[test]
fn stress_config_keeps_the_full_generated_filter() {
    let filter = large_stress_filter();
    assert!(
        filter.len() > 131_072,
        "generated filter is only {} bytes",
        filter.len()
    );
    let expected = format!("[profile.stress]\ndefault-filter = \"{filter}\"\n");
    let actual = stress_config(&filter);
    assert_same_without_dumping_large_values(&actual, &expected);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_rejects_an_argument_over_max_arg_strlen() {
    let argument = "x".repeat(131_073);
    let error = std::process::Command::new("/bin/true")
        .arg(argument)
        .spawn()
        .expect_err("Linux must reject an argument longer than MAX_ARG_STRLEN");
    assert_eq!(error.raw_os_error(), Some(7), "expected E2BIG");
}
