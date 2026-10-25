//! Binary-level tests of `fiber sessions delete --hub` and
//! `fiber sessions prune --hub` (`docs/invocation.md`, "Deleting and
//! pruning"): the client's binary runs against a hub on a test port, and
//! the hub removes only its own state. Every run and every wait carries a
//! deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

#[path = "support/hub_start.rs"]
mod hub_start;
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use support::*;

const ROOT: &str = "s_00000000000000a1";
const FORK: &str = "s_00000000000000b2";
const NEW: &str = "s_00000000000000c3";

fn run(setup: &Setup, args: &[&str]) -> Output {
    run_to_exit(setup.deadline, "the hub client command", setup.fiber(args))
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

/// A hub machine and a client paired to it as `box`: the client selects
/// `box` by default when `default` holds. The served hub stays alive for
/// the test through the returned process.
struct World {
    a: Setup,
    b: Setup,
    served: hub_start::Served,
}

fn world(default: bool) -> World {
    let a = Setup::new();
    let b = Setup::new();
    let served = hub_start::serve_installed(
        &a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &a.home().join("config.json"),
                &json!({"hub": {"port": port}}),
            );
            a.fiber(&["hub", "serve", "--installed"])
        },
    );
    assert_eq!(served.hub.1.kind, "hub_hello");
    let address = format!("ws://127.0.0.1:{}", served.port);
    let code = run(&a, &["hub", "pair", "laptop"]);
    succeeds(&code);
    let code = String::from_utf8(code.stdout).unwrap();
    let added = add(&b, "box", &address, code.trim(), default);
    succeeds(&added);
    assert!(added.stdout.is_empty());
    World { a, b, served }
}

/// Reaps a world: the held hub connection drops and the hub exits on
/// `TERM`, as `hub_list.rs` reaps its served hubs.
fn reap(world: World) {
    drop(world.served.hub);
    world.served.process.kill("TERM");
    assert_eq!(world.served.process.wait().code(), Some(143));
}

/// Runs `fiber` with `args` on the client: stdin null, in its workspace.
fn client(world: &World, args: &[&str]) -> Output {
    let mut command = world.b.fiber(args);
    command.current_dir(world.b.workspace());
    run_to_exit(world.b.deadline, "the hub client command", command)
}

/// Session `id` on the hub machine, continuing `from`: its log with an
/// old last line, and one artifact of ten bytes.
fn hub_session(home: &Path, project: &str, id: &str, from: Option<&str>) -> PathBuf {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": "/w",
        "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "session_id": id, "ts": 0,
        "schema_version": 1, "seq": 0, "payload": payload});
    let last = json!({"kind": "fiber_exited", "session_id": id, "ts": 0,
        "schema_version": 1, "payload": {}});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"0123456789").unwrap();
    dir
}

/// The wall time from a probe file's mtime: binary tests read the clock
/// from the filesystem, never from `SystemTime::now`.
fn wall(root: &Path) -> std::time::SystemTime {
    let probe = root.join("probe");
    fs::write(&probe, b"probe").unwrap();
    fs::metadata(&probe).unwrap().modified().unwrap()
}

/// An exited row for `id`, so the hub's `sessions` answer names it.
fn recent_row(id: &str) -> Value {
    json!({"session_id": id, "ts": 1, "project": "p",
        "workspace": "/w", "name": "remote session", "how": "exited"})
}

/// The recursive file bytes under `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    let mut bytes = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = fs::symlink_metadata(&path) {
                bytes += meta.len();
            }
        }
    }
    bytes
}

/// Spawns the client's `fiber` with `args` on a terminal: the delete and
/// prune prompts need a tty, as piped input without one is a usage error.
fn pty(world: &World, args: &[&str]) -> support::pty::Run {
    support::pty::Run::spawn(
        &world.b,
        120,
        32,
        args,
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    )
}

#[test]
fn delete_on_a_hub_removes_only_hub_side_state() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    let fork = hub_session(&world.a.home(), "p", FORK, Some(ROOT));
    fs::write(
        world.a.home().join("recent.jsonl"),
        format!("{}\n{}\n", recent_row(ROOT), recent_row(FORK)),
    )
    .unwrap();
    // No `--hub`: `hub.default=box` selects the listed hub.
    let deleted = client(&world, &["sessions", "delete", ROOT, "--cascade", "--yes"]);
    succeeds(&deleted);
    // `--yes` sends one `delete`: the printed id is the one it sent.
    assert_eq!(
        String::from_utf8_lossy(&deleted.stdout).into_owned(),
        format!("{ROOT}\n")
    );
    assert!(!root.exists(), "the hub session is gone");
    assert!(!fork.exists(), "its dependent is gone");
    assert!(
        !world.b.hub_socket().exists(),
        "the client started no hub of its own"
    );
    assert!(
        !world.b.home().join("projects").exists(),
        "the client kept no session state"
    );
    reap(world);
}

#[test]
fn prune_dry_run_prints_the_hub_list_and_frees_nothing() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    // No `--hub`: `hub.default=box` selects the listed hub.
    let dry = client(
        &world,
        &["sessions", "prune", "--older-than", "30d", "--dry-run"],
    );
    succeeds(&dry);
    let out = String::from_utf8_lossy(&dry.stdout);
    assert!(out.contains(ROOT), "{out}");
    assert!(out.contains("would free"), "{out}");
    assert!(root.join("events.jsonl").is_file(), "nothing was deleted");
    reap(world);
}

