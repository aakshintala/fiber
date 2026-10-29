//! `cargo xtask fixture-starts` run as a program: once against the
//! working tree, which must pass on the Part 1 tree, and otherwise
//! against small synthetic workspaces holding one site each. A negative
//! asserts its own failure line is present; a positive asserts its
//! site's failure line is absent, so the table's other rows never
//! decide these tests.

#![allow(
    clippy::unwrap_used,
    reason = "test code may unwrap (docs/code-quality.md, \"Lints\"); clippy exempts only #[test] functions here"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::process::{Command, Stdio};

use fakes::TempDir;

#[path = "common/mod.rs"]
mod common;

/// Runs the built `xtask fixture-starts` in `dir`, bounded so a hung
/// child fails the test instead of hanging it.
fn fixture_starts(dir: &std::path::Path) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .arg("fixture-starts")
        .current_dir(dir)
        .stdin(Stdio::null());
    common::run("xtask fixture-starts", command, &[], MUST_SUCCEED_WITHIN)
}

/// The repository root: the `xtask` manifest's parent.
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// A synthetic workspace whose `crates` member holds `files`: relative
/// paths to bodies. Untracked files count: the check reads tracked and
/// untracked files alike, so no commit is needed, only a repository.
fn workspace(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new("fiber-fixture-starts");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("crates/src")).unwrap();
    std::fs::write(
        dir.path().join("crates/Cargo.toml"),
        "[package]\nname = \"synth\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("crates/src/lib.rs"), "").unwrap();
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    common::git(&dir, &["init", "-q"]);
    dir
}

#[test]
fn part_1_tree_passes() {
    // The checked-in tree holds every Part 1 warmed site, so the
    // check passes on it whole.
    let root = repo_root();
    let (code, out) = fixture_starts(&root);
    assert_eq!(code, Some(0), "the Part 1 tree passes: {out}");
    assert!(
        out.contains("fixture-starts: every fixture start is a named warmed, read, or later site"),
        "the check names what held: {out}"
    );
}

