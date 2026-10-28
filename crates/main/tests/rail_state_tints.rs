//! Binary-level test that rail cards tint by state (`docs/tui.md`, "The
//! rail"): two live sessions in different states draw different card
//! backgrounds. At base every card draws on `surface`, so this fails
//! there: both backgrounds resolve to the same 256-colour entry.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Response};
use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{
    SessionGuard, Setup, Socket, close_session, connect_hub, hello, start_session, write_json,
};

/// The dark theme's `surface` at 256 colours: the tint every card draws
/// on at base (`crates/tui/src/theme.rs`, `crates/tui/src/look.rs`).
const SURFACE_256: Colour = Colour::Indexed(234);

/// The dark theme's retrying card at 256 colours: the tint a RETRYING
/// card draws on once cards tint by state.
const RETRYING_256: Colour = Colour::Indexed(16);

/// The background of the named card's row, `None` when the grid holds no
/// such card. The rail starts at the screen's left, and the card's tint
/// covers its text rows, so the third column sits inside the card.
fn card_bg(grid: &Grid, name: &str) -> Option<Colour> {
    grid.rows
        .iter()
        .position(|row| row.contains(name))
        .map(|row| grid.cell(2, u16::try_from(row).unwrap()).bg)
}

/// The background of the named card's row.
fn tint(grid: &Grid, name: &str) -> Colour {
    card_bg(grid, name).unwrap_or_else(|| panic!("a card for {name:?}:\n{}", grid.contents))
}

/// Quits the terminal and waits for it.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn rail_cards_tint_by_state() {
    let setup = Setup::new();
    // The first model call fails, the rest answer: the failure below
    // belongs to the retrying session, the held answers to the working
    // session and to the terminal's own turn, in start order.
    let server = ProviderServer::start([Response::status(503, "{}"), hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m",
            "retry": {"attempts": 5, "initial_delay_ms": 300000, "max_delay_ms": 300000},
            "hub": {"idle_exit_ms": 1000}}),
    );
    server.hold();
    let hub = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let launch_text = setup.workspace().to_string_lossy().into_owned();
    let other = setup.root.path().join("v");
    std::fs::create_dir_all(&other).unwrap();
    let other_text = other.to_string_lossy().into_owned();
    let launch_guard = SessionGuard::arm(setup.deadline, &launch_text);
    let other_guard = SessionGuard::arm(setup.deadline, &other_text);
    // The retrying card: its first model call fails, and the long backoff
    // holds it in RETRYING while the test reads the rail.
    let retrying = start_session(&control, &other_text, "retry work");
    assert!(
        server.await_requests(1, setup.deadline),
        "the retrying turn runs"
    );
    server.release_one();
    // The working card: its model call stays held, so its turn streams.
    let working = start_session(&control, &launch_text, "steady work");
    assert!(server.await_requests(2, setup.deadline), "both turns run");
    // No truecolour: the grid carries the dark theme's 256-colour
    // entries, so the assertion reads the palette indices directly.
    // The terminal's own turn puts its session on screen, leaving the
    // working and retrying cards unfocused.
    let mut run = Run::spawn(&setup, 160, 48, &[], &[]);
    run.ready();
    run.write(b"go\r");
    assert!(
        server.await_requests(3, setup.deadline),
        "all three turns run"
    );
    let grid = run.wait_screen("a working and a retrying card", |grid| {
        grid.contents.contains("steady work")
            && grid.contents.contains("retry work")
            && grid.contents.contains("WORKING")
            && grid.contents.contains("RETRYING")
    });
    let steady = tint(&grid, "steady work");
    let retry = tint(&grid, "retry work");
    assert_eq!(steady, SURFACE_256, "the working card's tint");
    assert_eq!(retry, RETRYING_256, "the retrying card's tint");
    assert_ne!(
        steady, retry,
        "cards in different states draw different tints"
    );

    // The hub exits on its own once the terminal quits: release the held
    // turn, close both sessions, then quit and reap the hub.
    server.release();
    for id in [&retrying, &working] {
        close_session(&Socket::connect(setup.deadline, &setup.session_socket(id)));
    }
    launch_guard.wait_gone();
    other_guard.wait_gone();
    drop(control);
    quit(run);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}
