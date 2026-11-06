use super::*;
use crate::select::tests::strings;

fn jobs(
    lint: bool,
    test_linux: bool,
    test: bool,
    mutants: bool,
    bug_red: bool,
    release: bool,
    stress: bool,
) -> BTreeMap<&'static str, bool> {
    BTreeMap::from([
        ("lint", lint),
        ("test_linux", test_linux),
        ("test", test),
        ("mutants", mutants),
        ("bug_red", bug_red),
        ("release", release),
        ("stress", stress),
    ])
}

#[test]
fn a_docs_only_pull_request_runs_no_job_after_the_selection() {
    let plan = plan("docs", &[], "pull_request", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_code_pull_request_runs_lint_and_linux_tests_only() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, false, false, true, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_pull_request_without_a_bug_label_skips_the_bug_check() {
    let plan = plan("all", &strings(&["log"]), "pull_request", false, false);
    assert_eq!(
        plan.jobs,
        jobs(true, true, false, false, false, false, false)
    );
}

#[test]
fn a_pull_request_that_selects_no_crate_skips_the_tests() {
    let plan = plan("crates", &[], "pull_request", false, false);
    assert_eq!(
        plan.jobs,
        jobs(true, false, false, false, false, false, false)
    );
    assert_eq!(plan.shards, 0);
}

#[test]
fn a_draft_pull_request_skips_test_but_runs_test_linux() {
    let draft = plan("crates", &strings(&["log"]), "pull_request", true, true);
    let ready = plan("crates", &strings(&["log"]), "pull_request", false, true);
    assert!(!draft.jobs["test"]);
    assert!(draft.jobs["test_linux"]);
    for job in ["lint", "test_linux", "mutants", "bug_red"] {
        assert_eq!(draft.jobs[job], ready.jobs[job], "{job}");
    }
    assert!(!draft.jobs["release"]);
    assert!(!draft.jobs["stress"]);
    assert!(!ready.jobs["stress"]);
    assert_eq!(draft.shards, ready.shards);
}

#[test]
fn no_pull_request_runs_the_release_job_for_any_input_that_selected_it() {
    let inputs = [
        ("binary package", strings(&["main"]), vec![]),
        (
            "benchmark path",
            strings(&["log"]),
            strings(&["crates/bench/src/lib.rs"]),
        ),
        (
            "release-size path",
            strings(&["log"]),
            strings(&["scripts/release-size"]),
        ),
    ];

    for (input, packages, _release_paths) in inputs {
        for draft in [true, false] {
            let plan = plan("crates", &packages, "pull_request", draft, false);
            assert!(!plan.jobs["release"], "{input}, draft {draft}");
            assert!(plan.jobs["lint"], "{input}, draft {draft}");
            assert!(plan.jobs["test_linux"], "{input}, draft {draft}");
        }
    }

    let push = plan("crates", &[], "push", false, false);
    assert!(push.jobs["release"]);
}

#[test]
fn a_ready_pull_request_with_packages_skips_the_stress_job() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, true);
    assert!(!plan.jobs["stress"]);
}

#[test]
fn a_draft_pull_request_skips_the_stress_job() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", true, true);
    assert!(!plan.jobs["stress"]);
}

#[test]
fn a_pull_request_without_packages_skips_the_stress_job() {
    let plan = plan("crates", &[], "pull_request", false, true);
    assert!(!plan.jobs["stress"]);
}

#[test]
fn a_push_runs_the_stress_job() {
    let plan = plan("crates", &strings(&["log"]), "push", false, true);
    assert!(plan.jobs["stress"]);
}

#[test]
fn a_docs_only_pull_request_skips_the_stress_job() {
    let plan = plan("docs", &[], "pull_request", false, true);
    assert!(!plan.jobs["stress"]);
}

#[test]
fn an_unlabelled_draft_skips_mutants_and_runs_the_rest() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", true, false);
    assert!(!plan.jobs["mutants"]);
    assert_eq!(plan.shards, 0);
    assert!(plan.jobs["lint"] && plan.jobs["test_linux"]);
}

#[test]
fn a_docs_push_runs_lint_tests_release_and_stress_but_no_mutants() {
    let plan = plan("docs", &[], "push", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, false, false, true, true),
            shards: 0
        }
    );
}

