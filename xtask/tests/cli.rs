//! `cargo xtask` run as a program, against a small git repository holding a
//! Cargo workspace.

#![allow(
    clippy::unwrap_used,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); clippy exempts only #[test] functions here"
)]

#[path = "../src/child.rs"]
mod child;
#[path = "../src/test_dir.rs"]
mod test_dir;

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use child::finished;
use test_dir::TestDir;

const DEPENDENCIES: &str = "# Dependencies

## Runtime dependencies

| Crate | Used for |
|---|---|
| serde_json | JSON |

## Tests and development tools

| Crate or tool | Kind | Used for |
|---|---|---|
| cargo-nextest | tool | tests |
";

const CODE_QUALITY: &str = "# Code quality

## `unsafe`

| Crate | File | Why |
|---|---|---|
| none yet | | |
";

/// How long a test waits for one child (`git` or `xtask`) to exit, in real
/// time.
///
/// The worst test, `select_counts_untracked_and_committed_changes`, waits on
/// 9 children (`workspace()`'s 3 `git` + 3 `git` + 3 `xtask`), plus one
/// bounded reap after the first miss, then stops: (9 + 1) x 5 s = 50 s <=
/// 60 s, half of nextest's 120 s kill. A passing run never waits on it; it
/// only bounds a hang.
const CHILD_WITHIN: Duration = Duration::from_secs(5);

fn xtask(dir: &TestDir, args: &[&str], env: &[(&str, &str)], stdin: &str) -> (i32, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .envs(env.iter().copied())
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let Output {
        status,
        stdout,
        stderr,
    } = finished(
        &format!("xtask {args:?}"),
        child,
        stdin.as_bytes(),
        CHILD_WITHIN,
    );
    let text = String::from_utf8(stdout).unwrap() + &String::from_utf8(stderr).unwrap();
    (status.code().unwrap(), text)
}

fn git(dir: &TestDir, args: &[&str]) {
    let child = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let status = finished(&format!("git {args:?}"), child, &[], CHILD_WITHIN).status;
    assert!(status.success(), "git {args:?}");
}

/// A committed workspace: `a`, `b` depending on `a`, binary-only `c`
/// depending on `a`, and `outside`, a path crate that is not a member.
fn workspace() -> TestDir {
    let dir = TestDir::new("cli");
    dir.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\", \"crates/c\"]\nexclude = [\"outside\"]\nresolver = \"3\"\n",
    );
    dir.write(".gitignore", "target/\nCargo.lock\n");
    let package = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\n{deps}"
        )
    };
    dir.write("crates/a/Cargo.toml", &package("a", ""));
    dir.write("crates/a/src/lib.rs", "//! a\n");
    dir.write(
        "crates/b/Cargo.toml",
        &package("b", "a = { path = \"../a\" }\n"),
    );
    dir.write("crates/b/src/lib.rs", "//! b\n");
    dir.write(
        "crates/c/Cargo.toml",
        &package("c", "a = { path = \"../a\" }\n"),
    );
    dir.write("crates/c/src/main.rs", "fn main() {}\n");
    dir.write("outside/Cargo.toml", &package("outside", ""));
    dir.write("outside/src/lib.rs", "");
    dir.write("docs/dependencies.md", DEPENDENCIES);
    dir.write("docs/code-quality.md", CODE_QUALITY);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "base"]);
    dir
}

#[test]
fn select_runs_a_changed_crate_and_its_dependents() {
    let dir = workspace();
    dir.write("crates/a/src/lib.rs", "//! a, changed\n");
    let (code, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "mode=crates\npackages=a b c\npackage_specs=a@0.0.0 b@0.0.0 c@0.0.0\n\
             libraries=a b\nlibrary_specs=a@0.0.0 b@0.0.0\n"
        )
    );
}

