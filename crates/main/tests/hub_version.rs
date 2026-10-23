//! Binary-level test for #2964 (`docs/tui.md`, "Notices" and "The session
//! list"): against a fake hub whose `hub_hello` names another Fiber build,
//! the real binary shows one `hub_version` notice naming both builds, says
//! on the unreadable row which build its session runs, and the rail's
//! CANNOT ATTACH card hides in this terminal through its ✕ without sending
//! `dismiss`. A second test reads the real hub's `hub_hello` for this
//! build's commit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use support::pty::{Colour, Grid, Run};
use support::*;

/// The fake hub's build: a version and a full commit this terminal was not
/// built from, so the difference shows wherever the terminal names it.
const HUB_VERSION: &str = "9.9.9";
const HUB_COMMIT: &str = "2121f38d1e4f0a9b6c3d2e1f0a9b8c7d6e5f4a3b";
/// The hub's build as `fiber --version` writes it: the version, then the
/// first 7 characters of the commit.
const HUB_TEXT: &str = "9.9.9 (2121f38)";

/// This terminal's own build as `fiber --version` writes it: the package
/// version, then the first 7 characters of the recorded commit, when the
/// build recorded one.
fn own_build_text() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match option_env!("FIBER_COMMIT") {
        Some(commit) if !commit.is_empty() => {
            format!("{version} ({})", commit.get(..7).unwrap_or(commit))
        }
        _ => version.to_owned(),
    }
}

/// Whether any screen row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.contents.contains(needle)
}

/// The first row holding `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

const SELECTION: Colour = Colour::Rgb(0x26, 0x4f, 0x78);

/// Whether `needle` is on home's selection bar.
fn selected(grid: &Grid, needle: &str) -> bool {
    let Some(y) = grid.rows.iter().position(|row| row.contains(needle)) else {
        return false;
    };
    let x = column_at(&grid.rows[y], needle);
    grid.cell(x, u16::try_from(y).unwrap()).bg == SELECTION
}

/// The column of `needle` in `row`, counting terminal cells.
fn column_at(row: &str, needle: &str) -> u16 {
    let chars: Vec<char> = row.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    chars
        .windows(needle.len().max(1))
        .position(|window| window == needle.as_slice())
        .map(|at| u16::try_from(at).unwrap())
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))
}

/// A readable `session_status` payload for `id`, shaped like the delegate
/// status in `session_list.rs`.
fn readable_status(name: &str, workspace: &str) -> Value {
    json!({
        "name": name,
        "workspace": workspace,
        "project": "p",
        "model": "m",
        "state": "idle",
        "since": 0,
        "started": 0,
        "calls": 0,
        "turns": 0,
        "spend": {"tokens": {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0}, "cost": 0.0, "subscription_cost": 0.0},
        "delegates": 0,
        "jobs": 0,
        "clients": 0,
    })
}

/// The fake hub: one connection, a `hub_hello` naming another Fiber build,
/// an acknowledgement for every command line keyed to its `command_id`
/// (`recent` lists no sessions), the feed's two statuses after its `feed`
/// is accepted, and the socket file removed at end of file so the run's
/// hub-exit wait ends. Every read and write takes what remains of the
/// test's deadline, so a stall fails instead of hanging.
fn fake_hub(
    listener: UnixListener,
    socket: PathBuf,
    deadline: Deadline,
    readable_id: String,
    unreadable_id: String,
    workspace: String,
    commands: Arc<Mutex<Vec<String>>>,
) {
    let (stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(deadline.left())).unwrap();
    stream.set_write_timeout(Some(deadline.left())).unwrap();
    let mut write = stream.try_clone().unwrap();
    let mut read = BufReader::new(stream);
    let mut send = |line: &Value| {
        write.write_all(format!("{line}\n").as_bytes()).unwrap();
    };
    send(&json!({
        "kind": "hub_hello",
        "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"fiber_version": HUB_VERSION, "fiber_commit": HUB_COMMIT},
    }));
    let name_a = "alpha readable session";
    let mut fed = false;
    loop {
        let mut raw = String::new();
        match read.read_line(&mut raw) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => panic!("reading the terminal's command: {error}"),
        }
        let command: Value = serde_json::from_str(&raw).unwrap();
        let id = command["id"].as_str().unwrap().to_owned();
        let name = command["command"].as_str().unwrap().to_owned();
        commands.lock().unwrap().push(name.clone());
        let result = if name == "recent" {
            json!({"sessions": []})
        } else {
            json!({})
        };
        let mut answer = json!({
            "kind": "command_accepted",
            "ts": 0,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": id, "result": result},
        });
        if let Some(session) = command.get("session_id").and_then(Value::as_str) {
            answer["session_id"] = Value::String(session.to_owned());
        }
        send(&answer);
        read.get_ref()
            .set_read_timeout(Some(deadline.left()))
            .unwrap();
        if name == "feed" && !fed {
            fed = true;
            for (id, payload) in [
                (readable_id.clone(), readable_status(name_a, &workspace)),
                (unreadable_id.clone(), json!({})),
            ] {
                send(&json!({
                    "kind": "session_status",
                    "session_id": id,
                    "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": payload,
                }));
            }
        }
    }
    fs::remove_file(&socket).unwrap();
}

