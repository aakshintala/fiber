//! Binary-level tests of `/clear` (`docs/tui.md`, "Clearing"): mid-turn
//! `/clear` switches to a new empty session at once while the old one
//! closes politely, and clearing past an asided approval asks first.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::json;
use support::pty::Run;
use support::{
    HubProc, SessionGuard, Setup, Socket, connect_hub, session_dir, subscribe, until, write_json,
};

/// The pty size the journeys draw on.
const COLS: u16 = 160;
const ROWS: u16 = 48;

/// A config naming the workspace script, with the hub exiting once no
/// client stays connected: the run and the watchers hold it up while the
/// test moves, and quitting lets it go. A waiting session stays waiting
/// until the test moves it.
fn scripted(setup: &Setup, steps: serde_json::Value) {
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
}

/// A global standing rule file with one rule.
fn allow_rule(setup: &Setup, rule: serde_json::Value) {
    fs::write(setup.home().join("rules"), format!("{rule}\n")).unwrap();
}

/// The ids of the sessions stored under the workspace, sorted.
fn stored(setup: &Setup) -> Vec<String> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let mut ids: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// The new session's log holds no user text: the clear attached an empty
/// conversation.
fn assert_empty_conversation(setup: &Setup, id: &str, what: &str) {
    let log = fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap();
    assert!(
        !log.contains(what),
        "the new session never saw the old prompt: {log}"
    );
}

/// Quits the terminal to the resume line and waits for a clean exit.
fn quit(run: Run) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

/// Tears the hub down once the assertions hold: the attached terminal
/// quits, and the run's wait plus the guard reap whatever session
/// processes remain. A leftover session may already have exited once the
/// terminal detached, so no close goes over its socket.
fn finish(hub: Arc<Mutex<Option<HubProc>>>, guard: SessionGuard, run: Run) {
    quit(run);
    guard.wait_gone();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// Tears the hub down from the home screen, which prints no resume line:
/// quits and waits for a clean exit, then reaps whatever remains.
fn finish_home(hub: Arc<Mutex<Option<HubProc>>>, guard: SessionGuard, mut run: Run) {
    run.write(b"\x03\x03\r");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    guard.wait_gone();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// `/clear` mid-turn switches to a new empty conversation at once, on the
/// `start` answer, while the old session closes politely: its stream marks
/// `closing` before it leaves once its turn finishes, and home keeps its
/// card reading CLOSING until then. The shell call waits on a FIFO the
/// test holds, so the turn still runs when `/clear` lands.
#[test]
fn clear_switches_to_a_new_empty_session_and_closes_the_old() {
    let setup = Setup::new();
    let held = setup.workspace().join("held");
    fakes::fifo(&held);
    // Held read-write, the FIFO always has a writer: opening it never
    // blocks, and the shell's `read` waits for the line the test writes
    // to release the turn.
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&held)
        .unwrap();
    // One plain command, so a standing rule can match it; the guard kills
    // the shell if the test fails before the release.
    let script = setup.workspace().join("wait.sh");
    fs::write(
        &script,
        format!("read -r _ < {}\necho done\n", held.display()),
    )
    .unwrap();
    let _job_guard = fakes::Watchdog::matching(script.to_str().unwrap());
    let command = format!("sh {}", script.display());
    scripted(
        &setup,
        json!([{"tool_calls": [{"name": "shell", "arguments": {"command": command}}]}]),
    );
    // A shell call is reviewed; a global standing rule allows this one, so
    // the turn runs instead of waiting on an approval.
    allow_rule(
        &setup,
        json!({"decision": "allow", "tool": "shell", "prefix": command}),
    );
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"run it\r");
    run.wait_screen("A's running turn", |grid| {
        grid.contents.contains("wait.sh") && grid.contents.contains("running")
    });
    // The shell holds the turn on the FIFO: its process is alive.
    assert!(
        !fakes::matching("wait.sh").unwrap().is_empty(),
        "the shell waits on the FIFO"
    );
    // The terminal started exactly one session, mid-turn: the old one.
    let sessions = stored(&setup);
    assert_eq!(sessions.len(), 1, "one session: {sessions:?}");
    let a = sessions.into_iter().next().unwrap();
    let watch = Socket::connect(setup.deadline, &setup.hub_socket());
    subscribe(&watch, &a);
    run.write(b"/clear\r");
    // The polite close folds first, while the shell still waits on the
    // FIFO: the hub reports the old session closing.
    until(&watch, "the polite close to fold", |line| {
        line["kind"] == "session_status" && line["payload"]["closing"] == json!(true)
    });
    // The switch folded: the new session shows an empty conversation.
    run.wait_screen("the new empty session", |grid| {
        !grid.contents.contains("run it")
    });
    // Home keeps the old session as a live CLOSING row while its shell
    // waits on the FIFO.
    run.write(b"/home\r");
    run.wait_screen("the old row closing", |grid| {
        grid.contents.contains("CLOSING")
    });
    // The release lets the old turn finish; only the finished turn lets
    // the old session leave.
    std::io::Write::write_all(&mut &release, b"\n").unwrap();
    until(&watch, "A to exit", |line| line["kind"] == "fiber_exited");
    // The switch attached a second session beside the old one, and the new
    // one never saw the old prompt.
    let ids = stored(&setup);
    assert_eq!(ids.len(), 2, "the old and the new session: {ids:?}");
    let b = ids.into_iter().find(|id| id != &a).unwrap();
    assert_empty_conversation(&setup, &b, "run it");
    // The old row leaves with its session.
    run.wait_screen("the old row gone", |grid| {
        !grid.contents.contains("CLOSING")
    });
    drop((watch, control));
    finish_home(hub, guard, run);
}