#[test]
fn prune_confirms_the_exact_returned_items() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    let fork = hub_session(&world.a.home(), "p", FORK, Some(ROOT));
    let mut run = pty(
        &world,
        &[
            "sessions",
            "prune",
            "--hub",
            "box",
            "--older-than",
            "30d",
            "--cascade",
        ],
    );
    run.wait_screen("the prune prompt", |grid| {
        grid.contents.contains("prune? [y/N]")
    });
    run.write(b"y\r");
    let confirmed = run.wait();
    assert_eq!(confirmed.status.code(), Some(0));
    let term = String::from_utf8_lossy(&confirmed.terminal);
    assert!(term.contains(ROOT), "{term}");
    assert!(term.contains(FORK), "{term}");
    assert!(term.contains("freed"), "{term}");
    assert!(!root.exists(), "the hub session is gone");
    assert!(!fork.exists(), "its dependent is gone");
    reap(world);
}

#[test]
fn prune_mutation_during_prompt_is_stale_and_removes_nothing() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    let mut run = pty(
        &world,
        &[
            "sessions",
            "prune",
            "--hub",
            "box",
            "--older-than",
            "30d",
            "--cascade",
        ],
    );
    run.wait_screen("the prune prompt", |grid| {
        grid.contents.contains("prune? [y/N]")
    });
    // The prompt stays open: a session added now changes the selection
    // the confirm preflights.
    hub_session(&world.a.home(), "p", NEW, None);
    run.write(b"y\r");
    let stale = run.wait();
    assert_eq!(stale.status.code(), Some(1));
    let term = String::from_utf8_lossy(&stale.terminal);
    assert!(term.contains("prune selection changed"), "{term}");
    assert!(root.exists(), "a stale prune removes nothing");
    assert!(
        world
            .a
            .home()
            .join("projects/p/sessions")
            .join(NEW)
            .exists(),
        "the meanwhile added session stays too"
    );
    reap(world);
}

#[test]
fn prune_partial_failure_reports_freed_and_failures() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    let fork = hub_session(&world.a.home(), "p", FORK, Some(ROOT));
    let expected = dir_bytes(&root) + dir_bytes(&fork);
    assert!(expected < 1024, "the freed line prints whole bytes");
    let crashes = world.a.home().join("crashes");
    fs::create_dir_all(&crashes).unwrap();
    let crash = crashes.join("old.crash");
    fs::write(&crash, b"crash!\n").unwrap();
    let old = wall(world.a.root.path()) - std::time::Duration::from_secs(31 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(&crash)
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::set_permissions(&crashes, fs::Permissions::from_mode(0o555)).unwrap();
    let pruned = client(
        &world,
        &[
            "sessions",
            "prune",
            "--hub",
            "box",
            "--older-than",
            "30d",
            "--cascade",
            "--yes",
        ],
    );
    fs::set_permissions(&crashes, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        pruned.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    let out = String::from_utf8_lossy(&pruned.stdout);
    assert_eq!(out, format!("freed {expected} B\n"), "{out}");
    let err = String::from_utf8_lossy(&pruned.stderr);
    assert!(
        err.contains(&crash.display().to_string()),
        "the failure names the crash file: {err}"
    );
    assert!(
        err.contains("1 of 2 could not be deleted"),
        "one failure of two items: {err}"
    );
    assert!(!root.exists(), "the sessions are gone");
    assert!(!fork.exists(), "their dependents are gone");
    assert!(crash.is_file(), "the undeletable crash stays");
    reap(world);
}

#[test]
fn unknown_hub_fails_before_connecting() {
    // `hub.default=box` is configured, but the explicit `--hub` wins:
    // the unknown hub fails before anything connects.
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    let deleted = client(
        &world,
        &["sessions", "delete", ROOT, "--hub", "nope", "--yes"],
    );
    assert_eq!(
        deleted.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&deleted.stderr).contains("No hub named `nope`"),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let pruned = client(
        &world,
        &[
            "sessions",
            "prune",
            "--hub",
            "nope",
            "--older-than",
            "30d",
            "--yes",
        ],
    );
    assert_eq!(
        pruned.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    assert!(root.exists(), "nothing was deleted");
    assert!(
        !world.b.hub_socket().exists(),
        "the client started no hub of its own"
    );
    reap(world);
}

#[test]
fn no_tty_without_yes_is_usage_and_removes_nothing() {
    let world = world(true);
    let root = hub_session(&world.a.home(), "p", ROOT, None);
    fs::write(
        world.a.home().join("recent.jsonl"),
        format!("{}\n", recent_row(ROOT)),
    )
    .unwrap();
    let deleted = client(&world, &["sessions", "delete", "--hub", "box", ROOT]);
    assert_eq!(
        deleted.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&deleted.stderr).contains("--yes"),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let pruned = client(
        &world,
        &["sessions", "prune", "--hub", "box", "--older-than", "30d"],
    );
    assert_eq!(
        pruned.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    assert!(
        String::from_utf8_lossy(&pruned.stderr).contains("--yes"),
        "{}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    assert!(root.exists(), "nothing was deleted");
    reap(world);
}
