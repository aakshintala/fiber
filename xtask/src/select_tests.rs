use super::*;

pub(super) fn members() -> Members {
    let member = |dir: &str, deps: &[&str]| {
        let deps: Vec<String> = deps.iter().map(|d| (*d).to_owned()).collect();
        Member {
            dir: dir.to_owned(),
            version: "0.0.0".to_owned(),
            deps: deps.clone(),
            normal_deps: deps,
            library: true,
            unrun_tests: Vec::new(),
        }
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

/// `members()` where `main` declares `tests/main_journeys.rs` with
/// `test = false`, as `crates/main/Cargo.toml` does.
pub(super) fn members_with_unrun_test() -> Members {
    let mut members = members();
    if let Some(main) = members.get_mut("main") {
        main.unrun_tests = vec!["crates/main/tests/main_journeys.rs".to_owned()];
    }
    members
}

pub(super) fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
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
        normal_deps: vec![dep.to_owned()],
        library: true,
        unrun_tests: Vec::new(),
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

/// `members_with_tools()` where `loop` also depends on `jobs` through a
/// development edge only, and `tui` normally depends on `loop`: a test-only
/// `jobs` change reaches `loop` but never `tui`.
fn members_with_dev_jobs() -> Members {
    let mut members = members_with_tools();
    members.insert(
        "jobs".to_owned(),
        Member {
            dir: "crates/jobs".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    if let Some(loop_member) = members.get_mut("loop") {
        loop_member.deps.push("jobs".to_owned());
    }
    if let Some(tui) = members.get_mut("tui") {
        tui.deps = vec!["loop".to_owned()];
        tui.normal_deps = vec!["loop".to_owned()];
    }
    members
}

/// Three fixture crates where `b` depends on `a` through a development edge
/// only and `c` normally depends on `b`: a source change to `a` reaches `b`
/// but never `c`.
fn members_with_dev_chain() -> Members {
    let member = |dir: &str, deps: &[&str], normal: &[&str]| Member {
        dir: dir.to_owned(),
        version: "0.0.0".to_owned(),
        deps: deps.iter().map(|d| (*d).to_owned()).collect(),
        normal_deps: normal.iter().map(|d| (*d).to_owned()).collect(),
        library: true,
        unrun_tests: Vec::new(),
    };
    BTreeMap::from([
        ("a".to_owned(), member("crates/a", &[], &[])),
        ("b".to_owned(), member("crates/b", &["a"], &[])),
        ("c".to_owned(), member("crates/c", &["b"], &["b"])),
    ])
}

#[test]
fn a_test_only_change_selects_its_own_crate_without_dependents() {
    let selection = classify(
        &strings(&[
            "crates/log/src/x_tests.rs",
            "crates/tui/src/app/snapshots/x.snap",
        ]),
        &members_with_tools(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["log", "tui"])));
    // A crate holding a test-only file beside a source file still expands.
    let selection = classify(
        &strings(&["crates/log/src/x_tests.rs", "crates/log/src/lib.rs"]),
        &members_with_tools(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["log", "loop"])));
    // A near-miss of the `snapshots/` segment is a source change.
    for path in [
        "crates/contract/src/snapshot/y.snap",
        "crates/contract/src/snapshots_old/y.snap",
    ] {
        let selection = classify(&strings(&[path]), &members_with_tools());
        assert_eq!(
            selection,
            Selection::Crates(strings(&["config", "contract", "log", "loop"])),
            "{path}"
        );
    }
}

#[test]
fn a_test_only_change_follows_one_dev_hop_and_stops() {
    let selection = classify(
        &strings(&["crates/jobs/src/x_tests.rs"]),
        &members_with_dev_jobs(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["jobs", "loop"])));
}

/// A test-only pair still follows each crate's own dev hop: `loop`
/// dev-depends on `jobs` and normally depends on `log`
/// (`crates/loop/Cargo.toml`), so a test-only `jobs` file plus a test-only
/// `log` file selects `loop` through the `jobs` hop, while `tui` — which
/// normally depends on `loop`, not on either narrow crate — stays out.
#[test]
fn a_test_only_pair_with_a_mixed_edge_still_follows_the_dev_hop() {
    let selection = classify(
        &strings(&["crates/jobs/src/x_tests.rs", "crates/log/src/x_tests.rs"]),
        &members_with_dev_jobs(),
    );
    assert_eq!(
        selection,
        Selection::Crates(strings(&["jobs", "log", "loop"]))
    );
}

/// The same per-crate partition with `main`'s shape from
/// `crates/main/Cargo.toml`: `main` dev-depends on `fakes` and normally
/// depends on `log`, so a test-only `fakes` file plus a test-only `log`
/// file selects `main`.
#[test]
fn a_test_only_fakes_plus_log_pair_selects_main() {
    let member = |dir: &str, deps: &[&str], normal: &[&str]| Member {
        dir: dir.to_owned(),
        version: "0.0.0".to_owned(),
        deps: deps.iter().map(|d| (*d).to_owned()).collect(),
        normal_deps: normal.iter().map(|d| (*d).to_owned()).collect(),
        library: true,
        unrun_tests: Vec::new(),
    };
    let members = BTreeMap::from([
        ("fakes".to_owned(), member("crates/fakes", &[], &[])),
        ("log".to_owned(), member("crates/log", &[], &[])),
        (
            "main".to_owned(),
            member("crates/main", &["fakes", "log"], &["log"]),
        ),
        ("tui".to_owned(), member("crates/tui", &["main"], &["main"])),
    ]);
    let selection = classify(
        &strings(&["crates/fakes/src/x_tests.rs", "crates/log/src/x_tests.rs"]),
        &members,
    );
    assert_eq!(
        selection,
        Selection::Crates(strings(&["fakes", "log", "main"]))
    );
}

/// The crate-relative slice starts past the `/` after the crate dir, so a
/// test file that is test-only only by its `tests/` directory still
/// narrows. An off-by-one in either direction reads it as a source file
/// and expands to dependents.
#[test]
fn a_tests_dir_helper_with_a_plain_name_still_narrows() {
    let selection = classify(&strings(&["crates/log/tests/helper.rs"]), &members());
    assert_eq!(selection, Selection::Crates(strings(&["log"])));
}

/// `members_from` reads the normal-edge subset from cargo metadata's
/// `kind`: a missing or null `kind` is a normal edge, anything else is
/// not. (Lives beside the selector tests because the gate counts it with
/// them; `main_tests.rs` only pins the `test = false` shape.)
#[test]
fn members_from_treats_a_missing_or_null_kind_as_a_normal_edge() {
    let package = |name: &str, deps: Vec<serde_json::Value>| {
        serde_json::json!({
            "name": name,
            "version": "0.0.0",
            "manifest_path": format!("/ws/crates/{name}/Cargo.toml"),
            "dependencies": deps,
            "targets": [],
        })
    };
    let dep = |path: &str| serde_json::json!({"path": path, "name": path});
    let mut null_kind = dep("/ws/crates/contract");
    null_kind["kind"] = serde_json::Value::Null;
    let mut dev_kind = dep("/ws/crates/jobs");
    dev_kind["kind"] = serde_json::json!("dev");
    // The third edge omits `kind` entirely: cargo metadata leaves it out
    // the same way for some path dependencies, and the rule treats a
    // missing `kind` like a null one.
    let packages = vec![
        package("loop", vec![null_kind, dev_kind, dep("/ws/crates/support")]),
        package("contract", vec![]),
        package("jobs", vec![]),
        package("support", vec![]),
    ];
    let members = crate::members_from("/ws", &packages);
    assert_eq!(
        members["loop"].deps,
        vec![
            "contract".to_owned(),
            "jobs".to_owned(),
            "support".to_owned()
        ]
    );
    assert_eq!(
        members["loop"].normal_deps,
        vec!["contract".to_owned(), "support".to_owned()]
    );
}

/// The `## Selection` section of `docs/ci.md`, which the test below pins
/// against `classify_with` clause by clause: not whole-file bytes, so
/// unrelated `docs/ci.md` edits never red it.
fn selection_section() -> String {
    let doc = include_str!("../../docs/ci.md").to_owned();
    let heading = "## Selection\n";
    let start = doc
        .find(heading)
        .expect("docs/ci.md has a Selection section");
    let rest = &doc[start + heading.len()..];
    let end = rest
        .find("\n## ")
        .map(|i| start + heading.len() + i)
        .unwrap_or(doc.len());
    // One space per whitespace run, so a promise matches however the doc
    // wraps its lines.
    doc[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_selection_sentence_matches_classify_with() {
    let section = selection_section();
    for phrase in [
        "is not a candidate",
        "every candidate file in that crate is a test file",
        "under a `snapshots/` directory",
        "one hop with no further fan-out",
        "development-edges propagate one hop and normal edges transitively",
        "keep their rules above and below",
    ] {
        assert!(
            section.contains(phrase),
            "the Selection section says: {phrase}"
        );
    }
    // The per-crate partition: the test-only log file narrows while the
    // source-changed config file expands.
    let selection = classify(
        &strings(&["crates/log/src/x_tests.rs", "crates/config/src/lib.rs"]),
        &members(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["config", "log"])));
    // The three candidate exclusions: docs, listed and package files.
    let selection = classify(
        &strings(&["crates/log/tests/x_tests.rs", "crates/log/README.md"]),
        &members(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["log"])));
    let selection = classify(
        &strings(&["crates/picture/tests/fixtures/cmyk.jpg"]),
        &members_with_tools(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["main"])));
    let selection = classify_with(
        &strings(&[
            "providers/opencode/extension.json",
            "crates/log/src/x_tests.rs",
        ]),
        &members(),
        COMPILED_IN,
        &["config"],
    );
    assert_eq!(
        selection,
        Selection::Crates(strings(&["config", "log", "main"]))
    );
    // The snapshots rule: a tui snapshot is a test-only file of tui.
    let selection = classify(
        &strings(&["crates/tui/src/app/snapshots/x.snap"]),
        &members_with_tools(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["tui"])));
    // The one dev hop: a source change to `a` reaches `b`, never `c`.
    let selection = classify(
        &strings(&["crates/a/src/lib.rs"]),
        &members_with_dev_chain(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["a", "b"])));
    // The prototype exclusion: the whole diff takes the old path.
    let selection = classify(
        &strings(&[
            "crates/log/tests/x_tests.rs",
            "research/tui-prototype/src/main.rs",
        ]),
        &members(),
    );
    assert_eq!(selection, Selection::Crates(strings(&["log", "loop"])));
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
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members.insert(
        "tui".to_owned(),
        Member {
            dir: "crates/tui".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members.insert(
        "provider".to_owned(),
        Member {
            dir: "crates/provider".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members.insert(
        "xtask".to_owned(),
        Member {
            dir: "xtask".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members.insert(
        "picture".to_owned(),
        Member {
            dir: "crates/picture".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members.insert(
        "hub".to_owned(),
        Member {
            dir: "crates/hub".to_owned(),
            version: "0.0.0".to_owned(),
            deps: Vec::new(),
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
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
            normal_deps: Vec::new(),
            library: true,
            unrun_tests: Vec::new(),
        },
    );
    members
}
