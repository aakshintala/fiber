use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use super::*;
use crate::rules::RustFile;

struct RestoreFile {
    path: PathBuf,
    source: String,
}

impl Drop for RestoreFile {
    fn drop(&mut self) {
        let restored = fs::write(&self.path, &self.source);
        assert!(
            restored.is_ok() || std::thread::panicking(),
            "could not restore {}: {restored:?}",
            self.path.display()
        );
    }
}

struct RemoveFile(PathBuf);

impl Drop for RemoveFile {
    fn drop(&mut self) {
        match fs::remove_file(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not remove {}: {error}", self.0.display()),
        }
    }
}

fn file(path: &str, rel: &str, source: &str) -> RustFile {
    RustFile {
        krate: "x".to_owned(),
        path: path.to_owned(),
        rel: rel.to_owned(),
        source: source.to_owned(),
    }
}

/// The failures for one file whose changed lines are `changed`.
fn check(path: &str, rel: &str, source: &str, changed: &[usize]) -> Vec<String> {
    let files = [file(path, rel, source)];
    let mut lines = BTreeSet::new();
    lines.extend(changed.iter().copied());
    let mut map = BTreeMap::new();
    map.insert(path.to_owned(), lines);
    check_files(&files, &map)
}

#[test]
fn the_bounds_match_the_shared_consts() {
    assert_eq!(WAITS, fakes::deadline::WAITS);
    assert_eq!(BUDGET, fakes::deadline::BUDGET);
    assert_eq!(CLEANUP, fakes::deadline::CLEANUP);
    let floor = std::time::Duration::from_secs(60);
    assert_eq!(known_ms("WAIT"), Some(fakes::deadline::WAIT >= floor));
    assert_eq!(known_ms("WAITS"), Some(fakes::deadline::WAITS >= floor));
    assert_eq!(known_ms("BUDGET"), Some(fakes::deadline::BUDGET >= floor));
    assert_eq!(known_ms("CLEANUP"), Some(fakes::deadline::CLEANUP >= floor));
    assert_eq!(
        known_ms("MUST_SUCCEED_WITHIN"),
        Some(fakes::MUST_SUCCEED_WITHIN >= floor)
    );
}

#[test]
fn malformed_rust_fails_closed() {
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", "fn a( {", &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("does not tokenise as Rust"), "{failures:?}");
}

#[test]
fn a_raised_guard_const_fails() {
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        "pub const WAITS: Duration = Duration::from_secs(91);",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with("crates/support/src/deadline.rs:1: const WAITS is 91 s, above"),
        "{failures:?}"
    );
}

#[test]
fn a_non_duration_budget_const_is_not_a_timeout_bound() {
    assert!(
        check(
            "crates/main/src/tool_budget_tests.rs",
            "src/tool_budget_tests.rs",
            "const BUDGET: usize = 15815;",
            &[1],
        )
        .is_empty()
    );
}

#[test]
fn each_named_guard_const_is_pinned_at_its_edge() {
    for (name, at, over) in [
        ("WAITS", "90", "91"),
        ("BUDGET", "110", "111"),
        ("CLEANUP", "100", "101"),
    ] {
        let source = format!("pub const {name}: Duration = Duration::from_secs({at});");
        assert!(
            check("crates/x/src/a.rs", "src/a.rs", &source, &[1]).is_empty(),
            "{name} at {at} passes"
        );
        let source = format!("pub const {name}: Duration = Duration::from_secs({over});");
        assert_eq!(
            check("crates/x/src/a.rs", "src/a.rs", &source, &[1]).len(),
            1,
            "{name} at {over} fails"
        );
    }
}

#[test]
fn a_lowered_guard_const_passes() {
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            "pub const WAITS: Duration = Duration::from_secs(89);",
            &[1],
        )
        .is_empty()
    );
}

