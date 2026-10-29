//! Whole-screen key bindings at the real terminal: a `run` binding
//! suppresses its default with no notice, and an unbound key behaves as
//! with no extension installed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
mod support;

use serde_json::json;
use support::pty::Run;
use support::{Setup, write_json};

/// Installs a client-only bare extension from `source`, like
/// `tui_extensions.rs` does for its loader cases.
fn install(setup: &Setup, name: &str, source: &str) {
    let dir = setup.root.path().join(name);
    let manifest = json!({"name":name,"version":"v1.0.0","fiber":"0.0.0","api":1});
    write_json(&dir.join("extension.json"), &manifest);
    std::fs::create_dir_all(dir.join("tui")).unwrap();
    std::fs::write(dir.join("init.lua"), "").unwrap();
    std::fs::write(dir.join("tui/init.lua"), source).unwrap();
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

/// Types `marker` and waits for it to draw: once the marker draws, every
/// earlier byte was read, so the assertions below see the keystroke's
/// final state.
fn marked(run: &mut Run, marker: &str) -> support::pty::Grid {
    run.write(marker.as_bytes());
    run.wait_screen("the typed marker", |grid| {
        grid.rows.iter().any(|row| row.contains(marker))
    })
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
fn run_binding_suppresses_toggle_ledgers_without_notice() {
    let setup = Setup::new();
    configure(&setup);
    install(
        &setup,
        "keys",
        "fiber.key('ctrl+o', { timeout = 100, run = function() return true end })",
    );
    let mut run = turn(&setup);
    run.write(b"\x0f");
    let grid = marked(&mut run, "zz");
    // Ledgers stay closed: the tool output draws only in the ledger rows
    // the toggle would open.
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c")),
        "the ledger opened: {}",
        grid.contents
    );
    for code in ["extension_failed", "hook_failed", "command_conflict"] {
        assert!(
            !grid.contents.contains(code),
            "a {code} notice drew: {}",
            grid.contents
        );
    }
    quit(run, "zz");
}

/// Presses the unbound Ctrl+Y with `with_extension`, then asserts the
/// turn still reads as with no extension installed: no notice, the reply
/// on screen, and the marker in the draft.
fn unbound_key(with_extension: bool) {
    let setup = Setup::new();
    configure(&setup);
    if with_extension {
        install(
            &setup,
            "keys",
            "fiber.key('ctrl+o', { timeout = 100, run = function() return true end })",
        );
    }
    let mut run = turn(&setup);
    run.write(b"\x19");
    let grid = marked(&mut run, "zz");
    assert!(
        grid.rows.iter().any(|row| row.contains("Complete.")),
        "the turn reply drew: {}",
        grid.contents
    );
    for code in ["extension_failed", "hook_failed", "command_conflict"] {
        assert!(
            !grid.contents.contains(code),
            "a {code} notice drew: {}",
            grid.contents
        );
    }
    quit(run, "zz");
}

#[test]
fn unbound_key_with_an_extension_behaves_as_without() {
    unbound_key(true);
}

#[test]
fn unbound_key_without_an_extension_behaves_as_without() {
    unbound_key(false);
}
