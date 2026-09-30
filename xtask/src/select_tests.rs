use super::*;

use crate::rules::RustFile;

fn members() -> Members {
    let member = |dir: &str, deps: &[&str]| Member {
        dir: dir.to_owned(),
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
    ])
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn markdown_docs_and_research_run_the_docs_job_alone() {
    let files = strings(&[
        "docs/ci.md",
        "README.md",
        "crates/loop/prompt/system.md",
        "research/x/run.sh",
    ]);
    let selection = classify(&files, &members());
    assert_eq!(selection, Selection::Docs);
    assert_eq!(selection.mode(), "docs");
    assert!(selection.packages().is_empty());
}

#[test]
fn manifests_toolchain_and_workflows_run_everything() {
    let everything = strings(&["config", "contract", "log", "loop", "loop-extra"]);
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
    for path in ["docs/events.md", "docs/errors.md", "docs/invocation.md"] {
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
fn readme_alone_runs_the_docs_job() {
    assert_eq!(
        classify(&strings(&["README.md"]), &members()),
        Selection::Docs
    );
}

#[test]
fn a_crate_prompt_runs_the_docs_job() {
    assert_eq!(
        classify(&strings(&["crates/loop/prompt/system.md"]), &members()),
        Selection::Docs
    );
}

#[test]
fn scripts_check_alone_runs_everything() {
    let everything = strings(&["config", "contract", "log", "loop", "loop-extra"]);
    let selection = classify(&strings(&["scripts/check"]), &members());
    assert_eq!(selection, Selection::All(everything));
    assert_eq!(selection.mode(), "all");
}

#[test]
fn clippy_toml_runs_everything() {
    let everything = strings(&["config", "contract", "log", "loop", "loop-extra"]);
    assert_eq!(
        classify(&strings(&["clippy.toml"]), &members()),
        Selection::All(everything)
    );
}

#[test]
fn nextest_toml_runs_everything() {
    let everything = strings(&["config", "contract", "log", "loop", "loop-extra"]);
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
    for extra in ["crates/contract/README.md", "crates/loop/prompt/system.md"] {
        let selection = classify(&strings(&["docs/events.md", extra]), &members());
        assert_eq!(
            selection,
            Selection::Crates(strings(&["contract"])),
            "{extra}"
        );
    }
}

#[test]
fn shards_are_one_per_25_mutants_at_most_6() {
    for (mutants, shards) in [
        (0, 0),
        (1, 1),
        (25, 1),
        (26, 2),
        (150, 6),
        (151, 6),
        (1000, 6),
    ] {
        assert_eq!(shard_count(mutants), shards, "{mutants} mutants");
    }
}

fn jobs(
    docs: bool,
    lint: bool,
    test: bool,
    mutants: bool,
    bug_base: bool,
) -> BTreeMap<&'static str, bool> {
    BTreeMap::from([
        ("docs", docs),
        ("lint", lint),
        ("test", test),
        ("mutants", mutants),
        ("bug_base", bug_base),
    ])
}

#[test]
fn a_docs_only_pull_request_runs_the_docs_job_alone() {
    let plan = plan("docs", &[], "pull_request", true, 40);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_code_pull_request_runs_what_it_selected() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", true, 40);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, true, true),
            shards: 2
        }
    );
}

#[test]
fn a_pull_request_without_mutants_or_a_bug_label_runs_neither_check() {
    let plan = plan("all", &strings(&["log"]), "pull_request", false, 0);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_pull_request_that_selects_no_crate_skips_the_tests() {
    let plan = plan("crates", &[], "pull_request", false, 0);
    assert_eq!(plan.jobs, jobs(true, true, false, false, false));
}

#[test]
fn the_backstop_runs_the_tests_alone() {
    let plan = plan("docs", &[], "push", true, 40);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, true, false, false),
            shards: 0
        }
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
fn the_ticket_is_the_resolved_issue() {
    assert_eq!(
        ticket("Does a thing.\n\nResolves #212\n"),
        Some("212".to_owned())
    );
    assert_eq!(ticket("fixes #7"), Some("7".to_owned()));
    assert_eq!(ticket("Closed #9."), Some("9".to_owned()));
    assert_eq!(ticket("See #3, then close\n#4"), Some("4".to_owned()));
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
        assert_eq!(ticket(body), None, "{body:?}");
    }
}

fn declared(declared_in: &[(String, String)]) -> Vec<(&str, &str)> {
    declared_in
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_str()))
        .collect()
}