#[test]
fn a_code_push_runs_lint_tests_release_and_stress_but_no_mutants_or_bug_check() {
    let plan = plan("all", &strings(&["log"]), "push", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, false, false, true, true),
            shards: 0
        }
    );
}

#[test]
fn a_push_with_mutants_requested_starts_no_shard() {
    let plan = plan("all", &strings(&["log"]), "push", false, true);
    assert!(!plan.jobs["mutants"]);
    assert_eq!(plan.shards, 0);
}

#[test]
fn a_pull_request_that_selects_the_binary_skips_the_release_job() {
    let plan = plan(
        "crates",
        &strings(&["log", "main"]),
        "pull_request",
        false,
        false,
    );
    assert_eq!(
        plan.jobs,
        jobs(true, true, false, false, false, false, false)
    );
}

#[test]
fn a_pull_request_that_runs_everything_skips_the_release_job() {
    let packages = strings(&["config", "log", "main", "xtask"]);
    let plan = plan("all", &packages, "pull_request", false, false);
    assert_eq!(
        plan.jobs,
        jobs(true, true, false, false, false, false, false)
    );
}

#[test]
fn a_pull_request_without_the_binary_skips_the_release_job() {
    let plan = plan(
        "crates",
        &strings(&["log", "xtask"]),
        "pull_request",
        false,
        false,
    );
    assert_eq!(
        plan.jobs,
        jobs(true, true, false, false, false, false, false)
    );
}

#[test]
fn a_pull_request_touching_the_bench_crate_skips_the_release_job() {
    for path in [
        "crates/bench/src/lib.rs",
        "crates/bench/examples/bench/wheel.rs",
    ] {
        let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
        assert!(!plan.jobs["release"], "{path}");
    }
}

#[test]
fn a_pull_request_touching_the_bench_harness_skips_the_release_job() {
    for path in [
        "xtask/src/bench.rs",
        "xtask/src/bench_tests.rs",
        "xtask/src/bench/judge.rs",
    ] {
        let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
        assert!(!plan.jobs["release"], "{path}");
    }
}

#[test]
fn a_pull_request_touching_the_release_size_script_skips_the_release_job() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
    assert!(!plan.jobs["release"]);
}

#[test]
fn a_pull_request_touching_the_stored_results_script_skips_the_release_job() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
    assert!(!plan.jobs["release"]);
}