#[test]
fn select_counts_untracked_and_committed_changes() {
    let dir = workspace();
    dir.write("crates/b/src/new.rs", "");
    let (_, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        out,
        "mode=crates\npackages=b\npackage_specs=b@0.0.0\nlibraries=b\nlibrary_specs=b@0.0.0\n"
    );

    git(&dir, &["checkout", "-qb", "topic"]);
    dir.write("docs/notes.md", "notes\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "docs"]);
    let (_, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        out,
        "mode=crates\npackages=b\npackage_specs=b@0.0.0\nlibraries=b\nlibrary_specs=b@0.0.0\n"
    );
    std::fs::remove_file(dir.path().join("crates/b/src/new.rs")).unwrap();
    let (_, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        out,
        "mode=docs\npackages=\npackage_specs=\nlibraries=\nlibrary_specs=\n"
    );
}

#[test]
fn select_prints_an_unambiguous_spec_for_a_crate_named_like_a_dependency() {
    // Fiber has a workspace crate named `log`. Once any crate depends on
    // `ureq`, which pulls in the crates.io `log` 0.4.x, `cargo -p log`
    // fails with "specification 'log' is ambiguous". `package_specs` and
    // `library_specs` must print `name@version` so `scripts/check` can pass
    // an unambiguous spec to `cargo clippy`/`nextest`/`test --doc`, which
    // resolve `-p` against the whole dependency graph, not just workspace
    // members.
    let dir = TestDir::new("named-like-a-dependency");
    dir.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/log\"]\nresolver = \"3\"\n",
    );
    dir.write(".gitignore", "target/\nCargo.lock\n");
    dir.write(
        "crates/log/Cargo.toml",
        "[package]\nname = \"log\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    dir.write("crates/log/src/lib.rs", "//! log\n");
    dir.write("docs/dependencies.md", DEPENDENCIES);
    dir.write("docs/code-quality.md", CODE_QUALITY);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "base"]);
    dir.write("crates/log/src/lib.rs", "//! log, changed\n");
    let (code, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "mode=crates\npackages=log\npackage_specs=log@0.0.0\n\
             libraries=log\nlibrary_specs=log@0.0.0\n"
        )
    );
}

#[test]
fn select_fails_without_a_base() {
    let dir = workspace();
    assert_eq!(xtask(&dir, &["select"], &[], "").0, 2);
    let (code, out) = xtask(&dir, &["select", "--base", "nonexistent"], &[], "");
    assert_eq!(code, 2);
    assert!(out.starts_with("xtask: git merge-base"), "{out}");
}

#[test]
fn plan_prints_the_jobs_and_shards() {
    let dir = TestDir::new("plan");
    let args = [
        "plan",
        "--mode",
        "crates",
        "--packages",
        "a b",
        "--event",
        "pull_request",
        "--bug",
        "true",
        "--mutants",
        "true",
        "--mutant-count",
        "40",
    ];
    let (code, out) = xtask(&dir, &args, &[], "");
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "jobs={\"bug_red\":true,\"lint\":true,\"mutants\":true,\"release\":false,\"test\":true}\nshards=[0,1,2]\nshard_total=3\nshard_timeout=20\n"
    );
    let skipped = [
        "plan",
        "--mode",
        "crates",
        "--packages",
        "a b",
        "--event",
        "pull_request",
        "--bug",
        "false",
        "--mutants",
        "false",
        "--mutant-count",
        "40",
    ];
    let (code, out) = xtask(&dir, &skipped, &[], "");
    assert_eq!(code, 0);
    assert!(out.contains("\"mutants\":false"), "{out}");
    assert!(out.contains("shards=[]\nshard_total=0\n"), "{out}");
    assert!(out.contains("shard_timeout=20\n"), "{out}");
    let bad = [
        "plan",
        "--mode",
        "crates",
        "--packages",
        "",
        "--event",
        "push",
    ];
    assert_eq!(xtask(&dir, &bad, &[], "").0, 2);
    let mut not_a_count = args.to_vec();
    *not_a_count.last_mut().unwrap() = "many";
    assert_eq!(xtask(&dir, &not_a_count, &[], "").0, 2);
}

