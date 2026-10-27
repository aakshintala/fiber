//! Binary-level tests of the slash aliases (see #3442): the built
//! `fiber` on a pty, typing each alias and asserting the same screen or
//! action as its canonical command, with the alias on the same
//! slash-list row.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::mpsc;
use std::thread;

use serde_json::json;
use support::pty::Run;
use support::{Deadline, Setup, write_json};

/// The terminal emits this synchronized-output marker after each frame
/// (`docs/tui.md`, "Performance").
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// The terminal on home: an offline model keeps boot free of the
/// network, and the hub exits at once after its last client leaves, so
/// `wait` never outlasts the test's deadline.
fn terminal(setup: &Setup) -> Run {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 0}}),
    );
    Run::spawn(
        setup,
        120,
        32,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    )
}

/// A setup whose `/rules` view lists one standing rule.
fn rules_setup() -> Setup {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "git status",
                "added": 1000, "session_id": "s_aaa"})
        ),
    )
    .unwrap();
    setup
}

/// Quits from home after Ctrl+C clears any remaining input, then checks
/// the restored terminal and a clean exit.
fn quit(mut run: Run, command: &str) {
    let input = run.output().len();
    run.write(b"\x03");
    run.wait_bytes(input, FRAME_END, "the cleared home-input frame");
    run.write(b"/quit\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0), "{command}");
}

/// The first grid row containing `needle`.
fn row_with(grid: &support::pty::Grid, needle: &str, command: &str) -> String {
    grid.rows
        .iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} for {command} in\n{}", grid.contents))
        .clone()
}

/// Types `prefix`, waits for the slash-list row and its frame to finish,
/// types `rest`, and returns the output offset before `rest`.
fn run_slash(run: &mut Run, prefix: &str, row: &str, rest: &str, command: &str) -> usize {
    let prefix_input = run.output().len();
    run.write(prefix.as_bytes());
    let what = format!("the slash-list row for {command}");
    run.wait_screen(&what, |grid| grid.contents.contains(row));
    run.wait_bytes(prefix_input, FRAME_END, "the slash-list frame");
    let rest_input = run.output().len();
    run.write(rest.as_bytes());
    rest_input
}

