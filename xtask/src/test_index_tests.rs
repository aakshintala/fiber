use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::rules::RustFile;
use crate::select::{
    Member, Members, TestRef, changed_tests, filter, test_files, test_filter, tests_in_files,
    unrun_test_files,
};
use crate::test_index::TestIndex;

struct Suite {
    key: &'static str,
    binary_id: &'static str,
    kind: &'static str,
    dependencies: &'static [&'static str],
    tests: &'static [&'static str],
}

struct Layout {
    bug: &'static str,
    name: &'static str,
    path: &'static str,
    source: &'static str,
    changed_line: usize,
    changed_expected: Option<Vec<TestRef>>,
    extra_sources: Vec<(&'static str, &'static str)>,
    suites: Vec<Suite>,
    expected: Vec<TestRef>,
    unrun: bool,
}

fn test(package: &str, binary: &str, unit: bool, name: &str) -> TestRef {
    TestRef {
        package: package.to_owned(),
        binary: (!unit).then(|| binary.to_owned()),
        name: name.to_owned(),
        unit,
    }
}

fn members() -> Members {
    let member = |dir: &str, unrun_tests: Vec<String>| Member {
        dir: dir.to_owned(),
        version: "0.0.0".to_owned(),
        deps: Vec::new(),
        library: true,
        unrun_tests,
    };
    BTreeMap::from([
        ("log".to_owned(), member("crates/log", Vec::new())),
        (
            "main".to_owned(),
            member(
                "crates/main",
                vec!["crates/main/tests/main_journeys.rs".to_owned()],
            ),
        ),
        ("loop".to_owned(), member("crates/loop", Vec::new())),
    ])
}

fn suite_json(root: &std::path::Path, suite: &Suite, number: usize) -> Value {
    let executable = root
        .join(format!("target/debug/deps/fixture-{number}"))
        .to_string_lossy()
        .into_owned();
    let dep_info = format!("{executable}.d");
    let dependencies = suite.dependencies.join(" ");
    std::fs::create_dir_all(
        std::path::Path::new(&dep_info)
            .parent()
            .expect("dep-info dir"),
    )
    .expect("create dep-info dir");
    std::fs::write(&dep_info, format!("{executable}: {dependencies}\n\n")).expect("write dep-info");
    let cases: Map<String, Value> = suite
        .tests
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                json!({"filter-match": {"status": "matches"}}),
            )
        })
        .collect();
    json!({
        "package-name": if suite.binary_id.starts_with("main::") { "main" } else if suite.binary_id.starts_with("loop::") { "loop" } else { "log" },
        "binary-id": suite.binary_id,
        "kind": suite.kind,
        "binary-path": executable,
        "testcases": cases,
    })
}

fn index_for(root: &std::path::Path, suites: &[Suite]) -> TestIndex {
    let mut rust_suites = Map::new();
    for (number, suite) in suites.iter().enumerate() {
        rust_suites.insert(suite.key.to_owned(), suite_json(root, suite, number));
    }
    TestIndex::from_list(root, &json!({"rust-suites": rust_suites}).to_string())
        .expect("compiler test index")
}

