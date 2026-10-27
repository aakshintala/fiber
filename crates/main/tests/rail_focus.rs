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

/// A `recent.jsonl` exited row for the launch workspace.
fn exited_row(
    id: &str,
    name: &str,
    workspace: &std::path::Path,
    project: &str,
    ts: u64,
) -> serde_json::Value {
    let workspace = std::fs::canonicalize(workspace)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    serde_json::json!({
        "session_id": id,
        "ts": ts,
        "project": project,
        "workspace": workspace,
        "name": name,
        "how": "exited",
    })
}

/// Seeds two exited rows for the launch workspace and keeps their
/// session directories, as the hub's recent reader requires.
fn seed_exited_rows(setup: &Setup, workspace: &std::path::Path) {
    let project = log::project_key(&doors::project(workspace));
    let sessions = log::sessions_dir(&setup.home(), &doors::project(workspace));
    std::fs::create_dir_all(&sessions).unwrap();
    let mut lines = Vec::new();
    for (index, name) in [(1u64, "seeded exit one"), (2, "seeded exit two")] {
        let id = format!("s_{index:016x}");
        std::fs::create_dir_all(sessions.join(&id)).unwrap();
        lines.push(exited_row(&id, name, workspace, &project, index * 1_000).to_string());
    }
    std::fs::write(
        setup.home().join("recent.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
}

/// The foot hint's middle: the row reads the full hint while focused.
const HINT_MIDDLE: &str = "↑↓ move · Enter switch";

/// Home's counts row with both sessions live.
const HOME_COUNTS: &str = "2 working · $0.00 live";

/// Adds 0x14 to each channel of an `Rgb` colour: the hover lift the
/// paint pass lays over a focused card.
fn lifted(colour: &Colour) -> Colour {
    try_lift(colour).unwrap_or_else(|| panic!("an Rgb tint, not {colour:?}"))
}

/// [`lifted`] without the panic: `None` for a colour that is not `Rgb`.
fn try_lift(colour: &Colour) -> Option<Colour> {
    match colour {
        Colour::Rgb(red, green, blue) => Some(Colour::Rgb(
            red.saturating_add(0x14),
            green.saturating_add(0x14),
            blue.saturating_add(0x14),
        )),
        Colour::Default | Colour::Indexed(_) => None,
    }
}

/// The first row containing the card of the session named `name`, if any.
fn card_row_at(grid: &Grid, name: &str) -> Option<usize> {
    grid.rows.iter().position(|row| row.contains(name))
}

/// The background of the named card's third-column cell, `None` when the
/// grid holds no such card.
fn card_bg(grid: &Grid, name: &str) -> Option<Colour> {
    card_row_at(grid, name).map(|row| grid.cell(2, u16::try_from(row).unwrap()).bg)
}

/// The background of the first card's third-column cell, `None` when the
/// grid holds no such card.
fn first_card_bg(grid: &Grid) -> Option<Colour> {
    card_bg(grid, "first live")
}

/// Which of the three hint-and-lift conditions holds on `grid`: the foot
/// hint, the first card's lift, the second card's flat tint. A missing
/// card, or a `raised` that is not `Rgb`, counts as false, so the report
/// names it instead of panicking.
fn hint_lift_checks(grid: &Grid, raised: &Colour, flat: &Colour) -> (bool, bool, bool) {
    let hint = grid.contents.contains(HINT_MIDDLE);
    let first =
        first_card_bg(grid).is_some_and(|bg| try_lift(raised).is_some_and(|lift| bg == lift));
    let second = card_bg(grid, "second live").is_some_and(|bg| bg == *flat);
    (hint, first, second)
}

/// Waits for the hint and the lifted first card, reporting which of the
/// three conditions was false in the last checked grid when the wait
/// expires, with the first card's actual background (`none` when the card
/// is absent), `raised` and `lifted(raised)`. It keeps `wait_screen`'s
/// deadline and fast-fail path; the report leads the one failure panic, so
/// it comes first, then the wait's name, then the screen dump.
#[track_caller]
fn wait_hint_lifted(run: &mut Run, raised: &Colour, flat: &Colour) -> Grid {
    let last = std::cell::Cell::new((false, false, false));
    let first_bg = std::cell::Cell::new(None);
    run.wait_screen_leading(
        "the hint and the lifted first card",
        |grid| {
            first_bg.set(first_card_bg(grid));
            let checks = hint_lift_checks(grid, raised, flat);
            last.set(checks);
            checks.0 && checks.1 && checks.2
        },
        || {
            let (hint, first, second) = last.get();
            let card = first_bg
                .get()
                .map_or_else(|| "none".to_owned(), |bg| format!("{bg:?}"));
            let lift = try_lift(raised).map_or_else(
                || "none (raised is not Rgb)".to_owned(),
                |lift| format!("{lift:?}"),
            );
            format!(
                "last check: hint present={hint}, first card lifted={first}, second card flat={second}, first card bg={card}, raised={raised:?}, lifted(raised)={lift}; "
            )
        },
    )
}

/// The bg of a cell inside the named card's text rows: the cards start at
/// the third column, and the tint covers the card's full width.
fn tint(grid: &Grid, name: &str) -> Colour {
    card_bg(grid, name).unwrap_or_else(|| panic!("a card for {name:?}:\n{}", grid.contents))
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

/// The bg of the "✓" cell on the second project's header row: focus
/// lifts that one cell, so its bg tells the "N ✓" stop from "+" and
/// the cards.
fn done_bg(grid: &Grid) -> Colour {
    let row = plus_row(grid);
    let found = grid.rows[row].chars().position(|cell| cell == '✓').unwrap();
    grid.cell(u16::try_from(found).unwrap(), u16::try_from(row).unwrap())
        .bg
}

/// Whether both sampled card tints differ from the rail region's
/// background: a card cell still showing the rail slab is not ready,
/// even when the rail header and both card names already draw.
fn cards_ready(header_bg: Colour, first: Colour, second: Colour) -> bool {
    first != header_bg && second != header_bg
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
    // Both cards draw with tints of their own: each card's tint differs
    // from the rail region's background, read from the header row's
    // third-column cell on the same grid, so an un-lifted frame whose
    // card cell still shows the rail slab cannot pass as ready.
    let grid = run.wait_screen("the rail with both cards", |grid| {
        if !(grid.contents.contains("first live") && grid.contents.contains("second live")) {
            return false;
        }
        let Some(header_row) = grid
            .rows
            .iter()
            .position(|row| row.starts_with(RAIL_HEADER))
        else {
            return false;
        };
        let header_bg = grid.cell(2, u16::try_from(header_row).unwrap()).bg;
        let (Some(first_bg), Some(second_bg)) =
            (card_bg(grid, "first live"), card_bg(grid, "second live"))
        else {
            return false;
        };
        cards_ready(header_bg, first_bg, second_bg)
    });
    let raised = tint(&grid, "first live");
    let flat = tint(&grid, "second live");
    assert_ne!(raised, flat, "the on-screen card's tint");

    // ← focuses the on-screen card: the hint shows and its bg lifts.
    run.write(b"\x1b[D");
    let grid = wait_hint_lifted(&mut run, &raised, &flat);
    let plus_flat = plus_bg(&grid);
    let done_flat = done_bg(&grid);

    // ↓ moves the lift to the next group's "+" stop: the hint stays,
    // neither card lifts, and the "+" cell lifts.
    run.write(b"\x1b[B");
    run.wait_screen("the lifted second plus", |grid| {
        grid.contents.contains(HINT_MIDDLE)
            && tint(grid, "first live") == raised
            && tint(grid, "second live") == flat
            && plus_bg(grid) != plus_flat
    });

    // ↓ moves the lift to the group's "0 ✓" stop: the hint stays,
    // neither card lifts, and the "✓" cell lifts.
    run.write(b"\x1b[B");
    run.wait_screen("the lifted zero count", |grid| {
        grid.contents.contains(HINT_MIDDLE)
            && tint(grid, "first live") == raised
            && tint(grid, "second live") == flat
            && plus_bg(grid) == plus_flat
            && done_bg(grid) != done_flat
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
    // ← ← widened home with the second session's row: the foot names
    // the back key.
    let home = run.wait_screen("home with both sessions live", |grid| {
        grid.contents.contains(HOME_COUNTS)
    });
    assert!(
        home.rows
            .iter()
            .any(|row| row.contains("→ or Esc back to the session")),
        "the back hint on home:\n{}",
        home.contents
    );

    // → goes back to the second session; ← ← widens home again, so
    // the Enter step below is unchanged.
    run.write(b"\x1b[C");
    run.wait_screen("the second session back on screen", |grid| {
        rail_drawn(grid) && tint(grid, "second live") == raised
    });
    run.write(b"\x1b[D");
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

/// The rail header draws the hub's total for the seeded exits: the
/// launch project reads "2 ✓" while the other project reads "0 ✓".
/// Opening the launch project's `Done` stop lands home on the first
/// exited row.
#[test]
fn rail_header_draws_the_hub_total_for_seeded_exits() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    server.hold();
    let launch = setup.workspace();
    seed_exited_rows(&setup, &launch);
    let hub = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let (first, second, launch_guard, other_guard) = start_two(&setup, &control);
    let mut run = Run::spawn(&setup, 160, 48, &["resume", first.as_str()], &TRUECOLOUR);
    run.ready();
    run.wait_screen("the rail with a two-exit total", |grid| {
        rail_drawn(grid) && grid.contents.contains("2 ✓") && grid.contents.contains("0 ✓")
    });
    // ← focuses the on-screen card, ↓ moves to the launch project's
    // "+" stop, ↓ again to its "2 ✓" stop.
    run.write(b"\x1b[D");
    run.wait_screen("the hint on the first card", |grid| {
        grid.contents.contains(HINT_MIDDLE)
    });
    run.write(b"\x1b[B");
    run.write(b"\x1b[B");
    // Enter opens home on the first exited row, newest first.
    run.write(b"\r");
    let home = run.wait_screen("home on the first seeded exit", |grid| {
        grid.contents.contains("exited · 2") && grid.contents.contains("seeded exit two")
    });
    // `docs/tui.md:254-255` (up to three exited rows, newest first) and `268` (count from the scoped total).
    assert!(
        home.rows.iter().any(|row| row.contains("seeded exit one")),
        "both seeded exits drawn:\n{}",
        home.contents
    );

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
    // its process once no terminal stays attached. The next test names
    // the false hint-and-lift condition on a spent deadline (see #2707).
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

/// The hint-and-lift wait names the false condition when its deadline is
/// spent: an empty grid reports all three false, and a grid holding only
/// the hint reports the hint true with both tints false, so the next
/// sighting says which condition failed. Both waits start on a fake clock
/// advanced past the waits bound, so they expire at once with no sleep
/// (`docs/testing.md`, "Waits and timeouts").
#[test]
fn hint_lift_reports_which_condition_was_false_on_a_spent_deadline() {
    use std::io::Write;

    let raised = Colour::Rgb(10, 20, 30);
    let flat = Colour::Rgb(40, 50, 60);
    // An empty grid: all three conditions false.
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = support::Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(support::WAITS);
    assert!(
        deadline.left().is_zero(),
        "the wait starts with no time left"
    );
    let terminal = support::pty::open(80, 24);
    let main = std::fs::File::from(terminal.main);
    let mut run = Run::attach(main, 80, 24, deadline);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_hint_lifted(&mut run, &raised, &flat)
    }));
    let message = result
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        message.contains("the hint and the lifted first card"),
        "the failure names the wait: {message:?}"
    );
    assert!(
        message.starts_with("last check:"),
        "the report leads: {message:?}"
    );
    assert!(
        message.contains("screen:\n"),
        "the screen dump follows: {message:?}"
    );
    assert!(
        message.contains("hint present=false"),
        "the failure names the hint: {message:?}"
    );
    assert!(
        message.contains("first card lifted=false"),
        "the failure names the first card: {message:?}"
    );
    assert!(
        message.contains("second card flat=false"),
        "the failure names the second card: {message:?}"
    );
    assert!(
        message.contains("left when the wait began"),
        "the failure names the spent deadline: {message:?}"
    );
    assert!(
        message.contains("first card bg=none,"),
        "the failure says the first card is absent: {message:?}"
    );
    assert!(
        message.contains("raised=Rgb(10, 20, 30),"),
        "the failure names the raised tint: {message:?}"
    );
    assert!(
        message.contains("lifted(raised)=Rgb(30, 40, 50)"),
        "the failure names the lifted tint: {message:?}"
    );
    // A grid holding only the hint: the hint flips true while both tints
    // stay false, so a flipped `&&` (an `||`) would stop failing here.
    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = support::Deadline::on(&**clock);
    let terminal = support::pty::open(80, 24);
    let main = std::fs::File::from(terminal.main);
    let mut run = Run::attach(main, 80, 24, deadline);
    let mut terminal_side = terminal.terminal;
    let from = run.output().len();
    terminal_side.write_all(HINT_MIDDLE.as_bytes()).unwrap();
    run.wait_bytes(from, HINT_MIDDLE.as_bytes(), "the fed hint");
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(support::WAITS);
    assert!(
        deadline.left().is_zero(),
        "the second wait starts with no time left"
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_hint_lifted(&mut run, &raised, &flat)
    }));
    let message = result
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        message.contains("hint present=true"),
        "the failure names the present hint: {message:?}"
    );
    assert!(
        message.contains("first card lifted=false"),
        "the failure still names the first card: {message:?}"
    );
    assert!(
        message.contains("second card flat=false"),
        "the failure still names the second card: {message:?}"
    );
}