/// Opens a command's view, returns the row containing `marker`, then
/// closes the view and quits.
fn view_row(
    setup: &Setup,
    command: &str,
    prefix: &str,
    row: &str,
    rest: &str,
    marker: &str,
) -> String {
    let mut run = terminal(setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let rest_input = run_slash(&mut run, prefix, row, rest, command);
    let view = run.wait_screen("the command view", |grid| grid.contents.contains(marker));
    run.wait_bytes(rest_input, FRAME_END, "the command view frame");
    let found = row_with(&view, marker, command);

    let close_input = run.output().len();
    run.write(b"\x1b[27u"); // Kitty-protocol Esc
    run.wait_screen("the closed command view", |grid| {
        !grid.contents.contains(marker)
    });
    run.wait_bytes(close_input, FRAME_END, "the closed command view frame");
    quit(run, command);
    found
}

/// The rebinding screen's footer line after running `command` through
/// `prefix` plus `rest`, for the row `row`.
fn rebinding_footer(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    view_row(&Setup::new(), command, prefix, row, rest, "Enter rebind")
}

#[test]
fn keymap_opens_the_rebinding_screen_like_keys() {
    // The row shows `/keybindings` before `/keymap` (`docs/tui.md:1962,1984`).
    let keys = rebinding_footer("/keys", "/keys", "keys /keybindings /keymap", "\r");
    let keymap = rebinding_footer("/keymap", "/keymap", "keys /keybindings /keymap", "\r");
    assert_eq!(keymap, keys);
}

/// The standing-rules row after running `command` through `prefix`
/// plus `rest`, for the row `row`.
fn rules_row(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    view_row(
        &rules_setup(),
        command,
        prefix,
        row,
        rest,
        "allow shell:git status",
    )
}

#[test]
fn permissions_opens_the_standing_rules_like_rules() {
    // The table and alias rule set this row order (`docs/tui.md:1958,1984`).
    let rules = rules_row("/rules", "/rules", "rules /permissions", "\r");
    let permissions = rules_row("/permissions", "/permissions", "rules /permissions", "\r");
    assert_eq!(permissions, rules);
}

/// The home notice after running `command` through `prefix` plus
/// `rest`, for the row `row`.
fn home_notice(command: &str, prefix: &str, row: &str, rest: &str, notice: &str) -> String {
    let setup = Setup::new();
    let mut run = terminal(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let rest_input = run_slash(&mut run, prefix, row, rest, command);
    let grid = run.wait_screen("the home notice", |grid| grid.contents.contains(notice));
    run.wait_bytes(rest_input, FRAME_END, "the home notice frame");
    let found = row_with(&grid, notice, command);
    quit(run, command);
    found
}

#[test]
fn compact_starts_a_handoff_like_handoff() {
    // The table and alias rule set this row order (`docs/tui.md:1965,1984`).
    let row = "handoff /compact";
    let handoff = home_notice(
        "/handoff",
        "/handof",
        row,
        "f take over\r",
        "No session on screen.",
    );
    let compact = home_notice(
        "/compact",
        "/compa",
        row,
        "ct take over\r",
        "No session on screen.",
    );
    assert_eq!(compact, handoff);
}

#[test]
fn stop_asks_to_close_all_like_close() {
    // The table and alias rule set this row order (`docs/tui.md:1972,1984`).
    let row = "close /stop";
    let close = home_notice(
        "/close all",
        "/close",
        row,
        " all\r",
        "No sessions to close.",
    );
    let stop = home_notice("/stop all", "/sto", row, "p all\r", "No sessions to close.");
    assert_eq!(stop, close);
}

/// The `/jobs` slash-list row, carrying its aliases.
const JOBS_ROW: &str = "jobs /tasks /ps";

/// A held background shell job: dropping the guard kills the job's
/// group, so a failure leaks nothing.
struct Release {
    file: fs::File,
    guard: fakes::Watchdog,
}

/// A setup whose scripted turn starts one background shell job, held
/// on a FIFO: the job reports its group id on the ready FIFO, then
/// blocks until `/close` stops it. The fake provider installs first;
/// the scripted `config.json` lands after `setup.provider`, which
/// rewrites it. Returns the setup with its provider server, held to
/// the test's end, the job's ready FIFO and its release.
fn jobs_setup() -> (
    Setup,
    fakes::ProviderServer,
    fakes::children::Ready,
    Release,
) {
    let setup = Setup::new();
    let workspace = setup.workspace();
    // The job reports its group id here; the read opens before the
    // run, so the first line is never lost.
    let ready_fifo = setup.root.path().join("ready.fifo");
    fakes::fifo(&ready_fifo);
    let ready = fakes::children::Ready::at(&ready_fifo, &|| setup.deadline.left());
    // Held open read-write, so the job's read blocks until `/close`.
    let hold = workspace.join("hold");
    fakes::fifo(&hold);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hold)
        .unwrap();
    let command = format!(
        "echo $$ > '{}'; head -n 1 '{}'",
        ready_fifo.display(),
        hold.display()
    );
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell",
                "arguments": {"command": command, "run_in_background": true}}]},
            {"text": ["Done."]},
        ]}),
    );
    // The reviewer allows far more calls than the turn makes, so no
    // approval stalls the job's start.
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
        ]}),
    );
    let server = fakes::ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json",
            "reviewer": {"model": "scripted/r.json"},
            "hub": {"idle_exit_ms": 200},
        }),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": command}),
        ),
    )
    .unwrap();
    // The job's guard kills its group if the test fails before the
    // release lets the read finish.
    let guard = fakes::Watchdog::matching(hold.to_str().unwrap());
    (setup, server, ready, Release { file: held, guard })
}

/// A setup whose scripted turn answers at once: the attached session
/// holds no job.
fn empty_setup() -> (Setup, fakes::ProviderServer) {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": ["Done."]},
        ]}),
    );
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]},
        ]}),
    );
    let server = fakes::ProviderServer::start([]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json",
            "reviewer": {"model": "scripted/r.json"},
            "hub": {"idle_exit_ms": 200},
        }),
    );
    (setup, server)
}

/// Waits under `deadline` until no session socket remains beside the
/// hub's: the closed session has exited, so `/quit` meets no live
/// session and asks nothing. A session-close wait, so the caller
/// passes the cleanup phase.
fn until_no_sessions(deadline: Deadline, setup: &Setup, what: &str) {
    let run = setup.home().join("run");
    let (done, reached) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let live = fs::read_dir(&run)
                .map(|entries| {
                    entries
                        .flatten()
                        .any(|entry| entry.file_name().to_string_lossy() != "hub")
                })
                .unwrap_or(false);
            if !live {
                break;
            }
            thread::yield_now();
        }
        done.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&reached).is_ok(),
        "waited until the deadline for {what}"
    );
}

/// The scripted run: spawns after the scripted `config.json` lands,
/// since `terminal()` would rewrite it.
fn scripted(setup: &Setup) -> Run {
    Run::spawn(
        setup,
        120,
        32,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    )
}

