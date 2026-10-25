use super::*;

#[test]
fn an_empty_label_list_runs_the_pilot_default() {
    assert_eq!(stress_count(""), STRESS_COUNT);
    assert_eq!(stress_count(""), 30);
}

#[test]
fn a_label_list_without_test_only_runs_the_pilot_default() {
    assert_eq!(stress_count("bug"), STRESS_COUNT);
    assert_eq!(stress_count("bug\nneeds-triage"), 30);
}

#[test]
fn a_test_only_label_runs_the_flaky_fix_count() {
    assert_eq!(stress_count("test-only"), TEST_ONLY_COUNT);
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