#[test]
fn a_pull_request_touching_only_the_performance_table_skips_the_release_job() {
    let plan = plan("crates", &strings(&["xtask"]), "pull_request", false, false);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_pull_request_touching_another_docs_file_skips_the_release_job() {
    let plan = plan("crates", &strings(&["xtask"]), "pull_request", false, false);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_pull_request_touching_a_release_path_lookalike_skips_the_release_job() {
    for path in [
        "crates/benchmark/src/lib.rs",
        "xtask/src/select.rs",
        "xtask/tests/bench.rs",
        "xtask/tests/release_size.rs",
        "scripts/release-size-old",
        "scripts/check",
    ] {
        let plan = plan(
            "crates",
            &strings(&["log", "xtask"]),
            "pull_request",
            false,
            false,
        );
        assert!(!plan.jobs["release"], "{path}");
    }
}

#[test]
fn a_docs_only_pull_request_touching_the_bench_crate_runs_no_job() {
    let plan = plan("docs", &[], "pull_request", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn an_ordinary_docs_only_pull_request_still_runs_no_job() {
    let plan = plan("docs", &[], "pull_request", false, true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, false, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_code_pull_request_with_a_bench_readme_skips_the_release_job() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
    assert!(!plan.jobs["release"]);
}

#[test]
fn a_pull_request_touching_the_loop_crate_runs_the_ten_turns_workload() {
    for path in [
        "crates/loop/src/status.rs",
        "crates/bench/examples/bench/busy.rs",
        "xtask/src/bench/judge.rs",
        "xtask/src/bench_tests.rs",
    ] {
        assert!(ten_turns_runs("pull_request", &strings(&[path])), "{path}");
    }
}

#[test]
fn a_pull_request_touching_nothing_the_workload_measures_skips_it() {
    for paths in [
        vec![],
        strings(&["crates/log/src/lib.rs"]),
        strings(&["crates/benchmark/src/lib.rs"]),
        strings(&["xtask/src/select.rs"]),
        // A Markdown file under a measured path is not measured code.
        strings(&["crates/bench/README.md"]),
        strings(&["docs/performance.md"]),
    ] {
        assert!(!ten_turns_runs("pull_request", &paths), "{paths:?}");
    }
}

#[test]
fn a_push_always_runs_the_ten_turns_workload() {
    for paths in [vec![], strings(&["crates/log/src/lib.rs"])] {
        assert!(ten_turns_runs("push", &paths), "{paths:?}");
    }
}

/// The base timing rows `paths` leaves out on `event`: `paths` are
/// test inputs, not files.
fn skipped(event: &str, paths: &[&str]) -> Vec<&'static str> {
    base_rows_skipped(event, &strings(paths))
}

#[test]
fn a_tui_diff_skips_the_session_rows() {
    assert_eq!(
        skipped("pull_request", &["crates/tui/src/editor.rs"]),
        vec!["session start", "sessions list", "sandbox setup"]
    );
}

#[test]
fn a_loop_diff_skips_the_list_and_paging_rows() {
    assert_eq!(
        skipped("pull_request", &["crates/loop/src/status.rs"]),
        vec!["sessions list", "paging", "sandbox setup"]
    );
}

#[test]
fn a_routing_diff_skips_the_list_and_paging_rows() {
    assert_eq!(
        skipped("pull_request", &["crates/routing/src/lib.rs"]),
        vec!["sessions list", "paging", "sandbox setup"]
    );
}

#[test]
fn a_hub_diff_skips_only_paging() {
    assert_eq!(
        skipped("pull_request", &["crates/hub/src/feed.rs"]),
        vec!["paging", "sandbox setup"]
    );
}

#[test]
fn a_cli_diff_skips_session_start_and_paging() {
    assert_eq!(
        skipped("pull_request", &["crates/cli/src/hub_status.rs"]),
        vec!["session start", "paging", "sandbox setup"]
    );
}

#[test]
fn a_worktree_diff_selects_the_list_row() {
    // The `docs/ci.md:120` Selection row names crates that select
    // `sessions list`; `worktree` belongs there because
    // `cli::sessions_list::list` resolves the project in `worktree`
    // (`cli/src/sessions_list.rs:137`), so a worktree change selects
    // `sessions list` alongside `session start` and the terminal rows:
    // only `paging` is skipped.
    assert_eq!(
        skipped("pull_request", &["crates/worktree/src/project.rs"]),
        vec!["paging", "sandbox setup"]
    );
}

#[test]
fn an_unmapped_path_selects_every_row() {
    for path in [
        "crates/bench/examples/bench/main.rs",
        "crates/bench/README.md",
        "xtask/src/bench.rs",
        "xtask/src/bench/judge.rs",
        "xtask/src/select.rs",
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
        "scripts/paging-jig",
        "scripts/release-size",
        "scripts/stored-results",
        ".github/workflows/ci.yml",
        "crates/fakes/src/lib.rs",
        "crates/picture/src/lib.rs",
        "crates/newcrate/src/lib.rs",
    ] {
        assert_eq!(
            skipped("pull_request", &[path]),
            Vec::<&'static str>::new(),
            "{path}"
        );
    }
}

#[test]
fn a_docs_file_selects_no_row() {
    // The pinned literal list of the seven names in table order, cited by
    // `crates/bench/examples/bench/main_tests.rs`: each side names the
    // other.
    let all = vec![
        "session start",
        "sessions list",
        "terminal first frame",
        "terminal attach",
        "terminal wheel",
        "paging",
        "sandbox setup",
    ];
    // `docs/invocation.md` is compiled in, so `release` runs on it, but
    // its paths are all docs: every base row is skipped.
    assert_eq!(skipped("pull_request", &["docs/ci.md"]), all);
    assert_eq!(skipped("pull_request", &["docs/invocation.md"]), all);
    // Docs beside code change nothing.
    assert_eq!(
        skipped("pull_request", &["docs/ci.md", "crates/tui/src/editor.rs"]),
        vec!["session start", "sessions list", "sandbox setup"]
    );
}

#[test]
fn markdown_under_crates_is_code() {
    // A Markdown file under a measured path is measured code, never a
    // docs skip: the loop's prompts compile in.
    assert_eq!(
        skipped("pull_request", &["crates/loop/prompt/system.md"]),
        vec!["sessions list", "paging", "sandbox setup"]
    );
    assert_eq!(
        skipped("pull_request", &["crates/tools/prompt/guidelines.md"]),
        vec!["sessions list", "paging"]
    );
}

#[test]
fn two_crates_take_the_union() {
    // The loop selects the session and terminal rows, `cli` the list and
    // terminal rows: neither selects paging, so only it is skipped.
    assert_eq!(
        skipped(
            "pull_request",
            &["crates/loop/src/status.rs", "crates/cli/src/hub_status.rs"]
        ),
        vec!["paging", "sandbox setup"]
    );
    // The tui selects the terminal and paging rows: with the loop only
    // the list row is left out.
    assert_eq!(
        skipped(
            "pull_request",
            &["crates/loop/src/status.rs", "crates/tui/src/editor.rs"]
        ),
        vec!["sessions list", "sandbox setup"]
    );
}

#[test]
fn odd_paths_select_every_row() {
    // The invariant "a path no row's crate list covers selects every
    // row": slash-bounded matching keeps lookalikes out.
    for path in [
        "CRATES/TUI/SRC/X.RS",
        "crates/tui",
        "crates/tui_extra/src/x.rs",
    ] {
        assert_eq!(
            skipped("pull_request", &[path]),
            Vec::<&'static str>::new(),
            "{path}"
        );
    }
    // Both ends of a rename count, and duplicates change nothing.
    assert_eq!(
        skipped(
            "pull_request",
            &["crates/tui/src/x.rs", "crates/tui/src/x.rs"]
        ),
        skipped("pull_request", &["crates/tui/src/x.rs"])
    );
}

#[test]
fn an_empty_diff_or_a_push_skips_nothing() {
    assert_eq!(skipped("pull_request", &[]), Vec::<&'static str>::new());
    assert_eq!(
        skipped("push", &["crates/tui/src/editor.rs"]),
        Vec::<&'static str>::new()
    );
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
    assert_eq!(
        verdict(&needs, &selected(), &[], &BaseResults::Untestable, false),
        Vec::<String>::new()
    );
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
            verdict(&needs, &selected(), &[], &BaseResults::Untestable, false),
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
        verdict(&needs, &selected(), &[], &BaseResults::Untestable, false),
        vec!["mutants: not selected, but success".to_owned()]
    );
}

#[test]
fn ci_passes_when_stress_was_not_selected_and_was_skipped() {
    let needs = results(&[
        ("select", "success"),
        ("docs", "success"),
        ("test", "success"),
        ("mutants", "skipped"),
        ("stress", "skipped"),
    ]);
    let jobs = BTreeMap::from([
        ("docs".to_owned(), true),
        ("test".to_owned(), true),
        ("mutants".to_owned(), false),
        ("stress".to_owned(), false),
    ]);
    assert_eq!(
        verdict(&needs, &jobs, &[], &BaseResults::Untestable, false),
        Vec::<String>::new()
    );
}

#[test]
fn ci_fails_when_selected_stress_failed() {
    let needs = results(&[
        ("select", "success"),
        ("docs", "success"),
        ("test", "success"),
        ("mutants", "skipped"),
        ("stress", "failure"),
    ]);
    let jobs = BTreeMap::from([
        ("docs".to_owned(), true),
        ("test".to_owned(), true),
        ("mutants".to_owned(), false),
        ("stress".to_owned(), true),
    ]);
    assert!(
        !verdict(&needs, &jobs, &[], &BaseResults::Untestable, false).is_empty(),
        "a selected stress failure must fail the verdict"
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
        verdict(
            &needs,
            &BTreeMap::new(),
            &[],
            &BaseResults::Untestable,
            false
        ),
        vec!["select: failure, so the selection failed".to_owned()]
    );
    assert_eq!(
        verdict(
            &BTreeMap::new(),
            &selected(),
            &[],
            &BaseResults::Untestable,
            false
        ),
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
        verdict(&needs, &selected(), &[], &BaseResults::Untestable, false),
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
    // (mutants, minutes): 40 minutes per 15 of the largest shard's
    // mutants, rounded up, at most 360.
    let table = [
        (0, 40),
        (1, 40),
        (15, 40),
        (240, 40),
        (241, 43),
        (256, 43),
        (257, 46),
        (480, 80),
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
    let plan = plan("crates", &strings(&["log"]), "pull_request", false, false);
    assert!(!plan.jobs["mutants"]);
    assert_eq!(plan.shards, 0);
}

fn listed(pairs: &[(&str, BaseOutcome)]) -> BaseResults {
    BaseResults::Listed(
        pairs
            .iter()
            .map(|(id, outcome)| ((*id).to_owned(), *outcome))
            .collect(),
    )
}

#[test]
fn a_head_failure_passing_at_base_is_newly_broken() {
    // A base pass reached on retry still counts as a pass: the id failed
    // at the head and passed at the base, so the head broke it.
    let head = strings(&["main::suite case"]);
    assert_eq!(
        new_breaks(&head, &listed(&[("main::suite case", BaseOutcome::Passed)])),
        NewBreaks {
            broken: head.clone(),
            dropped: Vec::new(),
        }
    );
}

#[test]
fn a_head_failure_also_failing_at_base_is_not_newly_broken() {
    assert_eq!(
        new_breaks(
            &strings(&["main::suite case"]),
            &listed(&[("main::suite case", BaseOutcome::Failed)]),
        ),
        NewBreaks {
            broken: Vec::new(),
            dropped: Vec::new(),
        }
    );
}

#[test]
fn a_head_failure_missing_at_base_is_pr_added_and_fails() {
    // The base list does not name the id, so the pull request added the
    // test and the head broke it.
    let head = strings(&["main::suite new_case"]);
    assert_eq!(
        new_breaks(&head, &listed(&[])),
        NewBreaks {
            broken: head.clone(),
            dropped: Vec::new(),
        }
    );
}

#[test]
fn a_head_failure_unrunnable_at_base_is_dropped_not_failed() {
    assert_eq!(
        new_breaks(
            &strings(&["main::suite case"]),
            &listed(&[("main::suite case", BaseOutcome::Unrunnable)]),
        ),
        NewBreaks {
            broken: Vec::new(),
            dropped: strings(&["main::suite case"]),
        }
    );
}

#[test]
fn an_untestable_base_fails_every_head_failure_closed() {
    let head = strings(&["main::suite first", "main::suite second"]);
    assert_eq!(
        new_breaks(&head, &BaseResults::Untestable),
        NewBreaks {
            broken: head.clone(),
            dropped: Vec::new(),
        }
    );
}

#[test]
fn a_mixed_base_splits_broken_from_dropped_in_head_order() {
    let head = strings(&[
        "main::suite added",
        "main::suite old",
        "main::suite new",
        "main::suite skipped",
    ]);
    assert_eq!(
        new_breaks(
            &head,
            &listed(&[
                ("main::suite old", BaseOutcome::Failed),
                ("main::suite new", BaseOutcome::Passed),
                ("main::suite skipped", BaseOutcome::Unrunnable),
            ]),
        ),
        NewBreaks {
            broken: strings(&["main::suite added", "main::suite new"]),
            dropped: strings(&["main::suite skipped"]),
        }
    );
}

#[test]
fn head_failures_parse_to_exact_nextest_ids() {
    assert_eq!(
        parse_head_failures("main::suite case\n\nlib::other other\n"),
        Ok(strings(&["main::suite case", "lib::other other"]))
    );
    assert_eq!(
        parse_head_failures("main::suite  padded\n"),
        Ok(strings(&["main::suite padded"]))
    );
}

#[test]
fn head_failures_reject_a_line_that_is_not_an_id() {
    for text in ["lonely\n", "three part id\n", "pass\tmain::suite case\n"] {
        assert!(parse_head_failures(text).is_err(), "{text:?}");
    }
}

#[test]
fn base_results_parse_each_outcome_by_exact_id() {
    assert_eq!(
        parse_base_results(
            "pass\tmain::suite case\nfail\tmain::suite old\nunrunnable\tmain::suite skipped\n"
        ),
        Ok(BTreeMap::from([
            ("main::suite case".to_owned(), BaseOutcome::Passed),
            ("main::suite old".to_owned(), BaseOutcome::Failed),
            ("main::suite skipped".to_owned(), BaseOutcome::Unrunnable),
        ]))
    );
}

#[test]
fn base_results_reject_a_line_that_is_not_an_outcome() {
    for text in [
        // Unknown outcome.
        "flaky\tmain::suite case\n",
        // No tab between the outcome and the id.
        "pass main::suite case\n",
        // Not an exact nextest id.
        "pass\tlonely\n",
        "pass\tthree part id\n",
        // A repeated id would double-count one test.
        "pass\tmain::suite case\nfail\tmain::suite case\n",
    ] {
        assert!(parse_base_results(text).is_err(), "{text:?}");
    }
}

fn break_jobs() -> BTreeMap<String, bool> {
    BTreeMap::from([("test_linux".to_owned(), true)])
}

fn break_needs(result: &str) -> BTreeMap<String, String> {
    results(&[("select", "success"), ("test_linux", result)])
}

#[test]
fn ci_passes_when_every_head_failure_also_fails_at_base() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite old"]),
            &listed(&[("main::suite old", BaseOutcome::Failed)]),
            false,
        ),
        Vec::<String>::new()
    );
}

#[test]
fn ci_passes_when_every_head_failure_is_dropped_at_base() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite skipped"]),
            &listed(&[("main::suite skipped", BaseOutcome::Unrunnable)]),
            false,
        ),
        Vec::<String>::new()
    );
}

