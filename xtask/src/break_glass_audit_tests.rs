//! Tests for `super::{audit, window_since, parse_args, parse_input}`: one
//! per rule, each failing when its comparison, condition or branch flips.
//! The input is the checked-in `gh` responses under
//! `xtask/fixtures/break_glass_audit/`; no `gh` runs.

use serde_json::{Value, json};

use super::{audit, declared, parse_args, parse_input, ticket, window_since};

/// 2026-11-06T21:20:00Z; the window opens at 2026-11-04T21:20:00Z.
const NOW: u64 = 1_794_000_000;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const H1: &str = "1111111111111111111111111111111111111111";
const H2: &str = "2222222222222222222222222222222222222222";
const H3: &str = "3333333333333333333333333333333333333333";

const COUNT_NONE: &str = "audited 2 merges, 0 without green CI, 0 undeclared";

macro_rules! fx {
    ($name:literal) => {
        include_str!(concat!("../fixtures/break_glass_audit/", $name, ".json"))
    };
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("a fixture is valid JSON")
}

/// A successful lookup around a fixture, as the workflow step wraps it.
fn lookup(text: &str) -> Value {
    json!({"ok": true, "body": parse(text)})
}

fn lookups(entries: &[(&str, &str)]) -> Value {
    entries
        .iter()
        .map(|(key, text)| ((*key).to_owned(), lookup(text)))
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

fn input(commits: &str, pulls: &[(&str, &str)], checks: &[(&str, &str)]) -> Value {
    json!({"commits": lookup(commits), "pulls": lookups(pulls), "checks": lookups(checks)})
}

/// The rendered report of a run that must be readable.
fn rendered(input: &Value) -> Vec<String> {
    audit(input, NOW).expect("the input is readable").render()
}

/// One audited commit `A` whose only PR is `pulls`, checked by `checks`.
fn one(pulls: &str, checks: &str) -> Value {
    input(fx!("commits_one"), &[(A, pulls)], &[(H1, checks)])
}

fn unreadable(input: &Value) -> String {
    audit(input, NOW).expect_err("the input is unreadable")
}

#[test]
fn green_merges_print_only_the_count() {
    let input = input(
        fx!("commits_two"),
        &[(A, fx!("pulls_plain")), (B, fx!("pulls_second"))],
        &[(H1, fx!("checks_success")), (H2, fx!("checks_success"))],
    );
    let report = audit(&input, NOW).expect("readable");
    assert_eq!(report.render(), [COUNT_NONE]);
    assert!(report.passed());
}

#[test]
fn an_undeclared_merge_without_green_ci_fails_and_is_listed() {
    let input = one(fx!("pulls_plain"), fx!("checks_failure"));
    let report = audit(&input, NOW).expect("readable");
    assert_eq!(
        report.render(),
        [
            format!("merge {A} head {H1} PR #4021 ticket #4101 undeclared"),
            "audited 1 merges, 1 without green CI, 1 undeclared".to_owned(),
        ]
    );
    assert!(!report.passed());
}

#[test]
fn a_declared_merge_is_listed_and_passes() {
    let input = one(fx!("pulls_declared"), fx!("checks_failure"));
    let report = audit(&input, NOW).expect("readable");
    assert_eq!(
        report.render(),
        [
            format!("merge {A} head {H1} PR #4021 ticket #4101 declared"),
            "audited 1 merges, 1 without green CI, 0 undeclared".to_owned(),
        ]
    );
    assert!(report.passed());
}

#[test]
fn one_undeclared_merge_among_declared_ones_fails_the_run() {
    let input = input(
        fx!("commits_two"),
        &[(A, fx!("pulls_declared")), (B, fx!("pulls_second"))],
        &[(H1, fx!("checks_failure")), (H2, fx!("checks_failure"))],
    );
    let report = audit(&input, NOW).expect("readable");
    assert_eq!(
        report.render(),
        [
            format!("merge {A} head {H1} PR #4021 ticket #4101 declared"),
            format!("merge {B} head {H2} PR #4022 ticket #4102 undeclared"),
            "audited 2 merges, 2 without green CI, 1 undeclared".to_owned(),
        ]
    );
    assert!(!report.passed());
    // Only the undeclared merge is annotated as an error.
    assert_eq!(
        report.failing().collect::<Vec<_>>(),
        [&format!(
            "merge {B} head {H2} PR #4022 ticket #4102 undeclared"
        )]
    );
}

#[test]
fn only_the_exact_prefix_declares_break_glass() {
    assert!(declared(Some("Break-glass: #4101")));
    assert!(declared(Some("Break-glass:")));
    assert!(!declared(Some("break-glass: #4101")));
    assert!(!declared(Some(" Break-glass: #4101")));
    assert!(!declared(Some("Break-glass #4101")));
    assert!(!declared(Some("BREAK-GLASS: #4101")));
    assert!(!declared(Some("")));
    assert!(!declared(None));
}

#[test]
fn near_miss_bodies_stay_undeclared_in_a_whole_run() {
    for (pulls, ticket_text) in [
        (fx!("pulls_lowercase"), "#4101"),
        (fx!("pulls_leading_space"), "#4101"),
        (fx!("pulls_null_body"), "-"),
    ] {
        let lines = rendered(&one(pulls, fx!("checks_failure")));
        assert_eq!(
            lines[0],
            format!("merge {A} head {H1} PR #4021 ticket {ticket_text} undeclared")
        );
    }
}

#[test]
fn the_ticket_is_the_first_hash_number_in_the_body() {
    assert_eq!(ticket(Some("Resolves #4101\n")), "#4101");
    assert_eq!(ticket(Some("Break-glass: #4101 blocks #4090")), "#4101");
    assert_eq!(ticket(Some("See #4101 and #4102")), "#4101");
    assert_eq!(ticket(Some("# heading, then #12.")), "#12");
    assert_eq!(ticket(Some("#x then #7")), "#7");
    assert_eq!(ticket(Some("ends with #")), "-");
    assert_eq!(ticket(Some("no ticket here")), "-");
    assert_eq!(ticket(Some("")), "-");
    assert_eq!(ticket(None), "-");
}

#[test]
fn a_head_without_a_completed_successful_ci_run_is_not_green() {
    for checks in [
        fx!("checks_absent"),
        fx!("checks_in_progress"),
        fx!("checks_in_progress_success"),
        fx!("checks_draft_only"),
        fx!("checks_cancelled"),
        fx!("checks_failure"),
    ] {
        let report = audit(&one(fx!("pulls_plain"), checks), NOW).expect("readable");
        assert_eq!(report.without_green, 1, "{checks}");
        assert!(!report.passed(), "{checks}");
    }
}

#[test]
fn a_completed_successful_ci_run_is_green() {
    let report = audit(&one(fx!("pulls_plain"), fx!("checks_success")), NOW).expect("readable");
    assert_eq!(report.without_green, 0);
    assert!(report.passed());
}

#[test]
fn the_newest_ci_run_decides_wherever_it_is_listed() {
    for (checks, green) in [
        (fx!("checks_newer_failure"), false),
        (fx!("checks_newer_success"), true),
        (fx!("checks_newer_success_first"), true),
        // A newer `CI (draft)` success is not a `CI` run.
        (fx!("checks_draft_newer"), false),
    ] {
        let report = audit(&one(fx!("pulls_plain"), checks), NOW).expect("readable");
        assert_eq!(report.without_green == 0, green, "{checks}");
    }
}

#[test]
fn a_commit_with_no_merged_pull_on_main_is_a_direct_push() {
    for pulls in [
        fx!("pulls_empty"),
        fx!("pulls_unmerged"),
        fx!("pulls_other_base"),
    ] {
        // No check-runs lookup exists: a push has no head to read.
        let input = input(fx!("commits_one"), &[(A, pulls)], &[]);
        let report = audit(&input, NOW).expect("readable");
        assert_eq!(
            report.render(),
            [
                format!("merge {A} head none PR none ticket - undeclared"),
                "audited 1 merges, 1 without green CI, 1 undeclared".to_owned(),
            ],
            "{pulls}"
        );
        assert!(!report.passed());
    }
}

#[test]
fn the_first_merged_pull_on_main_is_the_commits_pull() {
    // #4001 is unmerged, #4002 is the first merged, #4003 comes later.
    let input = input(
        fx!("commits_one"),
        &[(A, fx!("pulls_several"))],
        &[(H1, fx!("checks_failure"))],
    );
    assert_eq!(
        rendered(&input)[0],
        format!("merge {A} head {H1} PR #4002 ticket #4102 undeclared")
    );
}

#[test]
fn a_pull_on_the_second_page_of_the_pulls_response_is_found() {
    let input = input(
        fx!("commits_one"),
        &[(A, fx!("pulls_two_pages"))],
        &[(H1, fx!("checks_failure"))],
    );
    assert_eq!(
        rendered(&input)[0],
        format!("merge {A} head {H1} PR #4021 ticket #4101 undeclared")
    );
}

#[test]
fn the_window_opens_exactly_two_days_before_now() {
    assert_eq!(window_since(NOW), "2026-11-04T21:20:00Z");
    assert_eq!(window_since(172_800), "1970-01-01T00:00:00Z");
    // The cutoff on a leap day, across a year end and across a month end.
    assert_eq!(window_since(951_782_400 + 172_800), "2000-02-29T00:00:00Z");
    assert_eq!(window_since(1_767_225_600), "2025-12-30T00:00:00Z");
    assert_eq!(window_since(1_000_000_000), "2001-09-07T01:46:40Z");
    assert_eq!(
        window_since(1_772_323_200 + 172_800),
        "2026-03-01T00:00:00Z"
    );
}

#[test]
fn a_commit_older_than_the_window_is_not_audited() {
    // One second before the cutoff. No pulls lookup exists for it.
    let input = input(fx!("commits_just_before_cutoff"), &[], &[]);
    assert_eq!(
        rendered(&input),
        ["audited 0 merges, 0 without green CI, 0 undeclared"]
    );
}

#[test]
fn a_commit_exactly_on_the_cutoff_is_audited() {
    let input = input(
        fx!("commits_on_cutoff"),
        &[(A, fx!("pulls_plain"))],
        &[(H1, fx!("checks_success"))],
    );
    assert_eq!(
        rendered(&input),
        ["audited 1 merges, 0 without green CI, 0 undeclared"]
    );
}

#[test]
fn a_commit_inside_the_window_is_audited() {
    // 47 h 59 m old.
    let input = input(
        fx!("commits_inside_window"),
        &[(A, fx!("pulls_plain"))],
        &[(H1, fx!("checks_success"))],
    );
    assert_eq!(
        rendered(&input),
        ["audited 1 merges, 0 without green CI, 0 undeclared"]
    );
}

#[test]
fn commits_arriving_as_pages_are_all_read() {
    let input = input(
        fx!("commits_pages"),
        &[(A, fx!("pulls_plain")), (B, fx!("pulls_second"))],
        &[(H1, fx!("checks_success")), (H2, fx!("checks_success"))],
    );
    assert_eq!(rendered(&input), [COUNT_NONE]);
}

#[test]
fn an_empty_window_passes() {
    for commits in [fx!("commits_empty"), fx!("commits_empty_page")] {
        let report = audit(&input(commits, &[], &[]), NOW).expect("readable");
        assert_eq!(
            report.render(),
            ["audited 0 merges, 0 without green CI, 0 undeclared"]
        );
        assert!(report.passed());
    }
}

#[test]
fn a_failed_lookup_is_an_error_never_a_pass() {
    let failed = json!({"ok": false});
    let mut commits = one(fx!("pulls_plain"), fx!("checks_success"));
    commits["commits"] = failed.clone();
    assert!(unreadable(&commits).starts_with("commits:"));

    let mut pulls = one(fx!("pulls_plain"), fx!("checks_success"));
    pulls["pulls"][A] = failed.clone();
    assert!(unreadable(&pulls).starts_with("pulls"));

    let mut checks = one(fx!("pulls_plain"), fx!("checks_success"));
    checks["checks"][H1] = failed;
    assert!(unreadable(&checks).starts_with("check-runs"));
}

#[test]
fn a_lookup_without_a_flag_or_body_is_an_error() {
    let mut no_flag = one(fx!("pulls_plain"), fx!("checks_success"));
    no_flag["commits"] = json!({"body": []});
    unreadable(&no_flag);

    let mut no_body = one(fx!("pulls_plain"), fx!("checks_success"));
    no_body["commits"] = json!({"ok": true});
    unreadable(&no_body);
}

#[test]
fn a_missing_lookup_for_a_needed_commit_or_head_is_an_error() {
    let no_pulls = input(fx!("commits_one"), &[], &[]);
    assert!(unreadable(&no_pulls).contains(A));

    let no_checks = input(fx!("commits_one"), &[(A, fx!("pulls_plain"))], &[]);
    assert!(unreadable(&no_checks).contains(H1));

    let no_commits = json!({"pulls": {}, "checks": {}});
    unreadable(&no_commits);
}

#[test]
fn a_pulls_response_of_the_wrong_shape_is_an_error() {
    for pulls in [
        fx!("pulls_object"),
        fx!("pulls_null"),
        fx!("pulls_missing_merged_at"),
        fx!("pulls_missing_head"),
        fx!("pulls_missing_body"),
        fx!("pulls_null_entry"),
    ] {
        let input = input(
            fx!("commits_one"),
            &[(A, pulls)],
            &[(H1, fx!("checks_success"))],
        );
        let message = unreadable(&input);
        assert!(message.starts_with("pulls:"), "{pulls}: {message}");
    }
}

#[test]
fn a_check_runs_response_of_the_wrong_shape_is_an_error() {
    for checks in [
        fx!("checks_null_runs"),
        fx!("checks_missing_runs"),
        fx!("checks_missing_total"),
        fx!("checks_total_exceeds"),
        fx!("checks_missing_status"),
        fx!("checks_array"),
        fx!("checks_bad_id"),
    ] {
        let message = unreadable(&one(fx!("pulls_plain"), checks));
        assert!(message.starts_with("check-runs:"), "{checks}: {message}");
    }
}

#[test]
fn a_commits_response_of_the_wrong_shape_is_an_error() {
    for commits in [fx!("commits_object"), fx!("commits_no_date")] {
        let message = unreadable(&input(commits, &[], &[]));
        assert!(message.starts_with("commits:"), "{commits}: {message}");
    }
    let mut odd_date = input(fx!("commits_one"), &[], &[]);
    odd_date["commits"]["body"][0]["commit"]["committer"]["date"] =
        json!("2026-11-06T20:00:00+00:00");
    assert!(unreadable(&odd_date).starts_with("commits:"));

    let mut fractional = input(fx!("commits_one"), &[], &[]);
    fractional["commits"]["body"][0]["commit"]["committer"]["date"] =
        json!("2026-11-06T20:00:00.000Z");
    assert!(unreadable(&fractional).starts_with("commits:"));

    let mut no_sha = input(fx!("commits_one"), &[], &[]);
    no_sha["commits"]["body"][0] =
        json!({"commit": {"committer": {"date": "2026-11-06T20:00:00Z"}}});
    assert!(unreadable(&no_sha).starts_with("commits:"));
}

#[test]
fn input_that_is_not_json_is_an_error_naming_the_command() {
    let message = parse_input("{not json").expect_err("invalid JSON");
    assert!(message.starts_with("break-glass-audit:"), "{message}");
    assert!(parse_input("{}").is_ok());
}

fn args(list: &[&str]) -> Result<super::Args, String> {
    parse_args(&list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
}

#[test]
fn the_arguments_are_since_and_now_only() {
    let plain = args(&[]).expect("no arguments");
    assert!(!plain.since);
    assert_eq!(plain.now, None);

    let since = args(&["--since", "--now", "5"]).expect("both flags");
    assert!(since.since);
    assert_eq!(since.now, Some(5));

    assert_eq!(args(&["--now", "7"]).expect("now").now, Some(7));
    assert!(args(&["--since"]).expect("since").since);
}

#[test]
fn an_unknown_or_malformed_argument_is_a_usage_error() {
    for bad in [
        &["--bogus"][..],
        &["--now"],
        &["--now", "soon"],
        &["--now", "-1"],
        &["--since", "extra"],
    ] {
        assert!(args(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_later_merged_pull_is_never_read() {
    // `H3` heads the unmerged #4001 and `H2` the later merged #4003; only
    // `H1`, the first merged pull's head, is read.
    let input = input(
        fx!("commits_one"),
        &[(A, fx!("pulls_several"))],
        &[
            (H1, fx!("checks_success")),
            (H2, fx!("checks_failure")),
            (H3, fx!("checks_failure")),
        ],
    );
    assert_eq!(
        rendered(&input),
        ["audited 1 merges, 0 without green CI, 0 undeclared"]
    );
}