#[test]
fn compiler_index_covers_the_reported_layouts() {
    let rows = [
        Layout {
            bug: "#3906",
            name: "test = false target",
            path: "crates/main/tests/main_journeys.rs",
            source: "#[test] fn journey() {}\n",
            changed_line: 0,
            changed_expected: None,
            extra_sources: vec![],
            suites: vec![],
            expected: vec![],
            unrun: true,
        },
        Layout {
            bug: "#3907",
            name: "cfg(target_os) test",
            path: "crates/log/tests/platform.rs",
            source: "#[cfg(target_os = \"macos\")] #[test] fn macos_only() {}\n#[cfg(target_os = \"linux\")] #[test] fn linux_only() {}\n",
            changed_line: 1,
            changed_expected: None,
            extra_sources: vec![],
            suites: vec![Suite {
                key: "platform",
                binary_id: "log::platform",
                kind: "test",
                dependencies: &["crates/log/tests/platform.rs"],
                tests: &["linux_only"],
            }],
            expected: vec![test("log", "log::platform", false, "linux_only")],
            unrun: false,
        },
        Layout {
            bug: "#3960",
            name: "cfg-gated module",
            path: "crates/log/src/gated_tests.rs",
            source: "#[cfg(feature = \"optional\")] mod gated { #[test] fn enabled_only() {} }\n#[test] fn always_built() {}\n",
            changed_line: 0,
            changed_expected: Some(vec![]),
            extra_sources: vec![],
            suites: vec![Suite {
                key: "unit",
                binary_id: "log",
                kind: "lib",
                dependencies: &["crates/log/src/gated_tests.rs"],
                tests: &["always_built"],
            }],
            expected: vec![test("log", "log", true, "always_built")],
            unrun: false,
        },
        Layout {
            bug: "#3852",
            name: "tests/support helper shared by binaries",
            path: "crates/loop/tests/support/mod.rs",
            source: "pub fn shared_setup() {}\n",
            changed_line: 0,
            changed_expected: Some(vec![]),
            extra_sources: vec![],
            suites: vec![
                Suite {
                    key: "driver",
                    binary_id: "loop::driver",
                    kind: "test",
                    dependencies: &["crates/loop/tests/support/mod.rs"],
                    tests: &["runs_driver"],
                },
                Suite {
                    key: "journey",
                    binary_id: "loop::journey",
                    kind: "test",
                    dependencies: &["crates/loop/tests/support/mod.rs"],
                    tests: &["runs_journey"],
                },
            ],
            expected: vec![
                test("loop", "loop::driver", false, "runs_driver"),
                test("loop", "loop::journey", false, "runs_journey"),
            ],
            unrun: false,
        },
        Layout {
            bug: "#3963",
            name: "#[path]-declared unit test",
            path: "crates/log/src/renamed_tests.rs",
            source: "#[test] fn resolves_alias() {}\n",
            changed_line: 0,
            changed_expected: None,
            extra_sources: vec![(
                "crates/log/src/settings.rs",
                "#[path = \"renamed_tests.rs\"] mod aliases;\n",
            )],
            suites: vec![Suite {
                key: "unit",
                binary_id: "log",
                kind: "lib",
                dependencies: &[
                    "crates/log/src/settings.rs",
                    "crates/log/src/renamed_tests.rs",
                ],
                tests: &["settings::aliases::resolves_alias"],
            }],
            expected: vec![test(
                "log",
                "log",
                true,
                "settings::aliases::resolves_alias",
            )],
            unrun: false,
        },
        Layout {
            bug: "#3963",
            name: "foo_tests.rs beside foo.rs",
            path: "crates/log/src/foo_tests.rs",
            source: "#[test] fn adjacent() {}\n",
            changed_line: 0,
            changed_expected: None,
            extra_sources: vec![(
                "crates/log/src/foo.rs",
                "#[cfg(test)] #[path = \"foo_tests.rs\"] mod tests;\n",
            )],
            suites: vec![Suite {
                key: "unit",
                binary_id: "log",
                kind: "lib",
                dependencies: &["crates/log/src/foo.rs", "crates/log/src/foo_tests.rs"],
                tests: &["foo::tests::adjacent"],
            }],
            expected: vec![test("log", "log", true, "foo::tests::adjacent")],
            unrun: false,
        },
        Layout {
            bug: "#3963",
            name: "foo/tests.rs",
            path: "crates/log/src/foo/tests.rs",
            source: "#[test] fn nested() {}\n",
            changed_line: 0,
            changed_expected: None,
            extra_sources: vec![("crates/log/src/foo.rs", "#[cfg(test)] mod tests;\n")],
            suites: vec![Suite {
                key: "unit",
                binary_id: "log",
                kind: "lib",
                dependencies: &["crates/log/src/foo.rs", "crates/log/src/foo/tests.rs"],
                tests: &["foo::tests::nested"],
            }],
            expected: vec![test("log", "log", true, "foo::tests::nested")],
            unrun: false,
        },
        Layout {
            bug: "#2944 / #2977",
            name: "runtime repository reader in a test helper",
            path: "crates/log/tests/support/repo_reader.rs",
            source: "pub fn read_repo_file() { let _ = std::fs::read(\"docs/testing.md\"); }\n",
            changed_line: 0,
            changed_expected: Some(vec![]),
            extra_sources: vec![],
            suites: vec![Suite {
                key: "reader",
                binary_id: "log::reader",
                kind: "test",
                dependencies: &["crates/log/tests/support/repo_reader.rs"],
                tests: &["reads_repository_file"],
            }],
            expected: vec![test("log", "log::reader", false, "reads_repository_file")],
            unrun: false,
        },
    ];

    for (number, row) in rows.into_iter().enumerate() {
        let temp = fakes::TempDir::new(&format!("xtask-index-{number}"));
        let mut members = members();
        if row.unrun {
            members = members_with_unrun();
        }
        let mut sources = vec![RustFile {
            krate: if row.path.starts_with("crates/main/") {
                "main"
            } else if row.path.starts_with("crates/loop/") {
                "loop"
            } else {
                "log"
            }
            .to_owned(),
            path: row.path.to_owned(),
            rel: row
                .path
                .split_once('/')
                .and_then(|(_, rest)| rest.split_once('/').map(|(_, rel)| rel))
                .unwrap_or_default()
                .to_owned(),
            source: row.source.to_owned(),
        }];
        sources.extend(row.extra_sources.iter().map(|(path, source)| {
            RustFile {
                krate: "log".to_owned(),
                path: (*path).to_owned(),
                rel: path
                    .split_once('/')
                    .and_then(|(_, rest)| rest.split_once('/').map(|(_, rel)| rel))
                    .unwrap_or_default()
                    .to_owned(),
                source: (*source).to_owned(),
            }
        }));
        let index = index_for(temp.path(), &row.suites);
        let files = [row.path.to_owned()];
        let candidates = test_files(&files, &members, &sources).expect("test files in fixture");
        assert_eq!(
            candidates,
            if row.unrun {
                Vec::new()
            } else {
                files.to_vec()
            },
            "{} {} candidate files",
            row.bug,
            row.name
        );
        assert_eq!(
            unrun_test_files(&files, &members),
            if row.unrun {
                files.to_vec()
            } else {
                Vec::new()
            },
            "{} {} unrun files",
            row.bug,
            row.name
        );
        let actual = tests_in_files(&files, &members, &sources, &index).expect("tests in fixture");
        assert_eq!(actual, row.expected, "{} {}", row.bug, row.name);

        let changed_line = row
            .source
            .lines()
            .nth(row.changed_line)
            .expect("fixture source line");
        let new_line = row.changed_line + 1;
        let diff = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{old_start},0 +{new_line} @@\n+{changed_line}\n",
            path = row.path,
            old_start = row.changed_line,
        );
        let changed =
            changed_tests(&diff, &members, &sources, &index).expect("changed tests in fixture");
        let expected_changed = row.changed_expected.as_ref().unwrap_or(&row.expected);
        assert_eq!(
            &changed, expected_changed,
            "{} {} stress selector",
            row.bug, row.name
        );

        let (expression, packages) =
            test_filter(&files, &members, &sources, &index).expect("bug filter in fixture");
        assert_eq!(
            expression,
            filter(&row.expected),
            "{} {} bug filter",
            row.bug,
            row.name
        );
        let expected_packages: Vec<String> = row
            .expected
            .iter()
            .map(|test| test.package.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(
            packages, expected_packages,
            "{} {} packages",
            row.bug, row.name
        );
        assert_eq!(
            index.binaries_for(row.path).len(),
            row.suites.len(),
            "{} {}",
            row.bug,
            row.name
        );
    }
}

