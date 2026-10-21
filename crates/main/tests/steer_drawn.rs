//! A steering message draws its queue row while it waits and its card
//! rule once it lands (`docs/tui.md`, "Steering" and "Turns"): the real
//! binary on a pseudo-terminal, a prompt starting a turn on a held fake
//! reply, Enter sending a steer mid-turn, the queue row on the grid while
//! the reply stays held, then the `steer · HH:MM` rule in the turn's card
//! after the release lets the turn land.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;
use std::time::Duration;

use fakes::ProviderServer;
use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Deadline, TRUECOLOUR, function_call, hello, stream, write_json};

/// `attention` in the dark theme.
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
/// `rule` in the dark theme.
const RULE: Colour = Colour::Rgb(58, 58, 74);

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, makes `fake/m` the configured model, and idles the hub
/// out a second after its last client leaves, so no hub lingers.
fn provider(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    let model = json!({"id": "m", "protocol": "openai-responses",
        "base_url": format!("{}/v1", server.url()), "context_window": 100000});
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [model]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
}

/// Spawns `fiber` on a 120 by 32 terminal through the shared driver, with
/// truecolour after the fake key in the child's environment, so the grid
/// cells carry the theme's true colours.
fn terminal(setup: &support::Setup) -> Run {
    let mut env = vec![("FIBER_TEST_FAKE_KEY", "sk-test")];
    env.extend(TRUECOLOUR);
    Run::spawn(setup, 120, 32, &[], &env)
}

/// A reply that calls the shell with `echo hi`.
fn calls_echo_hi() -> fakes::Response {
    stream(&[function_call(
        "call_1",
        "shell",
        &json!({"command": "echo hi"}),
    )])
}

/// Whether the grid shows the queued steer: the queue row's mark beside
/// the steered text.
fn queue_row(grid: &Grid) -> bool {
    grid.rows
        .iter()
        .any(|row| row.contains("↳") && row.contains("use x"))
}

/// The first row holding `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let (cols, _) = grid.size();
    let want: Vec<char> = needle.chars().collect();
    for x in 0..cols {
        let mut ok = !want.is_empty();
        for (dx, want) in want.iter().enumerate() {
            let xi = x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
            if xi >= cols || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok {
            return x;
        }
    }
    panic!("{needle:?} on row {y}")
}

#[test]
fn a_steer_draws_its_queue_row_then_its_card_rule() {
    let setup = support::Setup::new();
    // The first reply runs a short shell call the standing rule allows
    // without asking. The reply is held until the test sees the steer on
    // the grid, so the steer is queued before the first step ends: the
    // hold forces the order, and the steer lands at the step boundary,
    // inside the turn.
    let server = ProviderServer::start([calls_echo_hi(), hello()]).unwrap();
    server.hold();
    provider(&setup, &server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "echo hi"})
        ),
    )
    .unwrap();
    let mut run = terminal(&setup);
    // The first frame draws the input line; its title proves the input
    // reader runs before the prompt goes out.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The loop pushes kitty's flags only after it processes the harness's
    // reply, so this also keeps the responder's pty write ahead of input.
    run.wait_bytes(0, support::pty::KITTY_PUSH, "kitty keyboard flags pushed");
    let from = run.output().len();
    run.write(b"go\r");
    // The first request in flight on the held reply proves the turn runs.
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    // The working line proves the turn is busy, so Enter steers.
    run.wait_screen("the running turn", |grid| {
        grid.contents.contains("Esc to interrupt")
    });
    // Enter during the turn sends the steer; the draft clears.
    run.write(b"use x\r");
    // The held reply leaves on every path: the explicit release below
    // frees it once the poll ends, and the guard frees it if the poll
    // fails first, so a failure never strands the held reply. Releasing
    // twice is harmless.
    struct ReleaseHold<'a> {
        server: &'a ProviderServer,
    }
    impl Drop for ReleaseHold<'_> {
        fn drop(&mut self) {
            self.server.release();
        }
    }
    let _hold = ReleaseHold { server: &server };
    // The queue row while the steer waits: a fixed bound well under the
    // run deadline, recording a bool and never asserting inside the poll,
    // so the held reply is released on every path below before any
    // assertion, and a red run fails on the assertion, never hanging.
    // The bound is one Deadline whose remainder ends the poll; each pause
    // is a short bounded receive on a live but empty channel, never a
    // sleep, and a poll pause is not a deadline (`docs/testing.md`,
    // "Waits and timeouts").
    let bound = Deadline::after(Duration::from_secs(10));
    let (_signal, paused) = std::sync::mpsc::channel::<()>();
    let mut saw_queue = false;
    while !bound.left().is_zero() {
        if queue_row(&run.screen()) {
            saw_queue = true;
            break;
        }
        assert!(
            Deadline::after(Duration::from_millis(50))
                .recv(&paused)
                .is_err(),
            "the pause elapses with no signal"
        );
    }
    server.release();
    // The release lets the turn land: the step boundary applies the steer
    // and the second request answers.
    run.turn_finished(from);
    assert!(
        saw_queue,
        "the steering queue row while the steer waited (↳ use x)"
    );
    // The card rule where the steer landed: the label bold in attention,
    // the dim separator, the dashes in rule to the edge, and the steered
    // text bold directly under it. The time is never read: only the
    // separator's dim span between the label and the dashes.
    let grid = run.screen();
    let y = row_with(&grid, "steer");
    let x = col_at(&grid, y, "steer");
    for dx in 0..5u16 {
        let cell = grid.cell(x + dx, y);
        assert_eq!(cell.fg, ATTENTION, "rule label col {dx}");
        assert!(cell.bold, "rule label col {dx}");
    }
    let (cols, _) = grid.size();
    let mut dashes = x + 5;
    while dashes < cols && grid.cell(dashes, y).symbol != "─" {
        dashes += 1;
    }
    assert!(
        dashes < cols,
        "the rule's dashes on row {y} in\n{}",
        grid.contents
    );
    for dx in 5..(dashes - x) {
        assert!(grid.cell(x + dx, y).dim, "rule separator col {dx}");
    }
    // The rule fills the card's width past one space, not the terminal:
    // every dash carries the rule tint, and nothing but spaces follows.
    let mut dx = dashes;
    while dx < cols && grid.cell(dx, y).symbol == "\u{2500}" {
        assert_eq!(grid.cell(dx, y).fg, RULE, "rule dash col {dx}");
        dx += 1;
    }
    let text = row_with(&grid, "use x");
    assert_eq!(text, y + 1, "the steered text under its rule");
    let tx = col_at(&grid, text, "use x");
    for dx in 0..5u16 {
        assert!(grid.cell(tx + dx, text).bold, "steered text col {dx}");
    }
    // Quit: the terminal is restored, with one resume line per live
    // session on the primary screen ("On exit").
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}
