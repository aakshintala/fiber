//! A remote terminal edits a changed workspace file through the hub
//! (`docs/tui.md`, "Changed files"): the binary-level acceptance for the
//! remote Ctrl+G path. Through the built binary on a pty, a client of the
//! [`hub_remote_terminal`](hub_remote_terminal.rs) jig's listed hub opens a
//! session whose scripted model appends a line to the hub's committed file,
//! and opens Changed files: Ctrl+G fetches the hub file and opens a client
//! editor on the temporary copy, and the returned copy writes back to the
//! hub, so the hub file holds the edit and the diff and numstat reread it.
//! A second Ctrl+G with a hub-side edit in mid-editor leaves the file
//! unwritten with the changed-file notice, and the view still rereads the
//! diff and numstat.
//!
//! The jig (world, pairing, session start and reap) is the
//! hub_remote_terminal.rs one, copied: that file is outside this lane's
//! fence, so this file carries its own copy.
//!
//! The editor script records its arguments, prints its start marker, blocks
//! reading the test's sentinel FIFO, then appends a line to the file it is
//! given: `$1` is `+N`, `$2` the path. The marker proves the fetch landed,
//! so the test's hub-side edit lands after it; the blocked read lets that
//! edit land between the fetch and the write-back, proving the stale path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

#[path = "support/hub_start.rs"]
mod hub_start;
mod support;

/// A completed frame's end: synchronized output's close marker, which the
/// TUI brackets every frame with. Waiting for it after an input proves the
/// frame drew after that input instead of matching a stale grid
/// (`docs/testing.md`, "Screens").
const FRAME_END: &[u8] = b"\x1b[?2026l";

use serde_json::json;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Output;
use support::pty::{Grid, KITTY_PUSH, Run};
use support::*;

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

fn run(setup: &Setup, args: &[&str]) -> Output {
    run_to_exit(setup.deadline, "the hub list command", setup.fiber(args))
}

fn succeeds(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn add(setup: &Setup, name: &str, address: &str, code: &str, default: bool) -> Output {
    let input = setup.root.path().join("pairing-input");
    fs::write(&input, format!("{code}\n")).unwrap();
    let mut args = vec!["hub", "add", name, address];
    if default {
        args.push("--default");
    }
    let mut command = setup.fiber(&args);
    command.stdin(fs::File::open(input).unwrap());
    run_to_exit(
        setup.deadline,
        "hub add with a pairing code on stdin",
        command,
    )
}

/// The paired world the cases share: `a` serves its hub on a free port,
/// `b` is paired with it, holds its own git repository with its own
/// file, and configures its own model. The hub connection is held so the
/// hub never idles out mid-case. The hub start (port pick, hello wait,
/// taken-port retry, diagnostics) is the shared `hub_start` helper's, which
/// adds no wait, poll or retry of this test's own.
struct World {
    a: Setup,
    b: Setup,
    process: HubProc,
    hub: doors::hub::Hub,
}

fn world(default: bool) -> World {
    let a = Setup::new();
    let b = Setup::new();
    // The listed hub must ignore the client's repository scope, so the
    // client launches from its own git repository with its own file.
    let mut git = std::process::Command::new("git");
    git.args(["init", "-q"])
        .arg(b.workspace())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", b.root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    succeeds(&run_to_exit(b.deadline, "the client's git repository", git));
    fs::write(b.workspace().join("blocal"), "client only").unwrap();
    // The client configures its own model and keys: the remote home must
    // show the hub's, never these.
    write_json(
        &b.home().join("config.json"),
        &json!({"model": "client/other", "tui": {"hover": false}}),
    );
    // The hub machine holds a file that exists only in its workspace, and
    // one recent row naming that workspace. The hub lists a recent row
    // only while its session directory stands, so the seed keeps the
    // exited session's directory. The hub's `@` search answers tracked
    // files, so the workspace is a git repository with the hub file and
    // the scripted model's script committed: the script's edit is the
    // change the client will edit through the hub.
    fs::write(a.workspace().join("only-on-a.txt"), "hub only\n").unwrap();
    write_json(
        &a.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "edit", "arguments": {"path": "only-on-a.txt", "edits": [
                {"old_text": "hub only\n", "new_text": "hub only\nhub line\n"},
            ]}}]},
            {"text": ["Done."]},
        ]}),
    );
    let mut git = std::process::Command::new("git");
    git.args(["init", "-q"])
        .arg(a.workspace())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", a.root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    succeeds(&run_to_exit(a.deadline, "the hub's git repository", git));
    for args in [
        vec!["config", "user.email", "test@fiber.dev"],
        vec!["config", "user.name", "fiber test"],
        vec!["add", "only-on-a.txt", "s.json"],
        vec!["commit", "-q", "-m", "add only-on-a"],
    ] {
        let mut git = std::process::Command::new("git");
        git.args(["-C"])
            .arg(a.workspace())
            .args(&args)
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", a.root.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        succeeds(&run_to_exit(
            a.deadline,
            &format!("the hub's git {args:?}"),
            git,
        ));
    }
    let row = json!({"session_id":"s_remote", "ts":1, "project":"remote-project", "workspace":a.workspace().display().to_string(), "name":"remote session", "how":"exited"});
    fs::write(a.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    fs::create_dir_all(a.home().join("projects/remote-project/sessions/s_remote")).unwrap();
    // The hub machine already imported: the first-run offer reads the
    // hub's receipts, and these cases cover the picker and resume
    // flows with the offer down, never the offer itself.
    for receipt in ["import-claude-code.json", "import-codex.json"] {
        write_json(
            &a.home().join(receipt),
            &json!({"source": receipt, "imported_at": 1}),
        );
    }
    // Serves the installed hub with the scripted model (the script lives
    // in the hub workspace: `docs/model-routing.md`, "The scripted
    // provider"), idling out a second after its last client leaves.
    // `~/` on the hub expands against the inherited `HOME`, which the
    // harness otherwise points at the setup root: point it at the hub's
    // home, as production has it, so the picker browses the hub's home.
    let served = hub_start::serve_installed(
        &a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &a.home().join("config.json"),
                &json!({"model": "scripted/s.json", "hub": {"port": port, "idle_exit_ms": 1000}}),
            );
            let mut serve = a.fiber(&["hub", "serve", "--installed"]);
            serve.env("HOME", a.home());
            serve
        },
    );
    let port = served.port;
    let (hub, process) = (served.hub, served.process);
    assert_eq!(hub.1.kind, "hub_hello");
    let address = format!("ws://127.0.0.1:{port}");
    let code = run(&a, &["hub", "pair", "laptop"]);
    succeeds(&code);
    let code = String::from_utf8(code.stdout).unwrap();
    succeeds(&add(&b, "box", &address, code.trim(), default));
    World { a, b, process, hub }
}