#[test]
fn a_hub_on_another_build_names_it_and_its_rows() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let run_dir = setup.home().join("run");
    fs::create_dir_all(&run_dir).unwrap();
    let socket = run_dir.join("hub");
    let listener = UnixListener::bind(&socket).unwrap();
    let readable_id = "s_00000000000000a1".to_owned();
    let unreadable_id = "s_00000000000000b2".to_owned();
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let hub = {
        let socket = socket.clone();
        let deadline = setup.deadline;
        let readable_id = readable_id.clone();
        let unreadable_id = unreadable_id.clone();
        let workspace = workspace.clone();
        let commands = Arc::clone(&commands);
        thread::spawn(move || {
            fake_hub(
                listener,
                socket,
                deadline,
                readable_id,
                unreadable_id,
                workspace,
                commands,
            )
        })
    };

    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    // The end of the first frame proves the input reader runs before
    // any typing goes out.
    run.ready();

    // One `hub_version` notice names both builds, each as
    // `fiber --version` writes it.
    let own = own_build_text();
    let notice = format!(
        "hub_version · The hub runs Fiber {HUB_TEXT} (this terminal: {own}). `fiber stop` restarts it on the next launch."
    );
    let screen = run.wait_screen("the hub_version notice", |screen| {
        shows(screen, "hub_version") && shows(screen, "The hub runs Fiber 9.9.9 (2121f38)")
    });
    assert!(
        shows(&screen, notice.as_str()),
        "the notice names both builds: {}",
        screen.contents
    );

    // The unreadable row says which build its session runs.
    let reason = format!("Cannot attach: this session runs Fiber {HUB_TEXT}");
    let home = run.wait_screen("the unreadable row's reason", |screen| {
        shows(screen, "alpha readable session") && shows(screen, reason.as_str())
    });
    assert!(
        shows(&home, reason.as_str()),
        "the row names the hub's build: {}",
        home.contents
    );

    // Opening the readable row attaches through the keyboard: a grid match
    // alone is no readiness signal for pointer input
    // (`docs/testing/screens.md`), so Up walks from the entry bar into the
    // list and Enter opens the focused row, after the frame showing the row
    // is final.
    run.write(b"\x1b[A");
    let frame = run.wait_screen("a home row focused", |screen| {
        selected(screen, "alpha readable session") || selected(screen, "Cannot attach")
    });
    if !selected(&frame, "alpha readable session") {
        // The last row (the unreadable one) took focus; one step up
        // reaches the readable row, and only that step's redraw draws
        // the selection bar there, so no stale frame satisfies the wait.
        run.write(b"\x1b[A");
        run.wait_screen("the readable row focused", |screen| {
            selected(screen, "alpha readable session")
        });
    }
    run.write(b"\r");
    let rail = run.wait_screen("the rail's CANNOT ATTACH card", |screen| {
        !shows(screen, "Enter starts a session")
            && shows(screen, "CANNOT ATTACH")
            && shows(screen, "✕")
    });
    let card_row = row_with(&rail, "CANNOT ATTACH");
    assert!(
        rail.rows[usize::from(card_row)].contains('✕'),
        "the card's first row carries the same ✕: {}",
        rail.contents
    );

    // The rail takes focus on the open session's card. The hidden card
    // sits in its own project group, so four steps down reach its ✕: that
    // group's "+", its "N ✓", the card, then the ✕. Enter on it hides the
    // card in this terminal.
    run.write(b"\x1b[D");
    run.wait_screen("the focused rail", |screen| shows(screen, "↑↓ move"));
    run.write(b"\x1b[B\x1b[B\x1b[B\x1b[B\r");
    let hidden = run.wait_screen("the hidden CANNOT ATTACH card", |screen| {
        !shows(screen, "CANNOT ATTACH")
    });
    assert!(
        shows(&hidden, "alpha readable"),
        "hiding keeps the open session: {}",
        hidden.contents
    );

    // Quitting restores the primary screen; the hub-exit wait ends when
    // the fake hub removes the socket at end of file.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(0));
    hub.join().unwrap();
    let sent = commands.lock().unwrap();
    assert!(
        !sent.iter().any(|command| command == "dismiss"),
        "the ✕ sent no dismiss: {sent:?}"
    );
}

#[test]
fn the_hub_hello_carries_this_builds_commit() {
    let setup = Setup::new();
    let hub = Arc::new(Mutex::new(None));
    let (_client, hello) = connect_hub(&setup, &hub);
    assert_eq!(hello["kind"], "hub_hello");
    assert_eq!(
        hello["payload"]["fiber_commit"],
        match option_env!("FIBER_COMMIT") {
            Some(commit) => Value::String(commit.to_owned()),
            None => Value::Null,
        },
        "{hello}"
    );
    if let Some(running) = hub.lock().unwrap().take() {
        running.kill_and_wait();
    }
}