/// Types `command` with a job running and asserts the jobs list opens
/// on its heading and job row, then closes the list, closes the
/// session and quits.
fn running_case(command: &str) {
    // The command table and alias rule place `/jobs` after `/delegates`
    // (`docs/tui.md:1978,1980,1984`).
    let (setup, _server, ready, release) = jobs_setup();
    let mut run = scripted(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"go\r");
    // Child readiness: the background job's group id.
    let group = ready.wait(setup.deadline)[0];
    // The finished turn plus the Jobs rail chip's `1 running` are the
    // stronger signal: Fiber has folded `job_started`, so the job list
    // holds the job.
    run.wait_screen("the finished turn", |grid| {
        grid.contents.contains("Done.") && grid.contents.contains("1 running")
    });
    // The slash row carries the aliases; Enter opens the list.
    let prefix_input = run.output().len();
    run.write(command.as_bytes());
    run.wait_screen("the slash-list row", |grid| {
        grid.contents.contains(JOBS_ROW)
    });
    run.wait_bytes(prefix_input, FRAME_END, "the slash-list frame");
    let view_input = run.output().len();
    run.write(b"\r");
    // The Jobs card has this heading and one row per job (`docs/tui.md:427-428,594`).
    let view = run.wait_screen("the jobs list", |grid| {
        grid.contents.contains("Jobs · 1 running")
    });
    run.wait_bytes(view_input, FRAME_END, "the jobs list frame");
    let _heading = row_with(&view, "Jobs · 1 running", command);
    assert!(
        view.rows.iter().any(|row| row.contains("◆")),
        "the listed job row for {command} in\n{}",
        view.contents
    );
    // Esc closes the list again, back on the conversation.
    let esc_input = run.output().len();
    run.write(b"\x1b[27u"); // Kitty-protocol Esc
    let _closed = run.wait_screen("the closed jobs list", |grid| {
        !grid.contents.contains("Jobs · 1 running") && grid.contents.contains("Done.")
    });
    run.wait_bytes(esc_input, FRAME_END, "the closed jobs list frame");
    // `/close` returns home and stops the held job: its group empties.
    run.write(b"/close\r");
    run.wait_screen("home after /close", |grid| {
        grid.contents.contains("Enter starts a session")
    });
    assert!(
        fakes::group_empties(group, setup.deadline.cleanup_phase()),
        "the held job's group empties after /close for {command}"
    );
    // The session itself has exited before `/quit` may meet it.
    until_no_sessions(
        setup.deadline.cleanup_phase(),
        &setup,
        "the closed session to exit",
    );
    let Release { file: _held, guard } = release;
    guard.stand_down(setup.deadline.cleanup_phase());
    quit(run, command);
}

/// Types `command` with an attached session but no job: the notice
/// shows and no list opens.
fn empty_case(command: &str) {
    // The command table and alias rule place `/jobs` after `/delegates`
    // (`docs/tui.md:1978,1980,1984`).
    let (setup, _server) = empty_setup();
    let mut run = scripted(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the finished turn", |grid| grid.contents.contains("Done."));
    let prefix_input = run.output().len();
    run.write(command.as_bytes());
    run.wait_screen("the slash-list row", |grid| {
        grid.contents.contains(JOBS_ROW)
    });
    run.wait_bytes(prefix_input, FRAME_END, "the slash-list frame");
    let notice_input = run.output().len();
    run.write(b"\r");
    let grid = run.wait_screen("the empty notice", |grid| {
        grid.contents.contains("No jobs running")
    });
    run.wait_bytes(notice_input, FRAME_END, "the empty notice frame");
    // No list opens: the conversation stays on screen, with no empty frame.
    assert!(grid.contents.contains("Done."), "{command}");
    assert!(!grid.contents.contains("Nothing running."), "{command}");
    let _notice = row_with(&grid, "No jobs running", command);
    // `/close` returns home; `/quit` exits once the session has.
    run.write(b"/close\r");
    run.wait_screen("home after /close", |grid| {
        grid.contents.contains("Enter starts a session")
    });
    until_no_sessions(
        setup.deadline.cleanup_phase(),
        &setup,
        "the closed session to exit",
    );
    quit(run, command);
}

#[test]
fn jobs_opens_the_running_jobs_list() {
    running_case("/jobs");
}

#[test]
fn tasks_opens_the_running_jobs_list_like_jobs() {
    running_case("/tasks");
}

#[test]
fn ps_opens_the_running_jobs_list_like_jobs() {
    running_case("/ps");
}

#[test]
fn jobs_shows_no_jobs_running_with_no_job() {
    empty_case("/jobs");
}

#[test]
fn tasks_shows_no_jobs_running_with_no_job_like_jobs() {
    empty_case("/tasks");
}

#[test]
fn ps_shows_no_jobs_running_with_no_job_like_jobs() {
    empty_case("/ps");
}

#[test]
fn jobs_on_home_shows_no_session_on_screen() {
    // The command table and alias rule place `/jobs` after `/delegates`
    // (`docs/tui.md:1978,1980,1984`).
    let command = "/jobs";
    let setup = Setup::new();
    let mut run = terminal(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let rest_input = run_slash(&mut run, command, JOBS_ROW, "\r", command);
    let grid = run.wait_screen("the home notice", |grid| {
        grid.contents.contains("No session on screen.")
    });
    run.wait_bytes(rest_input, FRAME_END, "the home notice frame");
    // Nothing opens: home stays on screen, with no jobs frame.
    assert!(
        grid.contents.contains("Enter starts a session"),
        "{command}"
    );
    assert!(!grid.contents.contains("Nothing running."), "{command}");
    let _notice = row_with(&grid, "No session on screen.", command);
    quit(run, command);
}
