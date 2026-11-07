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
    input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[(A, pulls)],
        &[(H1, checks)],
    )
}

fn unreadable(input: &Value) -> String {
    audit(input, NOW).expect_err("the input is unreadable")
}

#[test]
fn green_merges_print_only_the_count() {
    let input = input(
        include_str!("../fixtures/break_glass_audit/commits_two.json"),
        &[
            (
                A,
                include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
            ),
            (
                B,
                include_str!("../fixtures/break_glass_audit/pulls_second.json"),
            ),
        ],
        &[
            (
                H1,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            ),
            (
                H2,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            ),
        ],
    );
    let report = audit(&input, NOW).expect("readable");
    assert_eq!(report.render(), [COUNT_NONE]);
    assert!(report.passed());
}

#[test]
fn an_undeclared_merge_without_green_ci_fails_and_is_listed() {
    let input = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_failure.json"),
    );
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
    let input = one(
        include_str!("../fixtures/break_glass_audit/pulls_declared.json"),
        include_str!("../fixtures/break_glass_audit/checks_failure.json"),
    );
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
        include_str!("../fixtures/break_glass_audit/commits_two.json"),
        &[
            (
                A,
                include_str!("../fixtures/break_glass_audit/pulls_declared.json"),
            ),
            (
                B,
                include_str!("../fixtures/break_glass_audit/pulls_second.json"),
            ),
        ],
        &[
            (
                H1,
                include_str!("../fixtures/break_glass_audit/checks_failure.json"),
            ),
            (
                H2,
                include_str!("../fixtures/break_glass_audit/checks_failure.json"),
            ),
        ],
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
        (
            include_str!("../fixtures/break_glass_audit/pulls_lowercase.json"),
            "#4101",
        ),
        (
            include_str!("../fixtures/break_glass_audit/pulls_leading_space.json"),
            "#4101",
        ),
        (
            include_str!("../fixtures/break_glass_audit/pulls_null_body.json"),
            "-",
        ),
    ] {
        let lines = rendered(&one(
            pulls,
            include_str!("../fixtures/break_glass_audit/checks_failure.json"),
        ));
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
        include_str!("../fixtures/break_glass_audit/checks_absent.json"),
        include_str!("../fixtures/break_glass_audit/checks_in_progress.json"),
        include_str!("../fixtures/break_glass_audit/checks_in_progress_success.json"),
        include_str!("../fixtures/break_glass_audit/checks_draft_only.json"),
        include_str!("../fixtures/break_glass_audit/checks_cancelled.json"),
        include_str!("../fixtures/break_glass_audit/checks_failure.json"),
    ] {
        let report = audit(
            &one(
                include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
                checks,
            ),
            NOW,
        )
        .expect("readable");
        assert_eq!(report.without_green, 1, "{checks}");
        assert!(!report.passed(), "{checks}");
    }
}

#[test]
fn a_completed_successful_ci_run_is_green() {
    let report = audit(
        &one(
            include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
            include_str!("../fixtures/break_glass_audit/checks_success.json"),
        ),
        NOW,
    )
    .expect("readable");
    assert_eq!(report.without_green, 0);
    assert!(report.passed());
}

#[test]
fn the_newest_ci_run_decides_wherever_it_is_listed() {
    for (checks, green) in [
        (
            include_str!("../fixtures/break_glass_audit/checks_newer_failure.json"),
            false,
        ),
        (
            include_str!("../fixtures/break_glass_audit/checks_newer_success.json"),
            true,
        ),
        (
            include_str!("../fixtures/break_glass_audit/checks_newer_success_first.json"),
            true,
        ),
        // A newer `CI (draft)` success is not a `CI` run.
        (
            include_str!("../fixtures/break_glass_audit/checks_draft_newer.json"),
            false,
        ),
        // Runs sharing an id: the first listed decides.
        (
            include_str!("../fixtures/break_glass_audit/checks_tied_id.json"),
            true,
        ),
    ] {
        let report = audit(
            &one(
                include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
                checks,
            ),
            NOW,
        )
        .expect("readable");
        assert_eq!(report.without_green == 0, green, "{checks}");
    }
}

