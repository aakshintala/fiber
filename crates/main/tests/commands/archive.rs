//! Binary-level tests of archiving (`docs/invocation.md`, "Archiving"): a
//! real exited session under the fake provider, archive by id and by
//! prefix, the list with and without `--archived`, the log's last
//! envelope, resume unarchiving, continue skipping archived sessions
//! locally and on a listed hub, and running/held refusals that append
//! nothing. Every run and every wait carries a deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use crate::hub_start;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};
use support::pty::Run;
use support::*;

const COLS: u16 = 160;
const ROWS: u16 = 48;

/// Fiber home with the fake provider answering `replies` in order, and
/// the hub idling out 200 ms after its last client leaves, so no hub
/// lingers past the test. The server is returned: dropping it stops
/// answering, so the test holds it to the end.
fn installed(setup: &Setup, replies: Vec<fakes::Response>) -> fakes::ProviderServer {
    let server = fakes::ProviderServer::start(replies).unwrap();
    installed_with(setup, &server);
    server
}

/// Fiber home on the already-running fake `server`, with the hub idling
/// out 200 ms after its last client leaves.
fn installed_with(setup: &Setup, server: &fakes::ProviderServer) {
    setup.provider(server);
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["hub"] = json!({"idle_exit_ms": 200});
    write_json(&path, &config);
}

/// Runs `fiber` with `args` headless in its own process group, waiting
/// under the test's deadline.
#[track_caller]
fn headless(setup: &Setup, args: &[&str]) -> std::process::Output {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    run_to_exit(
        setup.deadline,
        &format!("`fiber {}`", args.join(" ")),
        command,
    )
}