/// The readiness guard takes the baseline only from a frame whose card
/// cells no longer show the rail region's background: the header's own
/// background fed as either card tint rejects, while two distinct card
/// tints accept.
#[test]
fn readiness_guard_rejects_a_card_tint_matching_the_rail() {
    let rail = Colour::Rgb(10, 20, 30);
    let raised = Colour::Rgb(40, 50, 60);
    let flat = Colour::Rgb(70, 80, 90);
    assert!(
        !cards_ready(rail, rail, flat),
        "the first card still shows the rail slab"
    );
    assert!(
        !cards_ready(rail, raised, rail),
        "the second card still shows the rail slab"
    );
    assert!(
        cards_ready(rail, raised, flat),
        "both cards carry tints of their own"
    );
}

/// The failure message of a spent-deadline hint-and-lift wait on a grid
/// holding a first card painted with background `1;2;3` and nothing else.
fn spent_wait_message_with_a_painted_first_card(raised: &Colour, flat: &Colour) -> String {
    use std::io::Write;

    let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
    let deadline = support::Deadline::on(&**clock);
    let terminal = support::pty::open(80, 24);
    let main = std::fs::File::from(terminal.main);
    let mut run = Run::attach(main, 80, 24, deadline);
    let mut terminal_side = terminal.terminal;
    let from = run.output().len();
    terminal_side
        .write_all(b"\x1b[48;2;1;2;3m  first live\x1b[0m")
        .unwrap();
    run.wait_bytes(from, b"first live\x1b[0m", "the fed card");
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(support::WAITS);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_hint_lifted(&mut run, raised, flat)
    }));
    result
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default()
}