/// `/clear` past an asided approval asks first: Esc stays on the old
/// session without starting one, and Enter clears anyway, closing the old
/// session at once once the `start` answers.
#[test]
fn clear_on_a_waiting_session_closes_it_at_once() {
    let setup = Setup::new();
    scripted(
        &setup,
        json!([
            {"tool_calls": [{"name": "shell", "arguments": {"command": "echo hi"}}]},
            {"text": ["done."]},
        ]),
    );
    // The shell call waits on an approval instead of running.
    allow_rule(
        &setup,
        json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"}),
    );
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (control, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"run it\r");
    run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1 · run it")
    });
    // The terminal started exactly one session: the old one.
    let sessions = stored(&setup);
    assert_eq!(sessions.len(), 1, "one session: {sessions:?}");
    let a = sessions.into_iter().next().unwrap();
    let watch = Socket::connect(setup.deadline, &setup.hub_socket());
    subscribe(&watch, &a);
    // Esc puts the approval aside behind its badge; the session still
    // waits on it.
    run.write(b"\x1b[27u");
    run.wait_screen("the badge for one", |grid| {
        grid.contents
            .contains("1 approvals waiting · click to reopen")
    });
    run.write(b"/clear\r");
    run.wait_screen("the clear question", |grid| {
        grid.contents.contains("Clear") && grid.contents.contains("waits on an approval.")
    });
    // Esc stays: the question closes, the old session stays attached, and
    // no start went out. The consumed command left the draft, so the next
    // `/clear` dispatches fresh.
    run.write(b"\x1b[27u");
    run.wait_screen("stayed on the old session", |grid| {
        grid.contents
            .contains("1 approvals waiting · click to reopen")
            && !grid.contents.contains("waits on an approval.")
    });
    assert_eq!(stored(&setup).len(), 1, "Esc started no session");
    // Enter clears anyway: the new session attaches on the `start` answer,
    // and the old session leaves at once instead of waiting for the
    // approval's answer, which nobody sends.
    run.write(b"/clear\r");
    run.wait_screen("the clear question again", |grid| {
        grid.contents.contains("Clear") && grid.contents.contains("waits on an approval.")
    });
    run.write(b"\r");
    until(&watch, "A to exit at once", |line| {
        line["kind"] == "fiber_exited"
    });
    let ids = stored(&setup);
    assert_eq!(ids.len(), 2, "the old and the new session: {ids:?}");
    let b = ids.into_iter().find(|id| id != &a).unwrap();
    assert_empty_conversation(&setup, &b, "run it");
    drop((watch, control));
    finish(hub, guard, run);
}
