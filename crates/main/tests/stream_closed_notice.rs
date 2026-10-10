//! Binary-level test for #1802 (`docs/tui.md`, "Notices"): the real
//! binary through the hub over a session log with an unreadable line
//! mid-page shows every readable line with the `stream_closed` notice,
//! keeps a rejected send's draft with its `not_subscribed` notice, and
//! subscribes again on reopen, seen by a second hub client.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::FileExt;
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::Value;
use support::pty::{COLS, Grid, HOME_TITLE, ROWS, Run};
use support::*;

/// The headless session's prompt: `zebraflight` names it on screen, and
/// no other text holds the word, so one occurrence is no repeat.
const PROMPT: &str = "say zebraflight alpha";
/// The composer's send after the close: `quetzal` names the draft, so
/// its return to the box is one occurrence after the rejection.
const FOLLOWUP: &str = "quetzal followup";

/// `fiber ask` with its stdout kept drained and its stderr kept for a failure.
struct Running {
    child: Child,
    watchdog: Watchdog,
    group: u32,
    stdout: mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
    deadline: Deadline,
}

fn start(setup: &Setup, args: &[&str]) -> Running {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_copy = Arc::clone(&stderr_text);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        match std::io::Read::read_to_string(&mut reader, &mut buf) {
            Ok(_) | Err(_) => {}
        }
        *stderr_copy.lock().unwrap() = buf;
    });
    let (tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match tx.send(line.unwrap()) {
                Ok(()) => {}
                Err(mpsc::SendError(_)) => break,
            }
        }
    });
    Running {
        child,
        watchdog,
        group,
        stdout: stdout_rx,
        stderr: stderr_text,
        deadline: setup.deadline,
    }
}

fn finish(running: Running) {
    let Running {
        mut child,
        watchdog,
        group,
        stdout,
        stderr,
        deadline,
    } = running;
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = match finished.recv_timeout(deadline.left()) {
        Ok(status) => status.unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            expired(deadline, group, &finished, "fiber to exit")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("fiber's wait thread ended before fiber exited")
        }
    };
    assert!(status.success(), "stderr: {}", stderr.lock().unwrap());
    assert!(
        !group_alive(deadline, group),
        "fiber left a process in its group"
    );
    drop(stdout);
    watchdog.stand_down(deadline.cleanup());
}

/// The session id of a started `fiber ask`: its first stdout line.
fn session_of(running: &Running, what: &str) -> String {
    let started: Value = serde_json::from_str(
        &running
            .stdout
            .recv_timeout(running.deadline.left())
            .unwrap_or_else(|_| panic!("waited until the deadline for {what}")),
    )
    .unwrap();
    started["session_id"].as_str().unwrap().to_owned()
}

/// Whether any screen row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.contents.contains(needle)
}

/// How many times `needle` occurs in the screen's text.
fn occurrences(screen: &Grid, needle: &str) -> usize {
    screen.contents.matches(needle).count()
}

