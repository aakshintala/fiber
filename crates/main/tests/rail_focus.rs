//! Binary-level test of rail focus (`docs/tui.md`, "The rail"): the real
//! binary on a 160x48 pty with two live sessions. ← moves focus into the
//! rail and lifts the on-screen card, ↓ moves the lift, Enter switches to
//! the focused card, the second ← widens home with the row focused, and
//! the foot hint shows only while focus holds. A second run under
//! `NO_COLOR` checks the bold, undimmed box.

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

use fakes::ProviderServer;
use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{
    SessionGuard, Setup, Socket, TRUECOLOUR, close_session, connect_hub, hello, start_session,
    write_json,
};

/// The rail's project header for the launch workspace: no other surface
/// starts a row with it.
const RAIL_HEADER: &str = "  w +";

/// The foot hint's middle: the row reads the full hint while focused.
const HINT_MIDDLE: &str = "↑↓ move · Enter switch";

/// Home's counts row with both sessions live.
const HOME_COUNTS: &str = "2 working · $0.00 live";

/// Adds 0x14 to each channel of an `Rgb` colour: the hover lift the
/// paint pass lays over a focused card.
fn lifted(colour: &Colour) -> Colour {
    match colour {
        Colour::Rgb(red, green, blue) => Colour::Rgb(
            red.saturating_add(0x14),
            green.saturating_add(0x14),
            blue.saturating_add(0x14),
        ),
        Colour::Default | Colour::Indexed(_) => {
            panic!("an Rgb tint, not {colour:?}")
        }
    }
}

/// The first row containing the card of the session named `name`, if any.
fn card_row_at(grid: &Grid, name: &str) -> Option<usize> {
    grid.rows.iter().position(|row| row.contains(name))
}

/// The first row containing the card of the session named `name`.
fn card_row(grid: &Grid, name: &str) -> usize {
    card_row_at(grid, name).unwrap_or_else(|| panic!("a card for {name:?}:\n{}", grid.contents))
}

/// The bg of a cell inside the named card's text rows: the cards start at
/// the third column, and the tint covers the card's full width.
fn tint(grid: &Grid, name: &str) -> Colour {
    grid.cell(2, u16::try_from(card_row(grid, name)).unwrap())
        .bg
}

/// The header row of the second project ("v", the sibling workspace):
/// the group name is the workspace's last segment, so its header starts
/// with `"  v +"`.
fn plus_row(grid: &Grid) -> usize {
    grid.rows
        .iter()
        .position(|row| row.starts_with("  v +"))
        .unwrap_or_else(|| panic!("a second project header:\n{}", grid.contents))
}

/// The bg of the "+" cell on the second project's header row: focus
/// lifts that one cell, so its bg tells the "+" stop from a card.
fn plus_bg(grid: &Grid) -> Colour {
    let row = plus_row(grid);
    let found = grid.rows[row].chars().position(|cell| cell == '+').unwrap();
    grid.cell(u16::try_from(found).unwrap(), u16::try_from(row).unwrap())
        .bg
}

/// Whether the rail draws: a row starts with its project header.
fn rail_drawn(grid: &Grid) -> bool {
    grid.rows.iter().any(|row| row.starts_with(RAIL_HEADER))
}

/// The columns holding the card's own `│` sides on `row`: the first
/// two `│` cells, since the conversation can draw more further right.
fn box_sides(grid: &Grid, row: usize) -> Option<(u16, u16)> {
    let (cols, _) = grid.size();
    let y = u16::try_from(row).unwrap();
    let mut at = Vec::new();
    for x in 0..cols {
        if grid.cell(x, y).symbol == "│" {
            at.push(x);
        }
    }
    if at.len() >= 2 {
        Some((at[0], at[1]))
    } else {
        None
    }
}

/// Two live sessions, the first in the launch workspace and the second in
/// a sibling workspace, both held live: the setups stay in the test so the
/// teardown reads them.
fn start_two(setup: &Setup, control: &Socket) -> (String, String, SessionGuard, SessionGuard) {
    let launch_text = setup.workspace().to_string_lossy().into_owned();
    let other = setup.root.path().join("v");
    std::fs::create_dir_all(&other).unwrap();
    let other_text = other.to_string_lossy().into_owned();
    let launch_guard = SessionGuard::arm(setup.deadline, &launch_text);
    let other_guard = SessionGuard::arm(setup.deadline, &other_text);
    let first = start_session(control, &launch_text, "first live");
    let second = start_session(control, &other_text, "second live");
    (first, second, launch_guard, other_guard)
}