fn text(output: &std::process::Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The project's sessions directory, through the canonical workspace.
fn sessions_dir(setup: &Setup) -> PathBuf {
    sessions_dir_in(setup, &fs::canonicalize(setup.workspace()).unwrap())
}

/// The sessions directory for `workspace` under this home.
fn sessions_dir_in(setup: &Setup, workspace: &Path) -> PathBuf {
    log::sessions_dir(&setup.home(), &worktree::project(workspace))
}

/// The stored session ids, sorted.
fn stored_ids(setup: &Setup) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(sessions_dir(setup))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// The one stored session id.
fn only_session(setup: &Setup) -> String {
    let ids = stored_ids(setup);
    assert_eq!(ids.len(), 1, "one session: {ids:?}");
    ids.into_iter().next().unwrap()
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

/// The last complete envelope of the session's log: trailing torn bytes
/// never decide the kind.
fn last_envelope(setup: &Setup, id: &str) -> Value {
    let log = fs::read_to_string(sessions_dir(setup).join(id).join("events.jsonl")).unwrap();
    log.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .next_back()
        .unwrap_or_else(|| panic!("{id} has a complete envelope"))
}

/// The ids `fiber sessions --json` lists with `args`.
fn listed(setup: &Setup, args: &[&str]) -> Vec<String> {
    let mut all = vec!["sessions", "--json"];
    all.extend_from_slice(args);
    let output = headless(setup, &all);
    assert_eq!(output.status.code(), Some(0), "{:?}", text(&output));
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// Runs `fiber ask` to exit and returns the session's id.
fn exited(setup: &Setup, prompt: &str) -> String {
    let asked = headless(setup, &["ask", prompt]);
    assert_eq!(asked.status.code(), Some(0), "{:?}", text(&asked));
    only_session(setup)
}

/// Archives `selector`, printing the full id: the hub the ask started
/// has idled out first, so the archive starts its own hub.
fn archived(setup: &Setup, selector: &str) -> String {
    until_absent(
        setup.deadline,
        &setup.hub_socket(),
        "the ask hub to idle out",
    );
    assert!(
        !setup.hub_socket().exists(),
        "no hub runs before the archive"
    );
    let output = headless(setup, &["sessions", "archive", selector]);
    assert_eq!(output.status.code(), Some(0), "{:?}", text(&output));
    let (out, _) = text(&output);
    let id = out.trim_end().to_owned();
    assert!(
        setup.hub_log().contains("hub_started"),
        "the archive started a hub"
    );
    id
}

#[test]
fn archive_hides_the_session_from_the_list_and_ends_its_log() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![hello()]);
    assert!(!setup.hub_socket().exists(), "no hub runs beforehand");
    let id = exited(&setup, "hi");
    assert_eq!(archived(&setup, &id), id);
    // The archived session leaves the default list and answers the
    // archived one, by id.
    assert_eq!(listed(&setup, &[]), Vec::<String>::new(), "omitted");
    assert_eq!(listed(&setup, &["--archived"]), vec![id.clone()]);
    // The log ends with the durable marker: kind `archived`, empty
    // payload, after the exit.
    let last = last_envelope(&setup, &id);
    assert_eq!(last["kind"], "archived", "{last}");
    assert_eq!(last["payload"], json!({}), "{last}");
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn archive_accepts_a_unique_prefix_and_prints_the_full_id() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![hello()]);
    let id = exited(&setup, "hi");
    assert_eq!(archived(&setup, &id[..4]), id);
    assert_eq!(listed(&setup, &[]), Vec::<String>::new(), "omitted");
    assert_eq!(listed(&setup, &["--archived"]), vec![id]);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn resume_unarchives_and_the_next_exit_lists_it() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![hello(), hello()]);
    let id = exited(&setup, "hi");
    assert_eq!(archived(&setup, &id), id);
    assert_eq!(listed(&setup, &["--archived"]), vec![id.clone()]);
    // Resuming the archived session runs another turn to exit: the
    // explicit resume names it without a flag.
    let resumed = headless(&setup, &["ask", "--resume", &id, "again"]);
    assert_eq!(resumed.status.code(), Some(0), "{:?}", text(&resumed));
    // A new `fiber_started` lands after the archived marker, and the
    // fresh exit's row carries no archive flag.
    let lines: Vec<Value> = fs::read_to_string(sessions_dir(&setup).join(&id).join("events.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    let marker = lines
        .iter()
        .position(|line| line["kind"] == "archived")
        .unwrap_or_else(|| panic!("an archived marker: {lines:?}"));
    assert!(
        lines[marker + 1..]
            .iter()
            .any(|line| line["kind"] == "fiber_started"),
        "a new turn starts after the marker: {lines:?}"
    );
    let last = last_envelope(&setup, &id);
    assert_ne!(last["kind"], "archived", "{last}");
    let recent: Vec<Value> = fs::read_to_string(setup.home().join("recent.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["session_id"] == id)
        .collect();
    let newest = recent
        .last()
        .unwrap_or_else(|| panic!("a recent row: {recent:?}"));
    assert!(
        newest.get("archived").is_none(),
        "the latest row omits archived: {newest}"
    );
    // The normal list includes the session again; the archived one omits it.
    assert_eq!(listed(&setup, &[]), vec![id.clone()]);
    assert_eq!(listed(&setup, &["--archived"]), Vec::<String>::new());
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// Two exited sessions from two asks: the first reply's session and the
/// second's, in ask order.
fn two_exited(setup: &Setup) -> (String, String) {
    let first = headless(setup, &["ask", "first"]);
    assert_eq!(first.status.code(), Some(0), "{:?}", text(&first));
    let older = only_session(setup);
    let second = headless(setup, &["ask", "second"]);
    assert_eq!(second.status.code(), Some(0), "{:?}", text(&second));
    let mut ids = stored_ids(setup);
    assert_eq!(ids.len(), 2, "two sessions: {ids:?}");
    ids.retain(|id| id != &older);
    (older, ids.pop().unwrap())
}

/// Opens `continue` on a terminal and waits for the screen to show
/// `want`, then quits: the session continue opened is the one whose
/// reply draws.
fn continued_shows(setup: &Setup, args: &[&str], want: &str) {
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(setup, COLS, ROWS, args, &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the continued reply", move |grid| {
        grid.contents.contains(want)
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    guard.wait_gone();
}

#[test]
fn continue_skips_the_archived_latest() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![text_reply("First."), text_reply("Second.")]);
    let (_, latest) = two_exited(&setup);
    assert_eq!(archived(&setup, &latest), latest);
    assert_eq!(listed(&setup, &[]).len(), 1, "only the older lists");
    continued_shows(&setup, &["continue"], "First.");
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn continue_with_every_session_archived_is_a_usage_error() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![text_reply("First."), text_reply("Second.")]);
    let (older, latest) = two_exited(&setup);
    assert_eq!(archived(&setup, &older), older);
    assert_eq!(archived(&setup, &latest), latest);
    // No eligible entry is the empty-project usage error
    // (`docs/invocation.md`, "Archiving").
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &["continue"], &[]);
    run.wait_screen("the usage error", |grid| {
        grid.contents
            .contains("No session in this project to continue")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(2));
    drop(guard);
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// A holder thread on the session's lock file: sends `acquired` once it
/// holds the lock, parks until the test drops `release`, then drops the
/// lock and ends. The test joins it before cleanup.
fn hold_lock(
    dir: &Path,
    acquired: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    let path = dir.join("session.lock");
    thread::spawn(move || {
        let lock = File::open(&path).unwrap();
        lock.lock().unwrap();
        acquired.send(()).unwrap_or(());
        let _released = release.recv();
        drop(lock);
    })
}

#[test]
fn archive_refuses_a_running_session_without_appending() {
    let setup = Setup::new();
    // A shell blocked on a FIFO keeps the turn, and the session lock,
    // held while the archive runs.
    let held = setup.workspace().join("held");
    fakes::fifo(&held);
    let _release_fifo = fs::OpenOptions::new()
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
    let _watchdog = fakes::Watchdog::matching(script.to_str().unwrap());
    let command = format!("sh {}", script.display());
    write_json(
        &setup.home().join("rules"),
        &json!({"decision": "allow", "tool": "shell", "prefix": command}),
    );
    let server = fakes::ProviderServer::start_responding(move |request: &fakes::Request| {
        let body = String::from_utf8_lossy(&request.body);
        if !body.contains("\"tools\":[") || body.contains("\"tools\":[]") {
            return text_reply("allow");
        }
        stream(&[function_call(
            "call_wait",
            "shell",
            &json!({"command": command.clone()}),
        )])
    })
    .unwrap();
    installed_with(&setup, &server);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let mut run = Run::spawn(&setup, COLS, ROWS, &[], &[]);
    run.ready();
    run.write(b"run it\r");
    run.wait_screen("the running shell job", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("Jobs") && row.contains("1 running"))
    });
    let id = only_session(&setup);
    let log = sessions_dir(&setup).join(&id).join("events.jsonl");
    // The "1 running" screen above is the readiness signal: the shell job
    // runs, so the session is held without waiting for its log line.
    let refused = headless(&setup, &["sessions", "archive", &id]);
    assert_eq!(refused.status.code(), Some(1));
    let (_, err) = text(&refused);
    assert!(err.contains("held"), "{err}");
    // A live turn keeps appending its own lines (a shell past 30 s is
    // backgrounded and the turn moves on), so byte equality with a live
    // session cannot hold. What the refusal owns is archive writes: no
    // archived marker in the log, no archived row for the session.
    let after: Vec<Value> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        after.iter().all(|line| line["kind"] != "archived"),
        "a refused archive writes no marker"
    );
    // A missing index means no row was ever written, refused or not.
    let recent: Vec<Value> = fs::read_to_string(setup.home().join("recent.jsonl"))
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|line| line["session_id"] == id)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        recent.iter().all(|row| row.get("archived").is_none()),
        "a refused archive writes no row: {recent:?}"
    );
    run.write(b"\x03\x03\r");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    drop(server);
    guard.wait_gone();
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

