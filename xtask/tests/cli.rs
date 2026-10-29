//! `cargo xtask` run as a program, against a small git repository holding a
//! Cargo workspace.

#![allow(
    clippy::unwrap_used,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); clippy exempts only #[test] functions here"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::process::{Command, Stdio};

use fakes::TempDir;

#[path = "common/mod.rs"]
mod common;

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

/// Runs the built `xtask` in `dir`. Each child, a `git` or an `xtask`, waits
/// under `fakes::MUST_SUCCEED_WITHIN`. A hang fails at the first child that
/// runs out, after one bounded reap, so one test runs out at most 2 x 10 s =
/// 20 s, under half of nextest's 120 s kill. A passing run never waits on
/// the bound.
fn xtask(dir: &TempDir, args: &[&str], env: &[(&str, &str)], stdin: &str) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .args(args)
        .envs(env.iter().copied())
        .current_dir(dir.path())
        .stdin(Stdio::piped());
    let (code, text) = common::run(
        &format!("xtask {args:?}"),
        command,
        stdin.as_bytes(),
        MUST_SUCCEED_WITHIN,
    );
    (code.unwrap(), text)
}

fn git(dir: &TempDir, args: &[&str]) {
    // This wrapper's shape differs from `common::git` (no output, the shared
    // must-succeed bound), so it keeps its body and shares only the wait.
    let mut command = Command::new("git");
    command
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir.path())
        .stdin(Stdio::null());
    let (code, out) = common::run(&format!("git {args:?}"), command, &[], MUST_SUCCEED_WITHIN);
    assert_eq!(code, Some(0), "git {args:?}: {out}");
}