#[test]
fn each_missing_suite_field_is_rejected_independently() {
    let temp = fakes::TempDir::new("xtask-index-missing-field");
    for missing in ["package-name", "binary-id", "kind", "binary-path"] {
        let mut suite = json!({
            "package-name": "log",
            "binary-id": "log::unit",
            "kind": "lib",
            "binary-path": temp.path().join("target/debug/deps/unit").display().to_string(),
        });
        suite.as_object_mut().expect("suite object").remove(missing);
        let listed = json!({"rust-suites": {"unit": suite}}).to_string();
        assert_eq!(
            TestIndex::from_list(temp.path(), &listed)
                .expect_err("missing suite field")
                .as_str(),
            "nextest list: suite is missing package-name, binary-id, kind or binary-path",
            "missing {missing}"
        );
    }
}

#[cfg(unix)]
#[test]
fn workspace_relative_resolves_a_source_through_a_symlink_alias() {
    let temp = fakes::TempDir::new("xtask-index-path-alias");
    let root = temp.path().join("workspace");
    let source = root.join("crates/log/tests/platform.rs");
    std::fs::create_dir_all(source.parent().expect("source directory")).expect("create source dir");
    std::fs::write(&source, "#[test] fn platform() {}\n").expect("write source");
    let alias = temp.path().join("workspace-alias");
    std::os::unix::fs::symlink(&root, &alias).expect("create workspace alias");

    assert_eq!(
        super::workspace_relative(&root, &alias.join("crates/log/tests/platform.rs")),
        Some("crates/log/tests/platform.rs".to_owned())
    );
}

#[test]
fn a_suite_without_testcases_still_indexes_its_dep_info_sources() {
    let temp = fakes::TempDir::new("xtask-index-empty-suite");
    let suite = Suite {
        key: "empty",
        binary_id: "log::empty",
        kind: "test",
        dependencies: &["crates/log/tests/empty.rs"],
        tests: &[],
    };
    let mut listed = suite_json(temp.path(), &suite, 0);
    listed
        .as_object_mut()
        .expect("suite object")
        .remove("testcases");
    let index = TestIndex::from_list(
        temp.path(),
        &json!({"rust-suites": {"empty": listed}}).to_string(),
    )
    .expect("empty suite index");

    let binaries = index.binaries_for("crates/log/tests/empty.rs");
    assert_eq!(binaries.len(), 1);
    assert!(binaries[0].tests.is_empty());
}

fn members_with_unrun() -> Members {
    let mut members = members();
    if let Some(main) = members.get_mut("main") {
        main.unrun_tests = vec!["crates/main/tests/main_journeys.rs".to_owned()];
    }
    members
}
