//! Clicks reach a TUI extension's `on_click`: the real binary on a
//! pseudo-terminal, a `group_line` replacement carrying two click ids,
//! and a handler that asserts its input and returns true for one id and
//! false for the other. A frame-locked click on the handled id leaves the
//! still-drawn group collapsed; the unhandled id opens its built-in
//! content. Keyboard navigation focuses each stop in turn, and Enter
//! follows the same route.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
use crate::support;

use serde_json::json;
use support::pty::{Grid, Run};
use support::{Setup, write_json};

/// Installs a TUI extension whose `group_line` replacement carries two
/// click ids. The handler asserts it sees a session id and one of the two
/// ids, then returns true for `yes` and false for `no`.
fn install(setup: &Setup) {
    let dir = setup.root.path().join("clicks");
    let manifest = json!({"name":"clicks","version":"v1.0.0","fiber":"0.0.0","api":1});
    write_json(&dir.join("extension.json"), &manifest);
    std::fs::create_dir_all(dir.join("tui")).unwrap();
    std::fs::write(dir.join("init.lua"), "").unwrap();
    std::fs::write(
        dir.join("tui/init.lua"),
        "fiber.slot('group_line', { timeout = 100, \
         render = function(input, width) \
           return { { {text='CLICKYES', click='yes'}, {text=' or '}, \
                       {text='CLICKNO', click='no'} } } \
         end, \
         on_click = { timeout = 100, run = function(input) \
           assert(input.click == 'yes' or input.click == 'no', 'unexpected click id'); \
           assert(type(input.session_id) == 'string' and #input.session_id > 0, \
                  'a session id'); \
           if input.click == 'yes' then return true else return false end \
         end } })",
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
/// one shell call, so one group whose summary the replacement covers.
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
    run.ready();
    let from = run.output().len();
    run.write(b"run the tool\r");
    run.turn_finished(from);
    run
}

/// The first cell of `needle` on the grid.
fn locate(grid: &Grid, needle: &str) -> Option<(u16, u16)> {
    grid.rows.iter().enumerate().find_map(|(y, row)| {
        row.find(needle)
            .map(|x| (u16::try_from(x).unwrap(), u16::try_from(y).unwrap()))
    })
}

/// Waits for the replacement row, so the click below acts on the drawn
/// stop rather than racing it.
fn replaced(run: &mut Run) {
    run.wait_screen("the click replacement", |grid| {
        grid.rows.iter().any(|row| row.contains("CLICKYES"))
    });
}

/// Types `marker` and waits for it: once the marker draws, every earlier
/// byte was read, so the click or keys above took effect.
fn marked(run: &mut Run, marker: &str) -> Grid {
    run.write(marker.as_bytes());
    run.wait_screen("the typed marker", |grid| {
        grid.rows.iter().any(|row| row.contains(marker))
    })
}

/// No extension failure drew: a handler that errored or a refused load
/// would name itself on screen.
fn assert_no_failures(grid: &Grid) {
    for code in ["extension_failed", "hook_failed", "command_conflict"] {
        assert!(
            !grid.contents.contains(code),
            "a {code} notice drew: {}",
            grid.contents
        );
    }
}

fn quit(mut run: Run, marker: &str) {
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
fn click_and_navigate_reach_handler() {
    let setup = Setup::new();
    configure(&setup);
    install(&setup);
    let mut run = turn(&setup);
    replaced(&mut run);
    // A frame-locked click on the handled id: the handler asserts its
    // session and click id, returns true, and the still-drawn group stays
    // collapsed. Without dispatch the built-in fallback would open it.
    // The draft stays empty throughout, so a missed Enter is a no-op
    // rather than a submitted turn.
    run.click_where("the handled stop", |grid| locate(grid, "CLICKYES"));
    // The marker proves the click took effect before these asserts run.
    let grid = marked(&mut run, "zz");
    assert!(
        grid.rows.iter().any(|row| row.contains("CLICKYES")),
        "the handled row still draws: {}",
        grid.contents
    );
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c")),
        "the handled click opened the group: {}",
        grid.contents
    );
    assert_no_failures(&grid);
    // Clear the draft: the draft stays empty below, so a missed Enter is
    // a no-op rather than a submitted turn.
    run.write(b"\x03");
    run.wait_screen("cleared draft", |grid| {
        !grid.rows.iter().any(|row| row.contains("zz"))
    });
    // BackTab lands on the collapsed summary, stepping down reaches its
    // stops left to right: the handled stop, then the declined one.
    // Enter on the handled stop routes through the handler again, still
    // suppressed; stepping on to the declined stop, Enter falls back to
    // the toggle and opens the group. The open proves both Enters ran in
    // order, and it proves the first was handled: a fallback then would
    // have opened the group early and the second Enter would have closed
    // it again. Extension stops carry no focused tint as links do, so
    // the routed effects prove each stop was focused.
    run.write(b"\x1b[Z\x1b[B\r\x1b[B\r");
    let grid = run.wait_screen("the opened group", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c"))
    });
    assert_no_failures(&grid);
    quit(run, "zz");
}

#[test]
fn unhandled_click_opens_builtin_row() {
    // A click the handler declines falls back to the built-in toggle:
    // the group opens on its real content.
    let setup = Setup::new();
    configure(&setup);
    install(&setup);
    let mut run = turn(&setup);
    replaced(&mut run);
    run.click_where("the unhandled stop", |grid| locate(grid, "CLICKNO"));
    let grid = marked(&mut run, "zz");
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c")),
        "the declined click opened the group: {}",
        grid.contents
    );
    assert_no_failures(&grid);
    quit(run, "zz");
    // A fresh run: BackTab lands on the collapsed summary, two steps
    // down on the declined stop, and Enter follows the same route,
    // opening the group on the fallback toggle.
    let setup = Setup::new();
    configure(&setup);
    install(&setup);
    let mut run = turn(&setup);
    replaced(&mut run);
    run.write(b"\x1b[Z\x1b[B\x1b[B");
    run.write(b"\r");
    // Opening is its own proof: the wait only passes once Enter's
    // fallback toggle drew the built-in content.
    let grid = run.wait_screen("the opened group", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c"))
    });
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("ledger-probe-9d2c")),
        "Enter on the declined stop opened the group: {}",
        grid.contents
    );
    assert_no_failures(&grid);
    quit(run, "zz");
}