#[test]
fn unlisted_fixture_start_fails() {
    // A fixture use in a function the table does not name fails, so a
    // new fixture start cannot enter without an explicit site row.
    let dir = workspace(&[(
        "crates/mcp/src/other_tests.rs",
        "fn other_test() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "an unlisted start fails: {out}");
    assert!(
        out.contains("unlisted fixture start in `other_test`"),
        "the failure names the function: {out}"
    );
}

#[test]
fn unlisted_jig_spawn_fails() {
    // A jig binary use in a function the table does not name fails too.
    let dir = workspace(&[(
        "crates/log/tests/dump_jig.rs",
        "fn run_jig_elsewhere() {\n    let _ = example_path();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "an unlisted jig spawn fails: {out}");
    assert!(
        out.contains("unlisted fixture start in `run_jig_elsewhere`"),
        "the failure names the function: {out}"
    );
}

#[test]
fn removed_warm_at_config_site_fails() {
    // The `mcp_view` config site warms through
    // `Setup::new_with_mcp_fixture`: without it the row fails.
    let dir = workspace(&[(
        "crates/main/tests/mcp_view.rs",
        "fn list_shows_a_ready_a_failed_and_a_needs_login_server() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "a config site without its warm fails: {out}");
    assert!(
        out.contains("`list_shows_a_ready_a_failed_and_a_needs_login_server` lost its `new_with_mcp_fixture()` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn warmed_config_site_passes() {
    // The same site with its warm holds no failure line of its own.
    let dir = workspace(&[(
        "crates/main/tests/mcp_view.rs",
        "fn list_shows_a_ready_a_failed_and_a_needs_login_server() {\n    let setup = support::Setup::new_with_mcp_fixture();\n    let _ = fakes::mcp_fixture();\n    let _ = setup;\n}\n",
    )]);
    let (_, out) = fixture_starts(dir.path());
    assert!(
        !out.contains("list_shows_a_ready_a_failed_and_a_needs_login_server"),
        "a warmed config site holds no failure: {out}"
    );
}

#[test]
fn removed_warm_at_wrapper_site_fails() {
    // The direct mcp-elicit start warms through
    // `warm_mcp_fixture_once` before its deadlines.
    let dir = workspace(&[(
        "crates/main/src/mcp_elicit_tests.rs",
        "fn an_unanswered_elicitation_is_declined_at_the_calls_deadline_then_the_call_times_out() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(
        code,
        Some(0),
        "a wrapper site without its warm fails: {out}"
    );
    assert!(
        out.contains("lost its `warm_mcp_fixture_once()` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn removed_warm_from_secret_wrapper_fails() {
    // `secret_wrapper` execs the fixture under `/bin/bash`, so it warms
    // explicitly before changing the spec.
    let dir = workspace(&[(
        "crates/mcp/src/start_tests.rs",
        "fn secret_wrapper() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(
        code,
        Some(0),
        "secret_wrapper without its warm fails: {out}"
    );
    assert!(
        out.contains("`secret_wrapper` lost its `Setup::warm_fixture()` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn warmed_secret_wrapper_passes() {
    // The same helper with its warm holds no failure line of its own.
    let dir = workspace(&[(
        "crates/mcp/src/start_tests.rs",
        "fn secret_wrapper() {\n    Setup::warm_fixture();\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (_, out) = fixture_starts(dir.path());
    assert!(
        !out.contains("secret_wrapper"),
        "a warmed secret_wrapper holds no failure: {out}"
    );
}

#[test]
fn removed_warm_from_flaky_wrapper_test_fails() {
    // The `flaky.sh` restart test execs the fixture under `/bin/bash`.
    let dir = workspace(&[(
        "crates/mcp/src/start_tests.rs",
        "fn a_second_restart_mid_start_coalesces_on_the_first_attempt() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(
        code,
        Some(0),
        "the flaky wrapper test without its warm fails: {out}"
    );
    assert!(
        out.contains("`a_second_restart_mid_start_coalesces_on_the_first_attempt` lost its `Setup::warm_fixture()` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn removed_warm_from_bash_pipeline_test_fails() {
    // The Bash pipeline test execs the fixture through `Server::start`.
    let dir = workspace(&[(
        "crates/mcp/src/server_tests.rs",
        "fn a_server_that_stops_reading_stdin_ends_every_call_gone() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(
        code,
        Some(0),
        "the pipeline test without its warm fails: {out}"
    );
    assert!(
        out.contains("`a_server_that_stops_reading_stdin_ends_every_call_gone` lost its `Setup::warm_fixture()` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn removed_warm_from_direct_start_fails() {
    // `endless_pages` starts the fixture directly through
    // `Server::start`.
    let dir = workspace(&[(
        "crates/mcp/src/server_tests.rs",
        "fn endless_pages_end_at_the_startup_deadline() {\n    let _ = fakes::mcp_fixture();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(
        code,
        Some(0),
        "a direct start without its warm fails: {out}"
    );
    assert!(
        out.contains(
            "`endless_pages_end_at_the_startup_deadline` lost its `Setup::warm_fixture()` call"
        ),
        "the failure names the helper: {out}"
    );
}

#[test]
fn jig_site_without_warm_fails() {
    // A jig site that stops warming its binary fails.
    let dir = workspace(&[(
        "crates/provider/tests/record_jig.rs",
        "fn run_jig() {\n    let _ = example_path();\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "a jig site without its warm fails: {out}");
    assert!(
        out.contains("`run_jig` lost its `warm_up(` call"),
        "the failure names the helper: {out}"
    );
}

#[test]
fn path_reader_with_a_start_fails() {
    // A path reader that grows a start is no reader anymore.
    let dir = workspace(&[(
        "crates/main/tests/repository_offer.rs",
        "fn declare_db() {\n    let _ = std::fs::read(fakes::mcp_fixture());\n    let _ = setup.start(vec![]);\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "a reader that grew a start fails: {out}");
    assert!(
        out.contains("path reader `declare_db` grew a start"),
        "the failure names the reader: {out}"
    );
}

#[test]
fn warming_accessor_fails() {
    // The pure path accessor never warms: a `warm_up` in it fails.
    let dir = workspace(&[(
        "crates/fakes/src/lib.rs",
        "pub fn mcp_fixture() -> PathBuf {\n    fakes::warm_up(&path, &[], 1);\n    path\n}\n",
    )]);
    let (code, out) = fixture_starts(dir.path());
    assert_ne!(code, Some(0), "a warming accessor fails: {out}");
    assert!(
        out.contains("the pure accessor `mcp_fixture` grew a `warm_up` call"),
        "the failure names the accessor: {out}"
    );
}
