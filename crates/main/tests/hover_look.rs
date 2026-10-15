//! Binary-level test of hover and focus on tinted targets (`docs/tui.md`,
//! palette `hover` row; "Mouse and hover"): the real binary on a 160x48
//! pty in truecolour on the dark theme, with one live session. The
//! focused workspace chip draws the surface tint lifted 0x14 per channel
//! while the other chips keep the surface tint; the pointer over another
//! chip lifts it the same way; a session row with no tint under it draws
//! the flat hover colour.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{
    SessionGuard, Setup, TRUECOLOUR, connect_hub, hello, recv, start_session, write_json,
};

/// The dark theme's surface tint and the hover colour in truecolour
/// (`crates/tui/src/theme.rs`): the surface lifted 0x14 per channel.
const SURFACE: Colour = Colour::Rgb(0x1a, 0x1a, 0x22);
const LIFTED: Colour = Colour::Rgb(0x2e, 0x2e, 0x36);
const HOVER: Colour = Colour::Rgb(0x1e, 0x1e, 0x26);

/// The row's symbols, one per cell.
#[track_caller]
fn row_text(screen: &Grid, y: u16) -> String {
    (0..160).map(|x| screen.cell(x, y).symbol).collect()
}

/// The first column on `y` where `needle` starts, cell by cell.
#[track_caller]
fn find_col(screen: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
    (0..160)
        .find(|x| {
            want.iter().enumerate().all(|(dx, ch)| {
                let dx = u16::try_from(dx).expect("the run fits its row");
                screen.cell(x + dx, y).symbol == *ch
            })
        })
        .expect("the run on its row")
}

#[test]
fn focused_and_hovered_chips_lift_the_surface_tint() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    let hub = Arc::new(Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    let here = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &here);
    let control = support::Socket::connect(setup.deadline, &setup.hub_socket());
    assert_eq!(recv(&control, "hub_hello")["kind"], "hub_hello");
    start_session(&control, &here, "first");
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the turn runs"
    );
    // Home lists the live session; the entry bar has focus at launch.
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let screen = run.wait_screen("the home session row", |screen| {
        (0..48).any(|y| row_text(screen, y).contains("first"))
    });

    // The chip row carries the workspace chip, the model and thinking
    // chips, and the trailing hint. The workspace chip names the
    // launch directory.
    let chip = (0..48)
        .find(|y| row_text(&screen, *y).contains("Enter starts a session"))
        .expect("the chip row");
    let segment = setup
        .workspace()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .expect("the workspace has a name");
    let workspace = format!("[{segment}]");
    let ws = find_col(&screen, chip, &workspace);
    let wide = u16::try_from(workspace.len()).expect("the chip fits its row");
    // Unfocused, the workspace chip sits on the surface tint.
    for dx in 0..wide {
        assert_eq!(
            screen.cell(ws + dx, chip).bg,
            SURFACE,
            "unfocused chip cell ({}, {chip})",
            ws + dx
        );
    }

    // Down from the entry bar focuses the chip row on the workspace
    // chip. Either background proves the focus landed; the assertion
    // below pins which one.
    run.write(b"\x1b[B");
    let screen = run.wait_screen("the focused workspace chip", |screen| {
        let bg = screen.cell(ws, chip).bg;
        bg == LIFTED || bg == HOVER
    });
    // The focused chip draws the surface tint lifted 0x14 per channel.
    for dx in 0..wide {
        assert_eq!(
            screen.cell(ws + dx, chip).bg,
            LIFTED,
            "focused chip cell ({}, {chip})",
            ws + dx
        );
    }
    // Every other text cell of the chip row keeps the surface tint.
    for x in 0..160 {
        if screen.cell(x, chip).symbol.trim().is_empty() {
            continue;
        }
        if (ws..ws + wide).contains(&x) {
            continue;
        }
        assert_eq!(
            screen.cell(x, chip).bg,
            SURFACE,
            "unfocused chip-row cell ({x}, {chip})"
        );
    }

    // The pointer over the model chip lifts it the same way; the
    // focused workspace chip stays lifted.
    let row = row_text(&screen, chip);
    let brackets: Vec<u16> = row
        .chars()
        .enumerate()
        .filter(|(_, ch)| *ch == '[')
        .map(|(at, _)| u16::try_from(at).expect("the row fits its cells"))
        .collect();
    assert!(brackets.len() >= 2, "another chip in {row:?}");
    let mx = brackets[1];
    run.write(format!("\x1b[<35;{};{}M", mx + 1, chip + 1).as_bytes());
    let screen = run.wait_screen("the hovered model chip", |screen| {
        let bg = screen.cell(mx, chip).bg;
        bg == LIFTED || bg == HOVER
    });
    let end = row[mx as usize..]
        .find(']')
        .map(|at| mx + u16::try_from(at).expect("the chip fits its row") + 1)
        .expect("the model chip closes");
    for x in mx..end {
        assert_eq!(
            screen.cell(x, chip).bg,
            LIFTED,
            "hovered chip cell ({x}, {chip})"
        );
    }
    for dx in 0..wide {
        assert_eq!(
            screen.cell(ws + dx, chip).bg,
            LIFTED,
            "the focus stays while hovering"
        );
    }

    // A session row sits on no tint: the pointer over it draws the flat
    // hover colour.
    let srow = (0..48)
        .find(|y| row_text(&screen, *y).contains("first"))
        .expect("the session row");
    let sx = find_col(&screen, srow, "first");
    run.write(format!("\x1b[<35;{};{}M", sx + 1, srow + 1).as_bytes());
    let screen = run.wait_screen("the hovered session row", |screen| {
        screen.cell(sx, srow).bg == HOVER
    });
    for dx in 0..5 {
        assert_eq!(
            screen.cell(sx + dx, srow).bg,
            HOVER,
            "untinted row cell ({}, {srow})",
            sx + dx
        );
    }

    // Back to the entry bar, then out: the hub exits on its own once
    // the terminal quits.
    drop((watch, control));
    run.write(b"\x1b[A");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    drop(guard);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}
