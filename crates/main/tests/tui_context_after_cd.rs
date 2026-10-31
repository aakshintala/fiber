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
use support::pty;

/// Polls under the deadline until some session log ends in `moved` —
/// the last line of a session that moved workspace (`docs/events.md`,
/// "`moved`") — returning the old and child ids. This confirms the
/// backend move after the terminal has emitted its `/cd` frame.
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

#[test]
fn context_after_cd_shows_the_total_against_the_inherited_window() {
    let setup = support::Setup::new();
    // A short target beside the launch workspace: the `/pwd` notice
    // cuts a long path with an ellipsis, so the whole target must fit on
    // one notice row for the screen to name it.
    let target = setup.root.path().join("c");
    fs::create_dir(&target).unwrap();
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
    // The terminal emits a complete frame after `/cd`; the log gate
    // confirms the move ended the old session with `moved`.
    let cd_from = run.output().len();
    run.write(b"/cd ../c\r");
    run.wait_bytes(cd_from, b"\x1b[?2026l", "the completed frame after `/cd`");
    let (old, child) = until_moved(&setup);
    assert_ne!(old, child, "the move names a new session");
    // The switched terminal answers `/pwd` with the target workspace, as
    // the loop records it: canonicalized, so `/var` reads `/private/var`.
    // The full path is not in the relative `/cd` input, so its terminal
    // output proves this notice came from the attached child.
    let recorded = fs::canonicalize(&target)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let pwd_from = run.output().len();
    run.write(b"/pwd\r");
    run.wait_bytes(
        pwd_from,
        recorded.as_bytes(),
        "the attached child's workspace notice",
    );
    run.wait_screen("the attached child's workspace notice", |grid| {
        grid.contents.contains(&recorded)
    });
    // One answered prompt in the moved child, whose log holds usage but
    // no `preamble_built` of its own.
    let from = run.output().len();
    run.write(b"say hi after cd\r");
    run.turn_finished(from);
    // The breakdown shows the total against the inherited window, not
    // the first-request placeholder.
    run.write(b"/context\r");
    run.wait_screen("the context total against the inherited window", |grid| {
        grid.contents.contains("Largest tool results")
            && grid.contents.contains("of the 100k window")
            && grid.contents.contains("tokens")
            && !grid
                .contents
                .contains("The context shows after the session's first request.")
            && !grid.contents.contains("automatic handoff")
            && !grid.contents.contains("handoff 70k")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