/// A committed workspace: `a`, `b` depending on `a`, binary-only `c`
/// depending on `a`, and `outside`, a path crate that is not a member.
fn workspace() -> TempDir {
    let dir = TempDir::new("xtask-cli");
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
    dir.write("crates/a/src/deleted.rs", "//! deleted fixture\n");
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

fn architecture_workspace() -> TempDir {
    let dir = workspace();
    dir.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/a\", \"crates/b\", \"crates/c\", \"crates/tools\", \"crates/loop\"]\nexclude = [\"outside\"]\nresolver = \"3\"\n",
    );
    dir.write(
        "crates/tools/Cargo.toml",
        "[package]\nname = \"tools\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    dir.write(
        "crates/tools/src/lib.rs",
        "pub struct Tool { pub name: String }\npub fn tool() -> Tool { Tool { name: \"shell\".to_owned() } }\n",
    );
    dir.write(
        "crates/loop/Cargo.toml",
        "[package]\nname = \"loop\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    dir.write("crates/loop/src/lib.rs", "//! loop\n");
    dir
}

fn add_architecture_noise(dir: &TempDir, source: &str) {
    dir.write(
        ".gitignore",
        "target/\nCargo.lock\ncrates/a/src/ignored.rs\n",
    );
    dir.write("crates/a/src/ignored.rs", source);
    dir.write("outside/src/lib.rs", source);
    dir.write("crates/a/src/lib.rs", "#[cfg(test)]\nmod test_support;\n");
    dir.write("crates/a/src/test_support.rs", source);
    dir.write("crates/a/src/architecture_tests.rs", source);
    dir.write("crates/a/tests/architecture.rs", source);
    dir.write("crates/a/src/deleted.rs", source);
    std::fs::remove_file(dir.path().join("crates/a/src/deleted.rs")).unwrap();
}

#[test]
fn select_runs_a_changed_crate_and_its_dependents() {
    let dir = workspace();
    dir.write("crates/a/src/lib.rs", "//! a, changed\n");
    // The exact output pins the keys `scripts/check` reads (`mode`,
    // `packages`, `package_specs`, `library_specs`) and the absence of
    // any `libraries=` line.
    let (code, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "mode=crates\npackages=a b c\npackage_specs=a@0.0.0 b@0.0.0 c@0.0.0\n\
             library_specs=a@0.0.0 b@0.0.0\n"
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
        "mode=crates\npackages=b\npackage_specs=b@0.0.0\nlibrary_specs=b@0.0.0\n"
    );

    git(&dir, &["checkout", "-qb", "topic"]);
    dir.write("docs/notes.md", "notes\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "docs"]);
    let (_, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        out,
        "mode=crates\npackages=b\npackage_specs=b@0.0.0\nlibrary_specs=b@0.0.0\n"
    );
    std::fs::remove_file(dir.path().join("crates/b/src/new.rs")).unwrap();
    let (_, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(
        out,
        "mode=docs\npackages=\npackage_specs=\nlibrary_specs=\n"
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
    let dir = TempDir::new("xtask-named-like-a-dependency");
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
             library_specs=log@0.0.0\n"
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
    let dir = workspace();
    let args = [
        "plan",
        "--base",
        "main",
        "--mode",
        "crates",
        "--packages",
        "a b",
        "--event",
        "pull_request",
        "--draft",
        "false",
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
        "jobs={\"bug_red\":true,\"lint\":true,\"mutants\":true,\"release\":false,\"stress\":true,\"test\":true,\"test_linux\":true}\nshards=[0,1,2]\nshard_total=3\nshard_timeout=40\nten_turns=false\nbase_skip=\n"
    );
    let bad = [
        "plan",
        "--base",
        "main",
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

/// `HOME` and `TMPDIR` point at the temp dir, so a test that runs `git`
/// or the built `xtask` reaches no real home.
fn home(dir: &TempDir) -> String {
    dir.path().to_str().unwrap_or_default().to_owned()
}

fn home_env(home: &str) -> Vec<(&str, &str)> {
    vec![("HOME", home), ("TMPDIR", home)]
}

/// A branch whose head adds a test to `crates/a/tests/t.rs` and edits one
/// of the two tests in `crates/b/src/fold_tests.rs`, on top of a base that
/// holds both tests.
fn stress_branch() -> TempDir {
    let dir = workspace();
    dir.write(
        "crates/b/src/fold_tests.rs",
        "#[test]\nfn first() {}\n\n#[test]\nfn second() {}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "two tests"]);
    git(&dir, &["switch", "-qc", "pr"]);
    dir.write("crates/a/tests/t.rs", "#[test]\nfn new_test() {}\n");
    dir.write(
        "crates/b/src/fold_tests.rs",
        "#[test]\nfn first() {}\n\n#[test]\nfn second() {\n    let _ = 1;\n}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "touch two tests"]);
    dir
}

#[test]
fn stress_plan_prints_the_touched_tests() {
    let dir = stress_branch();
    let home = home(&dir);
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main"],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0);
    // `docs/ci.md` promises which tests run but not the order of the
    // selection lines, so every line but the run/count head compares as a
    // set, and the filter terms compare as a set too.
    let mut head = Vec::new();
    let mut terms = Vec::new();
    let mut rest = Vec::new();
    for line in out.lines() {
        if let Some(filter) = line.strip_prefix("filter\t") {
            terms.extend(filter.split(" | ").map(str::to_owned));
        } else if line.starts_with("run\t") || line.starts_with("count\t") {
            head.push(line.to_owned());
        } else {
            rest.push(line.to_owned());
        }
    }
    head.sort();
    terms.sort();
    rest.sort();
    assert_eq!(head, ["count\t30".to_owned(), "run\ttrue".to_owned()]);
    assert_eq!(
        terms,
        [
            "(binary_id(a::t) & test(=new_test))".to_owned(),
            "(package(b) & test(=fold::tests::second))".to_owned(),
        ]
    );
    assert_eq!(
        rest,
        [
            "package\ta@0.0.0".to_owned(),
            "package\tb@0.0.0".to_owned(),
            "test\ta::t new_test".to_owned(),
            "test\tb fold::tests::second".to_owned(),
        ]
    );
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main", "--labels", "test-only"],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0);
    assert!(out.starts_with("run\ttrue\ncount\t100\n"), "{out}");
}

#[test]
fn stress_plan_reports_no_run_when_no_test_function_changes() {
    let dir = workspace();
    dir.write(
        "crates/b/src/fold_tests.rs",
        "#[test]\nfn first() {}\n\n#[test]\nfn second() {}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "two tests"]);
    git(&dir, &["switch", "-qc", "pr"]);
    dir.write(
        "crates/b/src/fold_tests.rs",
        "#[test]\nfn first() {}\n\n#[test]\nfn second() {}\n\nfn helper() {}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "helper only"]);
    let home = home(&dir);
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main"],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(out, "run\tfalse\ncount\t30\nfilter\t\n");
}

#[test]
fn stress_plan_appends_its_summary() {
    let dir = stress_branch();
    let home = home(&dir);
    dir.write("summary.md", "before\n");
    let summary = dir.path().join("summary.md");
    let summary = summary.to_str().unwrap();
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main", "--summary", summary],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(summary).unwrap(),
        "before\n\
         ### Stress job: 2 tests, 30 runs each\n\
         ```\n\
         test\ta::t new_test\n\
         test\tb fold::tests::second\n\
         ```\n"
    );
}

#[test]
fn stress_plan_with_no_touched_tests_summarises_the_empty_selection() {
    let dir = workspace();
    git(&dir, &["switch", "-qc", "pr"]);
    dir.write("crates/b/src/lib.rs", "//! b\n\nfn helper() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "helper only"]);
    let home = home(&dir);
    dir.write("summary.md", "");
    let summary = dir.path().join("summary.md");
    let summary = summary.to_str().unwrap();
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main", "--summary", summary],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(summary).unwrap(),
        "### Stress job: no test added or changed\n"
    );
}

#[test]
fn stress_plan_with_a_bad_base_is_an_error() {
    let dir = stress_branch();
    git(&dir, &["switch", "main"]);
    git(&dir, &["branch", "-D", "pr"]);
    git(&dir, &["switch", "-qc", "pr"]);
    git(&dir, &["branch", "-D", "main"]);
    let home = home(&dir);
    let (code, out) = xtask(
        &dir,
        &["stress-plan", "--base", "main"],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 2);
    assert!(out.starts_with("xtask: git merge-base"), "{out}");
    assert!(!out.contains("run\t"), "{out}");
}

#[test]
fn touched_retries_fails_on_a_touched_test() {
    let dir = stress_branch();
    let home = home(&dir);
    dir.write(
        "flaky.txt",
        "a::t new_test\tfirst failure\nb fold::tests::second\tfirst failure\n",
    );
    let list = dir.path().join("flaky.txt");
    let list = list.to_str().unwrap();
    let (code, out) = xtask(
        &dir,
        &["touched-retries", "--base", "main", "--list", list],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "touched-retries: a::t new_test was added or changed by this pull request and passed only on retry\n\
         touched-retries: b fold::tests::second was added or changed by this pull request and passed only on retry\n"
    );
}

#[test]
fn touched_retries_passes_on_an_untouched_test() {
    let dir = stress_branch();
    let home = home(&dir);
    dir.write(
        "flaky.txt",
        "a::t other\tfirst failure\nc whatever\tfirst failure\n",
    );
    let list = dir.path().join("flaky.txt");
    let list = list.to_str().unwrap();
    let (code, out) = xtask(
        &dir,
        &["touched-retries", "--base", "main", "--list", list],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(out, "touched-retries: ok\n");
}

#[test]
fn touched_retries_with_a_missing_list_is_an_error() {
    let dir = stress_branch();
    let home = home(&dir);
    let missing = dir.path().join("missing.txt");
    let missing = missing.to_str().unwrap();
    let (code, out) = xtask(
        &dir,
        &["touched-retries", "--base", "main", "--list", missing],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 2);
    assert!(out.starts_with("xtask: "), "{out}");
}

#[test]
fn stress_cost_prints_the_cost_block() {
    let dir = workspace();
    let home = home(&dir);
    let (code, out) = xtask(
        &dir,
        &[
            "stress-cost",
            "--start",
            "100",
            "--end",
            "532",
            "--count",
            "30",
            "--tests",
            "5",
            "--outcome",
            "success",
        ],
        &home_env(&home),
        "",
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "### Stress job cost\n\
         wall time: 7m 12s (432 s)\n\
         runner minutes: 8\n\
         stress-cost: wall_seconds=432 runner_minutes=8 count=30 tests=5 outcome=success\n"
    );
    let bad = [
        "stress-cost",
        "--start",
        "many",
        "--end",
        "532",
        "--count",
        "30",
        "--tests",
        "5",
        "--outcome",
        "success",
    ];
    assert_eq!(xtask(&dir, &bad, &home_env(&home), "").0, 2);
    let missing = [
        "stress-cost",
        "--end",
        "532",
        "--count",
        "30",
        "--tests",
        "5",
        "--outcome",
        "success",
    ];
    assert_eq!(xtask(&dir, &missing, &home_env(&home), "").0, 2);
}

/// The `plan` arguments the release-code CLI tests run: a code pull request
/// without the binary-level tests, so only a release-code path selects
/// `release`.
fn release_plan_args(base: &str) -> Vec<&str> {
    vec![
        "plan",
        "--base",
        base,
        "--mode",
        "crates",
        "--packages",
        "a",
        "--event",
        "pull_request",
        "--draft",
        "false",
        "--bug",
        "false",
        "--mutants",
        "false",
        "--mutant-count",
        "0",
    ]
}

fn plan_selects_release(dir: &TempDir, base: &str) -> bool {
    let args = release_plan_args(base);
    let (code, out) = xtask(dir, &args, &[], "");
    assert_eq!(code, 0, "{out}");
    let jobs = out
        .lines()
        .find(|line| line.starts_with("jobs="))
        .unwrap_or_default();
    assert!(!jobs.is_empty(), "{out}");
    jobs.contains("\"release\":true")
}

#[test]
fn plan_runs_the_ten_turns_workload_only_where_it_is_measured() {
    let dir = workspace();
    let args = release_plan_args("main");
    let ten_turns = |dir: &TempDir| {
        let (code, out) = xtask(dir, &args, &[], "");
        assert_eq!(code, 0, "{out}");
        let line = out
            .lines()
            .find(|line| line.starts_with("ten_turns="))
            .unwrap_or_default();
        assert!(!line.is_empty(), "{out}");
        line == "ten_turns=true"
    };
    assert!(!ten_turns(&dir));
    dir.write("crates/loop/src/status.rs", "//! status\n");
    assert!(ten_turns(&dir));
}

#[test]
fn plan_skips_the_base_rows_the_diff_does_not_select() {
    let dir = workspace();
    let args = release_plan_args("main");
    let base_skip = |dir: &TempDir| {
        let (code, out) = xtask(dir, &args, &[], "");
        assert_eq!(code, 0, "{out}");
        let line = out
            .lines()
            .find(|line| line.starts_with("base_skip="))
            .unwrap_or_default();
        assert!(!line.is_empty(), "{out}");
        line.to_owned()
    };
    assert_eq!(base_skip(&dir), "base_skip=");
    dir.write("crates/tui/src/x.rs", "//! x\n");
    assert_eq!(base_skip(&dir), "base_skip=session start,sessions list");
    dir.write("crates/loop/src/status.rs", "//! status\n");
    assert_eq!(base_skip(&dir), "base_skip=sessions list");
}

#[test]
fn plan_runs_release_for_a_changed_bench_harness() {
    let dir = workspace();
    assert!(!plan_selects_release(&dir, "main"));
    dir.write("xtask/src/bench.rs", "//! bench\n");
    assert!(plan_selects_release(&dir, "main"));
}

#[test]
fn plan_runs_release_when_the_bench_harness_is_renamed_away() {
    let dir = workspace();
    dir.write("xtask/src/bench.rs", "//! bench\n//! line\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "bench"]);
    git(&dir, &["checkout", "-qb", "topic"]);
    git(&dir, &["mv", "xtask/src/bench.rs", "xtask/src/judge.rs"]);
    git(&dir, &["commit", "-qm", "rename"]);
    // Without `--no-renames` git reports only `xtask/src/judge.rs`, so no
    // release-code path is seen and `release` stays off.
    assert!(plan_selects_release(&dir, "main"));
}

#[test]
fn plan_runs_release_for_a_non_ascii_bench_path() {
    let dir = workspace();
    git(&dir, &["checkout", "-qb", "topic"]);
    dir.write("crates/bench/examples/bench/\u{e9}.rs", "//! bench\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "bench"]);
    // Without `-z` git quotes the path, so the `crates/bench/` prefix is
    // missed and `release` stays off.
    assert!(plan_selects_release(&dir, "main"));

    let dir = workspace();
    dir.write("crates/bench/src/\u{e9}.rs", "//! bench\n");
    assert!(plan_selects_release(&dir, "main"));
}

#[test]
fn select_ignores_a_rename_source_for_its_own_selection() {
    let dir = workspace();
    dir.write("crates/a/src/x.rs", "//! x\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "x"]);
    git(&dir, &["checkout", "-qb", "topic"]);
    git(&dir, &["mv", "crates/a/src/x.rs", "docs/x.rs"]);
    git(&dir, &["commit", "-qm", "rename"]);
    let (code, out) = xtask(&dir, &["select", "--base", "main"], &[], "");
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("mode=docs\n"), "{out}");
}

#[test]
fn plan_fails_without_a_base() {
    let dir = workspace();
    let args = [
        "plan",
        "--mode",
        "crates",
        "--packages",
        "a",
        "--event",
        "pull_request",
        "--draft",
        "false",
        "--bug",
        "false",
        "--mutants",
        "false",
        "--mutant-count",
        "0",
    ];
    assert_eq!(xtask(&dir, &args, &[], "").0, 2);
}

#[test]
fn verdict_passes_and_fails_by_exit_code() {
    let dir = TempDir::new("xtask-verdict");
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
    assert_eq!(xtask(&dir, &["verdict"], &[("NEEDS", "[]")], "").0, 2);
    assert_eq!(xtask(&dir, &["verdict"], &[("NEEDS", "{")], "").0, 2);
}

#[test]
fn verdict_counts_stress_like_every_gating_job() {
    let dir = TempDir::new("xtask-verdict-stress");
    let skipped = r#"{"select":{"result":"success","outputs":{}},"stress":{"result":"skipped"}}"#;
    let unselected = [("NEEDS", skipped), ("JOBS", r#"{"stress":false}"#)];
    assert_eq!(
        xtask(&dir, &["verdict"], &unselected, ""),
        (
            0,
            "verdict: every selected job passed and every other job was skipped\n".to_owned()
        )
    );
    let failed = r#"{"select":{"result":"success","outputs":{}},"stress":{"result":"failure"}}"#;
    let selected = [("NEEDS", failed), ("JOBS", r#"{"stress":true}"#)];
    assert_ne!(
        xtask(&dir, &["verdict"], &selected, "").0,
        0,
        "a selected stress failure must fail the verdict"
    );
}

#[test]
fn ticket_reads_the_body_on_stdin() {
    let dir = TempDir::new("xtask-ticket");
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
fn fake_gh(dir: &TempDir, script: &str) -> String {
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
    let dir = TempDir::new("xtask-ticket-bug");
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
    let dir = TempDir::new("xtask-ticket-no-bug");
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
    let dir = TempDir::new("xtask-ticket-gh-fails");
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
fn session_paths_cli_reports_an_untracked_production_violation_only() {
    let dir = architecture_workspace();
    add_architecture_noise(&dir, "fn fake() { let _ = dir.join(\"sessions\"); }\n");
    assert_eq!(
        xtask(&dir, &["session-paths"], &[], ""),
        (0, "session-paths: ok\n".to_owned())
    );
    dir.write(
        "crates/b/src/new.rs",
        "fn open_session() { let _ = dir.join(\"sessions\"); }\n",
    );
    assert_eq!(
        xtask(&dir, &["session-paths"], &[], ""),
        (
            1,
            "session-paths: crates/b/src/new.rs:1: session path outside log\n".to_owned()
        )
    );
}

#[test]
fn fiber_home_paths_cli_reports_an_untracked_production_violation_only() {
    let dir = architecture_workspace();
    add_architecture_noise(
        &dir,
        "fn fake(home: &Path) { let _ = home.join(\"projects\"); }\n",
    );
    assert_eq!(
        xtask(&dir, &["fiber-home-paths"], &[], ""),
        (0, "fiber-home-paths: ok\n".to_owned())
    );
    dir.write(
        "crates/b/src/new.rs",
        "fn read_project(home: &Path) { let _ = home.join(\"projects\"); }\n",
    );
    assert_eq!(
        xtask(&dir, &["fiber-home-paths"], &[], ""),
        (
            1,
            "fiber-home-paths: crates/b/src/new.rs:1: Fiber-home path outside config\n".to_owned()
        )
    );
}

#[test]
fn loop_tool_names_cli_reports_an_untracked_production_violation_only() {
    let dir = architecture_workspace();
    add_architecture_noise(&dir, "fn fake() { let _ = \"shell\"; }\n");
    assert_eq!(
        xtask(&dir, &["loop-tool-names"], &[], ""),
        (0, "loop-tool-names: ok\n".to_owned())
    );
    dir.write(
        "crates/loop/src/new.rs",
        "fn choose_tool() { let _ = \"shell\"; }\n",
    );
    assert_eq!(
        xtask(&dir, &["loop-tool-names"], &[], ""),
        (
            1,
            "loop-tool-names: crates/loop/src/new.rs:1: built-in tool name shell\n".to_owned()
        )
    );
}

#[test]
fn architecture_command_reports_source_collection_failure_as_exit_two() {
    let dir = workspace();
    std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
    let (code, out) = xtask(&dir, &["session-paths"], &[], "");
    assert_eq!(code, 2);
    assert!(out.starts_with("xtask: git ls-files --cached --others --exclude-standard -- crates/a crates/b crates/c: fatal: not a git repository"), "{out}");
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
            "line-cap: crates/b/src/long.rs: 801 lines, over 800\n".to_owned()
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
    dir.write(
        "crates/b/tests/evil.rs",
        &format!("// call {pattern}(1);\n"),
    );
    assert_eq!(
        xtask(&dir, &["signal-sites"], &[], ""),
        (0, "signal-sites: ok\n".to_owned())
    );
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
    let dir = TempDir::new("xtask-signal-allowlist");
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
fn the_must_succeed_check_passes_a_clean_workspace_and_fails_a_short_bound() {
    let dir = workspace();
    assert_eq!(
        xtask(&dir, &["must-succeed"], &[], ""),
        (0, "must-succeed: ok\n".to_owned())
    );
    dir.write(
        "crates/b/tests/slow.rs",
        "fn wait() { fakes::Deadline::after(Duration::from_secs(5)); }\n",
    );
    let (code, out) = xtask(&dir, &["must-succeed"], &[], "");
    assert_eq!(code, 1);
    assert_eq!(
        out,
        "must-succeed: crates/b/tests/slow.rs:1: Deadline::after(Duration::from_secs(5)) is below MUST_SUCCEED_WITHIN (10 s)\n"
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
    let dir = TempDir::new("xtask-docs-only");
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
    const WORKFLOW: &str = "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  lint:\n    runs-on: ubuntu-24.04\n  backstop_report:\n    runs-on: ubuntu-24.04\n  bench_comment:\n    runs-on: ubuntu-24.04\n  cache_prune:\n    runs-on: ubuntu-24.04\n  flake_decide:\n    runs-on: ubuntu-24.04\n  flake_report:\n    runs-on: ubuntu-24.04\n  stress:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: [select, lint, stress]\n    runs-on: ubuntu-24.04\n";
    const DOC: &str = "# CI\n\n## The merge gate\n\nEvery job but the verdict job is in its needs, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`, `flake_decide`, `flake_report`. Done.\n";
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
        &WORKFLOW.replace("needs: [select, lint, stress]", "needs: [select, stress]"),
    );
    assert_eq!(xtask(&dir, &["ci-needs"], &[], ""), (1, FAILURE.to_owned()));
    std::fs::remove_file(dir.path().join("docs/ci.md")).unwrap();
    let (code, out) = xtask(&dir, &["ci-needs"], &[], "");
    assert_eq!(code, 2);
    assert!(out.contains("docs/ci.md"), "{out}");
}

#[test]
fn flake_report_prints_one_json_line_per_unmatched_title() {
    let dir = TempDir::new("xtask-flake-report");
    const INPUT: &str = "{\"flakes\": [{\"title\": \"Flaky tests: a\"}, {\"title\": \"Flaky tests: b\"}, {\"title\": \"Flaky tests: a\"}], \"open\": {\"ok\": true, \"issues\": [{\"number\": 7, \"title\": \"Flaky tests: b\"}]}, \"closed\": {\"ok\": true, \"issues\": [{\"number\": 9, \"title\": \"Flaky tests: a\", \"closed_at\": \"2026-10-24T11:00:00Z\"}]}, \"events\": {\"9\": {\"ok\": true, \"events\": [{\"event\": \"closed\", \"id\": 1, \"commit_id\": null, \"created_at\": \"2026-10-24T11:00:00Z\"}]}}, \"ancestry\": {}}";
    assert_eq!(
        xtask(&dir, &["flake-report"], &[], INPUT),
        (
            0,
            "{\"action\":\"reopen\",\"issue\":9,\"title\":\"Flaky tests: a\"}\n".to_owned()
        )
    );
    assert_eq!(xtask(&dir, &["flake-report"], &[], "{}").0, 2);
}

#[test]
fn flake_report_tested_commit_reopens_when_the_latest_fix_is_an_ancestor() {
    // The real `git merge-base --is-ancestor` probe: the fix commit is an
    // ancestor of the tested commit, so the decision reopens. Without the
    // existence check the probe answers unknown instead.
    let dir = TempDir::new("xtask-flake-report-ancestry");
    git(&dir, &["init", "-q", "-b", "main"]);
    dir.write("fix.txt", "the fix\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "fix"]);
    let fix = common::git(&dir, &["rev-parse", "HEAD"]);
    let input = format!(
        "{{\"flakes\": [{{\"title\": \"Flaky tests: a\"}}], \"open\": {{\"ok\": true, \"issues\": []}}, \"closed\": {{\"ok\": true, \"issues\": [{{\"number\": 9, \"title\": \"Flaky tests: a\", \"closed_at\": \"2026-10-24T11:00:00Z\"}}]}}, \"events\": {{\"9\": {{\"ok\": true, \"events\": [{{\"event\": \"closed\", \"id\": 1, \"commit_id\": \"{fix}\", \"created_at\": \"2026-10-24T11:00:00Z\"}}]}}}}}}"
    );
    assert_eq!(
        xtask(
            &dir,
            &["flake-report", "--tested-commit", &fix],
            &[],
            &input
        ),
        (
            0,
            "{\"action\":\"reopen\",\"issue\":9,\"title\":\"Flaky tests: a\"}\n".to_owned()
        )
    );
}

#[test]
fn flake_report_tested_commit_is_unknown_when_the_fix_object_is_missing() {
    // `git cat-file` fails for an object the checkout lacks, so the probe
    // answers unknown. A probe stuck at a boolean decides instead.
    let dir = TempDir::new("xtask-flake-report-missing-fix");
    git(&dir, &["init", "-q", "-b", "main"]);
    dir.write("fix.txt", "the fix\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "fix"]);
    let tested = common::git(&dir, &["rev-parse", "HEAD"]);
    let missing = "0123456789012345678901234567890123456789";
    let input = format!(
        "{{\"flakes\": [{{\"title\": \"Flaky tests: a\"}}], \"open\": {{\"ok\": true, \"issues\": []}}, \"closed\": {{\"ok\": true, \"issues\": [{{\"number\": 9, \"title\": \"Flaky tests: a\", \"closed_at\": \"2026-10-24T11:00:00Z\"}}]}}, \"events\": {{\"9\": {{\"ok\": true, \"events\": [{{\"event\": \"closed\", \"id\": 1, \"commit_id\": \"{missing}\", \"created_at\": \"2026-10-24T11:00:00Z\"}}]}}}}}}"
    );
    assert_eq!(
        xtask(
            &dir,
            &["flake-report", "--tested-commit", &tested],
            &[],
            &input
        ),
        (
            0,
            "{\"action\":\"unknown\",\"issue\":9,\"source\":\"ancestry\",\"title\":\"Flaky tests: a\"}\n"
                .to_owned()
        )
    );
}

#[test]
fn flake_report_events_prints_only_numbers_needing_event_fetches() {
    let dir = TempDir::new("xtask-flake-report-events");
    const INPUT: &str = "{\"flakes\": [{\"title\": \"Flaky tests: a\"}, {\"title\": \"Flaky tests: b\"}], \"open\": {\"ok\": true, \"issues\": [{\"number\": 7, \"title\": \"Flaky tests: b\"}]}, \"closed\": {\"ok\": true, \"issues\": [{\"number\": 9, \"title\": \"Flaky tests: a\", \"closed_at\": \"2026-10-24T11:00:00Z\"}]}}";
    assert_eq!(
        xtask(&dir, &["flake-report-events"], &[], INPUT),
        (0, "9\n".to_owned())
    );
    assert_eq!(xtask(&dir, &["flake-report-events"], &[], "{}").0, 2);
}

#[test]
fn flake_classify_prints_one_record_per_unique_title() {
    // A repeated title is written once, from its first occurrence; a
    // line with no TAB has an empty detail and no failing file.
    let dir = TempDir::new("xtask-flake-classify");
    dir.write(
        "flaky/flaky-tests-linux.txt",
        "main::a\tthread a panicked at crates/a.rs:10\nmain::a\tthread a panicked at crates/a.rs:10\n",
    );
    dir.write("flaky/flaky-tests-arm.txt", "main::b\n");
    let flaky = dir.path().join("flaky");
    let (code, out) = xtask(
        &dir,
        &[
            "flake-classify",
            "--flaky-dir",
            flaky.to_str().unwrap(),
            "--event",
            "push",
            "--tested-commit",
            "1111111111111111111111111111111111111111",
        ],
        &[],
        "",
    );
    assert_eq!(code, 0, "{out}");
    let records: Vec<serde_json::Value> = out
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // Records are matched by title: their order is not part of this
    // test's contract. The count holds docs/testing/running.md, "Flaky
    // tests": "Within one report job a file's title files or comments once;
    // a repeat is skipped".
    assert_eq!(records.len(), 2, "{out}");
    let by_title = |title: &str| {
        records
            .iter()
            .find(|record| record["title"] == title)
            .unwrap_or_else(|| panic!("no record titled {title}: {out}"))
    };
    let b = by_title("Flaky tests: main::b");
    assert_eq!(b["action"], "lookup");
    assert_eq!(b["runner"], "arm");
    assert_eq!(b["detail"], "");
    assert!(b.get("failing").is_none(), "{out}");
    let a = by_title("Flaky tests: main::a");
    assert_eq!(a["action"], "lookup");
    assert_eq!(a["failing"], "crates/a.rs");
    assert_eq!(a["runner"], "linux");
    assert_eq!(
        a["tested"],
        "tested 1111111111111111111111111111111111111111"
    );
}

#[test]
fn flake_classify_files_two_tests_of_one_binary_once() {
    // Two tests of one binary across two lists: file-name byte order reads
    // the arm list first, so one record carries the arm occurrence.
    let dir = TempDir::new("xtask-flake-classify-one-binary");
    dir.write(
        "flaky/flaky-tests-linux.txt",
        "main::a second\tthread second panicked at crates/a.rs:10\n",
    );
    dir.write(
        "flaky/flaky-tests-arm.txt",
        "main::a first\tthread first panicked at crates/a.rs:10\n",
    );
    let flaky = dir.path().join("flaky");
    let (code, out) = xtask(
        &dir,
        &[
            "flake-classify",
            "--flaky-dir",
            flaky.to_str().unwrap(),
            "--event",
            "push",
            "--tested-commit",
            "1111111111111111111111111111111111111111",
        ],
        &[],
        "",
    );
    assert_eq!(code, 0, "{out}");
    let records: Vec<serde_json::Value> = out
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // docs/testing/running.md, "Flaky tests": "Within one report job a file's
    // title files or comments once; a repeat is skipped".
    // docs/testing/running.md, "Flaky tests": "a repeated title takes its
    // runner and detail from its first occurrence in file-name byte order".
    assert_eq!(records.len(), 1, "{out}");
    assert_eq!(records[0]["title"], "Flaky tests: main::a", "{out}");
    assert_eq!(records[0]["name"], "main::a first", "{out}");
    assert_eq!(records[0]["runner"], "arm", "{out}");
    assert_eq!(
        records[0]["detail"], "thread first panicked at crates/a.rs:10",
        "{out}"
    );
}

#[test]
fn flake_classify_absent_compare_files_are_an_empty_list() {
    // No --compare-files flag: a pull request run with the file only in
    // the PR list is PR-owned, never stale.
    let dir = TempDir::new("xtask-flake-classify-no-compare");
    dir.write(
        "flaky/flaky-tests-linux.txt",
        "main::a\tthread a panicked at crates/a.rs:10\n",
    );
    dir.write("pr.txt", "crates/a.rs\n");
    let flaky = dir.path().join("flaky");
    let pr = dir.path().join("pr.txt");
    let (code, out) = xtask(
        &dir,
        &[
            "flake-classify",
            "--flaky-dir",
            flaky.to_str().unwrap(),
            "--event",
            "pull_request",
            "--tested-commit",
            "1111111111111111111111111111111111111111",
            "--base",
            "2222222222222222222222222222222222222222",
            "--head",
            "3333333333333333333333333333333333333333",
            "--pr-files",
            pr.to_str().unwrap(),
        ],
        &[],
        "",
    );
    assert_eq!(code, 0, "{out}");
    let record: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
    assert_eq!(record["action"], "pr-owned");
}

#[test]
fn flake_classify_skips_directories_named_like_lists() {
    // A directory named flaky-tests-d.txt holds no list: only the real
    // file is read.
    let dir = TempDir::new("xtask-flake-classify-dir");
    dir.write(
        "flaky/flaky-tests-linux.txt",
        "main::a\tthread a panicked at crates/a.rs:10\n",
    );
    std::fs::create_dir_all(dir.path().join("flaky/flaky-tests-d.txt")).unwrap();
    let flaky = dir.path().join("flaky");
    let (code, out) = xtask(
        &dir,
        &[
            "flake-classify",
            "--flaky-dir",
            flaky.to_str().unwrap(),
            "--event",
            "push",
            "--tested-commit",
            "1111111111111111111111111111111111111111",
        ],
        &[],
        "",
    );
    assert_eq!(code, 0, "{out}");
    for line in out.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(record["title"], "Flaky tests: main::a", "{out}");
        assert_eq!(record["runner"], "linux", "{out}");
    }
    assert!(out.contains("Flaky tests: main::a"), "{out}");
}

#[test]
fn flake_classify_missing_flags_and_unreadable_files_are_errors() {
    let dir = TempDir::new("xtask-flake-classify-errors");
    dir.write("flaky/flaky-tests-linux.txt", "main::a\n");
    let flaky = dir.path().join("flaky");
    let missing = dir.path().join("missing.txt");
    // A missing --flaky-dir exits 2.
    assert_eq!(
        xtask(&dir, &["flake-classify", "--event", "push"], &[], "").0,
        2
    );
    // An unreadable --flaky-dir exits 2.
    assert_eq!(
        xtask(
            &dir,
            &[
                "flake-classify",
                "--flaky-dir",
                missing.to_str().unwrap(),
                "--event",
                "push",
                "--tested-commit",
                "1111111111111111111111111111111111111111",
            ],
            &[],
            "",
        )
        .0,
        2
    );
    // A missing --event exits 2.
    assert_eq!(
        xtask(
            &dir,
            &[
                "flake-classify",
                "--flaky-dir",
                flaky.to_str().unwrap(),
                "--tested-commit",
                "1111111111111111111111111111111111111111",
            ],
            &[],
            "",
        )
        .0,
        2
    );
    // An unreadable --pr-files exits 2.
    assert_eq!(
        xtask(
            &dir,
            &[
                "flake-classify",
                "--flaky-dir",
                flaky.to_str().unwrap(),
                "--event",
                "push",
                "--tested-commit",
                "1111111111111111111111111111111111111111",
                "--pr-files",
                missing.to_str().unwrap(),
            ],
            &[],
            "",
        )
        .0,
        2
    );
}

#[test]
fn an_unknown_or_missing_command_is_an_error() {
    let dir = TempDir::new("xtask-unknown");
    assert_eq!(
        xtask(&dir, &["nope"], &[], ""),
        (2, "xtask: unknown command nope\n".to_owned())
    );
    assert_eq!(xtask(&dir, &[], &[], "").0, 2);
}