/// The report carries the first card's actual background beside `raised`
/// and `lifted(raised)`, and survives a `raised` that cannot be lifted:
/// the check reads it as not lifted, so the card's tint stays in the report.
#[test]
fn hint_lift_report_shows_the_first_cards_actual_background() {
    let flat = Colour::Rgb(40, 50, 60);
    let message = spent_wait_message_with_a_painted_first_card(&Colour::Rgb(10, 20, 30), &flat);
    assert!(
        message.contains("first card lifted=false"),
        "the painted card is not the lifted tint: {message:?}"
    );
    assert!(
        message.contains(
            "first card bg=Rgb(1, 2, 3), raised=Rgb(10, 20, 30), lifted(raised)=Rgb(30, 40, 50)"
        ),
        "the failure names the card's tint beside both references: {message:?}"
    );
    let message = spent_wait_message_with_a_painted_first_card(&Colour::Indexed(7), &flat);
    assert!(
        message.contains("first card lifted=false"),
        "a raised tint that cannot lift is a false check: {message:?}"
    );
    assert!(
        message.starts_with("last check:") && message.contains("screen:\n"),
        "the report still leads the wait and the screen: {message:?}"
    );
    assert!(
        message.contains(
            "first card bg=Rgb(1, 2, 3), raised=Indexed(7), lifted(raised)=none (raised is not Rgb)"
        ),
        "the report survives a raised tint that cannot lift: {message:?}"
    );
}