#[test]
fn verdict_passes_and_fails_by_exit_code() {
    let dir = TestDir::new("verdict");
    let needs = r#"{"select":{"result":"success","outputs":{}},"docs":{"result":"success"},"test":{"result":"skipped"}}"#;
    let pass = [("NEEDS", needs), ("JOBS", r#"{"docs":true,"test":false}"#)];
    assert_eq!(
        xtask(&dir, &["verdict"], &pass, ""),
        (
            0,
            "verdict: every selected job passed and every other job was skipped\n".to_owned()
        )
    );
    let fail = [("NEEDS", needs), ("JOBS", r#"{"docs":true,"test":true}"#)];
    assert_eq!(
        xtask(&dir, &["verdict"], &fail, ""),
        (1, "verdict: test: selected, but skipped\n".to_owned())
    );
    let failed_selection = [
        ("NEEDS", r#"{"select":{"result":"failure"}}"#),
        ("JOBS", ""),
    ];
    assert_eq!(
        xtask(&dir, &["verdict"], &failed_selection, ""),
        (
            1,
            "verdict: select: failure, so the selection failed\n".to_owned()
        )
    );
    assert_eq!(xtask(&dir, &["verdict"], &[("NEEDS", "[]")], "").0, 2);
    assert_eq!(xtask(&dir, &["verdict"], &[("NEEDS", "{")], "").0, 2);
}

#[test]
fn ticket_reads_the_body_on_stdin() {
    let dir = TestDir::new("ticket");
    assert_eq!(
        xtask(&dir, &["ticket"], &[], "Body.\n\nResolves #42\n"),
        (0, "42\n".to_owned())
    );
    assert_eq!(
        xtask(&dir, &["ticket"], &[], "No ticket.\n"),
        (0, String::new())
    );
}

/// A `gh` that runs `script` through `/bin/sh`: `fakes::script` links `bin/gh`
/// to a checked-in trampoline, so no test executes a file it wrote
/// (docs/testing.md, "Waits and timeouts").
#[cfg(unix)]
fn fake_gh(dir: &TestDir, script: &str) -> String {
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fakes::script(&bin, "gh", script);
    format!(
        "{}:{}",
        dir.path().join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn ticket_prefers_the_issue_labelled_bug() {
    let dir = TestDir::new("ticket-bug");
    let path = fake_gh(
        &dir,
        "#!/bin/sh\nif [ \"$3\" = \"2\" ]; then echo bug; else echo enhancement; fi\n",
    );
    assert_eq!(
        xtask(
            &dir,
            &["ticket"],
            &[("PATH", path.as_str())],
            "Resolves #1\nResolves #2\n"
        ),
        (0, "2\n".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn ticket_prints_the_first_issue_when_none_is_labelled_bug() {
    let dir = TestDir::new("ticket-no-bug");
    let path = fake_gh(&dir, "#!/bin/sh\necho enhancement\n");
    assert_eq!(
        xtask(
            &dir,
            &["ticket"],
            &[("PATH", path.as_str())],
            "Resolves #1\nResolves #2\n"
        ),
        (0, "1\n".to_owned())
    );
}

#[cfg(unix)]
#[test]
fn ticket_fails_when_the_label_lookup_fails() {
    let dir = TestDir::new("ticket-gh-fails");
    let path = fake_gh(&dir, "#!/bin/sh\nexit 1\n");
    assert_eq!(
        xtask(
            &dir,
            &["ticket"],
            &[("PATH", path.as_str())],
            "Resolves #1\nResolves #2\n"
        )
        .0,
        2
    );
}

#[test]
fn bug_filter_prints_the_filter_and_packages() {
    let dir = workspace();
    let args = [
        "bug-filter",
        "crates/b/src/lib.rs",
        "crates/b/src/fold_tests.rs",
        "crates/a/tests/t.rs",
    ];
    let (code, out) = xtask(&dir, &args, &[], "");
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "filter\t(package(b) & test(/^fold::tests::/)) | binary_id(a::t)\n\
         package\ta\npackage\tb\n"
    );
}

#[test]
fn the_checks_pass_on_a_clean_workspace() {
    let dir = workspace();
    for check in ["line-cap", "unsafe-table", "check-docs"] {
        assert_eq!(
            xtask(&dir, &[check], &[], ""),
            (0, format!("{check}: ok\n"))
        );
    }
}

#[test]
fn the_line_cap_lists_a_long_file() {
    let dir = workspace();
    dir.write("crates/b/src/long.rs", &"x\n".repeat(801));
    dir.write("crates/b/src/long_tests.rs", &"x\n".repeat(900));
    dir.write("crates/b/tests/long.rs", &"x\n".repeat(900));
    dir.write("crates/b/notes.txt", &"x\n".repeat(900));
    assert_eq!(
        xtask(&dir, &["line-cap"], &[], ""),
        (
            0,
            "line-cap: crates/b/src/long.rs: 801 lines, over 800; file a split ticket\n".to_owned()
        )
    );
}

#[test]
fn the_unsafe_table_fails_unlisted_unsafe() {
    let dir = workspace();
    dir.write("crates/c/src/main.rs", "fn main() { unsafe {} }\n");
    let (code, out) = xtask(&dir, &["unsafe-table"], &[], "");
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "unsafe-table: crates/c/src/main.rs: uses unsafe, but the table in docs/code-quality.md does not list it\n"
    );
}

#[test]
fn the_signal_check_fails_a_signal_outside_the_allowlist() {
    let dir = workspace();
    assert_eq!(
        xtask(&dir, &["signal-sites"], &[], ""),
        (0, "signal-sites: ok\n".to_owned())
    );
    // Built from parts so this file holds no signal pattern itself; the
    // file is an untracked test file, which the check still scans.
    let pattern = ["kill", "pg"].concat();
    dir.write("crates/b/tests/evil.rs", &format!("call {pattern}(1);\n"));
    let (code, out) = xtask(&dir, &["signal-sites"], &[], "");
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!("signal-sites: crates/b/tests/evil.rs:1: {pattern}\n")
    );
}

#[test]
fn the_signal_check_passes_a_signal_in_the_allowlist() {
    let dir = TestDir::new("signal-allowlist");
    dir.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/fakes\"]\nresolver = \"3\"\n",
    );
    dir.write(".gitignore", "target/\nCargo.lock\n");
    dir.write(
        "crates/fakes/Cargo.toml",
        "[package]\nname = \"fakes\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    // Built from parts so this file holds no signal pattern itself.
    let pattern = ["kill", "pg"].concat();
    dir.write("crates/fakes/src/lib.rs", "//! fakes\n");
    dir.write(
        "crates/fakes/src/process_group.rs",
        &format!("call {pattern}(1);\n"),
    );
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "base"]);
    assert_eq!(
        xtask(&dir, &["signal-sites"], &[], ""),
        (0, "signal-sites: ok\n".to_owned())
    );
}

#[test]
fn the_dependency_list_fails_an_unlisted_crate() {
    let dir = workspace();
    assert_eq!(
        xtask(&dir, &["dependency-list"], &[], ""),
        (0, "dependency-list: ok\n".to_owned())
    );
    dir.write("crates/a/Cargo.toml", "[package]\nname = \"a\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\noutside = { path = \"../../outside\" }\n");
    assert_eq!(
        xtask(&dir, &["dependency-list"], &[], ""),
        (
            1,
            "dependency-list: a depends on outside, which docs/dependencies.md does not list\n"
                .to_owned()
        )
    );
}

#[test]
fn the_isolation_checks_fail_a_crate_that_links_an_isolated_crate() {
    let dir = workspace();
    assert_eq!(
        xtask(&dir, &["image-isolation"], &[], ""),
        (
            0,
            "image-isolation: no crate but picture and main links image code\n".to_owned()
        )
    );
    assert_eq!(
        xtask(&dir, &["tui-isolation"], &[], ""),
        (
            0,
            "tui-isolation: no crate but tui and main links ratatui or crossterm\n".to_owned()
        )
    );
    // `outside` renamed `ratatui`: `a` links it, and so `b` and `c` do.
    dir.write(
        "outside/Cargo.toml",
        "[package]\nname = \"ratatui\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    dir.write("crates/a/Cargo.toml", "[package]\nname = \"a\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nratatui = { path = \"../../outside\" }\n");
    let why = "its normal dependency tree holds ratatui; only the terminal links terminal UI code";
    assert_eq!(
        xtask(&dir, &["tui-isolation"], &[], ""),
        (
            1,
            format!("tui-isolation: a: {why}\ntui-isolation: b: {why}\ntui-isolation: c: {why}\n")
        )
    );
}

#[test]
fn the_docs_check_fails_a_broken_link() {
    let dir = workspace();
    dir.write("README.md", "[x][y]\n\n[y]: docs/missing.md\n");
    assert_eq!(
        xtask(&dir, &["check-docs"], &[], ""),
        (
            1,
            "check-docs: README.md:3: link to docs/missing.md: no such file\n".to_owned()
        )
    );
}

#[test]
fn a_check_without_its_doc_is_an_error() {
    let dir = workspace();
    std::fs::remove_file(dir.path().join("docs/code-quality.md")).unwrap();
    assert_eq!(xtask(&dir, &["unsafe-table"], &[], "").0, 2);
}

#[test]
fn docs_only_reports_whether_the_files_are_docs() {
    let dir = TestDir::new("docs-only");
    assert_eq!(
        xtask(&dir, &["docs-only", "docs/ci.md", "GLOSSARY.md"], &[], ""),
        (0, "docs-only: yes\n".to_owned())
    );
    assert_eq!(
        xtask(
            &dir,
            &["docs-only", "docs/ci.md", "crates/a/src/lib.rs"],
            &[],
            ""
        ),
        (1, "docs-only: no\n".to_owned())
    );
    assert_eq!(
        xtask(&dir, &["docs-only"], &[], ""),
        (1, "docs-only: no\n".to_owned())
    );
}

#[test]
fn ci_needs_passes_fails_and_errors() {
    const WORKFLOW: &str = "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  lint:\n    runs-on: ubuntu-24.04\n  backstop_report:\n    runs-on: ubuntu-24.04\n  bench_comment:\n    runs-on: ubuntu-24.04\n  cache_prune:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: [select, lint]\n    runs-on: ubuntu-24.04\n";
    const DOC: &str = "# CI\n\n## The merge gate\n\nEvery job but the verdict job is in its needs, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`. Done.\n";
    const FAILURE: &str = "ci-needs: .github/workflows/ci.yml: job lint is not in the ci job's needs, so CI passes without it; add it to needs, or, if it reports and gates nothing, add it to REPORT_JOBS in xtask/src/ci_needs.rs and to docs/ci.md, \"The merge gate\"\n";
    let dir = workspace();
    dir.write(".github/workflows/ci.yml", WORKFLOW);
    dir.write("docs/ci.md", DOC);
    assert_eq!(
        xtask(&dir, &["ci-needs"], &[], ""),
        (
            0,
            "ci-needs: every job but ci and the report jobs is in the ci job's needs\n".to_owned()
        )
    );
    dir.write(
        ".github/workflows/ci.yml",
        &WORKFLOW.replace("needs: [select, lint]", "needs: [select]"),
    );
    assert_eq!(xtask(&dir, &["ci-needs"], &[], ""), (1, FAILURE.to_owned()));
    std::fs::remove_file(dir.path().join("docs/ci.md")).unwrap();
    let (code, out) = xtask(&dir, &["ci-needs"], &[], "");
    assert_eq!(code, 2);
    assert!(out.contains("docs/ci.md"), "{out}");
}

#[test]
fn an_unknown_or_missing_command_is_an_error() {
    let dir = TestDir::new("unknown");
    assert_eq!(
        xtask(&dir, &["nope"], &[], ""),
        (2, "xtask: unknown command nope\n".to_owned())
    );
    assert_eq!(xtask(&dir, &[], &[], "").0, 2);
}
