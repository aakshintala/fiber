//! Binary-level test of the Session card's cache countdown
//! (`docs/tui.md`, "The panel"): the real binary on a 160x48 pty in
//! truecolour, with the scripted provider answering one turn, draws the
//! "cache resets in" row figured once the turn's usage pins the expiry.
//! The test asserts presence only — a row that is neither the
//! pre-request dash nor "expired" — while the T2/T4 unit tests pin the
//! values and the format.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::json;
use support::pty::{Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// A setup whose scripted model answers one turn reporting input usage,
/// so the fold pins the cache expiry on the turn's first fold.
fn setup() -> Setup {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": ["Done."], "usage": {"input": 1200}},
        ]}),
    );
    let config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}});
    write_json(&setup.home().join("config.json"), &config);
    setup
}

/// The countdown row reads figured: present, and neither the
/// pre-request dash nor "expired" (the real clock runs, so presence,
/// not the minute value, is what the binary proves).
fn countdown_drawn(grid: &Grid) -> bool {
    grid.rows.iter().any(|row| {
        row.contains("cache resets in") && !row.contains('—') && !row.contains("expired")
    })
}

#[test]
fn the_session_card_counts_the_cache_reset_down() {
    let setup = setup();
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    let grid = run.wait_screen("the figured cache countdown", countdown_drawn);
    assert!(
        countdown_drawn(&grid),
        "a figured countdown in\n{}",
        grid.contents
    );
}
