//! TUI extension slash commands at the real terminal: a well-shaped
//! command lists after the session rows and Enter runs it with its
//! arguments, while a command returning the wrong shape reports one
//! `hook_failed` notice carrying the wire it checked.

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
use support::{Setup, write_json};

/// Installs one extension with a session command and two TUI commands:
/// `ledger-diff` asserts its wire and runs, while `ledger-bad` returns
/// `5` past an args-and-session gate, so the shape failure proves the
/// wire end to end.
fn install(setup: &Setup) {
    let dir = setup.root.path().join("cmds");
    let manifest = json!({"name":"cmds","version":"v1.0.0","fiber":"0.0.0","api":1});
    write_json(&dir.join("extension.json"), &manifest);
    std::fs::write(
        dir.join("init.lua"),
        "fiber.command('ledger-session', { timeout = 5000, description = 'Sessions the ledger.', run = function() end })",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("tui")).unwrap();
    std::fs::write(
        dir.join("tui/init.lua"),
        "fiber.command('ledger-diff', { timeout = 100, description = 'Diffs the ledger row.', run = function(input)\n\
           assert(type(input.session_id) == 'string' and #input.session_id > 0, 'a session id');\n\
           assert(input.args == 'hello', 'the typed args');\n\
           return true\n\
         end })\n\
         fiber.command('ledger-bad', { timeout = 100, description = 'Fails the shape check.', run = function(input)\n\
           if input.args ~= 'fast' or type(input.session_id) ~= 'string' then\n\
             error('wire mismatch: session=' .. tostring(input.session_id) .. ' args=' .. tostring(input.args))\n\
           end\n\
           return 5\n\
         end })",
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(dir),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// A bare home and workspace with a connected hub and a scripted turn:
/// `echo` names the ledger output only the open ledger shows.
fn configure(setup: &Setup) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps":[{"tool_calls":[{"name":"shell","arguments":{"command":"echo ledger-probe-9d2c"}}]},{"text":["Complete."]}]}),
    );
    std::fs::write(
        setup.home().join("rules"),
        "{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"echo\"}\n",
    )
    .unwrap();
    let config = json!({"model":"scripted/s.json","hub":{"idle_exit_ms":200},"tui":{"rail":{"shown":false},"panel":{"cards":[]}}});
    write_json(&setup.home().join("config.json"), &config);
}

/// Runs one turn, leaving the terminal at the input box.
fn turn(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &[]);
    run.ready(); // Existing mode/first-frame signal, not a grid match.
    let from = run.output().len();
    run.write(b"run the tool\r");
    run.turn_finished(from);
    run
}

fn quit(mut run: Run, marker: &str) {
    // The marker sits in the draft: one Ctrl+C clears it, and the usual
    // sequence quits from the empty box.
    run.write(b"\x03");
    run.wait_screen("cleared draft", |grid| {
        !grid.rows.iter().any(|row| row.contains(marker))
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn tui_command_lists_and_runs() {
    let setup = Setup::new();
    configure(&setup);
    install(&setup);
    let mut run = turn(&setup);
    run.write(b"/ledger-");
    // Frame-locked on the drawn list: the panel draws bare names, so the
    // TUI row draws after the session row, never matched off the `/`
    // input echo, which holds neither full name.
    run.wait_screen("the TUI row after the session row", |grid| {
        let session = grid
            .rows
            .iter()
            .position(|row| row.contains("ledger-session"));
        let tui = grid.rows.iter().position(|row| row.contains("ledger-diff"));
        if !matches!((session, tui), (Some(first), Some(second)) if first < second) {
            return false;
        }
        grid.rows
            .iter()
            .find(|row| row.contains("ledger-diff"))
            .is_some_and(|row| row.contains("Diffs the ledger row.") && row.contains("cmds"))
    });
    run.write(b"diff hello");
    // The full draft echoes first: once it draws, every earlier byte was
    // read, so the Enter below acts on the whole command, and the clear
    // wait cannot pass on a frame from before typing landed.
    run.wait_screen("the typed command", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("ledger-diff hello"))
    });
    run.write(b"\r");
    // Enter runs the command: the draft clears with no refusal and no
    // failure. Without dispatch the draft would refuse as unknown.
    run.wait_screen("the run clears the draft", |grid| {
        !grid
            .rows
            .iter()
            .any(|row| row.contains("ledger-diff hello"))
            && !grid.contents.contains("Unknown command")
            && !grid.contents.contains("hook_failed")
            && !grid.contents.contains("extension_failed")
            && !grid.contents.contains("command_conflict")
    });
    quit(run, "ledger-diff hello");
}

#[test]
fn tui_command_wrong_shape_notice_carries_args() {
    let setup = Setup::new();
    configure(&setup);
    install(&setup);
    let mut run = turn(&setup);
    run.write(b"/ledger-bad fast\r");
    // The args gate passes only with the received args, so the shape
    // failure proves the wire: a mismatch would error with its own text.
    run.wait_screen("the wrong-shape hook failure", |grid| {
        grid.contents.contains("hook_failed")
            && grid.contents.contains("ledger-bad")
            && grid.contents.contains("cmds")
            && grid.contents.contains("wrong shape")
            && grid.contents.contains("fast")
            && grid.contents.contains("with args `fast`")
            && !grid.contents.contains("wire mismatch")
    });
    quit(run, "/ledger-bad fast");
}