#[test]
fn an_unruled_sixty_second_after_in_a_test_fails() {
    for (after, case) in [
        ("Duration::from_secs(60)", "an unruled 60 s after"),
        ("Duration::from_secs(61)", "an unruled 61 s after"),
        (
            "Duration::from_millis(60_000)",
            "an unruled 60_000 ms after",
        ),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{case}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is 60 s or more"),
            "{case}: {failures:?}"
        );
    }
}

#[test]
fn a_fifty_nine_second_after_in_a_test_passes() {
    let source = "fn a() { Deadline::after(Duration::from_secs(59)); }";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_long_after_outside_a_test_file_passes() {
    let source = "fn a() { Deadline::after(Duration::from_secs(60)); }";
    assert!(check("crates/x/src/a.rs", "src/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_ruled_line_passes() {
    let source = "pub const WAITS: Duration = Duration::from_secs(90); // ruling: #3662";
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            source,
            &[1]
        )
        .is_empty()
    );
    let source = "fn a() { Deadline::after(Duration::from_secs(60)); // ruling: #3662\n}";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn an_unchanged_long_after_outside_the_diff_passes() {
    let source = "fn a() {\n    Deadline::after(Duration::from_secs(60));\n}";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_const_bound_passes_outside_the_diff() {
    let source = "pub const WAITS: Duration = Duration::from_secs(91);";
    assert!(
        check(
            "crates/support/src/deadline.rs",
            "src/deadline.rs",
            source,
            &[]
        )
        .is_empty()
    );
}

#[test]
fn suffixed_decimal_bounds_are_read() {
    let failures = check(
        "crates/x/tests/a.rs",
        "tests/a.rs",
        "fn a() { Deadline::after(Duration::from_secs(60u64)); }",
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
    assert!(
        check(
            "crates/x/tests/a.rs",
            "tests/a.rs",
            "fn a() { Deadline::after(Duration::from_secs(59u64)); }",
            &[1],
        )
        .is_empty()
    );
}

#[test]
fn a_shared_bound_name_passes() {
    let source = "fn a() { Deadline::after(fakes::deadline::WAIT); }";
    assert!(check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty());
}

#[test]
fn a_short_named_bound_passes() {
    for after in [
        "MUST_SUCCEED_WITHIN",
        "fakes::MUST_SUCCEED_WITHIN",
        "Duration::ZERO",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        assert!(
            check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]).is_empty(),
            "{after} passes"
        );
    }
}

#[test]
fn a_long_named_bound_fails() {
    for (after, name) in [
        ("fakes::deadline::WAITS", "WAITS"),
        ("fakes::deadline::BUDGET", "BUDGET"),
        ("fakes::deadline::CLEANUP", "CLEANUP"),
        ("Duration::MAX", "MAX"),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is 60 s or more"),
            "{name}: {failures:?}"
        );
    }
}

#[test]
fn an_unshown_after_fails_closed() {
    for (after, case) in [
        ("SPAN", "a bare name"),
        ("Duration::from_secs(30 + 30)", "a computed literal"),
        ("Duration::from_secs(SIXTY)", "a named literal"),
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{case}: {failures:?}");
        assert!(
            failures[0].starts_with("crates/x/tests/a.rs:1: Deadline::after(")
                && failures[0].ends_with("is not shown below 60 s"),
            "{case}: {failures:?}"
        );
    }
}

#[test]
fn radix_integer_bounds_are_read() {
    for after in [
        "Duration::from_secs(0x3c)",
        "Duration::from_secs(0o74)",
        "Duration::from_secs(0b111100)",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].ends_with("is 60 s or more"),
            "{after}: {failures:?}"
        );
    }
}

