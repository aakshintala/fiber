//! Binary-level test of `/doctor` (`docs/tui.md`, "Swapped views"): the
//! real binary on a pty shows the report `fiber doctor` prints for the
//! same home, from the same capture, and both Esc and the top-row click
//! return to the prior screen. Every run carries a wall-clock deadline,
//! and every child runs in its own process group under a watchdog.

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
use support::pty::Run;
use support::{Setup, connect_hub, run_to_exit};

/// Spawns bare `fiber` at home with the fake key the headless door also
/// reads, and waits for the first frame, so typed input is never too
/// early for its receiver. The screen is wide enough to hold the
/// report's longest line whole: a cut row would differ from the
/// headless text without the report differing.
fn home_run(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 1000, 48, &[], &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    run.ready();
    run
}

/// Whether any row holds `needle`.
fn shows(grid: &support::pty::Grid, needle: &str) -> bool {
    grid.rows.iter().any(|row| row.contains(needle))
}

/// The Doctor view's top row: its title and command, drawn once the
/// swapped view takes the screen.
fn doctor_open(grid: &support::pty::Grid) -> bool {
    shows(grid, "  /doctor")
}

/// Clicks the cell at `(x, y)`: an SGR press then its release on the
/// same cell.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// Esc as the kitty keyboard protocol encodes it.
const ESC: &[u8] = b"\x1b[27u";

/// Quits the run's terminal and waits for it.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn doctor_view_matches_headless_doctor_output() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}}),
    );
    // The hub both captures read: started before the terminal so the
    // pty and the headless run agree it is running. It idles out once
    // the terminal quits, so the exit wait ends.
    let hub = Arc::new(Mutex::new(None));
    let (watch, _) = connect_hub(&setup, &hub);
    let mut run = home_run(&setup);
    // Home, not the model picker: the seeded model is configured.
    run.wait_screen("home with its chips", |grid| shows(grid, "[fake/m]"));
    // Type `/doctor`: the view opens holding the capture.
    run.write(b"/doctor\r");
    let view = run.wait_screen("the doctor view", doctor_open);
    // Every visible body line below the top row, trailing blanks cut:
    // the rows the frame drew, in order.
    let top = view
        .rows
        .iter()
        .position(|row| row.contains("  /doctor"))
        .expect("the doctor top row");
    let mut body: Vec<String> = view.rows[top + 1..]
        .iter()
        .map(|row| row.trim_end().to_owned())
        .collect();
    while body.last().is_some_and(|row| row.is_empty()) {
        body.pop();
    }
    assert!(!body.is_empty(), "the view shows the report lines");
    // The same home's headless report, while the pty and its hub stay
    // up with no session started: the capture reads the same hub, the
    // same key and the same files, so the lines match in full.
    let mut doctor = setup.fiber(&["doctor"]);
    doctor.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "headless doctor", doctor);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let headless: Vec<String> = String::from_utf8(output.stdout)
        .expect("doctor prints UTF-8")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        body, headless,
        "every visible report line equals the headless output in order"
    );
    assert!(
        server.requests().is_empty(),
        "neither capture runs a provider request"
    );
    // A click on the top row returns to the prior screen: row 0 is
    // final, the matched frame drew it.
    click(&mut run, 0, 0);
    run.wait_screen("home again after the click", |grid| {
        !doctor_open(grid) && shows(grid, "[fake/m]")
    });
    // Re-open, and Esc returns to the prior screen too.
    run.write(b"/doctor\r");
    run.wait_screen("the doctor view again", doctor_open);
    run.write(ESC);
    run.wait_screen("home again after Esc", |grid| {
        !doctor_open(grid) && shows(grid, "[fake/m]")
    });
    drop(watch);
    quit(run);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}
