//! Binary-level tests of the delegate view and a job's pty view
//! (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty in
//! truecolour, with the scripted provider answering, draws the doc's
//! swapped-view rows (top row, bold breadcrumb, `surface` status card,
//! live-screen line, `turn` slab, input hints).

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
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use serde_json::json;
use support::hold::Hold;
use support::pty::{Colour, Grid, Run};
use support::{Deadline, Setup, TRUECOLOUR, write_json};

/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);
/// `surface` in the dark theme.
const SURFACE: Colour = Colour::Rgb(26, 26, 34);
/// `turn` in the dark theme.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The reviewer answers and the allow-rules both fixtures share: the
/// parent's steps differ, but every tool call is allowed either way.
fn write_shared(setup: &Setup, command: &str) {
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"},
            "roles": {"delegate": "fiber:scripted/d.json"},
            "hub": {"idle_exit_ms": 1000}
        }),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": command}),
        ),
    )
    .unwrap();
}

/// Only the delegate is held: the parent spawns one delegate whose
/// blocking step runs `head -n 1 <hold>` in the background, then
/// answers. The background move is immediate, so the panel row draws
/// without the foreground's 30-second move wait; the held job still
/// parks the delegate until the test releases it. No tty job starts,
/// so releasing the hold finishes the delegate alone.
fn delegate_setup() -> (Setup, Hold) {
    let setup = Setup::new();
    let command = format!("head -n 1 {}", setup.workspace().join("hold").display());
    write_json(
        &setup.workspace().join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": command, "run_in_background": true}}]},
            {"text": ["Reviewed."]},
            {"text": ["Steered."]},
            // The tool result, the steer and the shell's own end each
            // start a turn; the spare covers no further turn.
            {"text": ["Settled."]},
            {"text": ["Resting."]},
        ]}),
    );
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "delegate_spawn", "arguments": {
                "description": "review the parser", "prompt": "go",
                "model": "fiber:scripted/d.json"}}],
             "usage": {"input": 1200}},
            {"text": ["Done."]},
            // The delegate's finish is the parent's next input; the
            // spare covers no further turn.
            {"text": ["Noted."]},
            {"text": ["Calm."]},
        ]}),
    );
    write_shared(&setup, &command);
    let hold = Hold::park(&setup, &command);
    (setup, hold)
}

/// Dropping the helper without any release write reaps the parked holder:
/// the parent-death cleanup path, which is the leak this helper closes.
/// The pre-drop guard pins the test against vacuity: `try_matching_exits`
/// passes when nothing matches, so without a live match on the holder's
/// own pid the reap below would prove nothing.
#[test]
fn hold_drop_releases_and_reaps_the_holder() {
    let setup = Setup::new();
    // Pid-scoped like the job command, so parallel tests cannot
    // cross-match; the absolute path keeps the helper on its hold-path
    // derivation.
    let command = format!(
        "head -n 1 {} #{}",
        setup.workspace().join("hold").display(),
        std::process::id()
    );
    let hold = support::hold::Hold::park(&setup, &command);
    // A second read+write handle, held open across the drop below: the
    // helper's own handle closing must not EOF-release the holder, so the
    // reap proves the watchdog kill rather than the EOF path. Still no
    // release write anywhere in this test.
    let second = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(setup.workspace().join("hold"))
        .unwrap();
    let mut holder = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = holder.id();
    assert!(fakes::pid_alive(pid), "the holder lives before the drop");
    let pattern = setup.workspace().join("hold").to_str().unwrap().to_owned();
    let listed = pattern.clone();
    fakes::wait_until("the holder to park", setup.deadline, move || {
        fakes::matching(&listed)
            .map(|pids| pids.contains(&pid))
            .unwrap_or(false)
    });
    drop(hold);
    fakes::try_matching_exits(pattern.as_str(), setup.deadline)
        .unwrap_or_else(|error| panic!("the dropped helper reaps its holder: {error}"));
    drop(second);
    let _exited = holder.wait();
}

