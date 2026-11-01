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

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at_or(grid: &Grid, y: u16, needle: &str) -> Option<u16> {
    let want: Vec<char> = needle.chars().collect();
    if want.is_empty() {
        return None;
    }
    let (width, _) = grid.size();
    (0..width).find(|x| {
        want.iter().enumerate().all(|(dx, ch)| {
            let xi = x + dx as u16;
            xi < width && grid.cell(xi, y).symbol == ch.to_string()
        })
    })
}

/// The column where `needle` starts on `y`, or a panic.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    col_at_or(grid, y, needle).unwrap_or_else(|| panic!("{needle:?} on row {y}"))
}

/// Clicks cell `x`, `y`: press and release.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
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
    // is given: `$1` is `+N`, `$2` the path.
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let editor = editor.to_str().unwrap().to_owned();
    let env: Vec<(&str, &str)> = [("EDITOR", editor.as_str())]
        .into_iter()
        .chain(TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    let grid = run.wait_screen("the changed files card", |grid| {
        find_row(grid, "f.txt +1").is_some()
    });
    let y = row_with(&grid, "f.txt +1");
    click(&mut run, col_at(&grid, y, "f.txt") + 2, y);
    let grid = run.wait_screen("the file diff", |grid| {
        find_row(grid, "+call bar(a);").is_some() && find_row(grid, "@@ -1 +1 @@").is_some()
    });
    let _ = row_with(&grid, "@@ -1 +1 @@");
    run.write(b"\x07");
    // The hunk shows the appended line once the editor exits.
    let grid = run.wait_screen("the edited hunk", |grid| {
        find_row(grid, "+call baz(a);").is_some()
    });
    let _ = row_with(&grid, "+call baz(a);");
    // The script got `+N <path>`, N the first hunk's first line.
    let recorded = fs::read_to_string(&argv).unwrap_or_else(|error| panic!("argv: {error}"));
    assert_eq!(
        recorded,
        format!("+1\n{}\n", workspace.join("f.txt").display())
    );
    // Back in the list, the row's added count went up by one.
    run.write(b"\x1b[D");
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
