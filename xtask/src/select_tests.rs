use super::*;

use crate::rules::RustFile;

pub(super) fn members() -> Members {
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
        ("bench".to_owned(), member("crates/bench", &[])),
    ])
}

pub(super) fn strings(items: &[&str]) -> Vec<String> {
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
    for path in ["research/other/Cargo.toml", "research/other/Cargo.lock"] {
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
fn a_prototype_diff_selects_lint_and_no_tests() {
    for files in [
        &["research/tui-prototype/src/main.rs"][..],
        &["research/tui-prototype/src/main.rs", "docs/ci.md"][..],
        &["research/tui-prototype/Cargo.toml"][..],
    ] {
        let selection = classify(&strings(files), &members());
        assert_eq!(selection, Selection::Crates(vec![]), "{files:?}");
        let plan = plan(
            selection.mode(),
            selection.packages(),
            "pull_request",
            false,
            false,
            0,
        );
        assert!(plan.jobs["lint"], "{files:?}");
        assert!(!plan.jobs["test"], "{files:?}");
    }
}

#[test]
fn another_research_path_stays_docs_only_and_skips_lint() {
    let selection = classify(&strings(&["research/session-search/run.sh"]), &members());
    assert_eq!(selection, Selection::Docs);
    let plan = plan(
        selection.mode(),
        selection.packages(),
        "pull_request",
        false,
        false,
        0,
    );
    assert!(!plan.jobs["lint"]);
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
fn a_typed_let_without_a_value_does_not_bind_its_name_to_a_later_value() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let p: PathBuf;\nlet base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\np.join(\"docs/x\");\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_method_other_than_join_on_a_manifest_base_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "Path::new(env!(\"CARGO_MANIFEST_DIR\")).exists_at(\"docs/x\");\n",
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

/// `members()` with the `tools`, `tui` and `xtask` crates: only the
/// compiled-in tests name them, so the selection tests keep the smaller
/// fixture.
pub(super) fn members_with_tools() -> Members {
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

/// The members the package-reader tests use.
pub(super) fn package_members() -> Members {
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
