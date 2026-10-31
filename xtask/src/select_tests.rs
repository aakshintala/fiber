use super::*;

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
    members.insert(
        "hub".to_owned(),
        Member {
            dir: "crates/hub".to_owned(),
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
