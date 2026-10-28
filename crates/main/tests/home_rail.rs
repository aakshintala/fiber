//! Binary-level test of home as the rail (`docs/tui.md`, "Home"): live
//! sessions in two projects, four exited rows in one, keyboard expansion,
//! and the box fixed to the bottom of a 160x48 terminal.

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
use std::path::Path;
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::pty::{Colour, Grid, Run};
use support::{
    SessionGuard, Setup, TRUECOLOUR, close_session, connect_hub, git, hello, start_session,
    write_json,
};

const SELECTION: Colour = Colour::Rgb(0x26, 0x4f, 0x78);

/// The first row containing `needle`.
fn row_at(grid: &Grid, needle: &str) -> usize {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("a row containing {needle:?}:\n{}", grid.contents))
}

/// The column of `needle`, counting terminal cells.
fn column_at(row: &str, needle: &str) -> u16 {
    let chars: Vec<char> = row.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    chars
        .windows(needle.len().max(1))
        .position(|window| window == needle.as_slice())
        .map(|at| u16::try_from(at).unwrap())
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))
}

/// Whether `needle` is on home's selection bar.
fn selected(grid: &Grid, needle: &str) -> bool {
    let Some(y) = grid.rows.iter().position(|row| row.contains(needle)) else {
        return false;
    };
    let x = column_at(&grid.rows[y], needle);
    grid.cell(x, u16::try_from(y).unwrap()).bg == SELECTION
}

/// The box starts six rows from the bottom, with the foot on the last row.
fn assert_box_at_bottom(grid: &Grid) {
    let (cols, rows) = grid.size();
    assert_eq!((cols, rows), (160, 48));
    assert!(grid.rows[usize::from(rows - 6)].contains('▄'));
    assert!(grid.rows[usize::from(rows - 1)].contains("Enter opens the focused session"));
}

/// The project key of `workspace`, from its `projects/<key>` directory.
fn project_key(home: &Path, workspace: &Path) -> String {
    let _ = home;
    log::project_key(&worktree::project(workspace))
}

/// A `recent.jsonl` row with a readable status; `turns: None` models a
/// status written before the field existed.
fn exited_row(
    id: &str,
    name: &str,
    workspace: &Path,
    project: &str,
    ts: u64,
    turns: Option<u64>,
) -> Value {
    let workspace = fs::canonicalize(workspace)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut status = json!({
        "name": name,
        "workspace": workspace,
        "project": project,
        "model": "fake/m",
        "state": "idle",
        "since": ts,
        "started": ts,
        "calls": 0,
        "spend": {
            "tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
            "cost": 0.0,
            "subscription_cost": 0.0
        },
        "delegates": 0,
        "jobs": 0,
        "clients": 0
    });
    if let Some(turns) = turns {
        status["turns"] = json!(turns);
    }
    json!({
        "session_id": id,
        "ts": ts,
        "project": project,
        "workspace": workspace,
        "name": name,
        "how": "exited",
        "status": status
    })
}

