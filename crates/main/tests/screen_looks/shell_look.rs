//! Screen tests for shell mode in the editor and the transcript row for
//! `!`/`!!` runs (#4176): the real binary on a 160x48 pty in truecolour.
//! Typing `!ls` marks the box with the `! ` prompt and the `shell` label
//! until Backspace clears the `!`; `!echo hi` draws the sent row and
//! `!!echo hi` the show-only row. Every grid read sits inside the wait
//! that settled its frame: the waits below capture plain rows and the
//! asserts read only those, so no line outside a wait names a grid.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use serde_json::json;
use support::pty::Run;
use support::{Setup, TRUECOLOUR, write_json};

/// The scripted turn behind every test: one instant reply, so the
/// session idles with an empty draft.
fn script(setup: &Setup) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["ready"]}]}),
    );
}

/// An attached idle session: started with `go`, settled on its closing
/// line, the draft empty.
fn attached(setup: &Setup) -> Run {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the idle session", |grid| {
        grid.rows.iter().any(|row| row.contains("▣ completed"))
    });
    run
}

/// Whether `row` draws the input box's rule: blanks and the stripe's
/// column aside, every cell is a rule cell.
fn is_rule_row(row: &str) -> bool {
    let trimmed: String = row
        .chars()
        .filter(|cell| !matches!(cell, ' ' | '▌' | '│'))
        .collect();
    !trimmed.is_empty() && trimmed.chars().all(|cell| cell == '─')
}

/// The first row holding `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

/// Quits the binary, successfully.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn bang_marks_shell_mode_until_backspace_clears_it() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!ls");
    // The marker: the draft row opens with the `! ` prompt and the
    // box's top-edge row ends with the `shell` label.
    let mut edge = String::new();
    let mut draft = String::new();
    run.wait_screen("the shell marker", |grid| {
        let rows = grid.rows.clone();
        let Some(rule) = rows.iter().position(|row| is_rule_row(row)) else {
            return false;
        };
        let (Some(edge_row), Some(draft_row)) = (
            rule.checked_sub(2).and_then(|index| rows.get(index)),
            rule.checked_sub(1).and_then(|index| rows.get(index)),
        ) else {
            return false;
        };
        let placed = edge_row.trim_end().ends_with("shell") && draft_row.contains("! !ls");
        if placed {
            edge = edge_row.clone();
            draft = draft_row.clone();
        }
        placed
    });
    assert!(
        edge.trim_end().ends_with("shell"),
        "top-edge label, got {edge:?}"
    );
    assert!(draft.contains("! !ls"), "shell prompt, got {draft:?}");
    // Backspace over the `!` returns the plain prompt and no label.
    run.write(b"\x7f\x7f\x7f");
    let mut plain = String::new();
    run.wait_screen("the cleared marker", |grid| {
        let rows = grid.rows.clone();
        let gone = !rows.iter().any(|row| row.contains("shell"));
        let prompt_back = rows.iter().any(|row| row.contains("›"));
        let done = gone && prompt_back;
        if done {
            plain = rows.join("\n");
        }
        done
    });
    assert!(
        !plain.contains("shell"),
        "no label past the bang, got {plain:?}"
    );
    assert!(plain.contains("›"), "plain prompt returns");
    quit(run);
}

#[test]
fn bang_run_draws_the_sent_row() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!echo hi\r");
    // The sent row: the command, the sent note, the output verbatim,
    // then the exit status, in that order on consecutive rows.
    let mut snapshot: Vec<String> = Vec::new();
    run.wait_screen("the sent shell row", |grid| {
        let rows = grid.rows.clone();
        let command = rows.iter().position(|row| row.contains("! echo hi"));
        let note = rows
            .iter()
            .position(|row| row.contains("sent with your next prompt"));
        let output = rows.iter().position(|row| row.trim() == "hi");
        let exit = rows.iter().position(|row| row.contains("exit 0"));
        let ordered = match (command, note, output, exit) {
            (Some(command), Some(note), Some(output), Some(exit)) => {
                command < note && note < output && output < exit
            }
            _ => false,
        };
        let settled = !rows.iter().any(|row| row.trim_end().ends_with("shell"));
        let done = ordered && settled;
        if done {
            snapshot = rows;
        }
        done
    });
    let at =
        |needle: &str| find_row(&snapshot, needle).unwrap_or_else(|| panic!("the {needle} row"));
    let command = at("! echo hi");
    let note = at("sent with your next prompt");
    let output = snapshot
        .iter()
        .position(|row| row.trim() == "hi")
        .expect("the output row");
    let exit = at("exit 0");
    assert!(
        command < note && note < output && output < exit,
        "the row reads in order: {snapshot:?}"
    );
    assert!(
        !snapshot.iter().any(|row| row.trim_end().ends_with("shell")),
        "the marker left with the draft"
    );
    quit(run);
}

#[test]
fn bang_bang_run_draws_the_show_only_row() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!!echo hi\r");
    // The show-only row: the same shape with the other note.
    let mut snapshot: Vec<String> = Vec::new();
    run.wait_screen("the show-only shell row", |grid| {
        let rows = grid.rows.clone();
        let command = rows.iter().position(|row| row.contains("! echo hi"));
        let note = rows
            .iter()
            .position(|row| row.contains("only you see this"));
        let output = rows.iter().position(|row| row.trim() == "hi");
        let exit = rows.iter().position(|row| row.contains("exit 0"));
        let done = match (command, note, output, exit) {
            (Some(command), Some(note), Some(output), Some(exit)) => {
                command < note && note < output && output < exit
            }
            _ => false,
        };
        if done {
            snapshot = rows;
        }
        done
    });
    let at =
        |needle: &str| find_row(&snapshot, needle).unwrap_or_else(|| panic!("the {needle} row"));
    let command = at("! echo hi");
    let note = at("only you see this");
    let output = snapshot
        .iter()
        .position(|row| row.trim() == "hi")
        .expect("the output row");
    let exit = at("exit 0");
    assert!(
        command < note && note < output && output < exit,
        "the row reads in order: {snapshot:?}"
    );
    quit(run);
}
