//! The real terminal's extension conflict, session gate and error fallback.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
mod support;
use serde_json::{Value, json};
use support::pty::Run;
use support::{Setup, write_json};

fn install(setup: &Setup, name: &str, source: &str, range: Option<Value>) {
    let dir = setup.root.path().join(name);
    let mut manifest = json!({"name":name,"version":"v3.0.0","fiber":"0.0.0","api":1});
    if let Some(range) = range {
        manifest["tui"] = json!({"session_versions":range});
    }
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
fn source(text: &str) -> String {
    format!(
        "fiber.slot('ledger_row', {{timeout=100,render=function(input,width) return {{{{{{text='{text}'}}}}}} end}})"
    )
}
fn configure(setup: &Setup, winner: Option<&str>) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps":[{"tool_calls":[{"name":"shell","arguments":{"command":"echo builtin-marker"}}]},{"text":["Complete."]}]}),
    );
    std::fs::write(
        setup.home().join("rules"),
        "{\"decision\":\"allow\",\"tool\":\"shell\",\"prefix\":\"echo\"}\n",
    )
    .unwrap();
    let mut config = json!({"model":"scripted/s.json","hub":{"idle_exit_ms":200},"tui":{"rail":{"shown":false},"panel":{"cards":[]}}});
    if let Some(winner) = winner {
        config["tui"]["slots"] = json!({"ledger_row":winner});
    }
    write_json(&setup.home().join("config.json"), &config);
}
fn turn(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &[]);
    run.ready(); // Existing mode/first-frame signal, not a grid match.
    let from = run.output().len();
    run.write(b"run the tool\r");
    run.turn_finished(from);
    run.write(b"\x0f"); // Ctrl+O exposes the ledger rows under test.
    run
}
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    assert_eq!(run.wait().status.code(), Some(0));
}
#[test]
fn conflicting_rows_stay_builtin_and_configured_winner_draws() {
    for winner in [None, Some("alpha")] {
        let setup = Setup::new();
        configure(&setup, winner);
        install(&setup, "alpha", &source("ALPHA-ROW"), None);
        install(&setup, "beta", &source("BETA-ROW"), None);
        let mut run = turn(&setup);
        run.wait_screen("conflict fallback or configured winner", |grid| {
            if winner.is_some() {
                grid.contents.contains("ALPHA-ROW")
                    && !grid.contents.contains("BETA-ROW")
                    && !grid.contents.contains("command_conflict")
            } else {
                grid.contents.contains("builtin-marker")
                    && grid.contents.contains("command_conflict")
                    && grid.contents.contains("alpha, beta")
                    && !grid.contents.contains("ALPHA-ROW")
                    && !grid.contents.contains("BETA-ROW")
                    // docs/tui.md:2757: one command_conflict per slot and set of contenders.
                    && grid.contents.matches("command_conflict").count() == 1
            }
        });
        quit(run);
    }
}
#[test]
fn session_gate_event_keeps_builtin_and_names_both_versions_once() {
    let setup = Setup::new();
    configure(&setup, None);
    install(
        &setup,
        "gate",
        &source("INELIGIBLE-ROW"),
        Some(json!({"min":"v4.0.0"})),
    );
    let mut run = turn(&setup);
    run.wait_screen("incompatible session-half fallback", |grid| {
        grid.contents.contains("builtin-marker")
            && grid.contents.contains("extension_incompatible")
            && grid.contents.contains("TUI v3.0.0")
            && grid.contents.contains("session half v3.0.0")
            && !grid.contents.contains("INELIGIBLE-ROW")
            // docs/tui.md:2678: one extension_incompatible per extension and version.
            && grid.contents.matches("extension_incompatible").count() == 1
    });
    quit(run);
}
#[test]
fn specific_conflict_never_falls_through_to_a_valid_catch_all() {
    let setup = Setup::new();
    configure(&setup, None);
    let specific = source("SPECIFIC").replace("'ledger_row'", "'ledger_row:shell'");
    install(&setup, "alpha", &specific, None);
    install(&setup, "beta", &specific, None);
    install(&setup, "generic", &source("CATCHALL"), None);
    let mut run = turn(&setup);
    run.wait_screen("suppressed specific slot uses builtin", |grid| {
        grid.contents.contains("builtin-marker")
            && grid.contents.contains("command_conflict")
            && grid.contents.contains("ledger_row:shell")
            && !grid.contents.contains("CATCHALL")
            && !grid.contents.contains("SPECIFIC")
    });
    quit(run);
}
#[test]
fn incompatible_specific_slot_allows_catch_all_and_preserves_version_notice() {
    let setup = Setup::new();
    configure(&setup, None);
    let specific = source("SPECIFIC").replace("'ledger_row'", "'ledger_row:shell'");
    install(&setup, "specific", &specific, Some(json!({"min":"v9.0.0"})));
    install(&setup, "generic", &source("CATCHALL"), None);
    let mut run = turn(&setup);
    run.wait_screen("absent specific slot allows catchall", |grid| {
        grid.contents.contains("CATCHALL")
            && grid.contents.contains("extension_incompatible")
            && !grid.contents.contains("SPECIFIC")
            && !grid.contents.contains("command_conflict")
    });
    quit(run);
}
#[test]
fn timeout_handoff_never_reuses_the_disabled_winners_cached_row() {
    let setup = Setup::new();
    configure(&setup, Some("alpha"));
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps":[{"tool_calls":[{"name":"shell","arguments":{"command":"echo cached"}},{"name":"shell","arguments":{"command":"echo slow"}}]},{"text":["Complete."]}]}),
    );
    install(
        &setup,
        "alpha",
        "fiber.slot('ledger_row',{timeout=1,render=function(input) if input.arguments.command=='echo slow' then while true do end end return {{{text='ALPHA-CACHED'}}} end})",
        None,
    );
    install(&setup, "beta", &source("BETA-REPLACEMENT"), None);
    let mut run = turn(&setup);
    run.wait_screen("timeout hands cached X to surviving B", |grid| {
        // docs/tui.md:2740: the timeout switches the extension off, so both rows leave it for the surviving contender.
        grid.contents.matches("BETA-REPLACEMENT").count() == 2
            && !grid.contents.contains("ALPHA-CACHED")
            && grid.contents.contains("extension_failed")
            && !grid.contents.contains("hook_failed")
            && !grid.contents.contains("command_conflict")
    });
    quit(run);
}
#[test]
fn load_failure_is_already_visible_in_the_first_frame() {
    let setup = Setup::new();
    configure(&setup, None);
    install(&setup, "bad-init", "error('first-frame-fixture')", None);
    let mut run = Run::spawn(&setup, 160, 48, &[], &[]);
    let end = run.ready();
    let output = run.output();
    let mut parser = vt100::Parser::new(48, 160, 0);
    parser.process(&output[..end]);
    let first = parser.screen().contents();
    assert!(first.contains("extension_failed"), "{first}");
    assert!(first.contains("first-frame-fixture"), "{first}");
    // docs/tui.md:1233: an event notice reports its failure once.
    assert_eq!(first.matches("extension_failed").count(), 1);
    quit(run);
}
#[test]
fn renderer_timeout_switches_off_with_one_extension_notice() {
    let setup = Setup::new();
    configure(&setup, None);
    install(
        &setup,
        "slow",
        "fiber.slot('ledger_row',{timeout=1,render=function() while true do end end})",
        None,
    );
    let mut run = turn(&setup);
    run.wait_screen("timeout fallback", |grid| {
        grid.contents.contains("builtin-marker")
            && grid.contents.contains("extension_failed")
            && grid.contents.contains("slow")
            && !grid.contents.contains("hook_failed")
            // docs/tui.md:2740: a timeout switches the extension off with one notice.
            && grid.contents.matches("extension_failed").count() == 1
    });
    quit(run);
}
#[test]
fn configured_malformed_contender_is_named_in_the_hook_notice() {
    let setup = Setup::new();
    configure(&setup, Some("alpha"));
    install(
        &setup,
        "alpha",
        "fiber.slot('ledger_row',{timeout=100,render=function() return {{{text='bad',fg='not-a-role'}}} end})",
        None,
    );
    install(&setup, "beta", &source("BETA-ROW"), None);
    let mut run = turn(&setup);
    run.wait_screen("selected malformed span falls back", |grid| {
        grid.contents.contains("builtin-marker")
            && grid.contents.contains("hook_failed")
            && grid
                .contents
                .contains("extension alpha could not draw ledger_row")
            && !grid.contents.contains("TUI extensions")
            && !grid.contents.contains("BETA-ROW")
            // docs/tui.md:2734: one hook_failed per extension and slot.
            && grid.contents.matches("hook_failed").count() == 1
    });
    quit(run);
}
#[test]
fn reload_rebuilds_a_timed_out_vm_from_changed_source() {
    let setup = Setup::new();
    configure(&setup, None);
    install(
        &setup,
        "slow",
        "fiber.slot('ledger_row',{timeout=1,render=function() while true do end end})",
        None,
    );
    let mut run = turn(&setup);
    run.wait_screen("disabled VM notice", |grid| {
        grid.contents.contains("extension_failed") && grid.contents.contains("builtin-marker")
    });
    std::fs::write(
        config::extensions_dir(&setup.home()).join("slow/tui/init.lua"),
        source("REBUILT-AFTER-TIMEOUT"),
    )
    .unwrap();
    run.write(b"/reload\r");
    run.wait_screen("rebuilt renderer after slash reload", |grid| {
        grid.contents.contains("REBUILT-AFTER-TIMEOUT")
    });
    quit(run);
}
#[test]
fn renderer_error_draws_builtin_and_one_hook_notice() {
    let setup = Setup::new();
    configure(&setup, None);
    install(
        &setup,
        "broken",
        "fiber.slot('ledger_row',{timeout=100,render=function() error('render-fixture') end})",
        None,
    );
    let mut run = turn(&setup);
    run.wait_screen("renderer error fallback", |grid| {
        grid.contents.contains("builtin-marker")
            && grid.contents.contains("hook_failed")
            && grid.contents.contains("broken")
            // docs/tui.md:2734: one hook_failed per extension and slot.
            && grid.contents.matches("hook_failed").count() == 1
    });
    quit(run);
}