#[test]
fn a_commit_with_no_merged_pull_on_main_is_a_direct_push() {
    for pulls in [
        include_str!("../fixtures/break_glass_audit/pulls_empty.json"),
        include_str!("../fixtures/break_glass_audit/pulls_unmerged.json"),
        include_str!("../fixtures/break_glass_audit/pulls_other_base.json"),
    ] {
        // No check-runs lookup exists: a push has no head to read.
        let input = input(
            include_str!("../fixtures/break_glass_audit/commits_one.json"),
            &[(A, pulls)],
            &[],
        );
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
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_several.json"),
        )],
        &[(
            H1,
            include_str!("../fixtures/break_glass_audit/checks_failure.json"),
        )],
    );
    assert_eq!(
        rendered(&input)[0],
        format!("merge {A} head {H1} PR #4002 ticket #4102 undeclared")
    );
}

#[test]
fn a_pull_on_the_second_page_of_the_pulls_response_is_found() {
    let input = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_two_pages.json"),
        )],
        &[(
            H1,
            include_str!("../fixtures/break_glass_audit/checks_failure.json"),
        )],
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
fn the_cutoff_is_exact_past_the_century_and_400_year_leap_days() {
    // 2100 is not a leap year: the century term of the civil-date maths bites.
    assert_eq!(
        window_since(4_107_542_400 + 172_800),
        "2100-03-01T00:00:00Z"
    );
    assert_eq!(
        window_since(4_107_542_399 + 172_800),
        "2100-02-28T23:59:59Z"
    );
    // 2400 is: its leap day is the last day of the 400-year era.
    assert_eq!(
        window_since(13_574_608_245 + 172_800),
        "2400-02-29T12:30:45Z"
    );
    assert_eq!(
        window_since(13_574_649_600 + 172_800),
        "2400-03-01T00:00:00Z"
    );
}

#[test]
fn a_commit_older_than_the_window_is_not_audited() {
    // One second before the cutoff. No pulls lookup exists for it.
    let input = input(
        include_str!("../fixtures/break_glass_audit/commits_just_before_cutoff.json"),
        &[],
        &[],
    );
    assert_eq!(
        rendered(&input),
        ["audited 0 merges, 0 without green CI, 0 undeclared"]
    );
}

#[test]
fn a_commit_exactly_on_the_cutoff_is_audited() {
    let input = input(
        include_str!("../fixtures/break_glass_audit/commits_on_cutoff.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        )],
        &[(
            H1,
            include_str!("../fixtures/break_glass_audit/checks_success.json"),
        )],
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
        include_str!("../fixtures/break_glass_audit/commits_inside_window.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        )],
        &[(
            H1,
            include_str!("../fixtures/break_glass_audit/checks_success.json"),
        )],
    );
    assert_eq!(
        rendered(&input),
        ["audited 1 merges, 0 without green CI, 0 undeclared"]
    );
}

#[test]
fn commits_arriving_as_pages_are_all_read() {
    let input = input(
        include_str!("../fixtures/break_glass_audit/commits_pages.json"),
        &[
            (
                A,
                include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
            ),
            (
                B,
                include_str!("../fixtures/break_glass_audit/pulls_second.json"),
            ),
        ],
        &[
            (
                H1,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            ),
            (
                H2,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            ),
        ],
    );
    assert_eq!(rendered(&input), [COUNT_NONE]);
}

#[test]
fn an_empty_window_passes() {
    for commits in [
        include_str!("../fixtures/break_glass_audit/commits_empty.json"),
        include_str!("../fixtures/break_glass_audit/commits_empty_page.json"),
    ] {
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
    let mut commits = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_success.json"),
    );
    commits["commits"] = failed.clone();
    assert!(unreadable(&commits).starts_with("commits:"));

    let mut pulls = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_success.json"),
    );
    pulls["pulls"][A] = failed.clone();
    assert!(unreadable(&pulls).starts_with("pulls"));

    let mut checks = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_success.json"),
    );
    checks["checks"][H1] = failed;
    assert!(unreadable(&checks).starts_with("check-runs"));
}

