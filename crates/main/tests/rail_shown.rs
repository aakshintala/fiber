//! Binary-level tests of the rail's persistence (`docs/tui.md`, "The
//! rail"): the real binary on a 160x48 pty with one live session. The
//! rail shows until the person hides it; ⌥R and `/rail` save the choice
//! as `tui.rail.shown`, so it holds across launches.

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
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::json;
use support::pty::Run;
use support::{HubProc, Setup, TRUECOLOUR, connect_hub, hello, write_json};

/// Alt+R as the kitty keyboard protocol encodes it
/// (`crates/tui/src/keys.rs`).
const ALT_R: &[u8] = b"\x1b[114;3u";

/// The rail's project header for the launch workspace: no other surface
/// starts a row with it.
const RAIL_HEADER: &str = "  w +";

/// Whether any row starts with the rail's project header.
fn rail_shown(rows: &[String]) -> bool {
    rows.iter().any(|row| row.starts_with(RAIL_HEADER))
}

/// The global configuration file's `tui.rail.shown`, if set.
fn rail_shown_saved(setup: &Setup) -> Option<bool> {
    let file = fs::read_to_string(setup.home().join("config.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&file).unwrap();
    config.get("tui")?.get("rail")?.get("shown")?.as_bool()
}

/// Quits the run's terminal and waits for it. The hub's last client
/// is gone before this runs, so the hub exits on its own and the wait
/// reaps it too.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Reaps the hub the run above left behind, exited on its own.
fn reap(hub: &Arc<Mutex<Option<HubProc>>>) {
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

#[test]
fn the_rail_choice_persists_across_launches() {
    let setup = Setup::new();
    // One reply per launch: each spawn runs one turn.
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    // The provider installer writes `{"model": "fake/m"}`; the hub
    // carries the test credential, as its serve command sets it. The
    // idle exit lets the hub leave on its own once the terminal quits,
    // as in the other look tests.
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    let hub = Arc::new(Mutex::new(None));
    // Each launch gets its own hub: the control connection keeps its
    // hub from idling out, so it stays open only while its terminal
    // runs and drops before the quit the wait reaps.
    let (watch, _) = connect_hub(&setup, &hub);

    // The rail shows beside the one session.
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    let grid = run.wait_screen("the reply and the rail", |grid| {
        grid.contents.contains("Hello.") && rail_shown(&grid.rows)
    });
    assert!(rail_shown(&grid.rows), "the rail shows by default");

    // ⌥R hides it.
    run.write(ALT_R);
    run.wait_screen("the hidden rail", |grid| {
        grid.contents.contains("Hello.") && !rail_shown(&grid.rows)
    });
    drop(watch);
    quit(run);
    // The process saved before exiting.
    assert_eq!(rail_shown_saved(&setup), Some(false));
    reap(&hub);

    // The next launch starts hidden, on a fresh hub.
    let (watch, _) = connect_hub(&setup, &hub);
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    let grid = run.wait_screen("the reply", |grid| grid.contents.contains("Hello."));
    assert!(
        !rail_shown(&grid.rows),
        "the rail stays hidden across launches"
    );

    // `/rail` shows it again.
    run.write(b"/rail\r");
    run.wait_screen("the shown rail", |grid| rail_shown(&grid.rows));
    drop(watch);
    quit(run);
    assert_eq!(rail_shown_saved(&setup), Some(true));
    reap(&hub);
}