/// Only the tty job is held: the parent starts one scripted `shell`
/// (with `tty: true`) running `head -n 1 hold` in the workspace, then
/// answers. No delegate spawns, so releasing the hold finishes the job
/// alone. The command stays short on purpose: the absolute hold path
/// would push the status row's stop and outcome under the row's cut.
/// The command returns with the setup: the panel names the job row by
/// it, so the test clicks only the row that names it.
fn job_setup() -> (Setup, Hold, String) {
    let setup = Setup::new();
    // A shell comment with this process id keeps the watchdog's
    // pattern to this test's `head`: parallel tests hold their own.
    let command = format!("head -n 1 hold #{}", std::process::id());
    let workdir = setup.workspace().to_string_lossy().into_owned();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {
                "command": command, "workdir": workdir,
                "tty": true, "run_in_background": true}}]},
            {"text": ["Done."]},
            // A background job's end is the model's next input; the
            // spare covers no further turn.
            {"text": ["Noted."]},
            {"text": ["Calm."]},
        ]}),
    );
    write_shared(&setup, &command);
    let hold = Hold::park(&setup, &command);
    (setup, hold, command)
}

/// The binary started at 160x48 and prompted, so the target item starts.
fn start_run(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run
}

/// The job row, wherever the panel lays it out, or none before it draws.
fn job_row_index(rows: &[String], command: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(command))
}

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
        .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents))
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing, or a panic naming it.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<char> = needle.chars().collect();
    if want.is_empty() {
        panic!("{needle:?} on row {y}");
    }
    let (width, _) = grid.size();
    (0..width)
        .find(|x| {
            want.iter().enumerate().all(|(dx, ch)| {
                let xi = x + dx as u16;
                xi < width && grid.cell(xi, y).symbol == ch.to_string()
            })
        })
        .unwrap_or_else(|| panic!("{needle:?} on row {y}"))
}

/// Every cell of `needle` on `y` is bold, or a panic naming it.
fn assert_bold(grid: &Grid, y: u16, needle: &str) {
    let x = col_at(grid, y, needle);
    for (dx, ch) in needle.chars().enumerate() {
        let cell = grid.cell(x + dx as u16, y);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", x + dx as u16);
        assert!(cell.bold, "bold {} on row {y}", x + dx as u16);
    }
}

/// Every non-blank cell of `needle` on `y` is dim, or a panic naming it.
fn assert_dim(grid: &Grid, y: u16, needle: &str) {
    let x = col_at(grid, y, needle);
    for (dx, ch) in needle.chars().enumerate() {
        let cell = grid.cell(x + dx as u16, y);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", x + dx as u16);
        if ch != ' ' {
            assert!(cell.dim, "dim {} on row {y}", x + dx as u16);
        }
    }
}

/// Clicks the cell at `(x, y)`: an SGR press then its release on the
/// same cell.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// Opens the panel row holding `needle`: clicks its text, then waits
/// for the item view's top row. The caller waits for the turn's
/// completion first, so the row's position is final: the parent's turn
/// is done and the one held item parks on the test-held FIFO, so later
/// folds only tick in place.
fn open_row(run: &mut Run, needle: &str, top: &str) -> Grid {
    let grid = run.wait_screen("the panel lists the row", |grid| {
        grid.contents.contains(needle)
    });
    let y = row_with(&grid, needle);
    click(run, col_at(&grid, y, needle), y);
    run.wait_screen("the item view opens", |grid| grid.contents.contains(top))
}

/// Every cell spelling `needle` on `y` sits on `tint`, or a panic: the
/// slab shows past short text but the needle pins its ground, clear of
/// the rail and panel text sharing the row.
fn assert_span_tint(grid: &Grid, y: u16, needle: &str, tint: Colour) {
    let x = col_at(grid, y, needle);
    for (dx, ch) in needle.chars().enumerate() {
        let cell = grid.cell(x + dx as u16, y);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", x + dx as u16);
        assert_eq!(cell.bg, tint, "tint {} on row {y}", x + dx as u16);
    }
}

