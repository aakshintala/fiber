//! Screen tests for shell mode in the editor and the transcript row for
//! `!`/`!!` runs (#4176): the real binary on a 160x48 pty in truecolour.
//! Typing `!ls` marks the box with the `! ` prompt and the `shell` label
//! until Backspace clears the `!`; `!echo hi` draws the sent row and
//! `!!echo hi` the show-only row. Every grid read sits inside the wait
//! that settled its frame: the waits below capture plain rows and the
//! asserts read only those, so no line outside a wait names a grid.
//!
//! A run that keeps running draws its row before it ends (#4177): the
//! head, the note, the output so far and a `running` line, which the
//! ending replaces. The pins at the end hold what a run that fails to
//! start, or ends non-zero, already shows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::io::Write;
use std::path::Path;

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

/// Quits two terminals that share one hub. The hub exits when its last
/// client leaves, and each `wait` waits for it, so both are told to quit
/// before either is waited on.
fn quit_both(mut first: Run, mut second: Run) {
    first.write(b"\x03\x03\r");
    second.write(b"\x03\x03\r");
    for run in [first, second] {
        let finished = run.wait();
        assert_eq!(finished.status.code(), Some(0));
    }
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

/// One row a transcript row must show, in order.
enum Want<'a> {
    /// A row holding the text.
    Has(&'a str),
    /// A row whose trimmed text is exactly this.
    Is(&'a str),
    /// A row whose trimmed text starts with this.
    Starts(&'a str),
}

/// The index of each wanted row, each found below the one before, or
/// `None` when one is missing.
fn in_order(rows: &[String], want: &[Want<'_>]) -> Option<Vec<usize>> {
    let mut at = Vec::new();
    let mut from = 0;
    for wanted in want {
        let found = rows
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, row)| match wanted {
                Want::Has(text) => row.contains(text),
                Want::Is(text) => row.trim() == *text,
                Want::Starts(text) => row.trim().starts_with(text),
            });
        let (index, _) = found?;
        at.push(index);
        from = index + 1;
    }
    Some(at)
}

/// Whether a row below `from` has exactly this trimmed text.
fn below(rows: &[String], from: usize, text: &str) -> bool {
    rows.iter().skip(from + 1).any(|row| row.trim() == text)
}

/// Whether any row's trimmed text is exactly `text`.
fn has_line(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.trim() == text)
}

/// A named pipe in the workspace, held open for reading and writing so
/// the open never blocks and a line written to it releases one `read`.
fn hold(setup: &Setup, name: &str) -> fs::File {
    let path = setup.workspace().join(name);
    fakes::fifo(&path);
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

/// Releases one `read -r _ < hold`.
fn release(hold: &mut fs::File) {
    writeln!(hold, "go").unwrap();
}

/// The one session the run started: the name of its directory under
/// the home's projects.
fn session_id(home: &Path) -> String {
    let mut ids = Vec::new();
    for project in fs::read_dir(home.join("projects")).unwrap().flatten() {
        let sessions = project.path().join("sessions");
        if let Ok(entries) = fs::read_dir(&sessions) {
            for session in entries.flatten() {
                ids.push(session.file_name().to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(ids.len(), 1, "one session: {ids:?}");
    ids.remove(0)
}

/// A `!` or `!!` run that keeps running and writes output draws its
/// row with the output so far and `running`, before it exits; the
/// ending then replaces `running`.
/// Order: the row shows the head, the note, the output so far, then
/// `running`, and the ending replaces it (`docs/tui.md`, "The input box").
fn streams_before_it_exits(prefix: &str, note: &str) {
    let setup = Setup::new();
    script(&setup);
    let mut gate = hold(&setup, "hold-4177");
    let mut run = attached(&setup);
    let head = "! printf 'started-4177";
    run.write(
        format!(
            "{prefix}printf 'started-4177\\n'; read -r _ < hold-4177; printf 'ended-4177\\n'\r"
        )
        .as_bytes(),
    );
    let mut snapshot: Vec<String> = Vec::new();
    run.wait_screen("the running row with its streamed output", |grid| {
        let rows = grid.rows.clone();
        let done = match in_order(
            &rows,
            &[
                Want::Has(head),
                Want::Has(note),
                Want::Is("started-4177"),
                Want::Is("running"),
            ],
        ) {
            Some(at) => !below(&rows, at[1], "ended-4177"),
            None => false,
        };
        if done {
            snapshot = rows;
        }
        done
    });
    assert!(
        in_order(
            &snapshot,
            &[
                Want::Has(head),
                Want::Has(note),
                Want::Is("started-4177"),
                Want::Is("running"),
            ]
        )
        .is_some(),
        "the row shows its output so far, then running: {snapshot:?}"
    );
    release(&mut gate);
    let mut ended: Vec<String> = Vec::new();
    run.wait_screen("the ended row", |grid| {
        let rows = grid.rows.clone();
        let done = in_order(
            &rows,
            &[
                Want::Has(head),
                Want::Has(note),
                Want::Is("started-4177"),
                Want::Is("ended-4177"),
                Want::Has("exit 0"),
            ],
        )
        .is_some()
            && !has_line(&rows, "running");
        if done {
            ended = rows;
        }
        done
    });
    assert!(!has_line(&ended, "running"), "the ending replaced running");
    quit(run);
}

#[test]
fn bang_run_streams_output_and_shows_running_before_exit() {
    streams_before_it_exits("!", "sent with your next prompt");
}

#[test]
fn bang_bang_run_streams_output_and_shows_running_before_exit() {
    streams_before_it_exits("!!", "only you see this");
}

/// A `!` run sent while the agent's own call is held shows its row and
/// output at once, and a second terminal that attaches mid-run sees the
/// running row, the output streamed after it attached, and the full
/// output when the run ends.
#[test]
fn bang_run_during_a_held_tool_call_streams_and_a_second_terminal_sees_it() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": ["ready"]},
            {"tool_calls": [{"name": "shell", "arguments": {"command": "read -r _ < agent-hold-4177"}}]},
            {"text": ["ready"]},
        ]}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "read"})
        ),
    )
    .unwrap();
    let mut agent_gate = hold(&setup, "agent-hold-4177");
    let mut gate = hold(&setup, "hold-4177");
    let mut middle_gate = hold(&setup, "mid-4177");
    let mut run = attached(&setup);
    let from = run.output().len();
    run.write(b"go\r");
    // The call's group draws collapsed; Ctrl+O opens the ledger, whose
    // row for the call reads `running` while it is held.
    run.wait_screen("the agent's call group", |grid| {
        grid.rows.iter().any(|row| row.contains("Ran 1 command"))
    });
    run.write(b"\x0f");
    run.wait_screen("the agent's call running", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("agent-hold-4177") && row.contains("running"))
    });
    let head = "! printf 'started-4177";
    let note = "sent with your next prompt";
    run.write(
        b"!printf 'started-4177\\n'; read -r _ < hold-4177; printf 'middle-4177\\n'; read -r _ < mid-4177; printf 'ended-4177\\n'\r",
    );
    run.wait_screen("the running row with its streamed output", |grid| {
        let rows = grid.rows.clone();
        match in_order(
            &rows,
            &[
                Want::Has(head),
                Want::Has(note),
                Want::Is("started-4177"),
                Want::Is("running"),
            ],
        ) {
            Some(at) => !below(&rows, at[1], "ended-4177"),
            None => false,
        }
    });
    // A second terminal attaches to the same session mid-run.
    let id = session_id(&setup.home());
    let mut late = Run::spawn(&setup, 160, 48, &["resume", id.as_str()], &TRUECOLOUR);
    late.ready();
    late.wait_screen("the second terminal's running row", |grid| {
        in_order(
            &grid.rows,
            &[Want::Has(head), Want::Has(note), Want::Is("running")],
        )
        .is_some()
    });
    release(&mut gate);
    late.wait_screen("output streamed after attach", |grid| {
        let rows = grid.rows.clone();
        match in_order(
            &rows,
            &[
                Want::Has(head),
                Want::Has(note),
                Want::Is("middle-4177"),
                Want::Is("running"),
            ],
        ) {
            Some(at) => !below(&rows, at[1], "ended-4177"),
            None => false,
        }
    });
    release(&mut middle_gate);
    let full = [
        Want::Has(head),
        Want::Has(note),
        Want::Is("started-4177"),
        Want::Is("middle-4177"),
        Want::Is("ended-4177"),
        Want::Has("exit 0"),
    ];
    late.wait_screen("the full output at shell_command", |grid| {
        in_order(&grid.rows, &full).is_some() && !has_line(&grid.rows, "running")
    });
    run.wait_screen("the ended row", |grid| {
        in_order(&grid.rows, &full).is_some() && !has_line(&grid.rows, "running")
    });
    release(&mut agent_gate);
    run.turn_finished(from);
    quit_both(late, run);
}