/// Serves `a`'s hub on a test port and returns the live hub: the asks
/// seed through it, so no local hub lifecycle (and no idle timer) is
/// involved.
fn installed_hub(a: &Setup) -> hub_start::Served {
    hub_start::serve_installed(
        a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            // Preserve the provider config the seed needs: only the hub
            // port is set here.
            let path = a.home().join("config.json");
            let mut config: Value = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(json!({}));
            config["hub"]["port"] = json!(port);
            write_json(&path, &config);
            a.fiber(&["hub", "serve", "--installed"])
        },
    )
}

/// Two exited sessions in different projects on the hub machine, the
/// second newer: what `continue --hub` must choose between. The asks run
/// through the already-served test-port hub, so no local hub lifecycle
/// (and no idle timer) is involved.
fn seed_hub_sessions(a: &Setup) -> (String, String) {
    let first = headless(a, &["ask", "first"]);
    assert_eq!(first.status.code(), Some(0), "{:?}", text(&first));
    let older = only_session(a);
    let other = a.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let mut second = a.fiber(&["ask", "second"]);
    second.current_dir(&other);
    assert_eq!(
        run_to_exit(a.deadline, "the second ask", second)
            .status
            .code(),
        Some(0)
    );
    let latest = fs::read_dir(sessions_dir_in(a, &other))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .pop()
        .unwrap_or_else(|| panic!("one session in other"));
    (older, latest)
}

/// Reaps a served hub: the held connection drops and the hub exits on
/// `TERM`.
fn reap_served(served: hub_start::Served) {
    drop(served.hub);
    served.process.kill("TERM");
    assert_eq!(served.process.wait().code(), Some(143));
}