/// Writes four chronological exited rows and keeps the sessions directory
/// for each, as the hub's recent reader requires.
fn seed_exited_rows(setup: &Setup, workspace: &Path) {
    let project = project_key(&setup.home(), workspace);
    let sessions = log::sessions_dir(&setup.home(), &worktree::project(workspace));
    fs::create_dir_all(&sessions).unwrap();
    let mut lines = Vec::new();
    for (index, name, turns) in [
        (1u64, "exited 1", Some(1)),
        (2, "exited 2", Some(2)),
        (3, "exited 3", None),
        (4, "exited 4", Some(5)),
    ] {
        let id = format!("s_{index:016x}");
        fs::create_dir_all(sessions.join(&id)).unwrap();
        lines.push(exited_row(&id, name, workspace, &project, index * 1_000, turns).to_string());
    }
    fs::write(
        setup.home().join("recent.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
}

/// A git workspace with one commit on `main`, so a live row draws a branch.
fn commit_on_main(setup: &Setup, workspace: &Path) {
    fs::create_dir_all(workspace).unwrap();
    git(setup.deadline, workspace, &["init", "--quiet"]);
    fs::write(workspace.join("file.txt"), "hi\n").unwrap();
    git(setup.deadline, workspace, &["add", "."]);
    git(
        setup.deadline,
        workspace,
        &["commit", "--quiet", "-m", "first"],
    );
}

/// Quits the terminal and waits for its process and hub.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn home_lists_live_and_exited_projects_and_keeps_the_box_at_the_bottom() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    server.hold();

    let launch = setup.workspace();
    let git_workspace = setup.root.path().join("v");
    commit_on_main(&setup, &git_workspace);
    seed_exited_rows(&setup, &launch);

    let hub = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let launch_text = launch.to_string_lossy().into_owned();
    let git_text = git_workspace.to_string_lossy().into_owned();
    let launch_guard = SessionGuard::arm(setup.deadline, &launch_text);
    let git_guard = SessionGuard::arm(setup.deadline, &git_text);
    let first = start_session(&control, &launch_text, "first live");
    let second = start_session(&control, &git_text, "second live");
    assert!(
        server.await_requests(2, setup.deadline),
        "both live sessions reach the held provider"
    );

    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let grid = run.wait_screen("home with both projects and recent rows", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("2 working · $0.00 live"))
            && grid.rows.iter().any(|row| row.contains("w +"))
            && grid.rows.iter().any(|row| row.contains("v +"))
            && grid.rows.iter().any(|row| row.contains("exited · 4"))
            && grid.rows.iter().any(|row| row.contains("⎇ main"))
            && grid
                .rows
                .iter()
                .any(|row| row.contains("exited 4") && row.contains("5 turns"))
            && grid.rows.iter().any(|row| row.contains("exited 3"))
            && grid.rows.iter().any(|row| row.contains("… 1 more"))
    });
    let counts = grid.rows[row_at(&grid, "$0.00 live")]
        .chars()
        .skip(35)
        .collect::<String>();
    assert_eq!(counts.trim(), "2 working · $0.00 live");
    for name in ["exited 4", "exited 3", "exited 2"] {
        assert!(
            grid.rows.iter().any(|row| row.contains(name)),
            "{name}:\n{}",
            grid.contents
        );
    }
    assert!(!grid.rows.iter().any(|row| row.contains("exited 1")));
    assert_box_at_bottom(&grid);
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.contains("back to the session")),
        "no back hint on the launch home:\n{}",
        grid.contents
    );
    assert!(
        !grid.rows[row_at(&grid, "exited 3")].contains("turn"),
        "an old status without turns draws no turns segment"
    );

    // ↑ from the empty entry bar focuses the last list row; ↑ again reaches
    // the More row just above it. Enter expands that project's exited rows.
    run.write(b"\x1b[A");
    run.wait_screen("the second live row focused", |grid| {
        selected(grid, "second live")
    });
    run.write(b"\x1b[A");
    let more = run.wait_screen("the More row focused", |grid| selected(grid, "… 1 more"));
    let more_y = u16::try_from(row_at(&more, "… 1 more")).unwrap();
    let more_x = column_at(&more.rows[usize::from(more_y)], "… 1 more");
    assert_eq!(more.cell(more_x, more_y).bg, SELECTION);
    run.write(b"\r");
    let expanded = run.wait_screen("the fourth exited row", |grid| {
        grid.rows.iter().any(|row| row.contains("exited 1"))
            && !grid.rows.iter().any(|row| row.contains("… 1 more"))
    });
    assert!(expanded.rows.iter().any(|row| row.contains("exited 1")));
    assert_box_at_bottom(&expanded);

    // Finish both sessions before the terminal exits, so its ordinary hub
    // idle deadline can reap the hub without leaving child processes behind.
    server.release();
    for id in [&first, &second] {
        let socket = support::Socket::connect(setup.deadline, &setup.session_socket(id));
        close_session(&socket);
    }
    launch_guard.wait_gone();
    git_guard.wait_gone();
    drop(control);
    quit(run);
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}
