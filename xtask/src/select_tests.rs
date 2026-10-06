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
    let files = strings(&[
        "docs/ci.md",
        "README.md",
        "crates/loop/notes.md",
        "research/x/run.sh",
    ]);
    let selection = classify(&files, &members());
    assert_eq!(selection, Selection::Docs);
    assert_eq!(selection.mode(), "docs");
    assert!(selection.packages().is_empty());
}

#[test]
fn manifests_toolchain_and_workflows_run_everything() {
    let everything = strings(&[
        "config",
        "contract",
        "extensions",
        "log",
        "loop",
        "loop-extra",
        "main",
    ]);
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
fn a_compiled_in_crate_prompt_runs_that_crate() {
    assert_eq!(
        classify(&strings(&["crates/loop/prompt/system.md"]), &members()),
        Selection::Crates(strings(&["loop"]))
    );
}

#[test]
fn scripts_check_alone_runs_everything() {
    let everything = strings(&[
        "config",
        "contract",
        "extensions",
        "log",
        "loop",
        "loop-extra",
        "main",
    ]);
    let selection = classify(&strings(&["scripts/check"]), &members());
    assert_eq!(selection, Selection::All(everything));
    assert_eq!(selection.mode(), "all");
}

#[test]
fn clippy_toml_runs_everything() {
    let everything = strings(&[
        "config",
        "contract",
        "extensions",
        "log",
        "loop",
        "loop-extra",
        "main",
    ]);
    assert_eq!(
        classify(&strings(&["clippy.toml"]), &members()),
        Selection::All(everything)
    );
}

#[test]
fn nextest_toml_runs_everything() {
    let everything = strings(&[
        "config",
        "contract",
        "extensions",
        "log",
        "loop",
        "loop-extra",
        "main",
    ]);
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

fn jobs(lint: bool, test: bool, mutants: bool, bug_base: bool) -> BTreeMap<&'static str, bool> {
    BTreeMap::from([
        ("lint", lint),
        ("test", test),
        ("mutants", mutants),
        ("bug_base", bug_base),
    ])
}

#[test]
fn a_docs_only_pull_request_runs_no_job_after_the_selection() {
    let plan = plan("docs", &[], "pull_request", true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, false, false, false),
            shards: 0
        }
    );
}

#[test]
fn a_code_pull_request_runs_what_it_selected() {
    let plan = plan("crates", &strings(&["log"]), "pull_request", true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(true, true, true, true),
            shards: 6
        }
    );
}

#[test]
fn a_pull_request_without_a_bug_label_skips_the_bug_check() {
    let plan = plan("all", &strings(&["log"]), "pull_request", false);
    assert_eq!(plan.jobs, jobs(true, true, true, false));
}

#[test]
fn a_pull_request_that_selects_no_crate_skips_the_tests() {
    let plan = plan("crates", &[], "pull_request", false);
    assert_eq!(plan.jobs, jobs(true, false, true, false));
}

#[test]
fn the_backstop_runs_the_tests_alone() {
    let plan = plan("docs", &[], "push", true);
    assert_eq!(
        plan,
        Plan {
            jobs: jobs(false, true, false, false),
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

/// `members()` with the `tools` crate: only the compiled-in tests name
/// it, so the selection tests keep the smaller fixture.
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
    ]
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
        let files = [
            contract_src(&source),
            loop_src(&listed_includes("loop")),
            tools_src(&listed_includes("tools")),
        ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
        let files = [
            contract_src(&source),
            loop_src(&listed_includes("loop")),
            tools_src(&listed_includes("tools")),
        ];
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
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
fn package_readers_outside_the_workspace_are_dropped() {
    let mut members = members();
    members.remove("main");
    let selection = classify_with(
        &strings(&["providers/opencode/providers/opencode-go.json"]),
        &members,
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config"])));
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

fn package_ok_files() -> Vec<RustFile> {
    vec![config_reads_a_package(), main_reads_a_package()]
}

/// `members()` with the `tools` and `xtask` crates: only the package-reader
/// tests name them, so the selection tests keep the smaller fixture.
fn package_members() -> Members {
    let mut members = members_with_tools();
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
        ["main: listed as reading a first-party package, but no source reads one"]
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
fn xtask_sources_never_count_as_package_readers() {
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
    let files = [
        contract_src(&source),
        loop_src(&listed_includes("loop")),
        tools_src(&listed_includes("tools")),
    ];
    assert_eq!(
        compiled_in_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}