#[test]
fn ci_fails_listing_only_newly_broken_ids() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite new", "main::suite old"]),
            &listed(&[
                ("main::suite new", BaseOutcome::Passed),
                ("main::suite old", BaseOutcome::Failed),
            ]),
            false,
        ),
        vec!["test_linux: newly broken: main::suite new".to_owned()]
    );
}

#[test]
fn ci_fails_a_newly_broken_id_the_base_list_does_not_name() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite added"]),
            &listed(&[]),
            false,
        ),
        vec!["test_linux: newly broken: main::suite added".to_owned()]
    );
}

#[test]
fn ci_fails_closed_when_a_failed_leg_has_no_break_data() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &[],
            &BaseResults::Untestable,
            false,
        ),
        vec!["test_linux: selected, but failure".to_owned()]
    );
}

#[test]
fn ci_passes_a_successful_leg_even_with_break_data() {
    assert_eq!(
        verdict(
            &break_needs("success"),
            &break_jobs(),
            &strings(&["main::suite new"]),
            &listed(&[("main::suite new", BaseOutcome::Passed)]),
            false,
        ),
        Vec::<String>::new()
    );
}

#[test]
fn ci_does_not_excuse_a_leg_the_selection_did_not_choose() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &BTreeMap::from([("test_linux".to_owned(), false)]),
            &strings(&["main::suite old"]),
            &listed(&[("main::suite old", BaseOutcome::Failed)]),
            false,
        ),
        vec!["test_linux: not selected, but failure".to_owned()]
    );
}