#[test]
fn a_lookup_without_a_flag_or_body_is_an_error() {
    let mut no_flag = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_success.json"),
    );
    no_flag["commits"] = json!({"body": []});
    unreadable(&no_flag);

    let mut no_body = one(
        include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        include_str!("../fixtures/break_glass_audit/checks_success.json"),
    );
    no_body["commits"] = json!({"ok": true});
    unreadable(&no_body);
}

#[test]
fn a_missing_lookup_for_a_needed_commit_or_head_is_an_error() {
    let no_pulls = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[],
        &[],
    );
    assert!(unreadable(&no_pulls).contains(A));

    let no_checks = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
        )],
        &[],
    );
    assert!(unreadable(&no_checks).contains(H1));

    let no_commits = json!({"pulls": {}, "checks": {}});
    unreadable(&no_commits);
}

#[test]
fn a_pulls_response_of_the_wrong_shape_is_an_error() {
    for pulls in [
        include_str!("../fixtures/break_glass_audit/pulls_object.json"),
        include_str!("../fixtures/break_glass_audit/pulls_null.json"),
        include_str!("../fixtures/break_glass_audit/pulls_missing_merged_at.json"),
        include_str!("../fixtures/break_glass_audit/pulls_missing_head.json"),
        include_str!("../fixtures/break_glass_audit/pulls_missing_body.json"),
        include_str!("../fixtures/break_glass_audit/pulls_null_entry.json"),
    ] {
        let input = input(
            include_str!("../fixtures/break_glass_audit/commits_one.json"),
            &[(A, pulls)],
            &[(
                H1,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            )],
        );
        let message = unreadable(&input);
        assert!(message.starts_with("pulls:"), "{pulls}: {message}");
    }
}

#[test]
fn a_check_runs_response_of_the_wrong_shape_is_an_error() {
    for checks in [
        include_str!("../fixtures/break_glass_audit/checks_null_runs.json"),
        include_str!("../fixtures/break_glass_audit/checks_missing_runs.json"),
        include_str!("../fixtures/break_glass_audit/checks_missing_total.json"),
        include_str!("../fixtures/break_glass_audit/checks_total_exceeds.json"),
        include_str!("../fixtures/break_glass_audit/checks_missing_status.json"),
        include_str!("../fixtures/break_glass_audit/checks_array.json"),
        include_str!("../fixtures/break_glass_audit/checks_bad_id.json"),
    ] {
        let message = unreadable(&one(
            include_str!("../fixtures/break_glass_audit/pulls_plain.json"),
            checks,
        ));
        assert!(message.starts_with("check-runs:"), "{checks}: {message}");
    }
}

#[test]
fn a_commits_response_of_the_wrong_shape_is_an_error() {
    for commits in [
        include_str!("../fixtures/break_glass_audit/commits_object.json"),
        include_str!("../fixtures/break_glass_audit/commits_no_date.json"),
    ] {
        let message = unreadable(&input(commits, &[], &[]));
        assert!(message.starts_with("commits:"), "{commits}: {message}");
    }
    let mut odd_date = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[],
        &[],
    );
    odd_date["commits"]["body"][0]["commit"]["committer"]["date"] =
        json!("2026-11-06T20:00:00+00:00");
    assert!(unreadable(&odd_date).starts_with("commits:"));

    let mut fractional = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[],
        &[],
    );
    fractional["commits"]["body"][0]["commit"]["committer"]["date"] =
        json!("2026-11-06T20:00:00.000Z");
    assert!(unreadable(&fractional).starts_with("commits:"));

    let mut no_sha = input(
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[],
        &[],
    );
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
        include_str!("../fixtures/break_glass_audit/commits_one.json"),
        &[(
            A,
            include_str!("../fixtures/break_glass_audit/pulls_several.json"),
        )],
        &[
            (
                H1,
                include_str!("../fixtures/break_glass_audit/checks_success.json"),
            ),
            (
                H2,
                include_str!("../fixtures/break_glass_audit/checks_failure.json"),
            ),
            (
                H3,
                include_str!("../fixtures/break_glass_audit/checks_failure.json"),
            ),
        ],
    );
    assert_eq!(
        rendered(&input),
        ["audited 1 merges, 0 without green CI, 0 undeclared"]
    );
}