#[test]
fn the_delegate_view_matches_the_doc() {
    let (setup, hold) = delegate_setup();
    let mut run = start_run(&setup);
    // The parent's turn completes before the click, so the panel row's
    // position is final: the completed footer proves the turn is done,
    // and the delegate parks on the test-held FIFO, so no further
    // session event can move rows before the click. The finished title
    // never emits while an item is held, so the turn's completion —
    // not `turn_finished` — is the readiness signal here.
    run.wait_screen("the delegate parks", |grid| {
        grid.contents.contains("● review the parser")
            && grid.contents.contains("Done.")
            && grid.contents.contains("completed")
    });
    let top = "Delegate  ◆ review the parser";
    let grid = open_row(&mut run, "● review the parser", top);
    // The top row names the view; the breadcrumb is bold.
    let y_top = row_with(&grid, top);
    assert_bold(&grid, y_top, "Delegate");
    assert_dim(&grid, y_top, "◆ review the parser");
    let crumb = row_with(&grid, "› ◆ review the parser");
    assert_bold(&grid, crumb, "› ◆ review the parser");
    // The status card sits on the `surface` tint: the harness, the
    // model, the calls with the elapsed time and the session, then the
    // stop target with that same session. The card rows sit below the
    // header's own top row, so they anchor to it: the panel's session
    // list names the same harness and model on its own rows.
    let card = y_top + 2;
    let card_row = &grid.rows[card as usize];
    assert!(
        card_row.contains("harness fiber"),
        "the card's first row:\n{card_row}"
    );
    let model = card_row
        .split(" · model ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| panic!("the model in\n{}", grid.contents))
        .to_owned();
    let model_segment = format!(" · model {model}");
    assert_dim(&grid, card, &model_segment);
    assert_span_tint(&grid, card, "harness fiber", SURFACE);
    assert_span_tint(&grid, card, &model_segment, SURFACE);
    // No held status names no level: past the model, the `surface` slab
    // is blank until the panel's own ground.
    let seg_at = col_at(&grid, card, &model_segment);
    let seg_end = seg_at + model_segment.chars().count() as u16;
    let (width, _) = grid.size();
    let mut x = seg_end;
    while x < width && grid.cell(x, card).bg == SURFACE {
        assert_eq!(grid.cell(x, card).symbol, " ", "blank past the model {x}");
        x += 1;
    }
    let calls = y_top + 3;
    let calls_row = &grid.rows[calls as usize];
    assert!(
        calls_row.contains("calls · elapsed"),
        "the card's second row:\n{calls_row}"
    );
    let elapsed = calls_row
        .split("elapsed ")
        .nth(1)
        .and_then(|rest| rest.split(" · session ").next())
        .filter(|elapsed| elapsed.contains('s'))
        .unwrap_or_else(|| panic!("the elapsed value in\n{}", grid.contents))
        .to_owned();
    assert!(
        calls_row.contains(&format!("elapsed {elapsed} · session ")),
        "the elapsed value draws:\n{calls_row}"
    );
    let session = calls_row
        .split("session ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|id| id.starts_with("s_"))
        .unwrap_or_else(|| panic!("the session id in\n{}", grid.contents))
        .to_owned();
    assert_dim(&grid, calls, "calls · elapsed");
    assert_dim(&grid, calls, &session);
    assert_span_tint(&grid, calls, "calls · elapsed", SURFACE);
    assert_span_tint(&grid, calls, &session, SURFACE);
    let stop = format!("■ stop {session}");
    let y_stop = y_top + 4;
    assert!(
        grid.rows[y_stop as usize].contains(&stop),
        "the stop row:\n{}",
        grid.rows[y_stop as usize]
    );
    assert_span_tint(&grid, y_stop, &stop, SURFACE);
    let stop_at = col_at(&grid, y_stop, &stop);
    for (dx, ch) in stop.chars().enumerate() {
        let cell = grid.cell(stop_at + dx as u16, y_stop);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", stop_at + dx as u16);
        assert_eq!(cell.fg, ERROR, "error stop {}", stop_at + dx as u16);
    }
    // The slab runs past the stop label.
    let stop_len = stop.chars().count() as u16;
    for x in stop_at + stop_len..stop_at + stop_len + 5 {
        assert_eq!(grid.cell(x, y_stop).bg, SURFACE, "slab past the stop {x}");
    }
    // The input hint names the delegate, dim, with the caret clear of it.
    let hint = "steering goes to ◆ review the parser";
    let y_hint = row_with(&grid, hint);
    assert_dim(&grid, y_hint, hint);
    let caret = (0..160)
        .find(|x| grid.cell(*x, y_hint).symbol == "█")
        .unwrap_or_else(|| panic!("the caret in\n{}", grid.contents));
    assert!(
        caret < col_at(&grid, y_hint, hint),
        "the hint never covers the caret"
    );
    // Typing steers: the draft sends and the box clears back to the hint.
    run.write(b"go on");
    run.wait_screen("the draft shows the steer", |grid| {
        grid.contents.contains("go on")
    });
    run.write(b"\r");
    run.wait_screen("the steer sends and the draft clears", |grid| {
        !grid.contents.contains("go on")
            && grid
                .contents
                .contains("steering goes to ◆ review the parser")
    });
    let cleanup = setup.deadline.cleanup_phase();
    finish_and_quit(run, hold, cleanup, "steering goes to ◆ review the parser");
}

#[test]
fn job_row_predicate_rejects_the_pre_expansion_grid() {
    let command = "head -n 1 hold";
    let pre_expansion = vec![
        "Jobs                1 running ▸".to_owned(),
        "▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄".to_owned(),
    ];
    let old_predicate_accepted = pre_expansion
        .iter()
        .position(|row| row.contains("Jobs"))
        .and_then(|title| pre_expansion.get(title + 1))
        .is_some_and(|row| !row.trim().is_empty());

    assert!(
        old_predicate_accepted,
        "the old predicate accepts this grid"
    );
    assert_eq!(job_row_index(&pre_expansion, command), None);
}

#[test]
fn fifo_release_written_before_reader_opens_is_read() {
    let setup = Setup::new();
    let fifo = setup.workspace().join("hold");
    fakes::fifo(&fifo);
    let mut release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();

    // Write the release before the reader opens the FIFO, and keep the
    // O_RDWR end alive until it reads the line.
    release.write_all(b"\n").unwrap();
    let child = Command::new("head")
        .arg("-n")
        .arg("1")
        .arg(&fifo)
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let output = fakes::wait_output(child, "the FIFO reader to exit");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\n");
}

#[test]
fn the_job_pty_view_matches_the_doc() {
    let (setup, hold, command) = job_setup();
    let mut run = start_run(&setup);
    // The Jobs card starts closed; its title toggles the list. The
    // completed turn is the session-event signal that makes the held
    // command stable. The later grid predicate identifies that command's
    // row, rather than accepting any non-blank row below the title. The
    // finished title never emits while an item is held, so the turn's
    // completion — not `turn_finished` — is the readiness signal here.
    // No session event lands between the two clicks; only the first
    // click's own expansion changes the screen.
    let grid = run.wait_screen("the tty job starts", |grid| {
        grid.contents.contains("Jobs")
            && grid.contents.contains("Done.")
            && grid.contents.contains("completed")
    });
    let title = row_with(&grid, "Jobs");
    click(&mut run, col_at(&grid, title, "Jobs"), title);
    let grid = run.wait_screen("the job row draws", |grid| {
        job_row_index(&grid.rows, &command).is_some()
    });
    let job = job_row_index(&grid.rows, &command).unwrap() as u16;
    let (width, _) = grid.size();
    let job_x = (0..width)
        .find(|x| grid.cell(*x, job).symbol != " ")
        .unwrap_or_else(|| panic!("the job row in\n{}", grid.contents));
    click(&mut run, job_x, job);
    let grid = run.wait_screen("the item view opens", |grid| {
        grid.contents.contains("Job  ◆ ")
    });
    // The status row names the job twice: once in the row, once in the
    // stop target. The row is short enough to draw whole: no `…` cuts
    // it, so the finish fold below reads the outcome off the screen.
    // The live word and elapsed stay pinned by the unit tests.
    let status = row_with(&grid, "job ");
    let id = grid.rows[status as usize]
        .split("job ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|id| id.starts_with("j_"))
        .unwrap_or_else(|| panic!("the job id in\n{}", grid.contents))
        .to_owned();
    let row = &grid.rows[status as usize];
    assert!(
        row.contains(&format!("job {id} · head -n 1 ")),
        "the job row names the job and command:\n{row}"
    );
    let running = row
        .split("running ")
        .nth(1)
        .and_then(|rest| rest.split(" · ").next())
        .filter(|elapsed| elapsed.contains('s'))
        .unwrap_or_else(|| panic!("the running time in\n{row}"))
        .to_owned();
    assert!(
        row.contains(&format!("running {running}")),
        "the running time draws:\n{row}"
    );
    assert_span_tint(&grid, status, &format!("job {id}"), SURFACE);
    assert_span_tint(&grid, status, &format!("running {running}"), SURFACE);
    let stop = format!("■ stop {id}");
    let y_stop = row_with(&grid, &stop);
    assert_span_tint(&grid, y_stop, &stop, SURFACE);
    let stop_at = col_at(&grid, y_stop, &stop);
    for (dx, ch) in stop.chars().enumerate() {
        let cell = grid.cell(stop_at + dx as u16, y_stop);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", stop_at + dx as u16);
        assert_eq!(cell.fg, ERROR, "error stop {}", stop_at + dx as u16);
    }
    for x in stop_at.saturating_sub(3)..stop_at {
        assert!(grid.cell(x, y_stop).dim, "dim separator {x}");
    }
    // The live-screen line is dim; the screen sits on the `turn` slab
    // 80 wide.
    let live = row_with(&grid, "live screen · 80 columns × 24 rows from attach");
    assert_dim(
        &grid,
        live,
        "live screen · 80 columns × 24 rows from attach",
    );
    // The slab starts where the status text starts: rows are
    // full-width, so the rail and panel text shares them.
    let slab = col_at(&grid, status, &format!("job {id}"));
    let (width, _) = grid.size();
    assert!(slab + 80 < width, "the 80-column slab fits");
    for y in live + 2..live + 12 {
        for x in slab..slab + 80 {
            assert_eq!(grid.cell(x, y).bg, TURN, "screen tint {x},{y}");
        }
        assert_ne!(grid.cell(slab + 80, y).bg, TURN, "the slab ends at 80");
    }
    // The input hint names job input, dim, with the caret clear of it.
    // The tty job is never typed into: typing would complete `head`
    // and end the fixture.
    let hint = "type into the job";
    let y_hint = row_with(&grid, hint);
    assert_dim(&grid, y_hint, hint);
    let caret = (0..160)
        .find(|x| grid.cell(*x, y_hint).symbol == "█")
        .unwrap_or_else(|| panic!("the caret in\n{}", grid.contents));
    assert!(
        caret < col_at(&grid, y_hint, hint),
        "the hint never covers the caret"
    );
    let cleanup = setup.deadline.cleanup_phase();
    finish_and_quit(run, hold, cleanup, "type into the job");
}

/// Writes the release line so `head` reads it and exits and the one
/// held item finishes, closes the view, quits, and asserts the exit:
/// every wait is a screen predicate. The release is a written line,
/// never a dropped writer: the test's open end keeps the pipe alive
/// until the held `head` reads the line, while dropping the last writer
/// before `head` opens the FIFO strands it there forever
/// (`docs/testing.md`, "Waits and timeouts"). The holder already exited
/// on this path, so the helper stands its watchdog down instead of
/// dropping into its kill; a panic before the stand-down still drops
/// into the kill.
/// (`docs/testing.md`, "Running tests").
fn finish_and_quit(mut run: Run, mut hold: Hold, deadline: Deadline, closed_needle: &str) {
    hold.release().write_all(b"\n").unwrap();
    run.wait_screen("the finish folds", |grid| {
        !grid.contents.contains("■ stop")
            && (grid.contents.contains("completed") || grid.contents.contains("failed"))
    });
    // Esc is kitty `CSI 27u`: a lone `0x1b` would hold as an unfinished
    // sequence and never close the view.
    run.write(b"\x1b[27u");
    run.wait_screen("the view closes", |grid| {
        !grid.contents.contains(closed_needle)
    });
    run.write(b"\x03\x03\r");
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(0));
    hold.stand_down(deadline);
}
