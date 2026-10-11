use super::*;
use crate::select::tests::strings;

fn jobs(
    lint: bool,
    test: bool,
    mutants: bool,
    bug_red: bool,
    release: bool,
) -> BTreeMap<&'static str, bool> {
    BTreeMap::from([
        ("lint", lint),
        ("test", test),
        ("mutants", mutants),
        ("bug_red", bug_red),
        ("release", release),
    ])
}

#[test]
fn a_docs_only_pull_request_runs_no_job_after_the_selection() {
    let plan = plan("docs", &[], "pull_request", true, true, 100);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_code_pull_request_runs_what_it_selected() {
    let plan = plan(
        "crates",
        &strings(&["log"]),
        "pull_request",
        true,
        true,
        100,
    );
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, true, false),
            shards: 7
        }
    );
}

#[test]
fn a_pull_request_without_a_bug_label_skips_the_bug_check() {
    let plan = plan("all", &strings(&["log"]), "pull_request", false, true, 100);
    assert_eq!(plan.jobs, jobs(true, true, true, false, false));
}

#[test]
fn a_pull_request_that_selects_no_crate_skips_the_tests() {
    let plan = plan("crates", &[], "pull_request", false, true, 100);
    assert_eq!(plan.jobs, jobs(true, false, false, false, false));
    assert_eq!(plan.shards, 0);
}

#[test]
fn an_unlabelled_draft_skips_mutants_and_runs_the_rest() {
    let plan = plan(
        "crates",
        &strings(&["log"]),
        "pull_request",
        false,
        false,
        100,
    );
    assert!(!plan.jobs["mutants"]);
    assert_eq!(plan.shards, 0);
    assert!(plan.jobs["lint"] && plan.jobs["test"]);
}

#[test]
fn a_docs_push_runs_lint_and_tests_but_no_mutants() {
    let plan = plan("docs", &[], "push", true, true, 100);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, false, false, true),
            shards: 0
        }
    );
}

#[test]
fn a_code_push_runs_lint_tests_and_release_but_no_mutants_or_bug_check() {
    let plan = plan("all", &strings(&["log"]), "push", true, true, 1000);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, false, false, true),
            shards: 0
        }
    );
}

#[test]
fn a_pull_request_that_selects_the_binary_runs_the_release_job() {
    let plan = plan(
        "crates",
        &strings(&["log", "main"]),
        "pull_request",
        false,
        true,
        100,
    );
    assert_eq!(plan.jobs, jobs(true, true, true, false, true));
}

#[test]
fn a_pull_request_that_runs_everything_runs_the_release_job() {
    let packages = strings(&["config", "log", "main", "xtask"]);
    let plan = plan("all", &packages, "pull_request", false, true, 100);
    assert_eq!(plan.jobs, jobs(true, true, true, false, true));
}

#[test]
fn a_pull_request_without_the_binary_skips_the_release_job() {
    let plan = plan(
        "crates",
        &strings(&["log", "xtask"]),
        "pull_request",
        false,
        true,
        100,
    );
    assert_eq!(plan.jobs, jobs(true, true, true, false, false));
}

fn results(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn selected() -> BTreeMap<String, bool> {
    BTreeMap::from([
        ("docs".to_owned(), true),
        ("test".to_owned(), true),
        ("mutants".to_owned(), false),
    ])
}

#[test]
fn ci_passes_when_selected_jobs_passed_and_the_rest_skipped() {
    let needs = results(&[
        ("select", "success"),
        ("docs", "success"),
        ("test", "success"),
        ("mutants", "skipped"),
    ]);
    assert_eq!(verdict(&needs, &selected()), Vec::<String>::new());
}

#[test]
fn ci_fails_when_a_selected_job_failed_or_was_skipped() {
    for result in ["failure", "cancelled", "skipped"] {
        let needs = results(&[
            ("select", "success"),
            ("docs", "success"),
            ("test", result),
            ("mutants", "skipped"),
        ]);
        assert_eq!(
            verdict(&needs, &selected()),
            vec![format!("test: selected, but {result}")]
        );
    }
}

#[test]
fn ci_fails_when_an_unselected_job_ran() {
    let needs = results(&[
        ("select", "success"),
        ("docs", "success"),
        ("test", "success"),
        ("mutants", "success"),
    ]);
    assert_eq!(
        verdict(&needs, &selected()),
        vec!["mutants: not selected, but success".to_owned()]
    );
}

#[test]
fn ci_fails_when_the_selection_fails() {
    let needs = results(&[
        ("select", "failure"),
        ("docs", "skipped"),
        ("test", "skipped"),
        ("mutants", "skipped"),
    ]);
    assert_eq!(
        verdict(&needs, &BTreeMap::new()),
        vec!["select: failure, so the selection failed".to_owned()]
    );
    assert_eq!(
        verdict(&BTreeMap::new(), &selected()),
        vec!["select: missing, so the selection failed".to_owned()]
    );
}

#[test]
fn ci_fails_on_a_job_the_selection_does_not_name() {
    let needs = results(&[
        ("select", "success"),
        ("docs", "success"),
        ("test", "success"),
        ("mutants", "skipped"),
        ("extra", "skipped"),
    ]);
    assert_eq!(
        verdict(&needs, &selected()),
        vec!["extra: not in the selection".to_owned()]
    );
}

#[test]
fn shards_grow_with_the_mutant_count_between_one_and_the_cap() {
    // (mutants, shards): each boundary of MUTANTS_PER_SHARD and the cap.
    let table = [
        (0, 0),
        (1, 1),
        (15, 1),
        (16, 2),
        (30, 2),
        (31, 3),
        (240, 16),
        (241, 16),
        (319, 16),
        (480, 16),
        (481, 16),
        (100_000, 16),
        (u64::MAX, 16),
    ];
    for (count, shards) in table {
        assert_eq!(mutant_shards(count), shards, "{count} mutants");
    }
}

#[test]
fn shard_timeouts_grow_with_the_largest_shard_past_the_cap() {
    // (mutants, minutes): 20 minutes per 15 of the largest shard's
    // mutants, rounded up, at most 360.
    let table = [
        (0, 20),
        (1, 20),
        (15, 20),
        (240, 20),
        (241, 22),
        (256, 22),
        (257, 23),
        (480, 40),
        (4320, 360),
        (4321, 360),
        (100_000, 360),
        (u64::MAX, 360),
    ];
    for (count, minutes) in table {
        assert_eq!(shard_timeout_minutes(count), minutes, "{count} mutants");
    }
}

#[test]
fn a_selected_run_with_no_mutants_starts_no_shard() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, true, 0);
    assert!(!plan.jobs["mutants"]);
    assert_eq!(plan.shards, 0);
}