#[test]
fn bang_run_with_only_stderr_shows_it_and_the_exit_status() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!echo oops-4177 >&2; exit 3\r");
    run.wait_screen("the stderr row and its exit status", |grid| {
        in_order(
            &grid.rows,
            &[
                Want::Has("! echo oops-4177"),
                Want::Has("sent with your next prompt"),
                Want::Is("oops-4177"),
                Want::Has("exit 3"),
            ],
        )
        .is_some()
    });
    quit(run);
}

#[test]
fn bang_run_of_a_missing_command_shows_bash_and_the_exit_status() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    run.write(b"!no-such-command-4177\r");
    run.wait_screen("bash's complaint and exit 127", |grid| {
        in_order(
            &grid.rows,
            &[
                Want::Has("! no-such-command-4177"),
                Want::Has("sent with your next prompt"),
                Want::Has("command not found"),
                Want::Has("exit 127"),
            ],
        )
        .is_some()
    });
    quit(run);
}

#[test]
fn bang_run_that_cannot_start_shows_the_error() {
    let setup = Setup::new();
    script(&setup);
    let mut run = attached(&setup);
    // The workspace moves aside, so the command has no directory to run in.
    let aside = setup.root.path().join("w-aside");
    fs::rename(setup.workspace(), &aside).unwrap();
    run.write(b"!echo hi\r");
    run.wait_screen("the error row", |grid| {
        in_order(
            &grid.rows,
            &[Want::Has("! echo hi"), Want::Starts("invalid_arguments: ")],
        )
        .is_some()
    });
    // A refused command returns to the draft, which Ctrl+C would clear
    // before it quits: erase it first, and put the workspace back.
    run.write(&[0x7f; 8]);
    run.wait_screen("the cleared draft", |grid| {
        !grid.rows.iter().any(|row| row.contains("!echo hi"))
    });
    fs::rename(&aside, setup.workspace()).unwrap();
    quit(run);
}