#[test]
fn imported_duration_aliases_are_read() {
    for (after, fails) in [
        ("Span::from_secs(30)", false),
        ("Span::from_secs(60)", true),
    ] {
        let source =
            format!("use std::time::Duration as Span;\nfn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[2]);
        assert_eq!(failures.len(), usize::from(fails), "{after}: {failures:?}");
    }
}

#[test]
fn unsupported_duration_paths_fail_closed() {
    for after in [
        "Span::from_secs(30)",
        "Duration.:from_secs(30)",
        "Duration:.from_secs(30)",
        "Duration::from_secs[30]",
        "foo.bar.Duration::from_secs(30)",
        ":x std::time::Duration::from_secs(30)",
    ] {
        let source = format!("fn a() {{ Deadline::after({after}); }}");
        let failures = check("crates/x/tests/a.rs", "tests/a.rs", &source, &[1]);
        assert_eq!(failures.len(), 1, "{after}: {failures:?}");
        assert!(
            failures[0].ends_with("is not shown below 60 s"),
            "{after}: {failures:?}"
        );
    }
}

#[test]
fn absolute_duration_paths_are_read() {
    let source = "fn a() { Deadline::after(::std::time::Duration::from_secs(60)); }";
    let failures = check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].ends_with("is 60 s or more"), "{failures:?}");
}

#[test]
fn other_after_methods_are_not_deadline_bounds() {
    for source in [
        "fn a() { Other::after(Duration::from_secs(60)); }",
        "fn a() { Deadline.:after(Duration::from_secs(60)); }",
    ] {
        assert!(
            check("crates/x/tests/a.rs", "tests/a.rs", source, &[1]).is_empty(),
            "{source}"
        );
    }
}

#[test]
fn budget_bounds_reads_changed_lines_and_rejects_a_new_long_after() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "budget_bounds::tests::budget_bounds_probe_child",
            "--test-threads=1",
        ])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "invoked by budget_bounds_reads_changed_lines_and_rejects_a_new_long_after"]
fn budget_bounds_probe_child() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let test_path = root.join("xtask/src/budget_bounds_tests.rs");
    let test_source = include_str!("budget_bounds_tests.rs");
    let _restore_test = RestoreFile {
        path: test_path.clone(),
        source: test_source.to_owned(),
    };
    let probe = "\nfn budget_bounds_probe() { Deadline::after(Duration::from_secs(60)); }\n";
    let probe_line = test_source.lines().count() + 2;
    fs::write(&test_path, format!("{test_source}{probe}")).unwrap();

    let docs_path = root.join("docs/testing.md");
    let docs_source = include_str!("../../docs/testing.md");
    let _restore_docs = RestoreFile {
        path: docs_path.clone(),
        source: docs_source.to_owned(),
    };
    fs::write(
        &docs_path,
        format!("{docs_source}\n<!-- budget bounds probe -->\n"),
    )
    .unwrap();

    let ignored_path = root.join("xtask/src/budget_bounds_probe.txt");
    fs::write(&ignored_path, "not Rust\n").unwrap();
    let _remove_ignored = RemoveFile(ignored_path);

    let changed = changed_lines("HEAD").unwrap();
    let test_rel = "xtask/src/budget_bounds_tests.rs";
    assert!(
        changed
            .get(test_rel)
            .is_some_and(|lines| lines.contains(&probe_line)),
        "the added line must be included in its hunk: {changed:?}"
    );
    assert!(!changed.contains_key("docs/testing.md"), "{changed:?}");
    assert!(
        !changed.contains_key("xtask/src/budget_bounds_probe.txt"),
        "{changed:?}"
    );

    let failures = budget_bounds("HEAD").unwrap();
    assert!(
        failures.iter().any(|failure| {
            failure.starts_with(&format!("{test_rel}:{probe_line}: Deadline::after("))
        }),
        "{failures:?}"
    );
}

#[test]
fn an_unshown_guard_const_fails_closed() {
    let source = "pub const WAITS: Duration = Duration::from_secs(SIXTY);";
    let failures = check(
        "crates/support/src/deadline.rs",
        "src/deadline.rs",
        source,
        &[1],
    );
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].ends_with("is not shown at or below 90 s"),
        "{failures:?}"
    );
}
