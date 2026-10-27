//! Binary-level tests of session deletion (`docs/invocation.md`,
//! "Deleting and pruning"): CLI cases start without a hub, and `/delete`
//! pty journeys run against the hub the built `fiber` starts. Every run
//! and every wait carries a deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};
use support::pty::Run;
use support::*;

const FORK: &str = "s_00000000000000d2";
const MOVED: &str = "s_00000000000000d3";

/// Appends a `moved` last line to `dir`'s log naming `next`: the parent's
/// post-move state, as `fiber sessions delete` meets it.
fn append_moved(dir: &Path, old: &str, next: &str) {
    let line = json!({"kind": "moved", "session_id": old, "ts": 1,
        "schema_version": 1, "seq": 3,
        "payload": {"new_session_id": next, "seq": 1, "workspace": "/w2"}});
    let mut log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    log.push_str(&format!("{line}\n"));
    fs::write(dir.join("events.jsonl"), log).unwrap();
}

/// Session `id` in another project's sessions, continuing `from`: the
/// cross-project moved child, as `fiber sessions delete` meets it.
fn moved_child(setup: &Setup, id: &str, from: &str) -> PathBuf {
    let workspace = setup.root.path().join("elsewhere");
    fs::create_dir_all(&workspace).unwrap();
    let dir = log::sessions_dir(&setup.home(), &doors::project(&workspace)).join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0,
        "payload": {"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
        "forked_from": {"session_id": from, "seq": 1}}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"bytes").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

/// The hub idles out soon after the run's connection closes, so none
/// lingers past the test.
fn setup() -> Setup {
    let setup = Setup::new();
    write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 200}}),
    );
    setup
}

