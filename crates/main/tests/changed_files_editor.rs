//! Binary-level test that Ctrl+G on a Changed files diff opens the file
//! in `$EDITOR` at its first hunk and re-reads the diff on return
//! (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty in
//! truecolour, with one changed file, runs the pointed editor script,
//! which records its arguments and appends a line to the file. The
//! script gets `+N <path>` with the first hunk's first line; after it
//! exits the hunk shows the appended line and the row's added count is
//! up by one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::{Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// A completed frame's end: synchronized output's close marker, which the
/// TUI brackets every frame with. Waiting for it after an input proves the
/// frame drew after that input instead of matching a stale grid
/// (`docs/testing/screens.md`, "Screens").
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// The scripted model and the hidden rail.
fn config(setup: &Setup, extra: serde_json::Value) {
    let mut config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
        "tui": {"rail": {"shown": false}}});
    for (key, value) in extra.as_object().unwrap() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// The first row containing `needle`.
fn find_row(grid: &Grid, needle: &str) -> Option<u16> {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    find_row(grid, needle).unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents))
}

/// Runs `git` in `dir`, panicking on failure.
fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn ctrl_g_opens_the_file_at_its_first_hunk_and_rereads_on_return() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@fiber.dev"]);
    git(&workspace, &["config", "user.name", "fiber test"]);
    fs::write(workspace.join("f.txt"), "call foo(a);\n").unwrap();
    git(&workspace, &["add", "f.txt"]);
    git(&workspace, &["commit", "-m", "add f"]);
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "call foo(a);\n", "new_text": "call bar(a);\n"},
            ]}}]},
            {"text": ["Done."]},
        ]}),
    );
    config(&setup, json!({}));
    // The editor records its arguments and appends a line to the file it
    // is given: `$1` is `+N`, `$2` the path. The script runs through its
    // interpreter (`docs/testing.md`, "Waits and timeouts"): the kernel
    // execs the checked-in `/bin/sh`, which reads the freshly written file.
    let editor = workspace.join("editor.sh");
    let argv = workspace.join("argv");
    fs::write(
        &editor,
        format!(
            "printf '%s\\n' \"$@\" > {}\nprintf 'call baz(a);\\n' >> \"$2\"\n",
            argv.display()
        ),
    )
    .unwrap();
    let editor_cmd = format!("/bin/sh {}", editor.to_str().unwrap());
    let env: Vec<(&str, &str)> = [("EDITOR", editor_cmd.as_str())]
        .into_iter()
        .chain(TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    // The Changed files list opens through `/diff`, keyboard-only: no
    // pointer input, so no row-position signal is needed
    // (`docs/testing/screens.md`, "A pointer event…"). The frame-end
    // marker proves the list drew after the command, and the top-row
    // title plus the file row proves the swapped list (not the panel
    // card) is on screen.
    let list_from = run.output().len();
    run.write(b"/diff\r");
    run.wait_bytes(list_from, FRAME_END, "the changed files frame");
    run.wait_screen("the changed files card", |grid| {
        grid.rows[0].contains("Changed files") && find_row(grid, "f.txt +1").is_some()
    });
    // Enter opens the file's diff, keyboard-only. The frame-end marker
    // proves a frame drew after Enter, and the title plus the hunk rows
    // prove the diff response arrived and drew (not a stale ledger hunk
    // from the turn's own edit view).
    let diff_from = run.output().len();
    run.write(b"\r");
    run.wait_bytes(diff_from, FRAME_END, "the file diff frame");
    let grid = run.wait_screen("the file diff", |grid| {
        grid.rows[0].contains("Changed files")
            && find_row(grid, "+call bar(a);").is_some()
            && find_row(grid, "@@ -1 +1 @@").is_some()
    });
    let _ = row_with(&grid, "@@ -1 +1 @@");
    // The frame-end plus hunk grid above is Ctrl+G's readiness signal
    // (`docs/testing/screens.md`, "Screens"): the diff response drew
    // after Enter, so the editor opens the shown file at its first hunk.
    run.write(b"\x07");
    // The hunk shows the appended line once the editor exits.
    let grid = run.wait_screen("the edited hunk", |grid| {
        grid.rows[0].contains("Changed files") && find_row(grid, "+call baz(a);").is_some()
    });
    let _ = row_with(&grid, "+call baz(a);");
    // The script got `+N <path>`, N the first hunk's first line.
    let recorded = fs::read_to_string(&argv).unwrap_or_else(|error| panic!("argv: {error}"));
    assert_eq!(
        recorded,
        format!("+1\n{}\n", workspace.join("f.txt").display())
    );
    // Back in the list, the row's added count went up by one.
    let list_from = run.output().len();
    run.write(b"\x1b[D");
    run.wait_bytes(list_from, FRAME_END, "the returned list frame");
    let grid = run.wait_screen("the refreshed row", |grid| {
        grid.rows[0].contains("Changed files")
            && find_row(grid, "f.txt").is_some_and(|y| grid.rows[usize::from(y)].contains("+2"))
    });
    let y = row_with(&grid, "f.txt");
    assert!(
        grid.rows[usize::from(y)].contains("+2"),
        "{}",
        grid.contents
    );
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}