/// Quits the terminal and waits for it. The hub's last client is gone
/// before this runs, so the hub exits on its own and the wait reaps it
/// too.
fn finish_run(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Finishes both sessions before the terminal exits, so the hub's idle
/// deadline reaps it without leaving child processes behind.
fn close_two(
    setup: &Setup,
    server: &ProviderServer,
    first: &str,
    second: &str,
    launch_guard: SessionGuard,
    other_guard: SessionGuard,
) {
    server.release();
    for id in [first, second] {
        let socket = Socket::connect(setup.deadline, &setup.session_socket(id));
        close_session(&socket);
    }
    launch_guard.wait_gone();
    other_guard.wait_gone();
}

/// Reaps the hub the run above left behind, exited on its own.
fn reap(hub: &Arc<Mutex<Option<support::HubProc>>>) {
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

#[test]
fn rail_focus_lifts_moves_switches_and_widens_home() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    server.hold();
    let hub = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let (first, second, launch_guard, other_guard) = start_two(&setup, &control);
    let mut run = Run::spawn(&setup, 160, 48, &["resume", first.as_str()], &TRUECOLOUR);
    run.ready();
    // Both cards draw; the on-screen card's tint differs from the other's.
    let grid = run.wait_screen("the rail with both cards", |grid| {
        rail_drawn(grid)
            && grid.contents.contains("first live")
            && grid.contents.contains("second live")
    });
    let raised = tint(&grid, "first live");
    let flat = tint(&grid, "second live");
    assert_ne!(raised, flat, "the on-screen card's tint");

    // ← focuses the on-screen card: the hint shows and its bg lifts.
    run.write(b"\x1b[D");
    let grid = run.wait_screen("the hint and the lifted first card", |grid| {
        grid.contents.contains(HINT_MIDDLE)
            && tint(grid, "first live") == lifted(&raised)
            && tint(grid, "second live") == flat
    });
    let plus_flat = plus_bg(&grid);

    // ↓ moves the lift to the next group's "+" stop: the hint stays,
    // neither card lifts, and the "+" cell lifts.
    run.write(b"\x1b[B");
    run.wait_screen("the lifted second plus", |grid| {
        grid.contents.contains(HINT_MIDDLE)
            && tint(grid, "first live") == raised
            && tint(grid, "second live") == flat
              && plus_bg(grid) != plus_flat
    });

    // ↓ moves the lift to the second card.
    run.write(b"\x1b[B");
    run.wait_screen("the lifted second card", |grid| {
        tint(grid, "second live") == lifted(&flat) && tint(grid, "first live") == raised
    });

    // Enter switches to the focused card: its tint turns raised, the
    // first card's turns flat, and the hint goes with the focus.
    run.write(b"\r");
    run.wait_screen("switched to the second session", |grid| {
        !grid.contents.contains(HINT_MIDDLE)
            && tint(grid, "second live") == raised
            && tint(grid, "first live") == flat
    });

    // ← focuses the on-screen card again; ← widens home with its row.
    run.write(b"\x1b[D");
    run.wait_screen("the hint back on the second card", |grid| {
        grid.contents.contains(HINT_MIDDLE) && tint(grid, "second live") == lifted(&raised)
    });
    run.write(b"\x1b[D");
    run.wait_screen("home with both sessions live", |grid| {
        grid.contents.contains(HOME_COUNTS)
    });

    // Enter re-opens the focused row, the second session. Typing keeps
    // the box's focus: ← moves the cursor, X lands between a and b, and
    // with no rail focus there is no hint and no lifted card.
    run.write(b"\r");
    run.wait_screen("the second session resumed", |grid| {
        rail_drawn(grid) && tint(grid, "second live") == raised
    });
    run.write(b"ab");
    run.write(b"\x1b[D");
    run.write(b"X");
    // The block cursor covers the cell it sits on, so → moves it off
    // the b before the draft reads whole.
    run.write(b"\x1b[C");
    let grid = run.wait_screen("aXb in the box", |grid| grid.contents.contains("aXb"));
    assert!(
        !grid.contents.contains(HINT_MIDDLE),
        "no hint without focus"
    );
    for name in ["first live", "second live"] {
        let bg = tint(&grid, name);
        assert!(bg == raised || bg == flat, "no lifted card for {name}");
    }

    // ⌥R hides the rail; with the draft cleared, ← goes home.
    run.write(b"\x1br");
    run.wait_screen("the hidden rail", |grid| !rail_drawn(grid));
    run.write(b"\x03");
    run.write(b"\x1b[D");
    run.wait_screen("home again", |grid| grid.contents.contains(HOME_COUNTS));

    close_two(&setup, &server, &first, &second, launch_guard, other_guard);
    drop(control);
    finish_run(run);
    reap(&hub);
}

#[test]
fn rail_focus_bolds_the_focused_card_without_colour() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    server.hold();
    let hub = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let (first, second, launch_guard, other_guard) = start_two(&setup, &control);
    let env: Vec<(&str, &str)> = [("NO_COLOR", "1")].into_iter().chain(TRUECOLOUR).collect();
    let mut run = Run::spawn(&setup, 160, 48, &["resume", first.as_str()], &env);
    run.ready();
    run.wait_screen("the rail with both cards", |grid| {
        rail_drawn(grid)
            && grid.contents.contains("first live")
            && grid.contents.contains("second live")
    });
    // ← focuses the on-screen card: its │ sides go bold and lose their
    // dim, while the other card's sides stay dim and unbold.
    run.write(b"\x1b[D");
    run.wait_screen("the focused card's bold box", |grid| {
        let Some(focused_row) = card_row_at(grid, "first live") else {
            return false;
        };
        let Some(other_row) = card_row_at(grid, "second live") else {
            return false;
        };
        let Some((left, right)) = box_sides(grid, focused_row) else {
            return false;
        };
        let Some((oleft, oright)) = box_sides(grid, other_row) else {
            return false;
        };
        let y = u16::try_from(focused_row).unwrap();
        let oy = u16::try_from(other_row).unwrap();
        let side = grid.cell(left, y);
        let side_right = grid.cell(right, y);
        let other = grid.cell(oleft, oy);
        let other_right = grid.cell(oright, oy);
        side.bold
            && !side.dim
            && side_right.bold
            && !side_right.dim
            && other.dim
            && !other.bold
            && other_right.dim
            && !other_right.bold
    });
    // Back home before the teardown: closing a session only releases
    // its process once no terminal stays attached.
    run.write(b"\x1b[D");
    run.wait_screen("the cleared focus", |grid| {
        !grid.contents.contains(HINT_MIDDLE)
    });
    run.write(b"\x1b[D");
    run.wait_screen("home with both sessions live", |grid| {
        grid.contents.contains(HOME_COUNTS)
    });

    close_two(&setup, &server, &first, &second, launch_guard, other_guard);
    drop(control);
    finish_run(run);
    reap(&hub);
}