#[test]
fn stream_closed_notice_draft_returns_and_reopen_resubscribes() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    // The hub idles out a second after its last client leaves, so no
    // hub lingers past the run's wait.
    write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    // The session stays running on its held response while the log is
    // damaged and the terminal attaches, as a held session does.
    server.hold();
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let running = start(&setup, &["ask", PROMPT]);
    let id = session_of(&running, "the session_started");
    assert!(
        server.await_requests(1, setup.deadline.left()),
        "the held response was requested"
    );

    // Damage the log mid-page: one line overwritten with `x`s, every
    // line on one page.
    let log_path = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace())).join(&id);
    let events = log_path.join("events.jsonl");
    let whole = fs::read(&events).unwrap();
    let file_lines: Vec<&[u8]> = whole.split_inclusive(|b| *b == b'\n').collect();
    assert!(
        file_lines.len() >= 3,
        "the log holds a page to corrupt: {whole:?}"
    );
    assert!(
        file_lines.len() < 1_024 && whole.len() < 1024 * 1024,
        "every line is on one page"
    );
    // Damage the line after the prompt's `turn_started`, as a full
    // subscriber over a damaged log does: one line overwritten with
    // `x`s, every line on one page, the prompt arriving before the cut
    // so the conversation keeps it.
    let prompt_at = file_lines
        .iter()
        .position(|line| String::from_utf8_lossy(line).contains("zebraflight"))
        .expect("the prompt is logged before the damage");
    let bad = prompt_at + 1;
    assert!(bad + 1 < file_lines.len(), "a line past the cut: {whole:?}");
    let mut start_at = 0;
    for line in file_lines.iter().take(bad) {
        start_at += line.len();
    }
    let damaged = file_lines[bad].to_vec();
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&vec![b'x'; damaged.len() - 1], start_at as u64)
        .unwrap();
    drop(file);

    // A second hub client holds a `summary` subscription: `summary`
    // reads no log, so the damage never touches it, and every full
    // attach and detach reaches it as a `session_status` count.
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (observer, _) = connect_hub(&setup, &hub);
    observer.send(&format!(
        "{{\"id\":\"c_sum\",\"session_id\":\"{id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}"
    ));
    let summary_ack = until(&observer, "the summary subscribe acknowledgement", |line| {
        line.get("payload")
            .and_then(|p| p.get("command_id"))
            .and_then(Value::as_str)
            == Some("c_sum")
            && (line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
    });
    assert_eq!(
        summary_ack.last().unwrap()["kind"],
        "command_accepted",
        "{summary_ack:?}"
    );
    let first_status = until(
        &observer,
        "the summary session_status with no full client",
        |line| {
            line.get("kind").and_then(Value::as_str) == Some("session_status")
                && line["payload"].get("clients").and_then(Value::as_u64) == Some(0)
        },
    );
    assert_eq!(
        first_status.last().unwrap()["payload"]["clients"],
        0,
        "{first_status:?}"
    );

    // The terminal attaches through the hub to the damaged session.
    let mut run = Run::spawn(
        &setup,
        COLS,
        ROWS,
        &["resume", &id],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    // The end of the first frame proves the input reader runs before
    // any typing goes out.
    run.ready();
    // The hub sends every durable line before the bad one, then
    // `stream_closed`: the conversation keeps each readable line once
    // and the notice draws with its text.
    let screen = run.wait_screen("the stream_closed notice", |screen| {
        shows(screen, "stream_closed") && shows(screen, "zebraflight")
    });
    assert!(
        shows(&screen, "can't be read past here"),
        "the notice text draws whole: {}",
        screen.contents
    );
    assert_eq!(
        occurrences(&screen, "zebraflight"),
        1,
        "no conversation line repeats: {}",
        screen.contents
    );
    assert_eq!(
        occurrences(&screen, "stream_closed"),
        1,
        "one notice: {}",
        screen.contents
    );

    // The summary subscriber is promised the latest count, not every
    // change (`docs/events.md`, "session_status": the latest wins): the
    // session reads the settled count when it folds a line, so an attach
    // and a close that finish before it folds send no `clients: 1` and no
    // second `clients: 0` (`crates/loop/src/status.rs`, `Fold::observe`).
    // The test waits for nothing between them; the reopen's count below
    // is the first one that stays.
    let clients_is = |count: u64| {
        move |line: &Value| {
            line.get("kind").and_then(Value::as_str) == Some("session_status")
                && line["payload"].get("clients").and_then(Value::as_u64) == Some(count)
        }
    };

    // A send from the composer is rejected `not_subscribed`: the hub's
    // rejection message is `Send `subscribe` first.`
    // (`crates/doors/src/client.rs`), which the terminal pushes as the
    // notice, and the draft comes back to the box with it.
    run.write(b"quetzal followup\r");
    run.wait_screen("the not_subscribed rejection", |screen| {
        shows(screen, "Send `subscribe` first.")
    });
    let rejected = run.screen();
    assert!(
        shows(&rejected, FOLLOWUP),
        "the draft returns to the composer: {}",
        rejected.contents
    );

    // The draft is cleared, so `/resume` leaves for home alone.
    run.write(b"\x03");
    run.wait_screen("the cleared composer", |screen| !shows(screen, "quetzal"));
    let home_from = run.output().len();
    run.write(b"/resume\r");
    // The home title proves home drew before the reopen goes out.
    run.wait_bytes(home_from, HOME_TITLE, "the home title");
    run.wait_screen("the home session list", |screen| {
        shows(screen, "zebraflight")
    });

    // Restore the damaged line before reopening: the new subscribe
    // replays the whole log past where the close cut it.
    let file = fs::OpenOptions::new().write(true).open(&events).unwrap();
    file.write_all_at(&damaged, start_at as u64).unwrap();
    drop(file);
    assert_eq!(fs::read(&events).unwrap(), whole);
    // Enter opens the focused list row through the normal open path:
    // the hub gets a new `subscribe` for the session, and the summary
    // client reads its count going back to one full client.
    run.write(b"\r");
    let resubscribed = until(
        &observer,
        "the reopened subscribe's clients count",
        clients_is(1),
    );
    assert_eq!(
        resubscribed.last().unwrap()["payload"]["clients"],
        1,
        "{resubscribed:?}"
    );
    let reopened = run.wait_screen("the reopened conversation", |screen| {
        shows(screen, "zebraflight")
    });
    assert_eq!(
        occurrences(&reopened, "stream_closed"),
        1,
        "no second close notice: {}",
        reopened.contents
    );
    drop(observer);

    // The held turn runs on: the reply finishes the turn, the session
    // exits, and the terminal quits with its resume line.
    let completed_from = run.output().len();
    server.release_one();
    run.turn_finished(completed_from);
    finish(running);
    // The session has exited, so nothing is live: quitting restores
    // the primary screen with no resume line (`docs/tui.md`, "On exit").
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(0));
    if let Some(hub) = hub.lock().unwrap().take() {
        hub.kill_and_wait();
    }
    guard.wait_gone();
}
