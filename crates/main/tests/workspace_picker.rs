//! Binary-level test that the workspace picker takes a typed path
//! (`docs/tui.md`, "Home"): in a pty, the test types a path toward a real
//! temporary directory tree, Tab-completes one directory, and Enter starts
//! a session there, with the `start` command's `workspace` as that path
//! (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::ProviderServer;
use serde_json::{Value, json};
use std::fs;
use support::Setup;
use support::pty::{Grid, Run};

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// The `session_started` line's workspace: the journey starts exactly one
/// session, and its log names the workspace it runs in.
fn started_workspace(setup: &Setup) -> String {
    let mut logs = Vec::new();
    let projects = fs::read_dir(setup.home().join("projects")).unwrap();
    for project in projects.flatten() {
        let sessions = project.path().join("sessions");
        if !sessions.is_dir() {
            continue;
        }
        for session in fs::read_dir(&sessions).unwrap().flatten() {
            logs.push(session.path().join("events.jsonl"));
        }
    }
    assert_eq!(logs.len(), 1, "the journey starts exactly one session");
    let workspace = fs::read_to_string(&logs[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line["kind"] == "session_started")
        .expect("session_started is logged");
    workspace["payload"]["workspace"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_picker_completes_a_typed_path_and_starts_there() {
    let setup = Setup::new_under(std::path::Path::new("/tmp"));
    let server = ProviderServer::start([support::hello()]).unwrap();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    let tree = setup.root.path().join("tree");
    fs::create_dir_all(tree.join("alpha")).unwrap();
    fs::create_dir_all(tree.join("beta")).unwrap();
    let mut run = Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("TERM", "xterm-256color"),
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ],
    );
    // The end of the first frame proves the input reader runs before any
    // key goes out; every later key waits on the grid state it needs.
    run.ready();
    // ↓ focuses the chip row on the workspace chip; Enter opens the
    // picker browsing the launch directory.
    run.write(b"\x1b[B");
    run.write(b"\r");
    run.wait_screen("the open picker", |screen| {
        shows(screen, "Workspaces") && shows(screen, "↑↓ move · Enter open · Esc closes")
    });
    // Backspace past the launch directory's path empties the typed row,
    // whatever the platform's temporary root; extra presses do nothing.
    run.write(&[0x7f; 512]);
    // The `~/` listing answers with the root's `tree` directory. The
    // wait holds for the focused completion (`› tree`), not the bare
    // name: the recents below still show the launch directory, whose
    // path can itself hold the name, and Tabbing on that lookalike
    // drops the keystroke before the listing lands, with no recovery.
    run.write(b"~/tr");
    run.wait_screen("the tree completion", |screen| {
        shows(screen, "› ~/tr") && shows(screen, "› tree")
    });
    // Tab completes it; the `~/tree/` listing answers with the real
    // tree's subdirectories.
    run.write(b"\t");
    run.wait_screen("the completed tree", |screen| {
        shows(screen, "› ~/tree/") && shows(screen, "alpha") && shows(screen, "beta")
    });
    // Typing `a` narrows the completions to `alpha`: only its listing
    // has drawn, so Enter cannot be loading.
    run.write(b"a");
    run.wait_screen("the narrowed path", |screen| {
        shows(screen, "› ~/tree/a") && shows(screen, "alpha") && !shows(screen, "beta")
    });
    // Enter opens the focused completion as the workspace.
    run.write(b"\r");
    run.wait_screen("the alpha workspace", |screen| {
        shows(screen, "[alpha]") && !shows(screen, "Workspaces")
    });
    // The guard is armed before the prompt goes out, and watches the
    // chosen workspace, not the launch one.
    let ws = tree.join("alpha").display().to_string();
    let guard = support::SessionGuard::arm(setup.deadline, &ws);
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    // Quitting is taken in any state; the restored grid above proves the
    // terminal drew before the quit goes out.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    fakes::kill_matching(&ws).unwrap();
    guard.wait_gone();
    // The `start` command's workspace is the completed path, however the
    // platform spells the temporary root.
    let logged = fs::canonicalize(started_workspace(&setup)).unwrap();
    assert_eq!(logged, fs::canonicalize(tree.join("alpha")).unwrap());
}
