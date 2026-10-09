use super::*;

use crate::rules::RustFile;

fn members() -> Members {
    let member = |dir: &str, deps: &[&str]| Member {
        dir: dir.to_owned(),
        version: "0.0.0".to_owned(),
        deps: deps.iter().map(|d| (*d).to_owned()).collect(),
        library: true,
    };
    BTreeMap::from([
        ("contract".to_owned(), member("crates/contract", &[])),
        ("log".to_owned(), member("crates/log", &["contract"])),
        ("config".to_owned(), member("crates/config", &["contract"])),
        (
            "loop".to_owned(),
            member("crates/loop", &["contract", "log"]),
        ),
        ("loop-extra".to_owned(), member("crates/loop/extra", &[])),
        ("extensions".to_owned(), member("crates/extensions", &[])),
        ("main".to_owned(), member("crates/main", &[])),
    ])
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn a_bare_name_is_ambiguous_once_a_dependency_shares_it() {
    // `members()` has a workspace crate named `log` (`crates/log`), like
    // Fiber's own `log` crate. Once any crate depends on `ureq`, which pulls
    // in the crates.io `log` 0.4.x, `cargo -p log` fails with "specification
    // 'log' is ambiguous". `spec` must print something that still names
    // exactly one package: the bare name alone never does, no matter which
    // dependency a crate later gains.
    let spec = spec("log", &members());
    assert_ne!(spec, "log");
    assert_eq!(spec, "log@0.0.0");
}

#[test]
fn markdown_docs_and_research_run_the_docs_job_alone() {
    let files = strings(&["README.md", "crates/loop/notes.md", "research/x/run.sh"]);
    let selection = classify(&files, &members());
    assert_eq!(selection, Selection::Docs);
    assert_eq!(selection.mode(), "docs");
    assert!(selection.packages().is_empty());
}

#[test]
fn manifests_toolchain_and_workflows_run_everything() {
    let everything: Vec<String> = members().keys().cloned().collect();
    for trigger in [
        "Cargo.lock",
        "crates/log/Cargo.toml",
        "rust-toolchain.toml",
        ".github/workflows/ci.yml",
        "scripts/check",
        "clippy.toml",
        "deny.toml",
        ".cargo/config.toml",
        ".config/nextest.toml",
    ] {
        let selection = classify(&strings(&["docs/ci.md", trigger]), &members());
        assert_eq!(selection, Selection::All(everything.clone()), "{trigger}");
        assert_eq!(selection.mode(), "all");
        assert_eq!(selection.packages(), everything.as_slice());
    }
}

#[test]
fn a_file_merely_named_like_a_manifest_does_not_run_everything() {
    let selection = classify(&strings(&["crates/log/src/Cargo.toml.rs"]), &members());
    assert_eq!(selection, Selection::Crates(strings(&["log", "loop"])));
}

#[test]
fn a_crate_change_runs_it_and_its_dependents() {
    let selection = classify(&strings(&["crates/contract/src/lib.rs"]), &members());
    assert_eq!(
        selection,
        Selection::Crates(strings(&["config", "contract", "log", "loop"]))
    );
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn dependents_are_transitive() {
    let selected = dependents(BTreeSet::from(["log".to_owned()]), &members());
    assert_eq!(
        selected,
        BTreeSet::from(["log".to_owned(), "loop".to_owned()])
    );
}

#[test]
fn a_leaf_crate_runs_alone() {
    let selection = classify(
        &strings(&["crates/config/src/lib.rs", "docs/configuration.md"]),
        &members(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["config"])));
}

#[test]
fn the_innermost_crate_owns_a_file() {
    let members = members();
    assert_eq!(
        owner("crates/loop/extra/src/lib.rs", &members),
        Some("loop-extra")
    );
    assert_eq!(owner("crates/loop/src/lib.rs", &members), Some("loop"));
    assert_eq!(owner("crates/logger/src/lib.rs", &members), None);
}

#[test]
fn code_outside_every_crate_runs_no_crate() {
    assert_eq!(
        classify(&strings(&["LICENSE"]), &members()),
        Selection::Crates(vec![])
    );
}

#[test]
fn a_compiled_in_doc_runs_its_crate_alone() {
    for path in [
        "docs/events.md",
        "docs/errors.md",
        "docs/invocation.md",
        "docs/tui.md",
    ] {
        let selection = classify(&strings(&[path]), &members());
        assert_eq!(
            selection,
            Selection::Crates(strings(&["contract"])),
            "{path}"
        );
        assert_eq!(selection.mode(), "crates", "{path}");
    }
}

#[test]
fn a_shared_compiled_in_doc_runs_every_crate_that_compiles_it_in() {
    let selection = classify(&strings(&["docs/tui.md"]), &members_with_tools());
    assert_eq!(selection, Selection::Crates(strings(&["contract", "tui"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_compiled_in_ci_doc_runs_xtask() {
    let selection = classify(&strings(&["docs/ci.md"]), &members_with_tools());
    assert_eq!(selection, Selection::Crates(strings(&["xtask"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn readme_alone_runs_the_docs_job() {
    assert_eq!(
        classify(&strings(&["README.md"]), &members()),
        Selection::Docs
    );
}

#[test]
fn a_compiled_in_crate_prompt_runs_that_crate() {
    assert_eq!(
        classify(&strings(&["crates/loop/prompt/system.md"]), &members()),
        Selection::Crates(strings(&["loop"]))
    );
}

#[test]
fn a_manifest_under_research_runs_the_docs_job_alone() {
    for path in [
        "research/tui-prototype/Cargo.toml",
        "research/tui-prototype/Cargo.lock",
    ] {
        let selection = classify(&strings(&[path]), &members());
        assert_eq!(selection, Selection::Docs, "{path}");
        assert_eq!(selection.mode(), "docs");
    }
    let everything: Vec<String> = members().keys().cloned().collect();
    assert_eq!(
        classify(
            &strings(&["research/tui-prototype/Cargo.toml", "Cargo.toml"]),
            &members()
        ),
        Selection::All(everything)
    );
}

#[test]
fn scripts_check_alone_runs_everything() {
    let everything: Vec<String> = members().keys().cloned().collect();
    let selection = classify(&strings(&["scripts/check"]), &members());
    assert_eq!(selection, Selection::All(everything));
    assert_eq!(selection.mode(), "all");
}

#[test]
fn clippy_toml_runs_everything() {
    let everything: Vec<String> = members().keys().cloned().collect();
    assert_eq!(
        classify(&strings(&["clippy.toml"]), &members()),
        Selection::All(everything)
    );
}

#[test]
fn nextest_toml_runs_everything() {
    let everything: Vec<String> = members().keys().cloned().collect();
    assert_eq!(
        classify(&strings(&[".config/nextest.toml"]), &members()),
        Selection::All(everything)
    );
}

#[test]
fn a_compiled_in_doc_unions_with_a_crate_change() {
    let selection = classify(
        &strings(&["docs/events.md", "crates/log/src/lib.rs"]),
        &members(),
    );
    assert_eq!(
        selection,
        Selection::Crates(strings(&["contract", "log", "loop"]))
    );
}

#[test]
fn uncompiled_docs_in_a_mixed_diff_do_not_add_crates() {
    for extra in ["crates/contract/README.md", "crates/loop/notes.md"] {
        let selection = classify(&strings(&["docs/events.md", extra]), &members());
        assert_eq!(
            selection,
            Selection::Crates(strings(&["contract"])),
            "{extra}"
        );
    }
}

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

fn tickets(body: &str) -> Vec<String> {
    ticket(body).into_iter().collect()
}

#[test]
fn the_ticket_is_the_resolved_issue() {
    assert_eq!(
        tickets("Does a thing.\n\nResolves #212\n"),
        vec!["212".to_owned()]
    );
    assert_eq!(tickets("fixes #7"), vec!["7".to_owned()]);
    assert_eq!(tickets("Closed #9."), vec!["9".to_owned()]);
    assert_eq!(tickets("See #3, then close\n#4"), vec!["4".to_owned()]);
}

#[test]
fn the_ticket_lists_every_resolved_issue_in_order() {
    assert_eq!(
        tickets("Resolves #365\nResolves #384\n"),
        vec!["365".to_owned(), "384".to_owned()]
    );
    assert_eq!(
        tickets("Fixes #1, closes #2 and resolves #3"),
        vec!["1".to_owned(), "2".to_owned(), "3".to_owned()]
    );
}

#[test]
fn there_is_no_ticket_without_a_closing_keyword() {
    for body in [
        "See #212",
        "",
        "Resolves#212",
        "unresolves #5",
        "Resolves #",
        "Resolves #12a",
        "Resolves 12",
    ] {
        assert_eq!(tickets(body), Vec::<String>::new(), "{body:?}");
    }
}

#[test]
fn test_files_map_to_their_tests() {
    let files = strings(&[
        "crates/log/src/writer_tests.rs",
        "crates/log/src/fold/tests.rs",
        "crates/log/src/tests.rs",
        "crates/loop/tests/turns.rs",
        "crates/loop/tests/support/mod.rs",
        "crates/log/src/fold/inner_tests.rs",
        "crates/log/src/lib_tests.rs",
        "crates/loop/src/main_tests.rs",
        "crates/log/src/writer.rs",
        "crates/log/build_tests.rs",
        "docs/events.md",
    ]);
    let (expression, packages) = test_filter(&files, &members(), &[]);
    assert_eq!(
        expression.split(" | ").collect::<Vec<_>>(),
        [
            "(package(log) & test(/^writer::tests::/))",
            "(package(log) & test(/^fold::tests::/))",
            "(package(log) & test(/^tests::/))",
            "binary_id(loop::turns)",
            "(package(log) & test(/^fold::inner::tests::/))",
            "(package(log) & test(/^tests::/))",
            "(package(loop) & test(/^tests::/))",
        ]
    );
    assert_eq!(packages, strings(&["log", "loop"]));
}

#[test]
fn an_example_test_file_maps_to_its_binary() {
    let files = strings(&[
        "crates/main/examples/bench/run_tests.rs",
        "crates/main/examples/bench/main_tests.rs",
        "crates/main/examples/bench/fold/inner_tests.rs",
        "crates/main/examples/bench/run.rs",
    ]);
    let (expression, packages) = test_filter(&files, &members(), &[]);
    assert_eq!(
        expression.split(" | ").collect::<Vec<_>>(),
        [
            "(binary_id(main::example/bench) & test(/^run::tests::/))",
            "(binary_id(main::example/bench) & test(/^tests::/))",
            "(binary_id(main::example/bench) & test(/^fold::inner::tests::/))",
        ]
    );
    assert_eq!(packages, strings(&["main"]));
}

#[test]
fn a_path_declared_example_test_file_maps_to_its_declaring_module() {
    let files = strings(&["crates/main/examples/bench/run_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/examples/bench/run.rs",
        "#[cfg(test)]\n#[path = \"run_tests.rs\"]\nmod tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(binary_id(main::example/bench) & test(/^run::tests::/))"
    );
}

#[test]
fn only_a_crate_root_file_is_a_crate_root() {
    let (expression, _) = test_filter(
        &strings(&["crates/log/src/fold/lib_tests.rs"]),
        &members(),
        &[],
    );
    assert_eq!(expression, "(package(log) & test(/^fold::lib::tests::/))");
}

#[test]
fn no_test_files_give_an_empty_filter() {
    assert_eq!(
        test_filter(&strings(&["crates/log/src/writer.rs"]), &members(), &[]),
        (String::new(), vec![])
    );
}

fn decl(krate: &str, path: &str, source: &str) -> RustFile {
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

#[test]
fn support_files_under_tests_add_no_filter_term() {
    let files = strings(&[
        "crates/loop/tests/support/mod.rs",
        "crates/loop/tests/support/fake.rs",
    ]);
    assert_eq!(
        test_filter(&files, &members(), &[]),
        (String::new(), vec![])
    );
}

#[test]
fn a_path_declared_test_file_maps_to_its_declaring_module() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[cfg(test)]\n#[path = \"idle_tests.rs\"]\nmod idle_tests;",
    )];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(package(main) & test(/^settings::idle_tests::/))"
    );
    assert_eq!(packages, strings(&["main"]));
}

#[test]
fn a_path_declared_test_file_inside_a_path_declared_module_resolves_recursively() {
    // The intermediate module's declared name (`background`) differs from
    // its file stem (`background_impl`), so the conventional answer
    // (`shell::background_impl::tests::`) differs: resolving only one level
    // would fail this test.
    let files = strings(&["crates/log/src/shell/background_tests.rs"]);
    let sources = vec![
        decl(
            "log",
            "crates/log/src/shell.rs",
            "#[path = \"shell/background_impl.rs\"] mod background;",
        ),
        decl(
            "log",
            "crates/log/src/shell/background_impl.rs",
            "#[cfg(test)] #[path = \"background_tests.rs\"] mod tests;",
        ),
    ];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(package(log) & test(/^shell::background::tests::/))"
    );
    assert_eq!(packages, strings(&["log"]));
}

#[test]
fn a_path_declaration_in_a_mod_rs_file_resolves_beside_it() {
    let files = strings(&["crates/log/src/fold/fold_extra_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/fold/mod.rs",
        "#[path = \"fold_extra_tests.rs\"] mod extra;",
    )];
    let (expression, packages) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^fold::extra::/))");
    assert_eq!(packages, strings(&["log"]));
}

#[test]
fn a_source_that_does_not_tokenise_keeps_the_conventional_filter() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "fn broken( {\n",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn two_declarations_for_one_file_fall_back_to_convention() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![
        decl(
            "main",
            "crates/main/src/settings.rs",
            "#[path = \"idle_tests.rs\"] pub mod idle_tests;",
        ),
        decl(
            "main",
            "crates/main/src/other.rs",
            "#[path = \"idle_tests.rs\"] mod idle_tests;",
        ),
    ];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_path_attribute_on_a_non_mod_item_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] use crate::idle;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_pending_path_does_not_survive_a_semicolon() {
    // The `use` item's `;` ends the attribute run, so the later `mod`
    // without its own attribute registers nothing.
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] use idle;\nmod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_pending_path_does_not_survive_a_braced_item() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] fn f() {}\nmod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn an_inline_module_after_a_path_attribute_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] mod idle_tests {}",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_visibility_between_a_path_attribute_and_its_mod_still_registers() {
    // A paren group is `pub(crate)`: only brace groups clear a pending
    // `#[path]`.
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] pub(crate) mod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(
        expression,
        "(package(main) & test(/^settings::idle_tests::/))"
    );
}

#[test]
fn a_hash_before_a_paren_group_registers_nothing() {
    // Only a bracket group after `#` starts an attribute.
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#(path = \"idle_tests.rs\")\nmod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn an_inner_attribute_registers_nothing() {
    // `#![...]`: the `#` is not followed by the bracket group, and the
    // bracket group alone starts no attribute.
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#![path = \"idle_tests.rs\"]\nmod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_lone_hash_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#\nmod idle_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_mod_without_a_trailing_semicolon_registers_nothing() {
    let files = strings(&["crates/main/src/idle_tests.rs"]);
    let sources = vec![decl(
        "main",
        "crates/main/src/settings.rs",
        "#[path = \"idle_tests.rs\"] mod idle_tests !",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(main) & test(/^idle::tests::/))");
}

#[test]
fn a_test_file_declared_in_lib_rs_maps_to_the_crate_root() {
    let files = strings(&["crates/log/src/foo_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/lib.rs",
        "#[path = \"foo_tests.rs\"] mod foo_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo_tests::/))");
}

#[test]
fn a_test_file_declared_in_main_rs_maps_to_the_crate_root() {
    let files = strings(&["crates/log/src/foo_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/main.rs",
        "#[path = \"foo_tests.rs\"] mod foo_tests;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo_tests::/))");
}

#[test]
fn a_test_file_declared_in_a_nested_file_chains_through_it() {
    // The declarer `src/a/b.rs` is a non-`mod` nested file: `a::b`.
    let files = strings(&["crates/log/src/c_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/a/b.rs",
        "#[path = \"../c_tests.rs\"] mod c;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^a::b::c::/))");
}

#[test]
fn a_test_file_declared_in_a_mod_rs_chains_through_it() {
    let files = strings(&["crates/log/src/d_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/a/mod.rs",
        "#[path = \"../d_tests.rs\"] mod d;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^a::d::/))");
}

#[test]
fn a_test_file_declared_in_a_top_level_file_chains_through_it() {
    let files = strings(&["crates/log/src/bar_tests.rs"]);
    let sources = vec![decl(
        "log",
        "crates/log/src/foo.rs",
        "#[path = \"bar_tests.rs\"] mod bar;",
    )];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^foo::bar::/))");
}

#[test]
fn a_declaration_cycle_falls_back_to_convention() {
    // `a.rs` declares `b.rs` and `b.rs` declares `a.rs`: resolving the
    // test file through them must terminate at the conventional filter.
    let files = strings(&["crates/log/src/cycle_tests.rs"]);
    let sources = vec![
        decl(
            "log",
            "crates/log/src/a.rs",
            "#[path = \"b.rs\"] mod b;\n#[path = \"cycle_tests.rs\"] mod cycle_tests;",
        ),
        decl("log", "crates/log/src/b.rs", "#[path = \"a.rs\"] mod a;"),
    ];
    let (expression, _) = test_filter(&files, &members(), &sources);
    assert_eq!(expression, "(package(log) & test(/^cycle::tests::/))");
}

#[test]
fn test_files_are_named_or_under_tests() {
    assert!(is_test_file("src/tests.rs"));
    assert!(is_test_file("src/a/b_tests.rs"));
    assert!(is_test_file("tests/journey.rs"));
    assert!(!is_test_file("src/tests_helper.rs"));
    assert!(!is_test_file("src/a/tests/fixture.rs"));
}

#[test]
fn dependents_follow_a_chain_of_any_length() {
    let mut members = members();
    let chained = |dep: &str| Member {
        dir: format!("crates/{dep}-user"),
        version: "0.0.0".to_owned(),
        deps: vec![dep.to_owned()],
        library: true,
    };
    members.insert("tui".to_owned(), chained("loop"));
    members.insert("zed".to_owned(), chained("tui"));
    let selected = dependents(BTreeSet::from(["log".to_owned()]), &members);
    assert_eq!(
        selected.into_iter().collect::<Vec<_>>(),
        strings(&["log", "loop", "tui", "zed"])
    );
}

fn contract_src(source: &str) -> RustFile {
    RustFile {
        krate: "contract".to_owned(),
        path: "crates/contract/src/lib.rs".to_owned(),
        rel: "src/lib.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn loop_src(source: &str) -> RustFile {
    RustFile {
        krate: "loop".to_owned(),
        path: "crates/loop/src/reviewer.rs".to_owned(),
        rel: "src/reviewer.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn tools_src(source: &str) -> RustFile {
    RustFile {
        krate: "tools".to_owned(),
        path: "crates/tools/src/guidelines.rs".to_owned(),
        rel: "src/guidelines.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn tui_src(source: &str) -> RustFile {
    RustFile {
        krate: "tui".to_owned(),
        path: "crates/tui/src/theme_tests.rs".to_owned(),
        rel: "src/theme_tests.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn xtask_src(source: &str) -> RustFile {
    RustFile {
        krate: "xtask".to_owned(),
        path: "xtask/src/ci_needs_tests.rs".to_owned(),
        rel: "src/ci_needs_tests.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn main_src(source: &str) -> RustFile {
    RustFile {
        krate: "main".to_owned(),
        path: "crates/main/tests/ask.rs".to_owned(),
        rel: "tests/ask.rs".to_owned(),
        source: source.to_owned(),
    }
}

/// `members()` with the `tools`, `tui` and `xtask` crates: only the
/// compiled-in tests name them, so the selection tests keep the smaller
/// fixture.
fn members_with_tools() -> Members {
    let mut members = members();
    members.insert(
        "tools".to_owned(),
        Member {
            dir: "crates/tools".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            library: true,
        },
    );
    members.insert(
        "tui".to_owned(),
        Member {
            dir: "crates/tui".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            library: true,
        },
    );
    members.insert(
        "xtask".to_owned(),
        Member {
            dir: "xtask".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            library: true,
        },
    );
    members
}

/// The listed includes of `krate`, each resolvable from its fixture source.
fn listed_includes(krate: &str) -> String {
    COMPILED_IN
        .iter()
        .filter(|(_, listed)| *listed == krate)
        .map(|(path, _)| format!("include_str!(\"../../../{path}\");\n"))
        .collect()
}

/// Every listed include, each in a source of the crate the list names.
fn listed_files() -> Vec<RustFile> {
    vec![
        contract_src(&listed_includes("contract")),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
        tui_src(&listed_includes("tui")),
        xtask_src(&listed_includes("xtask")),
        main_src(&listed_includes("main")),
    ]
}

/// `listed_files()` with the `contract` source replaced by `source`.
fn listed_files_with_contract(source: &str) -> Vec<RustFile> {
    let mut files = listed_files();
    for file in &mut files {
        if file.krate == "contract" {
            *file = contract_src(source);
        }
    }
    files
}

#[test]
fn a_listed_include_passes() {
    let files = listed_files();
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_unlisted_outside_include_fails() {
    let source = format!(
        "{}include_str!(\"../../../README.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        ["README.md: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_non_docs_outside_include_fails() {
    let source = format!(
        "{}include_str!(\"../../../LICENSE\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        ["LICENSE: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_markdown_inside_the_crate_dir_fails() {
    let source = format!(
        "{}include_str!(\"../prompt/system.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/prompt/system.md: contract compiles it in, but the compiled-in list does not list it"
        ]
    );
}

#[test]
fn a_listed_markdown_inside_the_crate_dir_runs_that_crate_alone() {
    let mut listed = COMPILED_IN.to_vec();
    listed.push(("crates/contract/prompt/system.md", "contract"));
    let selection = classify_with(
        &strings(&["crates/contract/prompt/system.md"]),
        &members(),
        &listed,
        &[],
    );
    assert_eq!(selection, Selection::Crates(strings(&["contract"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_non_docs_include_inside_the_crate_dir_is_unlisted() {
    let source = format!(
        "{}include_str!(\"owned.bin\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_raw_string_include_is_resolved() {
    for extra in [
        r#"include_str!(r"../../../README.md");"#,
        r##"include_str!(r#"../../../README.md"#);"##,
    ] {
        let source = format!("{}{extra}", listed_includes("contract"));
        let files = listed_files_with_contract(&source);
        assert_eq!(
            compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
            ["README.md: contract compiles it in, but the compiled-in list does not list it"],
            "{extra}"
        );
    }
}

#[test]
fn an_unresolvable_include_argument_fails() {
    let source = format!(
        "{}include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../README.md\"));",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: include_str! argument is not a string literal; the compiled-in check cannot resolve it"
        ]
    );
}

#[test]
fn another_macro_with_a_string_argument_yields_no_target() {
    let source = format!("{}my_macro!(\"../x.md\");", listed_includes("contract"));
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn include_bytes_yields_its_target() {
    let source = format!(
        "{}include_bytes!(\"../x.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/x.md: contract compiles it in, but the compiled-in list does not list it"
        ]
    );
}

#[test]
fn a_trailing_comma_on_an_include_is_accepted() {
    for extra in [
        r#"include_str!("../../../README.md",);"#,
        r#"include_bytes!("../../../README.md",);"#,
    ] {
        let source = format!("{}{extra}", listed_includes("contract"));
        let files = listed_files_with_contract(&source);
        assert_eq!(
            compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
            ["README.md: contract compiles it in, but the compiled-in list does not list it"],
            "{extra}"
        );
    }
}

#[test]
fn an_include_with_tokens_after_the_literal_fails() {
    let source = format!(
        r#"{}include_str!("a.md", "b");"#,
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: include_str! argument is not a string literal; the compiled-in check cannot resolve it"
        ]
    );
}

#[test]
fn a_provider_package_diff_runs_the_package_readers() {
    let selection = classify_with(
        &strings(&["providers/opencode/providers/opencode-go.json"]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "main"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn an_extension_readme_is_a_package_file_not_a_docs_only_diff() {
    let selection = classify_with(
        &strings(&["extensions/x/README.md"]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "main"])));
}

#[test]
fn a_file_under_crates_extensions_is_not_a_package_file() {
    let selection = classify_with(
        &strings(&["crates/extensions/src/lib.rs"]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["extensions"])));
}

#[test]
fn a_package_file_plus_a_manifest_runs_everything() {
    let everything: Vec<String> = members().keys().cloned().collect();
    let selection = classify_with(
        &strings(&[
            "providers/opencode/providers/opencode-go.json",
            "crates/log/Cargo.toml",
        ]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::All(everything));
}

#[test]
fn a_package_file_unions_with_a_crate_change() {
    let selection = classify_with(
        &strings(&[
            "providers/opencode/providers/opencode-go.json",
            "crates/log/src/lib.rs",
        ]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(
        selection,
        Selection::Crates(strings(&["config", "log", "loop", "main"]))
    );
}

#[test]
fn a_package_file_plus_a_plain_doc_runs_only_the_package_readers() {
    let selection = classify_with(
        &strings(&[
            "providers/opencode/providers/opencode-go.json",
            "docs/ci.md",
        ]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "main"])));
}

#[test]
fn a_package_diff_runs_the_binary_tests_beside_the_listed_readers() {
    let selection = classify_with(
        &strings(&["providers/opencode/providers/opencode-go.json"]),
        &members(),
        COMPILED_IN,
        &["config"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "main"])));
}

#[test]
fn only_a_repo_root_providers_or_extensions_dir_is_a_package_file() {
    for path in [
        "providers/opencode/providers/opencode-go.json",
        "providers/x.json",
        "extensions/x/README.md",
        "extensions/x/extension.json",
    ] {
        assert!(is_package_file(path), "{path}");
    }
    for path in [
        "providersX/opencode-go.json",
        "crates/extensions/src/lib.rs",
        "research/topic/providers/notes.md",
        "docs/ci.md",
    ] {
        assert!(!is_package_file(path), "{path}");
    }
}

#[test]
fn a_providers_lookalike_diff_selects_as_before() {
    let selection = classify_with(
        &strings(&[
            "providersX/opencode-go.json",
            "research/topic/providers/notes.md",
        ]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(Vec::new()));
}

fn package_src(krate: &str, path: &str, source: &str) -> RustFile {
    RustFile {
        krate: krate.to_owned(),
        path: path.to_owned(),
        rel: "src/lib.rs".to_owned(),
        source: source.to_owned(),
    }
}

fn config_reads_a_package() -> RustFile {
    package_src(
        "config",
        "crates/config/tests/credentials.rs",
        "fn dir() -> PathBuf {\n    std::path::PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers/opencode\")\n}\n",
    )
}

fn main_reads_a_package() -> RustFile {
    package_src(
        "main",
        "crates/main/tests/ask.rs",
        "fn package(name: &str) -> PathBuf {\n    Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers\").join(name)\n}\n",
    )
}

fn cli_reads_a_package() -> RustFile {
    package_src(
        "cli",
        "crates/cli/src/login/browser_tests.rs",
        "fn copy() -> PathBuf {\n    std::path::PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers/codex\")\n}\n",
    )
}

fn extensions_reads_a_package() -> RustFile {
    package_src(
        "extensions",
        "crates/extensions/build.rs",
        "let manifest = env!(\"CARGO_MANIFEST_DIR\");\nlet root = \"../../providers\";\n",
    )
}

fn xtask_reads_a_package() -> RustFile {
    package_src(
        "xtask",
        "xtask/tests/models_dev.rs",
        "let providers = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../providers\");\n",
    )
}

fn package_ok_files() -> Vec<RustFile> {
    vec![
        cli_reads_a_package(),
        config_reads_a_package(),
        extensions_reads_a_package(),
        main_reads_a_package(),
        xtask_reads_a_package(),
    ]
}

/// The members the package-reader tests use.
fn package_members() -> Members {
    let mut members = members_with_tools();
    members.insert(
        "cli".to_owned(),
        Member {
            dir: "crates/cli".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            library: true,
        },
    );
    members
}

#[test]
fn the_package_reader_list_matches_its_sources() {
    assert_eq!(
        package_reader_mismatches(&package_ok_files(), &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_unlisted_crate_that_reads_a_package_fails() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn a_listed_crate_with_no_reading_source_fails() {
    assert_eq!(
        package_reader_mismatches(&[config_reads_a_package()], &package_members()).unwrap(),
        [
            "cli: listed as reading a first-party package, but no source reads one",
            "extensions: listed as reading a first-party package, but no source reads one",
            "main: listed as reading a first-party package, but no source reads one",
            "xtask: listed as reading a first-party package, but no source reads one"
        ]
    );
}

#[test]
fn a_package_literal_in_a_comment_does_not_count() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\n// let old = \"../../providers/opencode\";\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_package_literal_without_the_manifest_does_not_count() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = \"../../providers/opencode\";\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_joined_providers_segment_counts() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let root = PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"..\").join(\"..\").join(\"providers\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn a_raw_string_package_literal_counts() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(r\"../../providers/opencode\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn xtask_sources_count_as_package_readers_like_any_crate() {
    let mut files = package_ok_files();
    files.push(package_src(
        "xtask",
        "xtask/src/select_tests.rs",
        "let dir = \"../../providers/opencode\";\nlet manifest = env!(\"CARGO_MANIFEST_DIR\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        Vec::<String>::new()
    );
    let without_xtask: Vec<RustFile> = package_ok_files()
        .into_iter()
        .filter(|file| file.krate != "xtask")
        .collect();
    assert_eq!(
        package_reader_mismatches(&without_xtask, &package_members()).unwrap(),
        ["xtask: listed as reading a first-party package, but no source reads one"]
    );
}

#[test]
fn a_crates_extensions_segment_counts() {
    // Fails safe: a `CARGO_MANIFEST_DIR` file naming `"crates/extensions"`
    // counts as reading a package, so the list carries it.
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/extensions/foo\");\n",
    ));
    assert_eq!(
        package_reader_mismatches(&files, &package_members()).unwrap(),
        [
            "tools: its sources read a first-party package, but the package-reader list does not list it"
        ]
    );
}

#[test]
fn non_package_literals_with_the_manifest_do_not_count() {
    // Kills the `==` -> `!=` mutants on either segment comparison in
    // `reads_package::walk`: with `!=`, every one of these literals would
    // count as a package path. `"tests/pages"` is the real case
    // (`crates/tools/src/web_fetch/pages_tests.rs` joins it onto
    // `CARGO_MANIFEST_DIR` without reading a package).
    for literal in ["../../research", "tests/pages", "providersX/a"] {
        let mut files = package_ok_files();
        files.push(package_src(
            "tools",
            "crates/tools/src/search.rs",
            &format!("let dir = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"{literal}\");\n"),
        ));
        assert_eq!(
            package_reader_mismatches(&files, &package_members()).unwrap(),
            Vec::<String>::new(),
            "{literal}"
        );
    }
}

#[test]
fn a_file_that_does_not_tokenise_fails_the_package_check() {
    let mut files = package_ok_files();
    files.push(package_src(
        "tools",
        "crates/tools/src/search.rs",
        "fn broken( {\n",
    ));
    let failure = package_reader_mismatches(&files, &package_members()).unwrap_err();
    assert!(
        failure.starts_with("crates/tools/src/search.rs: does not tokenise as Rust: "),
        "{failure}"
    );
}

#[test]
fn an_include_str_in_a_comment_is_ignored() {
    let source = format!(
        "{}// include_str!(\"../../../README.md\");",
        listed_includes("contract")
    );
    let files = listed_files_with_contract(&source);
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

fn runtime_src(krate: &str, path: &str, rel: &str, source: &str) -> RustFile {
    RustFile {
        krate: krate.to_owned(),
        path: path.to_owned(),
        rel: rel.to_owned(),
        source: source.to_owned(),
    }
}

#[test]
fn a_read_to_string_of_a_docs_path_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let doc = std::fs::read_to_string(\"../../docs/tui.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_fs_read_of_a_prompt_path_is_a_run_time_read() {
    let files = [runtime_src(
        "loop",
        "crates/loop/src/reviewer.rs",
        "src/reviewer.rs",
        "let raw = std::fs::read(\"../prompt/system.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/loop/src/reviewer.rs: reads ../prompt/system.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_file_open_of_a_docs_path_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let file = std::fs::File::open(\"../../docs/errors.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: reads ../../docs/errors.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_concat_on_the_manifest_dir_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let doc = read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../docs/ci.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: reads /../docs/ci.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_join_in_a_manifest_file_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let path = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../docs/tui.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_read_of_a_computed_path_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let doc = std::fs::read_to_string(home.join(\"docs/README.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_join_without_the_manifest_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let path = home.join(\"docs/README.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_join_onto_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/ask.rs",
        "tests/ask.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet doc = std::fs::read_to_string(home.join(\"docs/README.md\")).unwrap();\nlet dir = home.join(\"docs/skills\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_join_on_a_bound_manifest_base_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet doc = std::fs::read_to_string(base.join(\"../../docs/tui.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_chained_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let a = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet b = a.join(\"x\");\nlet doc = b.join(\"docs/y\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads docs/y at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_bound_temp_dir_base_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let dir = tempdir().unwrap();\nlet doc = std::fs::read_to_string(dir.join(\"docs/README.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_let_without_the_manifest_binds_nothing() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = home_dir();\nlet p = base.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_typed_manifest_base_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let base: &Path = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet doc = base.join(\"../../docs/tui.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_mut_manifest_base_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let mut base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet doc = base.join(\"../../docs/tui.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_manifest_base_shadowed_by_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet base = tempdir();\nlet p = base.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_temp_dir_shadowed_by_a_manifest_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = tempdir();\nlet base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = base.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/x at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_block_binding_does_not_leak_to_a_sibling_block() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "{ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); }\nlet p = base.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_outer_binding_is_visible_in_a_nested_block() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nif x { base.join(\"docs/x\"); }\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/x at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_ref_to_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = fs::read_to_string(&base.join(\"docs/a\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/a at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_mut_ref_to_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = &mut base.join(\"docs/b\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/b at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_deref_of_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = *base.join(\"docs/c\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/c at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_parenthesised_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = (base).join(\"docs/d\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/d at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_parenthesised_ref_to_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = (&base).join(\"docs/e\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/e at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_rebuilt_path_on_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = Path::new(base).join(\"docs/f\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        ["crates/main/tests/release.rs: reads docs/f at run time; compile it in with include_str!"]
    );
}

#[test]
fn a_ref_to_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = &home.join(\"docs/a\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_mut_ref_to_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = &mut home.join(\"docs/b\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_deref_of_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = *home.join(\"docs/c\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_parenthesised_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = (home).join(\"docs/d\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_parenthesised_ref_to_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = (&home).join(\"docs/e\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_rebuilt_path_on_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = Path::new(home).join(\"docs/f\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_read_argument_block_does_not_leak_its_bindings() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "read_to_string({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"x\" });\nbase.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_concat_argument_block_does_not_leak_its_bindings() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let c = concat!({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"x\" });\nbase.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_join_argument_block_does_not_leak_its_bindings() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "outer.join({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"docs/x\" });\nbase.join(\"docs/y\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_partial_segment_is_not_a_repository_path() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let a = std::fs::read_to_string(\"mydocs/x.md\").unwrap();\nlet b = std::fs::read_to_string(\"docs.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_concat_without_the_manifest_dir_or_the_bang_is_not_a_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let a = concat!(\"docs/tui.md\", \"b\");\nlet b = concat(\"docs/tui.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_xtask_command_read_is_not_a_test_read() {
    let source = "let doc = std::fs::read_to_string(\"../../docs/ci.md\").unwrap();\n";
    let files = [
        runtime_src("xtask", "xtask/src/main.rs", "src/main.rs", source),
        runtime_src(
            "xtask",
            "xtask/src/ci_needs_tests.rs",
            "src/ci_needs_tests.rs",
            source,
        ),
        runtime_src("xtask", "xtask/tests/cli.rs", "tests/cli.rs", source),
    ];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "xtask/src/ci_needs_tests.rs: reads ../../docs/ci.md at run time; compile it in with include_str!",
            "xtask/tests/cli.rs: reads ../../docs/ci.md at run time; compile it in with include_str!"
        ]
    );
}

#[test]
fn a_path_in_a_comment_is_not_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "// let doc = read_to_string(\"../../docs/tui.md\");\nlet x = 1;\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_include_str_of_the_same_path_is_not_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let doc = include_str!(\"../../../docs/tui.md\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn no_files_give_no_run_time_reads() {
    assert_eq!(
        runtime_read_mismatches(&[], &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_file_outside_the_workspace_is_not_a_run_time_read() {
    let files = [runtime_src(
        "nope",
        "elsewhere/src/lib.rs",
        "src/lib.rs",
        "let doc = std::fs::read_to_string(\"../../docs/tui.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_file_that_does_not_tokenise_fails_the_run_time_check() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "fn broken( {\n",
    )];
    let failure = runtime_read_mismatches(&files, &members_with_tools()).unwrap_err();
    assert!(
        failure.starts_with("crates/contract/src/lib.rs: does not tokenise as Rust: "),
        "{failure}"
    );
}

#[test]
fn a_shared_compiled_in_skill_runs_every_crate_that_compiles_it_in() {
    let selection = classify(
        &strings(&["docs/skills/using-fiber/SKILL.md"]),
        &members_with_tools(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["loop", "main"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn docs_only_passes_for_docs_files() {
    for files in [
        vec!["docs/ci.md"],
        vec!["GLOSSARY.md"],
        vec!["docs/adr/0002-x.md"],
        vec!["README.md", "docs/events.md"],
        vec!["research/x/notes.txt"],
    ] {
        assert!(docs_only(&strings(&files)), "{files:?}");
    }
}

#[test]
fn docs_only_fails_for_empty_or_code_lists() {
    for files in [
        vec![],
        vec!["scripts/bug-red"],
        vec!["docs/ci.md", "crates/a/src/lib.rs"],
        vec!["extensions/foo/README.md"],
        vec!["providers/p/README.md"],
        vec![".github/workflows/ci.yml"],
    ] {
        assert!(!docs_only(&strings(&files)), "{files:?}");
    }
}

fn extension_case_repo() -> crate::test_dir::TestDir {
    let root = crate::test_dir::TestDir::new("extension-packages");
    root.write("providers/openrouter/tests/cost.json", "{}");
    root.write("providers/acme/tests/case.json", "{}");
    root.write("extensions/alpha/tests/case.json", "{}");
    root.write("providers/no-cases/tests/notes.md", "not a case");
    root.write("providers/no-tests/extension.json", "{}");
    root.write("providers/nested-only/tests/sub/case.json", "{}");
    root.write("providers/not-a-package", "not a directory");
    root.write("extensions/alpha/tests/nested/ignored.json", "{}");
    std::fs::create_dir(root.path().join("providers/no-cases/tests/directory.json")).unwrap();
    root
}

fn selection_with_main_for_loop_change() -> Selection {
    let mut members = package_members();
    if let Some(main) = members.get_mut("main") {
        main.deps = vec!["loop".to_owned()];
    }
    classify(&strings(&["crates/loop/src/x.rs"]), &members)
}

#[test]
fn loop_change_selects_all_direct_case_packages_in_sorted_order() {
    let root = extension_case_repo();
    let selection = selection_with_main_for_loop_change();
    assert!(selection.packages().iter().any(|package| package == "main"));
    assert_eq!(
        extension_packages(&selection, root.path()).unwrap(),
        Some(strings(&[
            "extensions/alpha",
            "providers/acme",
            "providers/openrouter"
        ]))
    );
}

#[test]
fn a_diff_without_binary_tests_selects_no_extension_packages() {
    let root = extension_case_repo();
    let selection = classify(&strings(&["xtask/src/x.rs"]), &package_members());
    assert!(!selection.packages().iter().any(|package| package == "main"));
    assert_eq!(extension_packages(&selection, root.path()).unwrap(), None);
}

#[test]
fn first_party_package_change_selects_every_package_with_cases() {
    let root = extension_case_repo();
    let selection = classify(
        &strings(&["providers/openrouter/init.lua"]),
        &package_members(),
    );
    assert_eq!(
        extension_packages(&selection, root.path()).unwrap(),
        Some(strings(&[
            "extensions/alpha",
            "providers/acme",
            "providers/openrouter"
        ]))
    );
}

#[test]
fn only_direct_json_files_make_a_case_package() {
    let root = extension_case_repo();
    let selection = selection_with_main_for_loop_change();
    let packages = extension_packages(&selection, root.path())
        .unwrap()
        .unwrap();
    for ignored in [
        "providers/no-cases",
        "providers/no-tests",
        "providers/nested-only",
    ] {
        assert!(!packages.contains(&ignored.to_owned()), "{packages:?}");
    }
}

#[test]
fn a_package_root_that_is_a_file_reports_the_root_path() {
    let selection = selection_with_main_for_loop_change();

    let providers = crate::test_dir::TestDir::new("extension-packages-provider-root-file");
    providers.write("providers", "not a directory");
    let error = extension_packages(&selection, providers.path()).unwrap_err();
    assert!(
        error.starts_with(&providers.path().join("providers").display().to_string()),
        "{error}"
    );

    let extensions = crate::test_dir::TestDir::new("extension-packages-extension-root-file");
    extensions.write("providers/.keep", "");
    extensions.write("extensions", "not a directory");
    let error = extension_packages(&selection, extensions.path()).unwrap_err();
    assert!(
        error.starts_with(&extensions.path().join("extensions").display().to_string()),
        "{error}"
    );
}

#[test]
fn a_package_tests_path_that_is_a_file_reports_that_path() {
    let root = crate::test_dir::TestDir::new("extension-packages-tests-file");
    root.write("providers/acme/tests", "not a directory");
    let selection = selection_with_main_for_loop_change();

    let error = extension_packages(&selection, root.path()).unwrap_err();
    assert!(
        error.starts_with(
            &root
                .path()
                .join("providers/acme/tests")
                .display()
                .to_string()
        ),
        "{error}"
    );
}

#[test]
fn missing_package_roots_and_tests_are_skipped() {
    let root = crate::test_dir::TestDir::new("extension-packages-missing");
    root.write("providers/no-tests/extension.json", "{}");
    let selection = selection_with_main_for_loop_change();

    assert_eq!(
        extension_packages(&selection, root.path()).unwrap(),
        Some(Vec::<String>::new())
    );
}

#[test]
fn a_missing_package_root_group_is_empty() {
    let root = crate::test_dir::TestDir::new("extension-packages-no-group");
    root.write("providers/acme/tests/case.json", "{}");
    let selection = selection_with_main_for_loop_change();
    assert_eq!(
        extension_packages(&selection, root.path()).unwrap(),
        Some(strings(&["providers/acme"]))
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