/// Pairs a fresh client to the already-running hub as `box`.
fn paired_client(a: &Setup, served: &hub_start::Served) -> Setup {
    let b = Setup::new();
    let paired = headless(a, &["hub", "pair", "laptop"]);
    assert_eq!(paired.status.code(), Some(0), "{:?}", text(&paired));
    let code = String::from_utf8(paired.stdout).unwrap();
    let address = format!("ws://127.0.0.1:{}", served.port);
    let input = b.root.path().join("pairing-input");
    fs::write(&input, format!("{}\n", code.trim())).unwrap();
    let mut command = b.fiber(&["hub", "add", "box", &address]);
    command.stdin(File::open(input).unwrap());
    let added = run_to_exit(b.deadline, "hub add with a pairing code", command);
    assert_eq!(added.status.code(), Some(0), "{:?}", text(&added));
    b
}

#[test]
fn continue_hub_skips_the_archived_latest() {
    let a = Setup::new();
    let server =
        fakes::ProviderServer::start(vec![text_reply("First."), text_reply("Second.")]).unwrap();
    installed_with(&a, &server);
    let served = installed_hub(&a);
    let (_, latest) = seed_hub_sessions(&a);
    let b = paired_client(&a, &served);
    // Archive the latest through the listed hub: the client names the
    // session by full id, as `resume --hub` resolves it.
    let archived = headless(&b, &["sessions", "archive", "--hub", "box", &latest]);
    assert_eq!(archived.status.code(), Some(0), "{:?}", text(&archived));
    assert_eq!(text(&archived).0, format!("{latest}\n"));
    // The archived latest is skipped, so the resumed session lives in
    // the hub machine's main workspace: arm before the PTY flow starts
    // so its process dies with this test, pass or panic.
    let resumed_guard = a.workspace().to_string_lossy().into_owned();
    let _resumed = SessionGuard::arm(a.deadline, &resumed_guard);
    let mut run = Run::spawn(&b, COLS, ROWS, &["continue", "--hub", "box"], &[]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_screen("the latest ordinary reply", |grid| {
        grid.contents.contains("First.")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    reap_served(served);
}

#[test]
fn archive_refuses_a_held_lock_without_appending() {
    let setup = Setup::new();
    let _server = installed(&setup, vec![hello()]);
    let id = exited(&setup, "hi");
    let dir = sessions_dir(&setup).join(&id);
    let log = dir.join("events.jsonl");
    let before = fs::read(&log).unwrap();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = hold_lock(&dir, acquired_tx, release_rx);
    setup
        .deadline
        .recv(&acquired_rx)
        .expect("the holder holds the lock");
    let refused = headless(&setup, &["sessions", "archive", &id]);
    assert_eq!(refused.status.code(), Some(1));
    let (_, err) = text(&refused);
    assert!(err.contains("held"), "{err}");
    assert_eq!(
        fs::read(&log).unwrap(),
        before,
        "a refused archive appends nothing"
    );
    drop(release_tx);
    holder.join().unwrap();
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn sessions_list_reports_a_missing_hub_instead_of_success() {
    // Pins the failing exit of the `sessions_list` wrapper: a missing
    // listed hub fails instead of succeeding. Fails when the wrapper
    // always returns `0`.
    let setup = Setup::new();
    let output = headless(&setup, &["sessions", "--hub", "missing"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "a missing hub fails: {:?}",
        text(&output)
    );
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}

#[test]
fn sessions_all_lists_every_project_through_the_wrapper() {
    // Pins `all || is_listed()` in the `sessions_list` wrapper: from a
    // repository, the default lists only this project's session while
    // `--all` lists every project. Fails when `||` reads as `&&`.
    let setup = Setup::new();
    let _server = installed(&setup, vec![hello(), hello()]);
    support::git(setup.deadline, &setup.workspace(), &["init", "--quiet"]);
    let first = exited(&setup, "hi");
    let other = setup.root.path().join("other");
    fs::create_dir_all(&other).unwrap();
    let mut second = setup.fiber(&["ask", "second"]);
    second.current_dir(&other);
    let second_out = run_to_exit(setup.deadline, "the second ask", second);
    assert_eq!(second_out.status.code(), Some(0), "{:?}", text(&second_out));
    let mut ids = stored_ids(&setup);
    let mut other_ids: Vec<String> = fs::read_dir(sessions_dir_in(&setup, &other))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    ids.append(&mut other_ids);
    ids.sort();
    assert_eq!(ids.len(), 2, "two sessions: {ids:?}");
    let default = listed(&setup, &[]);
    assert_eq!(default, vec![first.clone()], "default lists one project");
    let mut all = listed(&setup, &["--all"]);
    all.sort();
    ids.sort();
    assert_eq!(all, ids, "--all lists every project");
    until_absent(setup.deadline, &setup.hub_socket(), "the hub to idle out");
}