/// The workspace's project directory, as the command derives it.
fn project(setup: &Setup) -> PathBuf {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    log::sessions_dir(&setup.home(), &doors::project(&workspace))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Session `id` in the workspace's project, continuing `from`: its log,
/// an artifact and an unheld lock file. The first line starts the session
/// in this workspace, so `resolve` keeps it.
fn session(setup: &Setup, id: &str, from: Option<&str>) -> PathBuf {
    let dir = project(setup).join("sessions").join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let mut payload = json!({"workspace": workspace,
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0, "payload": payload});
    fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
    fs::write(dir.join("artifacts/a_1.txt"), b"bytes").unwrap();
    fs::write(dir.join("session.lock"), b"").unwrap();
    dir
}

/// Runs `fiber sessions delete` with `args` in the workspace, stdin null.
#[track_caller]
fn delete(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut all = vec!["sessions", "delete"];
    all.extend_from_slice(args);
    let mut command = setup.fiber(&all);
    command.current_dir(setup.workspace());
    run_to_exit(setup.deadline, "fiber sessions delete", command)
}

/// Waits under the test's [`Deadline`] until `socket` is gone, naming
/// `what`: the hub holds the `run/` flock until `serve` returns, after
/// unlinking the socket, so a returned flock means the hub is gone.
#[track_caller]
fn until_absent(deadline: Deadline, socket: &Path, what: &str) {
    let run = socket.parent().map(Path::to_owned);
    let socket = socket.to_owned();
    let what = what.to_owned();
    bounded(deadline, &what.clone(), move || {
        if let Some(run) = run
            && let Ok(file) = File::open(&run)
        {
            file.lock().unwrap();
        }
        assert!(
            !socket.exists(),
            "{what}: the hub exited but left its socket"
        );
    });
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Accepts one connection, blocking: `None` is the stand-in ending,
/// `stop` set before a client connected.
fn accept_unless_stopped(listener: &UnixListener, stop: &AtomicBool) -> Option<UnixStream> {
    match listener.accept() {
        Ok((stream, _)) if !stop.load(Ordering::SeqCst) => Some(stream),
        _ => None,
    }
}

/// Sets `stop` when dropped, then connects to the socket so a thread
/// blocked in `accept` wakes and ends on every path, including a test
/// panic. The stream drops at once.
struct StopGuard {
    stop: Arc<AtomicBool>,
    socket: PathBuf,
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        match UnixStream::connect(&self.socket) {
            Ok(_) | Err(_) => {}
        }
    }
}

#[test]
fn a_delete_that_meets_an_exiting_hub_waits_and_starts_a_new_one() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    fs::create_dir_all(setup.home().join("run")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopGuard {
        stop: Arc::clone(&stop),
        socket: setup.hub_socket(),
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let (released_tx, released_rx) = mpsc::channel();
    let home = setup.home();
    let deadline = setup.deadline;
    let thread_stop = Arc::clone(&stop);
    thread::spawn(move || {
        // The exiting hub: the `run/` lock as `listen::lock` takes it,
        // then `run/hub` bound 0600. The lock drops before the unlink,
        // the unlink before the held stream drops, so the hub the client
        // starts always gets the lock.
        let run = home.join("run");
        let lock = File::open(&run).ok();
        let locked = lock.filter(|file| file.try_lock().is_ok());
        let socket = run.join("hub");
        fs::remove_file(&socket).unwrap_or(());
        let Ok(listener) = UnixListener::bind(&socket) else {
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap_or(());
        ready_tx.send(()).unwrap_or(());
        let Some(first) = accept_unless_stopped(&listener, &thread_stop) else {
            drop(locked);
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        drop(first);
        let Some(held) = accept_unless_stopped(&listener, &thread_stop) else {
            drop(locked);
            fs::remove_file(&socket).unwrap_or(());
            return;
        };
        drop(locked);
        fs::remove_file(&socket).unwrap_or(());
        drop(held);
        drop(listener);
        released_tx.send(()).unwrap_or(());
    });
    assert!(
        deadline.recv(&ready_rx).is_ok(),
        "the exiting hub is ready before the delete runs"
    );
    let output = delete(&setup, &["--yes", ROOT]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&output.stdout), format!("{ROOT}\n"), "{stderr}");
    assert!(!dir.exists(), "the session is gone: {stderr}");
    assert!(
        deadline.recv(&released_rx).is_ok(),
        "the client outlasted both of the exiting hub's connections: {stderr}"
    );
    assert!(
        setup.hub_log().contains("hub_started"),
        "a real hub started: {stderr}"
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn yes_starts_a_hub_that_removes_the_session_and_keeps_its_worktree_and_row() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let worktree = project(&setup).join("worktrees").join(ROOT);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("file"), b"kept").unwrap();
    let row = json!({"session_id": ROOT, "ts": 1, "project": "p", "workspace": "/w", "name": "n", "how": "exited"});
    fs::write(setup.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    let rows = fs::read(setup.home().join("recent.jsonl")).unwrap();
    assert!(!setup.hub_socket().exists(), "no hub runs beforehand");
    let output = delete(&setup, &["--yes", "s_00000000000000d"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n"));
    assert!(!dir.exists(), "the log and artifacts are gone");
    assert_eq!(fs::read(worktree.join("file")).unwrap(), b"kept");
    assert_eq!(fs::read(setup.home().join("recent.jsonl")).unwrap(), rows);
    assert!(
        setup.hub_log().contains("hub_started"),
        "{}",
        setup.hub_log()
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn a_held_session_exits_one_and_stays() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let lock = File::open(dir.join("session.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("is held by another process"), "{stderr}");
    assert!(dir.join("events.jsonl").is_file());
    assert_eq!(text(&output.stdout), "");
    drop(lock);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn dependents_exit_one_naming_the_fork_and_cascade_removes_both() {
    let setup = setup();
    let root = session(&setup, ROOT, None);
    let fork = session(&setup, FORK, Some(ROOT));
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains(FORK), "{stderr}");
    assert!(stderr.contains("--cascade"), "{stderr}");
    assert!(root.is_dir() && fork.is_dir());
    let output = delete(&setup, &["--cascade", "--yes", ROOT]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n{FORK}\n"));
    assert!(!root.exists() && !fork.exists());
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn dependents_exit_one_naming_the_moved_child_and_cascade_removes_both() {
    let setup = setup();
    let root = session(&setup, ROOT, None);
    append_moved(&root, ROOT, MOVED);
    let moved = moved_child(&setup, MOVED, ROOT);
    // The moved child continues the parent across projects: deleting the
    // parent names it and refuses without `--cascade`.
    let output = delete(&setup, &["--yes", ROOT]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.contains(MOVED), "{stderr}");
    assert!(stderr.contains("--cascade"), "{stderr}");
    assert!(root.is_dir() && moved.is_dir());
    let output = delete(&setup, &["--cascade", "--yes", ROOT]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), format!("{ROOT}\n{MOVED}\n"));
    assert!(!root.exists() && !moved.exists());
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn without_yes_and_without_a_terminal_it_is_usage_and_removes_nothing() {
    let setup = setup();
    let dir = session(&setup, ROOT, None);
    let output = delete(&setup, &[ROOT]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--yes"));
    assert!(dir.join("events.jsonl").is_file());
    assert!(
        !setup.home().join("logs").join("hub.log").exists(),
        "no hub was started"
    );
}

// `/delete` in the terminal (`docs/tui.md`, "Slash commands"): the
// journeys below drive the built terminal in a pty, type the command,
// and assert on the screen. Every run and every wait carries a
// deadline.

/// The pty size the journeys draw on.
const COLS: u16 = 160;
const ROWS: u16 = 48;

/// A config naming the workspace script, with the hub exiting once no
/// client stays connected. Sessions idle out only after a minute, so a
/// journey's driving never races their idle exit.
fn scripted(setup: &Setup, steps: Value) {
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json",
            "session": {"idle_exit_ms": 60000}, "hub": {"idle_exit_ms": 200}}),
    );
}

/// The stored session ids, or `None` while the first session directory is
/// still being created.
fn stored_at(dir: &Path) -> Option<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("read session directory {}: {error}", dir.display()),
    };
    let mut ids: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    Some(ids)
}

/// The ids of the sessions stored under the workspace, sorted.
fn stored(setup: &Setup) -> Vec<String> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    stored_at(&dir).unwrap_or_else(|| panic!("session directory {} does not exist", dir.display()))
}

/// Waits for exactly `count` sessions to be stored before reading their ids.
fn wait_stored(setup: &Setup, count: usize) -> Vec<String> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    fakes::wait_until("the stored session rows", setup.deadline, move || {
        stored_at(&dir).is_some_and(|ids| ids.len() == count)
    });
    stored(setup)
}

/// Whether the session's lock is held: only a running session holds it.
fn lock_held(setup: &Setup, id: &str) -> bool {
    File::open(session_dir(setup, id).join("session.lock"))
        .is_ok_and(|lock| lock.try_lock().is_err())
}

/// The hub feed on a fresh socket: the watcher keeps its own
/// subscription, so session lines and feed lines never share a socket.
fn feed(client: &Socket) {
    client.send(r#"{"id":"c_feed","command":"feed"}"#);
    let ack = recv_answer(client, "c_feed", "the feed acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_feed");
}

/// A standing rule allowing the blocked script's shell call, so the
/// turn runs instead of waiting on an approval.
fn allow_shell(setup: &Setup, command: &str) {
    write_json(
        &setup.home().join("rules"),
        &json!({"decision": "allow", "tool": "shell", "prefix": command}),
    );
}

/// The terminal on a session blocked in a FIFO shell script, then any
/// trailing scripted steps: types the first driver message, waits for
/// the running job, and returns the run, its guard, the session id and
/// the FIFO handles. The caller holds the handles until the session
/// stops: dropping the release handle would hand the shell end of file
/// and end the turn.
fn launch_blocked(
    setup: &Setup,
    tail: &[Value],
) -> (Run, SessionGuard, String, File, fakes::Watchdog) {
    let held = setup.workspace().join("held");
    fakes::fifo(&held);
    // Held read-write, the FIFO always has a writer: opening it never
    // blocks, and the shell's `read` waits for a line the journey only
    // writes when the turn may end.
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&held)
        .unwrap();
    let script = setup.workspace().join("wait.sh");
    fs::write(
        &script,
        format!("read -r _ < {}\necho done\n", held.display()),
    )
    .unwrap();
    let watchdog = fakes::Watchdog::matching(script.to_str().unwrap());
    let command = format!("sh {}", script.display());
    let mut steps =
        vec![json!({"tool_calls": [{"name": "shell", "arguments": {"command": command}}]})];
    steps.extend(tail.iter().cloned());
    scripted(setup, Value::Array(steps));
    allow_shell(setup, &command);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"run it\r");
    run.wait_screen("the running shell job", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("Jobs") && row.contains("1 running"))
    });
    let a = wait_stored(setup, 1).into_iter().next().unwrap();
    (run, guard, a, release, watchdog)
}

/// Quits the terminal from any screen and reaps what remains: Ctrl-C
/// twice leaves an attached session for the resume line, and quits from
/// home; either way the run exits cleanly.
fn quit_pty(run: Run, guard: SessionGuard) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    guard.wait_gone();
}

/// `/delete` with no session on screen shows the shared no-session
/// notice and changes nothing.
#[test]
fn no_session_delete_shows_the_shared_notice() {
    let setup = Setup::new();
    scripted(&setup, json!([]));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"/delete\r");
    let notice = run.wait_screen("the no-session notice", |grid| {
        grid.contents.contains("No session on screen.")
    });
    assert!(notice.contents.contains("No session on screen."));
    quit_pty(run, guard);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// Esc cancels `/delete` and the live session answers a distinct second
/// prompt in the same uninterrupted session: the log holds one
/// `fiber_started`, both `turn_completed` events, and no `fiber_exited`
/// between them. The log is the assertion source: durable output equals
/// the log byte for byte.
#[test]
fn esc_keeps_the_live_session_for_a_second_prompt() {
    let setup = Setup::new();
    scripted(&setup, json!([{"text": ["first."]}, {"text": ["second."]}]));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"run it\r");
    let first = run.wait_screen("the first reply", |grid| grid.contents.contains("first."));
    assert!(first.contents.contains("run it"));
    let a = wait_stored(&setup, 1).into_iter().next().unwrap();
    run.write(b"/delete\r");
    let question = run.wait_screen("the delete question", |grid| {
        grid.contents.contains("It cannot be undone")
    });
    assert!(question.contents.contains("It cannot be undone"));
    run.write(b"\x1b[27u");
    let back = run.wait_screen("the session after Esc", |grid| {
        grid.contents.contains("first.") && !grid.contents.contains("It cannot be undone")
    });
    assert!(back.contents.contains("first."));
    assert!(
        lock_held(&setup, &a),
        "the session is still running after Esc"
    );
    run.write(b"again\r");
    run.wait_screen("the second reply", |grid| grid.contents.contains("second."));
    let dir = session_dir(&setup, &a);
    assert!(dir.is_dir(), "the session directory stays");
    quit_pty(run, guard);
    let events: Vec<Value> = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds = support::kinds(&events);
    assert!(
        !kinds.contains(&"fiber_exited"),
        "no exit between the turns: {kinds:?}"
    );
    // Each turn opens its one step before the assistant message
    // (`docs/loop.md`, "One step").
    assert_eq!(
        kinds,
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "the session start and both turns in order"
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// `/delete` on the live parent stops it through the app and deletes the
/// exact confirmed set: the feed reports the parent leaving exited
/// before the parent's and both forks' directories are gone, and home is
/// empty after.
#[test]
fn delete_after_stop_completion_removes_the_parent_and_its_forks() {
    let setup = Setup::new();
    let (mut run, guard, a, _release, _watchdog) = launch_blocked(&setup, &[]);
    let b = "s_00000000000000b1";
    let c = "s_00000000000000c1";
    session(&setup, b, Some(&a));
    session(&setup, c, Some(&a));
    let feed_watch = Socket::connect(setup.deadline, &setup.hub_socket());
    feed(&feed_watch);
    assert!(lock_held(&setup, &a), "the parent is live at delete time");
    run.write(b"/delete\r");
    let question = run.wait_screen("the delete question", |grid| {
        grid.contents.contains("It cannot be undone")
            && grid.contents.contains("delete all")
            && grid.contents.contains(b)
            && grid.contents.contains(c)
    });
    assert!(question.contents.contains("It cannot be undone"));
    run.write(b"\r");
    let leaving = a.clone();
    until(&feed_watch, "the parent to leave", move |line| {
        line["kind"] == "session_left"
            && line["payload"]["session_id"] == leaving.as_str()
            && line["payload"]["how"] == "exited"
    });
    // Home only shows after the delete answers, and the hub removes the
    // directories before answering, so by the time home is empty the
    // exact set is gone: no polling, no race with a loaded machine.
    run.wait_screen("home after delete", |grid| {
        grid.contents.contains("No sessions yet.")
    });
    assert!(
        !session_dir(&setup, &a).exists()
            && !session_dir(&setup, b).exists()
            && !session_dir(&setup, c).exists(),
        "the parent and both forks are gone"
    );
    drop(feed_watch);
    quit_pty(run, guard);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// A fork held at confirm time refuses the whole delete at its stop: the
/// live parent leaves exited on the feed, then the hub refuses the held
/// fork's close as `session_held`, so no delete goes out. The refusal
/// names the running fork, both directories stay, and the attached
/// screen stays.
#[test]
fn held_fork_refusal_keeps_both_sessions() {
    let setup = Setup::new();
    let (mut run, guard, a, _release, _watchdog) = launch_blocked(&setup, &[]);
    let b = "s_00000000000000b1";
    let b_dir = session(&setup, b, Some(&a));
    let held = File::open(b_dir.join("session.lock")).unwrap();
    held.try_lock().unwrap();
    assert!(lock_held(&setup, b), "the fork reads held");
    let feed_watch = Socket::connect(setup.deadline, &setup.hub_socket());
    feed(&feed_watch);
    assert!(lock_held(&setup, &a), "the parent is live at delete time");
    run.write(b"/delete\r");
    let question = run.wait_screen("the delete question", |grid| {
        grid.contents.contains("It cannot be undone")
            && grid.contents.contains("delete all")
            && grid.contents.contains(b)
    });
    assert!(question.contents.contains(b));
    run.write(b"\r");
    let leaving = a.clone();
    until(&feed_watch, "the parent to leave", move |line| {
        line["kind"] == "session_left"
            && line["payload"]["session_id"] == leaving.as_str()
            && line["payload"]["how"] == "exited"
    });
    // The delete's exact-set refusal carries the structured `held`
    // data: the busy fork is named in the notice. The full notice is
    // `Not deleted: {b} is running again. Nothing was deleted.`
    // (`delete.rs`, "Not deleted"; the refusal's `session_held` code).
    let refused = run.wait_screen("the held refusal", |grid| {
        grid.contents.contains("session_held")
            && grid.contents.contains("Not deleted:")
            && grid.contents.contains(&format!("{b} is running again"))
            && grid.contents.contains("Nothing was deleted")
    });
    assert!(refused.contents.contains("session_held"));
    assert!(refused.contents.contains("Not deleted:"));
    assert!(refused.contents.contains(&format!("{b} is running again")));
    assert!(refused.contents.contains("Nothing was deleted"));
    assert!(
        refused.contents.contains("turns"),
        "the attached screen stays"
    );
    assert!(session_dir(&setup, &a).exists(), "the stopped parent stays");
    assert!(b_dir.exists(), "the held fork stays");
    drop(feed_watch);
    drop(held);
    quit_pty(run, guard);
    assert!(
        session_dir(&setup, &a).exists(),
        "no retry deletes the parent after"
    );
    assert!(b_dir.exists(), "no retry deletes the fork after");
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// A real live fork delegate is in a cascading delete set but absent from
/// the feed: its close goes out, the parent reports exited, and deletion
/// completes without waiting for a delegate feed row.
#[test]
fn cascading_delete_waits_for_live_parent_exit_not_delegate_feed_row() {
    const COLS: u16 = 160;
    const ROWS: u16 = 48;
    let setup = Setup::new();
    let hold = setup.workspace().join("delegate.hold.fifo");
    fakes::fifo(&hold);
    let hold_arg = hold.to_string_lossy().into_owned();
    let server_parent = fakes::ProviderServer::start_responding(move |request: &fakes::Request| {
        let body = String::from_utf8_lossy(&request.body);
        if !body.contains("\"tools\":[") || body.contains("\"tools\":[]") {
            return text_reply("allow");
        }
        if body.contains("Started delegate") {
            return hello();
        }
        if body.contains("you are a fork of session") {
            return stream(&[function_call(
                "call_hold",
                "shell",
                &json!({"command": format!("read line < '{hold_arg}'")}),
            )]);
        }
        stream(&[function_call(
            "call_fork",
            "delegate_fork",
            &json!({"description": "work", "prompt": "run the fork"}),
        )])
    })
    .unwrap();
    for (name, server) in [("pa", &server_parent)] {
        let source = setup.root.path().join(format!("src-{name}"));
        let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
        manifest["providers"] = Value::Object(
            [(name.to_owned(), json!([format!("{}/v1", server.url())]))]
                .into_iter()
                .collect(),
        );
        write_json(&source.join("extension.json"), &manifest);
        write_json(
            &source.join(format!("providers/{name}.json")),
            &json!({"name": name, "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses",
                    "base_url": format!("{}/v1", server.url()), "context_window": 100000}]}),
        );
        extensions::plan(
            &setup.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}, "hub": {"idle_exit_ms": 200}}),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_fork", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": ""})
        ),
    )
    .unwrap();

    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(
        &setup,
        COLS,
        ROWS,
        &[],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    run.ready();
    run.write(b"run it\r");
    let sessions_dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    fakes::wait_until("the delegate start event", setup.deadline, move || {
        stored_at(&sessions_dir).is_some_and(|ids| {
            ids.iter().any(|id| {
                fs::read_to_string(sessions_dir.join(id).join("events.jsonl"))
                    .is_ok_and(|events| events.contains("delegate_started"))
            })
        })
    });
    let parent = stored(&setup)
        .into_iter()
        .find(|id| {
            fs::read_to_string(session_dir(&setup, id).join("events.jsonl"))
                .is_ok_and(|events| events.contains("delegate_started"))
        })
        .unwrap();
    let parent_dir = session_dir(&setup, &parent);
    let events = fs::read_to_string(parent_dir.join("events.jsonl")).unwrap();
    let delegate = events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["kind"] == "delegate_started")
        .unwrap()["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(delegate, parent);
    fakes::wait_until(
        "the parent's delegate-fork turn to finish",
        setup.deadline,
        move || {
            fs::read_to_string(parent_dir.join("events.jsonl"))
                .is_ok_and(|events| events.contains("turn_completed"))
        },
    );
    // A writer opening the FIFO proves the real delegate shell is blocked
    // in its read, so the delegate is still running when delete confirms.
    let hold_writer = support::bounded(setup.deadline, "the delegate shell to hold", {
        let hold = hold.clone();
        move || fs::OpenOptions::new().write(true).open(hold).unwrap()
    });

    let feed_watch = Socket::connect(setup.deadline, &setup.hub_socket());
    feed(&feed_watch);
    let parent_status = parent.clone();
    until(&feed_watch, "the parent in the live feed", move |line| {
        line["kind"] == "session_status" && line["session_id"] == parent_status.as_str()
    });
    run.write(b"/delete\r");
    let question = run.wait_screen("the cascading delete question", |grid| {
        grid.contents.contains("It cannot be undone")
            && grid.contents.contains("delete all")
            && grid.contents.contains(&delegate)
    });
    assert!(question.contents.contains(&delegate));
    run.write(b"\r");

    let leaving = parent.clone();
    let exited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        until(&feed_watch, "the parent to leave exited", move |line| {
            line["kind"] == "session_left"
                && line["payload"]["session_id"] == leaving.as_str()
                && line["payload"]["how"] == "exited"
        })
    }));
    assert!(
        exited.is_ok(),
        "parent exit missing; screen: {}; hub: {}; parent log: {}",
        run.screen().contents,
        setup.hub_log(),
        fs::read_to_string(session_dir(&setup, &parent).join("events.jsonl")).unwrap_or_default()
    );
    run.wait_screen("home after the exact-set delete", |grid| {
        grid.contents.contains("No sessions yet.")
    });
    assert!(
        !session_dir(&setup, &parent).exists(),
        "the parent was deleted"
    );
    assert!(
        !session_dir(&setup, &delegate).exists(),
        "the delegate was deleted"
    );
    drop(hold_writer);
    drop(feed_watch);
    quit_pty(run, guard);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// A cascade that changes between the dry run and the confirm refuses as
