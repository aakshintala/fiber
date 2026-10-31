//! Binary-level screen test for see #3756: after `/cd`, one answered
//! prompt and `/context`, the breakdown shows the total against the
//! inherited window instead of the first-request placeholder. The moved
//! child continues the parent's log without a `preamble_built` of its
//! own, so the window comes from the `session_status` the child
//! reports. The built `fiber` runs in a temporary home with an ordinary
//! provider whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::hello_inline_completed as hello;
use support::{Deadline, pty};

/// Polls under the deadline until some session log ends in `moved` —
/// the last line of a session that moved workspace (`docs/events.md`,
/// "`moved`") — returning the old and child ids: the gate between
/// typing `/cd` and observing the switched terminal, so no later key
/// races the attach.
#[track_caller]
fn until_moved(setup: &support::Setup) -> (String, String) {
    let projects = setup.home().join("projects");
    let deadline = setup.deadline;
    loop {
        let mut found = None;
        if let Ok(keys) = fs::read_dir(&projects) {
            for key in keys.flatten() {
                let sessions = key.path().join("sessions");
                let Ok(ids) = fs::read_dir(&sessions) else {
                    continue;
                };
                for id in ids.flatten() {
                    let log = id.path().join("events.jsonl");
                    let Ok(text) = fs::read_to_string(&log) else {
                        continue;
                    };
                    let Some(last) = text.lines().last() else {
                        continue;
                    };
                    let Ok(line): Result<Value, _> = serde_json::from_str(last) else {
                        continue;
                    };
                    if line["kind"] == "moved" {
                        found = Some((
                            id.file_name().to_string_lossy().into_owned(),
                            line["payload"]["new_session_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                        ));
                    }
                }
            }
        }
        if let Some(found) = found {
            return found;
        }
        assert!(
            !deadline.left().is_zero(),
            "waited until the deadline for a session log ending in moved"
        );
        std::thread::yield_now();
    }
}

/// Writes `/pwd` until the screen names `target`, proving the terminal
/// attached to the moved child: the move's accept may land after an
/// early probe goes out, answering it with the old workspace. The
/// readiness signals are the log gate before this helper (the old
/// session ends `moved` before the next key goes out) and the screen
/// naming `target` here; between probes it waits on the terminal's own
/// output through `wait_screen` for either a changed screen or the
/// target, never a sleep and never a second deadline (`docs/testing.md`,
/// "Waits and timeouts").
#[track_caller]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn until_pwd_shows(run: &mut pty::Run, target: &str, deadline: Deadline) {
    loop {
        let before = run.screen().contents.clone();
        if before.contains(target) {
            return;
        }
        assert!(
            !deadline.left().is_zero(),
            "waited until the deadline for the target workspace {target} in a notice; screen:\n{before}"
        );
        run.write(b"/pwd\r");
        run.wait_screen("the answered `/pwd` notice", |grid| {
            grid.contents.contains(target) || grid.contents != before
        });
    }
}

#[test]
fn context_after_cd_shows_the_total_against_the_inherited_window() {
    let setup = support::Setup::new();
    // A short target outside the temp root: the `/pwd` notice cuts a
    // long path with an ellipsis, so the whole target must fit on one
    // notice row for the screen to name it.
    let target = std::env::temp_dir().join(format!("fiber-context-cd-{}", std::process::id()));
    if target.exists() {
        fs::remove_dir_all(&target).unwrap();
    }
    fs::create_dir_all(&target).unwrap();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    // The hub a client starts exits on idle: without the short idle the
    // PTY's hub would outlive the test's deadline after the terminal
    // quits, with the moved sessions still running.
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["hub"] = json!({"idle_exit_ms": 200});
    support::write_json(&path, &config);
    let mut run = pty::Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    );
    // The first frame draws the input line; its title proves the input
    // reader runs before the prompt goes out.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    // The prompt starts the old session in the launch workspace.
    let from = run.output().len();
    run.write(b"say hi\r");
    run.turn_finished(from);
    // The move names the target; the log gate proves the old session
    // ended `moved` before the next key goes out.
    run.write(format!("/cd {}\r", target.to_string_lossy()).as_bytes());
    let (old, child) = until_moved(&setup);
    assert_ne!(old, child, "the move names a new session");
    // The switched terminal answers `/pwd` with the target workspace, as
    // the loop records it: canonicalized, so `/var` reads `/private/var`.
    let recorded = fs::canonicalize(&target)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    until_pwd_shows(&mut run, &recorded, setup.deadline);
    // One answered prompt in the moved child, whose log holds usage but
    // no `preamble_built` of its own.
    let from = run.output().len();
    run.write(b"say hi after cd\r");
    run.turn_finished(from);
    // The breakdown shows the total against the inherited window, not
    // the first-request placeholder.
    run.write(b"/context\r");
    let grid = run.wait_screen("the context total against the inherited window", |grid| {
        grid.contents.contains("Largest tool results")
            && grid.contents.contains("of the 100k window")
            && grid.contents.contains("tokens")
            && !grid
                .contents
                .contains("The context shows after the session's first request.")
            && !grid.contents.contains("automatic handoff")
            && !grid.contents.contains("handoff 70k")
    });
    // settled: the wait matched the sizes row and the window tail with
    // no input in between, so the grid it returns is the settled view.
    assert!(
        grid.contents.contains("tokens"),
        "the total is shown; screen:\n{}",
        grid.contents
    );
    assert!(
        !grid
            .contents
            .contains("The context shows after the session's first request."),
        "the placeholder is gone; screen:\n{}",
        grid.contents
    );
    assert!(
        !grid.contents.contains("automatic handoff"),
        "the unknown handoff point draws no marker text and no off row; screen:\n{}",
        grid.contents
    );
    assert!(
        !grid.contents.contains("handoff 70k"),
        "the parent's handoff marker is gone; screen:\n{}",
        grid.contents
    );
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    fs::remove_dir_all(&target).unwrap();
}
