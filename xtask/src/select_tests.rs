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
    for (path, crates) in [
        ("docs/events.md", &["contract"][..]),
        ("docs/errors.md", &["contract"][..]),
        ("docs/tui.md", &["contract", "extensions"][..]),
    ] {
        let selection = classify(&strings(&[path]), &members());
        assert_eq!(selection, Selection::Crates(strings(crates)), "{path}");
        assert_eq!(selection.mode(), "crates", "{path}");
    }
}

#[test]
fn an_invocation_doc_change_runs_contract_and_main() {
    let selection = classify(&strings(&["docs/invocation.md"]), &members());
    assert_eq!(selection, Selection::Crates(strings(&["contract", "main"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_shared_compiled_in_doc_runs_every_crate_that_compiles_it_in() {
    let selection = classify(&strings(&["docs/tui.md"]), &members_with_tools());
    assert_eq!(
        selection,
        Selection::Crates(strings(&["contract", "extensions", "tui"]))
    );
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_compiled_in_ci_doc_runs_xtask() {
    let selection = classify(&strings(&["docs/ci.md"]), &members_with_tools());
    assert_eq!(selection, Selection::Crates(strings(&["xtask"])));
    assert_eq!(selection.mode(), "crates");
}

#[test]
fn a_compiled_in_performance_doc_runs_xtask() {
    let selection = classify(&strings(&["docs/performance.md"]), &members_with_tools());
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
            false,
            0,
            &[],
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
        false,
        0,
        &[],
    );
    assert!(!plan.jobs["lint"]);
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
        &strings(&["providers/opencode/extension.json"]),
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
        &strings(&["providers/opencode/extension.json", "crates/log/src/lib.rs"]),
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
        &strings(&["providers/opencode/extension.json", "docs/ci.md"]),
        &members(),
        COMPILED_IN,
        &["config", "main"],
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "main"])));
}

#[test]
fn a_package_diff_runs_the_binary_tests_beside_the_listed_readers() {
    let selection = classify_with(
        &strings(&["providers/opencode/extension.json"]),
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
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str! or include_bytes!"
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
            "crates/loop/src/reviewer.rs: reads ../prompt/system.md at run time; compile it in with include_str! or include_bytes!"
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
            "crates/contract/src/lib.rs: reads ../../docs/errors.md at run time; compile it in with include_str! or include_bytes!"
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
            "crates/contract/src/lib.rs: reads /../docs/ci.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_join_in_a_manifest_file_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let path = read_to_string(std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../docs/tui.md\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_manifest_rooted_join_read_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let src = std::fs::read_to_string(Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/contract/src/golden.rs\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads crates/contract/src/golden.rs at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_read_dir_of_a_manifest_rooted_join_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let entries = std::fs::read_dir(Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/contract/src/data\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_spawn_join_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let child = std::process::Command::new(\"tool\").arg(Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/contract/src/golden.rs\")).spawn().unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_join_passed_to_a_spawn_or_a_check_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let exe = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/contract/src/tool\");\nlet child = std::process::Command::new(\"tool\").arg(&exe).spawn().unwrap();\nlet present = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"crates/contract/src/tool\").exists();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
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
        "let path = read_to_string(home.join(\"docs/README.md\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet doc = std::fs::read_to_string(home.join(\"docs/README.md\")).unwrap();\nlet dir = read_to_string(home.join(\"docs/skills\"));\n",
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
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_join_chained_two_levels_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let a = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet b = a.join(\"x\");\nlet doc = read_to_string(b.join(\"docs/y\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads docs/y at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_read_of_a_one_level_join_reports_its_literal() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let a = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet b = a.join(\"x\");\nlet doc = read_to_string(&b);\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads x at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_joined_name_in_a_read_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let src = std::fs::read_to_string(Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"golden.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads golden.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn junk_literals_in_a_read_are_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet a = std::fs::read_to_string(\".\").unwrap();\nlet b = std::fs::read_to_string(base.join(\"..\")).unwrap();\nlet c = std::fs::read_to_string(base.join(\"/\")).unwrap();\nlet d = std::fs::read_to_string(base.join(\"\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_one_level_join_passed_to_a_read_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = base.join(\"data.bin\");\nlet bytes = std::fs::read(&p).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads data.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_shadowed_joined_name_reads_through_the_old_binding() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"data.bin\");\nlet p = std::fs::read(&p).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads data.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_bound_name_with_a_trailing_comma_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"data.bin\");\nlet bytes = std::fs::read(&p,).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads data.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_bound_name_followed_by_another_argument_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"data.bin\");\nlet bytes = std::fs::read(&p, extra).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_two_level_join_passed_to_a_read_reports_the_second_join() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = base.join(\"data\");\nlet q = p.join(\"out.bin\");\nlet r = p;\nlet a = std::fs::read(q).unwrap();\nlet b = std::fs::read_to_string(r).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads out.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_two_level_chain_ending_in_a_recording_path_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/ask.rs",
        "tests/ask.rs",
        "let root = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../..\");\nlet call = root.join(\"crates/provider/tests/recordings/file.sse\");\nlet bytes = std::fs::read(call).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/ask.rs: reads crates/provider/tests/recordings/file.sse at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn shadowing_the_manifest_base_keeps_a_two_level_join_bound() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/ask.rs",
        "tests/ask.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet mid = base.join(\"data\");\nlet q = mid.join(\"out.bin\");\nlet base = tempdir();\nlet bytes = std::fs::read(q).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/ask.rs: reads out.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_manifest_rooted_join_of_an_absolute_path_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let p = std::fs::read(Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"/proc/cpuinfo\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_contained_relative_read_in_a_main_test_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/ask.rs",
        "tests/ask.rs",
        "let bytes = std::fs::read(\"../../crates/provider/tests/recordings/file.sse\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/ask.rs: reads ../../crates/provider/tests/recordings/file.sse at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_root_escape_and_an_absolute_path_are_not_run_time_reads() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let a = std::fs::read(\"../../../docs/secret.md\").unwrap();\nlet b = std::fs::read(\"../../../../etc/passwd\").unwrap();\nlet c = std::fs::read(\"/proc/cpuinfo\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn bare_event_names_and_urls_are_not_repository_paths() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let a = std::fs::read(\"started\").unwrap();\nlet b = std::fs::read(\"https://example.com/docs/x\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_concat_with_an_escaping_suffix_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let doc = read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../../docs/secret.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_bare_relative_providers_read_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let bytes = std::fs::read(\"providers/opencode/extension.json\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads providers/opencode/extension.json at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_manifest_token_providers_read_stays_with_the_package_reader_check() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers/opencode\");\nlet bytes = std::fs::read(&p).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_crate_internal_providers_path_is_a_run_time_read() {
    // Only a root `providers/` or `extensions/` path yields to the
    // package-reader check: a crate-internal segment stays a run-time read.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"src/providers/file.bin\");\nlet bytes = std::fs::read(&p).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads src/providers/file.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_bare_providers_dir_with_the_manifest_token_stays_with_the_package_reader_check() {
    // Exactly one disjunct of the exemption holds: the resolved path is the
    // bare `providers` directory itself, so the `||` joining it to
    // `resolved == "extensions"` must stay a disjunction for the read to
    // stay exempt.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let p = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../../providers\");\nlet bytes = std::fs::read(&p).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_providers_read_is_exempt_only_with_the_manifest_token() {
    // Both sides of the `literal == "CARGO_MANIFEST_DIR"` equality: with
    // the token the root `providers/` read stays with the package-reader
    // check, without it the same read is reported.
    let files = [
        runtime_src(
            "main",
            "crates/main/tests/with_token.rs",
            "tests/with_token.rs",
            "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet bytes = std::fs::read(\"../../providers/opencode/extension.json\").unwrap();\n",
        ),
        runtime_src(
            "main",
            "crates/main/tests/release.rs",
            "tests/release.rs",
            "let bytes = std::fs::read(\"../../providers/opencode/extension.json\").unwrap();\n",
        ),
    ];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads ../../providers/opencode/extension.json at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_join_onto_a_non_joined_base_binds_nothing() {
    // `tmp` never joins onto the manifest directory, so `q` binds nothing
    // and the read through `q` stays unreported.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let tmp = tempdir().unwrap();\nlet q = tmp.join(\"data\");\nlet bytes = std::fs::read(&q).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_format_with_a_runtime_value_is_not_a_run_time_read() {
    // The second argument is not `env!`: the macro takes a runtime value,
    // so the read stays unreported even though the template names a file.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let runtime = value;\nlet bytes = std::fs::read(format!(\"{}/docs/x\", env!(\"CARGO_MANIFEST_DIR\"), runtime)).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_format_without_the_manifest_dir_is_not_a_run_time_read() {
    // No literal names the manifest directory, so the template has no
    // repository root and the read stays unreported.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let bytes = std::fs::read(format!(\"{}/{}\", \"a/b\", \"c/d\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_format_plain_bare_word_suffix_is_not_a_run_time_read() {
    // The template names no file and the plain suffix holds no slash:
    // a bare word is a computed value, not a repository path.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let bytes = std::fs::read(format!(\"{}{}\", env!(\"CARGO_MANIFEST_DIR\"), \"x\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_bound_name_filters_absolute_and_escaping_joins() {
    // Each bound suffix fails one filter: absolute, escaping, and the
    // junk marker the containment check rejects, so no read reports.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet k = base.join(\"/proc/cpuinfo\");\nlet b = std::fs::read(&k).unwrap();\nlet e = base.join(\"../../../docs/secret.md\");\nlet c = std::fs::read(&e).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_bound_name_with_a_marker_suffix_is_not_a_run_time_read() {
    // The manifest marker is a junk suffix, not a file: the bound-name
    // filter drops it, so no read reports. Flipping either `&&` around
    // the marker check to `||` reports it, so this pins both mutants.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet k = base.join(\"CARGO_MANIFEST_DIR\");\nlet b = std::fs::read(&k).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_read_dir_inside_a_read_argument_never_reports() {
    // The directory read forces its argument out of read mode, so the
    // manifest-rooted join inside it never reports.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet x = std::fs::read_to_string(std::fs::read_dir(base.join(\"data\")).unwrap()).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_concat_skips_marker_junk_and_bare_words() {
    // Only the path-like suffix reports: the marker, the junk slash and
    // the bare word each hit one arm of the skip.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let doc = read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/\", \"x\", \"/docs/ci.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: reads /docs/ci.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_three_level_chain_is_a_run_time_read() {
    // A join onto a two-level bound name still names a repository file:
    // the report names the final join's literals.
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = base.join(\"data\");\nlet q = p.join(\"sub\");\nlet bytes = std::fs::read(q.join(\"docs/out.bin\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/golden_tests.rs: reads docs/out.bin at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_manifest_rooted_format_read_of_a_contained_path_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let bytes = std::fs::read(format!(\"{}/../../research/pdf-tool-results/text.pdf\", env!(\"CARGO_MANIFEST_DIR\"))).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads {}/../../research/pdf-tool-results/text.pdf at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn an_escaping_or_absolute_format_suffix_is_not_a_run_time_read() {
    // An escaping template and an absolute suffix never report, even when
    // the file holds the manifest token elsewhere.
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let a = std::fs::read(format!(\"{}/../../../docs/x.md\", env!(\"CARGO_MANIFEST_DIR\"))).unwrap();\nlet b = std::fs::read(format!(\"{}/{}\", env!(\"CARGO_MANIFEST_DIR\"), \"/proc/docs/x\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_temp_dir_join_in_a_read_is_not_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let dir = tempdir().unwrap();\nlet s = std::fs::read_to_string(dir.join(\"out.txt\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_manifest_literal_joined_onto_another_base_binds_nothing() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/golden_tests.rs",
        "src/golden_tests.rs",
        "let h = home.join(env!(\"CARGO_MANIFEST_DIR\"));\nlet s = std::fs::read_to_string(h.join(\"data.md\")).unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
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
        "let base = home_dir();\nlet p = read_to_string(base.join(\"docs/x\"));\n",
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
        "let base: &Path = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet doc = read_to_string(base.join(\"../../docs/tui.md\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_mut_manifest_base_is_a_run_time_read() {
    let files = [runtime_src(
        "tui",
        "crates/tui/src/theme_tests.rs",
        "src/theme_tests.rs",
        "let mut base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet doc = read_to_string(base.join(\"../../docs/tui.md\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/tui/src/theme_tests.rs: reads ../../docs/tui.md at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_manifest_base_shadowed_by_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet base = tempdir();\nlet p = read_to_string(base.join(\"docs/x\"));\n",
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
        "let base = tempdir();\nlet base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string(base.join(\"docs/x\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/x at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_block_binding_does_not_leak_to_a_sibling_block() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "{ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); }\nlet p = read_to_string(base.join(\"docs/x\"));\n",
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
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nif x { read_to_string(base.join(\"docs/x\")); }\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/x at run time; compile it in with include_str! or include_bytes!"
        ]
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
        [
            "crates/main/tests/release.rs: reads docs/a at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_mut_ref_to_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string(&mut base.join(\"docs/b\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/b at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_deref_of_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string(*base.join(\"docs/c\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/c at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_parenthesised_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string((base).join(\"docs/d\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/d at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_parenthesised_ref_to_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string((&base).join(\"docs/e\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/e at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_rebuilt_path_on_a_bound_base_is_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let base = Path::new(env!(\"CARGO_MANIFEST_DIR\"));\nlet p = read_to_string(&Path::new(base).join(\"docs/f\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/main/tests/release.rs: reads docs/f at run time; compile it in with include_str! or include_bytes!"
        ]
    );
}

#[test]
fn a_ref_to_a_temp_dir_is_not_a_run_time_read() {
    let files = [runtime_src(
        "main",
        "crates/main/tests/release.rs",
        "tests/release.rs",
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string(&home.join(\"docs/a\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string(&mut home.join(\"docs/b\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string(*home.join(\"docs/c\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string((home).join(\"docs/d\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string((&home).join(\"docs/e\"));\n",
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
        "let m = env!(\"CARGO_MANIFEST_DIR\");\nlet p = read_to_string(Path::new(home).join(\"docs/f\"));\n",
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
        "read_to_string({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"x\" });\nread_to_string(base.join(\"docs/x\"));\n",
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
        "let c = concat!({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"x\" });\nread_to_string(base.join(\"docs/x\"));\n",
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
        "outer.join({ let base = Path::new(env!(\"CARGO_MANIFEST_DIR\")); \"docs/x\" });\nread_to_string(base.join(\"docs/y\"));\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_contained_partial_segment_is_a_run_time_read() {
    let files = [runtime_src(
        "contract",
        "crates/contract/src/lib.rs",
        "src/lib.rs",
        "let a = std::fs::read_to_string(\"mydocs/x.md\").unwrap();\nlet b = std::fs::read_to_string(\"docs.md\").unwrap();\n",
    )];
    assert_eq!(
        runtime_read_mismatches(&files, &members_with_tools()).unwrap(),
        [
            "crates/contract/src/lib.rs: reads mydocs/x.md at run time; compile it in with include_str! or include_bytes!"
        ]
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
    // One level up: the manifest-relative read must stay inside the
    // repository for the containment check to report it.
    let source = "let doc = std::fs::read_to_string(\"../docs/ci.md\").unwrap();\n";
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
            "xtask/src/ci_needs_tests.rs: reads ../docs/ci.md at run time; compile it in with include_str! or include_bytes!",
            "xtask/tests/cli.rs: reads ../docs/ci.md at run time; compile it in with include_str! or include_bytes!"
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

fn extension_case_repo() -> fakes::TempDir {
    let root = fakes::TempDir::new("xtask-extension-packages");
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
        &strings(&["providers/openrouter/providers/openrouter.lua"]),
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

    let providers = fakes::TempDir::new("xtask-extension-packages-provider-root-file");
    providers.write("providers", "not a directory");
    let error = extension_packages(&selection, providers.path()).unwrap_err();
    assert!(
        error.starts_with(&providers.path().join("providers").display().to_string()),
        "{error}"
    );

    let extensions = fakes::TempDir::new("xtask-extension-packages-extension-root-file");
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
    let root = fakes::TempDir::new("xtask-extension-packages-tests-file");
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
    let root = fakes::TempDir::new("xtask-extension-packages-missing");
    root.write("providers/no-tests/extension.json", "{}");
    let selection = selection_with_main_for_loop_change();

    assert_eq!(
        extension_packages(&selection, root.path()).unwrap(),
        Some(Vec::<String>::new())
    );
}

#[test]
fn a_missing_package_root_group_is_empty() {
    let root = fakes::TempDir::new("xtask-extension-packages-no-group");
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
        "provider".to_owned(),
        Member {
            dir: "crates/provider".to_owned(),
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
    members.insert(
        "picture".to_owned(),
        Member {
            dir: "crates/picture".to_owned(),
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