/// Reaps a world whose cases started sessions: the sessions die first,
/// then the held hub connection drops and the hub exits on `TERM`.
fn reap(world: World, workspace: &str, guard: SessionGuard) {
    fakes::kill_matching(workspace).unwrap();
    guard.wait_gone();
    drop(world.hub);
    world.process.kill("TERM");
    assert_eq!(world.process.wait().code(), Some(143));
}

/// The recents section and the workspace row under it: the picker shows
/// the hub's recent workspace, while the row's cut path cannot name it
/// in full.
fn recents_row(screen: &Grid) -> bool {
    let Some(at) = screen
        .rows
        .iter()
        .position(|row| row.contains("── recents ──"))
    else {
        return false;
    };
    screen
        .rows
        .get(at + 1)
        .is_some_and(|row| row.contains('/') && !row.contains("──"))
}

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
        .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents))
}

#[test]
fn changed_files_editor_remote() {
    let world = world(true);
    // The editor records its arguments, prints its start marker, waits
    // for the test's sentinel, then appends a line to the file it is
    // given: `$1` is `+N`, `$2` the path. The marker proves the fetch
    // landed, so the test's hub-side edit lands after it; the sentinel
    // lets that edit land between the fetch and the write-back.
    let script = world.b.workspace().join("editor.sh");
    let argv = world.b.workspace().join("argv");
    let sentinel = world.b.workspace().join("sentinel");
    // The sentinel is a FIFO the editor blocks reading (`docs/testing.md`,
    // "Waits and timeouts"): no sleep or poll. The start marker proves the
    // fetch landed, so the test's release lands after it; the blocked read
    // lets the hub-side edit land between the fetch and the write-back.
    fakes::fifo(&sentinel);
    fs::write(
        &script,
        format!(
            "printf '%s\\n' \"$@\" >> \"{}\"\nprintf 'fiber-editor-started\\n'\nread line < \"{}\"\nprintf 'editor line\\n' >> \"$2\"\n",
            argv.display(),
            sentinel.display(),
        ),
    )
    .unwrap();
    let editor_cmd = format!("/bin/sh {}", script.display());
    let argv_text = argv.display().to_string();
    let sentinel_text = sentinel.display().to_string();
    let env = [
        ("EDITOR", editor_cmd.as_str()),
        ("ARGV", argv_text.as_str()),
        ("SENTINEL", sentinel_text.as_str()),
        ("FIBER_TEST_FAKE_KEY", "sk-test"),
        TRUECOLOUR[0],
        TRUECOLOUR[1],
        TRUECOLOUR[2],
    ];
    let mut run = Run::spawn(&world.b, 200, 32, &["--hub", "box"], &env);
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.wait_screen("the hub home picker", |screen| {
        shows(screen, "Workspaces")
            && shows(screen, "› ~/")
            && shows(screen, "── recents ──")
            && recents_row(screen)
            && shows(screen, "remote session")
    });
    run.write(&[0x7f; 64]);
    run.write(b"\r");
    run.wait_screen("the chosen hub workspace", |screen| {
        shows(screen, "[w]") && !shows(screen, "Workspaces")
    });
    let ws = world.a.workspace().display().to_string();
    let guard = support::SessionGuard::arm(world.a.deadline, &ws);
    // The scripted model appends the hub-side line through Fiber's edit
    // tool, so the hub file is dirty the same way a person's edit would
    // leave it.
    let from = run.output().len();
    run.write(b"add a hub line\r");
    run.wait_screen("the reply", |screen| shows(screen, "Done."));
    run.turn_finished(from);
    // The Changed files list opens through `/diff`, keyboard-only: no
    // pointer input, so no row-position signal is needed
    // (`docs/testing/screens.md`, "A pointer event…"). The frame-end
    // marker proves the list drew after the command, and the file row
    // proves the swapped list is on screen.
    let list_from = run.output().len();
    run.write(b"/diff\r");
    run.wait_bytes(list_from, FRAME_END, "the changed files frame");
    run.wait_screen("the changed files card", |screen| {
        screen.rows[0].contains("Changed files") && shows(screen, "only-on-a.txt")
    });
    // Enter opens the file's diff; Ctrl+G fetches the hub file and
    // opens the client editor on the temporary copy.
    let diff_from = run.output().len();
    run.write(b"\r");
    run.wait_bytes(diff_from, FRAME_END, "the file diff frame");
    run.wait_screen("the file diff", |screen| {
        screen.rows[0].contains("Changed files") && shows(screen, "+hub line")
    });
    run.write(b"\x07");
    // The marker proves the fetch landed and the editor waits; releasing
    // the sentinel lets it append: the copy writes back on the fetched
    // version, so the hub file holds the edit. The reread screen proves
    // the editor ran and exited, so the recorded arguments and the hub
    // file read whole with no wait.
    run.wait_screen("the editor start", |screen| {
        shows(screen, "fiber-editor-started")
    });
    fs::write(&sentinel, "go\n").unwrap();
    let grid = run.wait_screen("the edited hunk", |screen| {
        screen.rows[0].contains("Changed files") && shows(screen, "+editor line")
    });
    let recorded: Vec<String> = fs::read_to_string(&argv)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0], "+1");
    let hub_file = fs::canonicalize(world.a.workspace().join("only-on-a.txt")).unwrap();
    assert_ne!(
        PathBuf::from(&recorded[1]),
        hub_file,
        "the editor opens the temporary copy, not the hub file"
    );
    assert!(
        recorded[1].ends_with("only-on-a.txt"),
        "the copy keeps the hub file's name: {}",
        recorded[1]
    );
    assert_eq!(
        fs::read_to_string(&hub_file).unwrap(),
        "hub only\nhub line\neditor line\n"
    );
    let _ = row_with(&grid, "+editor line");
    // Back in the list, the row's added count went up: the hub line and
    // the editor line are both new against HEAD. The figures answer
    // round-trips through the hub, so the wait names the new counts and
    // the list footer, which neither the diff card nor the stale row
    // matches.
    let list_from = run.output().len();
    run.write(b"\x1b[D");
    run.wait_bytes(list_from, FRAME_END, "the returned list frame");
    let grid = run.wait_screen("the refreshed row", |screen| {
        shows(screen, "Enter show diff") && shows(screen, "only-on-a.txt · +2 −0")
    });
    let _ = row_with(&grid, "only-on-a.txt · +2 −0");
    // A second Ctrl+G fetches the edited file; a hub-side edit in
    // mid-editor leaves the copy unwritten with the changed-file
    // notice, and the view still rereads the diff and numstat.
    let diff_from = run.output().len();
    run.write(b"\r");
    run.wait_bytes(diff_from, FRAME_END, "the file diff frame again");
    run.wait_screen("the file diff again", |screen| {
        screen.rows[0].contains("Changed files") && shows(screen, "+editor line")
    });
    run.write(b"\x07");
    // The marker proves the fetch landed, so the hub-side edit lands
    // after it and the write-back finds the stale version.
    run.wait_screen("the second editor start", |screen| {
        shows(screen, "fiber-editor-started")
    });
    let mut hub_text = fs::read_to_string(&hub_file).unwrap();
    hub_text.push_str("hub-side\n");
    fs::write(&hub_file, hub_text).unwrap();
    fs::write(&sentinel, "go\n").unwrap();
    let grid = run.wait_screen("the stale notice and reread diff", |screen| {
        shows(screen, "changed since it was read") && shows(screen, "+hub-side")
    });
    let recorded: Vec<String> = fs::read_to_string(&argv)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(recorded.len(), 4, "{recorded:?}");
    let _ = row_with(&grid, "+hub-side");
    assert_eq!(
        fs::read_to_string(&hub_file).unwrap(),
        "hub only\nhub line\neditor line\nhub-side\n",
        "the stale write-back leaves the hub file unwritten"
    );
    run.write(b"\x03\x03\r");
    let sockets: Vec<String> = fs::read_dir(world.a.home().join("run"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "hub")
        .collect();
    assert_eq!(sockets.len(), 1, "the hub started one session");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen
            && !grid.hide_cursor
            && grid
                .contents
                .contains(&format!("fiber resume {} --hub box", sockets[0]))
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    assert!(
        !world.b.hub_socket().exists(),
        "no hub starts on the client"
    );
    reap(world, &ws, guard);
}