#[test]
fn ci_keeps_a_touched_retry_failure_with_no_new_breaks() {
    // Every head failure also fails at the base, so nothing is newly
    // broken, but a touched test passed only on retry, which blocks the
    // merge (`docs/testing/running.md`, "Flaky tests").
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite old"]),
            &listed(&[("main::suite old", BaseOutcome::Failed)]),
            true,
        ),
        vec!["test_linux: touched test passed only on retry".to_owned()]
    );
}

#[test]
fn ci_reports_both_new_breaks_and_a_touched_retry() {
    assert_eq!(
        verdict(
            &break_needs("failure"),
            &break_jobs(),
            &strings(&["main::suite new", "main::suite old"]),
            &listed(&[
                ("main::suite new", BaseOutcome::Passed),
                ("main::suite old", BaseOutcome::Failed),
            ]),
            true,
        ),
        vec![
            "test_linux: newly broken: main::suite new".to_owned(),
            "test_linux: touched test passed only on retry".to_owned(),
        ]
    );
}

#[test]
fn ci_passes_a_successful_leg_despite_a_touched_retry_signal() {
    assert_eq!(
        verdict(
            &break_needs("success"),
            &break_jobs(),
            &strings(&["main::suite old"]),
            &listed(&[("main::suite old", BaseOutcome::Failed)]),
            true,
        ),
        Vec::<String>::new()
    );
}