/// stale after stopping the live parent: the parent leaves exited on the
/// feed, home keeps its stopped row, the removed fork stays removed, and
/// the notice names the set the delete would remove now.
#[test]
fn stale_set_refusal_returns_home_with_the_stopped_parent() {
    let setup = Setup::new();
    let (mut run, guard, a, _release, _watchdog) = launch_blocked(&setup, &[]);
    let b = "s_00000000000000b1";
    let c = "s_00000000000000c1";
    session(&setup, b, Some(&a));
    session(&setup, c, Some(&a));
    let feed_watch = Socket::connect(setup.deadline, &setup.hub_socket());
    feed(&feed_watch);
    assert!(lock_held(&setup, &a), "the parent is live at delete time");
    run.write(b"/delete\r");
    run.wait_screen("the delete question", |grid| {
        grid.contents.contains("It cannot be undone")
            && grid.contents.contains("delete all")
            && grid.contents.contains(b)
            && grid.contents.contains(c)
    });
    // The cascade changes while the question stands: the CLI removes one
    // fork, which the confirm must not silently drop from its set.
    let output = delete(&setup, &["--yes", b]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    // The CLI delete is synchronous: once it exits zero, the fork is
    // gone.
    assert!(
        !session_dir(&setup, b).exists(),
        "the CLI removed the fork while the question stood"
    );
    run.write(b"\r");
    let leaving = a.clone();
    until(&feed_watch, "the parent to leave", move |line| {
        line["kind"] == "session_left"
            && line["payload"]["session_id"] == leaving.as_str()
            && line["payload"]["how"] == "exited"
    });
    let mut remaining = [a.clone(), c.to_owned()];
    remaining.sort();
    let expected = format!(
        "The sessions this delete would remove are now {}. Nothing was deleted.",
        remaining
            .iter()
            .map(|id| format!("`{id}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let home = run.wait_screen("home with the stopped parent", |grid| {
        grid.contents.contains(&expected) && grid.contents.contains(c)
    });
    assert!(home.contents.contains(&expected));
    assert!(session_dir(&setup, &a).exists(), "the stopped parent stays");
    assert!(session_dir(&setup, c).exists(), "the kept fork stays");
    drop(feed_watch);
    quit_pty(run, guard);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}