const PLAIN: &str = "#[cfg(test)]\nmod tests;";

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
    let (expression, packages, tests) = test_filter(&files, &members());
    assert_eq!(
        expression.split(" | ").collect::<Vec<_>>(),
        [
            "(package(log) & test(/^writer::tests::/))",
            "(package(log) & test(/^fold::tests::/))",
            "(package(log) & test(/^tests::/))",
            "binary_id(loop::turns)",
            "binary_id(loop::support)",
            "(package(log) & test(/^fold::inner::tests::/))",
            "(package(log) & test(/^tests::/))",
            "(package(loop) & test(/^tests::/))",
        ]
    );
    assert_eq!(packages, strings(&["log", "loop"]));
    let paths: Vec<&str> = tests.iter().map(|t| t.path.as_str()).collect();
    assert_eq!(paths, files.get(..8).unwrap());
    let all: Vec<Vec<(&str, &str)>> = tests.iter().map(|t| declared(&t.declared_in)).collect();
    assert_eq!(
        all,
        [
            vec![
                (
                    "crates/log/src/writer.rs",
                    "#[cfg(test)]\n#[path = \"writer_tests.rs\"]\nmod tests;"
                ),
                (
                    "crates/log/src/writer/mod.rs",
                    "#[cfg(test)]\n#[path = \"../writer_tests.rs\"]\nmod tests;"
                ),
            ],
            vec![
                ("crates/log/src/fold.rs", PLAIN),
                ("crates/log/src/fold/mod.rs", PLAIN)
            ],
            vec![
                ("crates/log/src/lib.rs", PLAIN),
                ("crates/log/src/main.rs", PLAIN)
            ],
            vec![],
            vec![],
            vec![
                (
                    "crates/log/src/fold/inner.rs",
                    "#[cfg(test)]\n#[path = \"inner_tests.rs\"]\nmod tests;"
                ),
                (
                    "crates/log/src/fold/inner/mod.rs",
                    "#[cfg(test)]\n#[path = \"../inner_tests.rs\"]\nmod tests;"
                ),
            ],
            vec![(
                "crates/log/src/lib.rs",
                "#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;"
            )],
            vec![(
                "crates/loop/src/main.rs",
                "#[cfg(test)]\n#[path = \"main_tests.rs\"]\nmod tests;"
            )],
        ]
    );
}

#[test]
fn only_a_crate_root_file_is_a_crate_root() {
    let (expression, _, tests) =
        test_filter(&strings(&["crates/log/src/fold/lib_tests.rs"]), &members());
    assert_eq!(expression, "(package(log) & test(/^fold::lib::tests::/))");
    assert_eq!(
        declared(&tests.first().unwrap().declared_in),
        [
            (
                "crates/log/src/fold/lib.rs",
                "#[cfg(test)]\n#[path = \"lib_tests.rs\"]\nmod tests;"
            ),
            (
                "crates/log/src/fold/lib/mod.rs",
                "#[cfg(test)]\n#[path = \"../lib_tests.rs\"]\nmod tests;"
            ),
        ]
    );
}

#[test]
fn no_test_files_give_an_empty_filter() {
    assert_eq!(
        test_filter(&strings(&["crates/log/src/writer.rs"]), &members()),
        (String::new(), vec![], vec![])
    );
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

fn listed_includes() -> String {
    COMPILED_IN
        .iter()
        .map(|(path, _)| format!("include_str!(\"../../../{path}\");\n"))
        .collect()
}

#[test]
fn a_listed_include_passes() {
    let files = [contract_src(&listed_includes())];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn an_unlisted_outside_include_fails() {
    let source = format!("{}include_str!(\"../../../README.md\");", listed_includes());
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        ["README.md: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_non_docs_outside_include_fails() {
    let source = format!("{}include_str!(\"../../../LICENSE\");", listed_includes());
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        ["LICENSE: contract compiles it in, but the compiled-in list does not list it"]
    );
}

#[test]
fn an_unlisted_markdown_inside_the_crate_dir_fails() {
    let source = format!(
        "{}include_str!(\"../prompt/system.md\");",
        listed_includes()
    );
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
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
    );
    assert_eq!(selection, Selection::Crates(strings(&["contract"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_non_docs_include_inside_the_crate_dir_is_unlisted() {
    let source = format!("{}include_str!(\"owned.bin\");", listed_includes());
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_raw_string_include_is_resolved() {
    for extra in [
        r#"include_str!(r"../../../README.md");"#,
        r##"include_str!(r#"../../../README.md"#);"##,
    ] {
        let source = format!("{}{extra}", listed_includes());
        let files = [contract_src(&source)];
        assert_eq!(
            compiled_in_mismatches(&files, &members()).unwrap(),
            ["README.md: contract compiles it in, but the compiled-in list does not list it"],
            "{extra}"
        );
    }
}

#[test]
fn an_unresolvable_include_argument_fails() {
    let source = format!(
        "{}include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../README.md\"));",
        listed_includes()
    );
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        [
            "crates/contract/src/lib.rs: include_str! argument is not a string literal; the compiled-in check cannot resolve it"
        ]
    );
}

#[test]
fn another_macro_with_a_string_argument_yields_no_target() {
    let source = format!("{}my_macro!(\"../x.md\");", listed_includes());
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn include_bytes_yields_its_target() {
    let source = format!("{}include_bytes!(\"../x.md\");", listed_includes());
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        [
            "crates/contract/x.md: contract compiles it in, but the compiled-in list does not list it"
        ]
    );
}

#[test]
fn an_include_str_in_a_comment_is_ignored() {
    let source = format!(
        "{}// include_str!(\"../../../README.md\");",
        listed_includes()
    );
    let files = [contract_src(&source)];
    assert_eq!(
        compiled_in_mismatches(&files, &members()).unwrap(),
        Vec::<String>::new()
    );
}
