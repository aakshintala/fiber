//! Binary-level tests of the `@` panel's two searches (`docs/tui.md`,
//! "File search"): in a pty with a temporary `HOME` holding a seeded
//! tree, `@~/proj/rea` lists the matching file, choosing the `sub`
//! directory reopens the panel on it, and `@foo` inside a git
//! repository lists tracked files only (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::process::Command;
use support::Setup;
use support::pty::{Grid, Run};

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// A setup whose home composer never opens the model picker: the
/// scripted model is configured, so `@` reaches the file panel at
/// once, and no session is ever started.
fn setup() -> Setup {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    setup
}

/// A run at home with its first frame drawn, ready for keys.
fn run(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 120, 32, &[], &[("TERM", "xterm-256color")]);
    run.ready();
    run
}

/// Quits from any state and checks the terminal was restored: one
/// Ctrl+C clears the `@` draft and closes its panel, then `/quit`
/// leaves, which needs no session and asks nothing.
fn quit(mut run: Run) {
    run.write(b"\x03");
    run.write(b"/quit\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_at_path_lists_matching_entries_and_a_chosen_directory_reopens_the_panel() {
    let setup = setup();
    let proj = setup.root.path().join("proj");
    fs::create_dir_all(proj.join("sub")).unwrap();
    fs::write(proj.join("README.md"), "x").unwrap();
    fs::write(proj.join("readme-notes.md"), "x").unwrap();
    fs::write(proj.join("sub").join("inner.txt"), "x").unwrap();
    let mut run = run(&setup);
    // `@~/proj/rea` lists the matching file under the temporary home.
    run.write(b"@~/proj/rea");
    run.wait_screen("the path match", |screen| {
        shows(screen, "~/proj/README.md")
    });
    // Back to the base, then into `su`: the subdirectory row shows.
    run.write(b"\x7f\x7f\x7f");
    run.write(b"su");
    run.wait_screen("the subdirectory row", |screen| {
        shows(screen, "~/proj/sub/")
    });
    // Choosing the directory keeps the panel open on the new base: the
    // input reads the base and lists what it holds.
    run.write(b"\r");
    run.wait_screen("the reopened panel", |screen| {
        shows(screen, "@~/proj/sub/") && shows(screen, "inner.txt")
    });
    quit(run);
}

#[test]
fn an_at_foo_search_in_a_repository_lists_tracked_files_only() {
    let setup = setup();
    let workspace = setup.workspace();
    let status = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .arg("init")
        .arg("-q")
        .status()
        .unwrap();
    assert!(status.success(), "git init: {status}");
    fs::write(workspace.join("foo_tracked.txt"), "x").unwrap();
    let status = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["add", "foo_tracked.txt"])
        .status()
        .unwrap();
    assert!(status.success(), "git add: {status}");
    fs::write(workspace.join("foo_untracked.txt"), "x").unwrap();
    let mut run = run(&setup);
    run.write(b"@foo");
    let screen = run.wait_screen("the tracked match", |screen| {
        shows(screen, "foo_tracked.txt")
    });
    assert!(
        !shows(&screen, "foo_untracked.txt"),
        "an untracked file is never walked"
    );
    quit(run);
}
