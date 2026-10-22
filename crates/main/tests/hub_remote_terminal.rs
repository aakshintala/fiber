//! Bare `fiber --hub` and `fiber resume --hub` against a listed hub on
//! another machine (`docs/invocation.md`, "Two doors"): through the built
//! binary, a paired client with its own git repository and its own model
//! opens the hub's home, resumes its sessions, and never starts a hub of
//! its own (`docs/testing.md`, "Screens").
//!
//! The picker's overlay keeps long paths cut on the right at its
//! preferred width, so a temporary workspace never shows in full there:
//! the cases assert the recents row's presence, the feed behind it, and
//! the session log's exact workspace instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use contract::clock::Clock;
use fakes::ProviderServer;
use serde_json::{Value, json};
use std::fs;
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};
use std::time::Duration;
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

/// Installs a provider `fake` with model `m` at the fake server on the
/// hub machine, makes `fake/m` its configured model, and serves the hub
/// on `port`, idling the hub out a second after its last client leaves.
fn provider_with_port(setup: &Setup, server: &ProviderServer, port: u16) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
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
    serve_on(setup, port);
}

/// Configures the hub machine's model and serves its hub on `port`.
fn serve_on(setup: &Setup, port: u16) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"port": port, "idle_exit_ms": 1000}}),
    );
}

/// A port nothing holds right now: bound on port 0, read, released.
fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// Every `events.jsonl` under `home`'s projects: the logs of the sessions
/// the hub machine started.
fn session_logs(home: &Path) -> Vec<PathBuf> {
    let mut logs = Vec::new();
    let projects = fs::read_dir(home.join("projects")).unwrap();
    for project in projects.flatten() {
        let sessions = project.path().join("sessions");
        if !sessions.is_dir() {
            continue;
        }
        for session in fs::read_dir(&sessions).unwrap().flatten() {
            let log = session.path().join("events.jsonl");
            if log.is_file() {
                logs.push(log);
            }
        }
    }
    logs
}

/// The `session_started` line's workspace: the journey starts exactly one
/// session on the hub, and its log names the workspace it runs in.
fn started_workspace(home: &Path) -> String {
    let logs = session_logs(home);
    assert_eq!(logs.len(), 1, "the hub started exactly one session");
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

/// The ids `fiber sessions --hub <name> --json` lists on the client: the
/// hub's live and exited sessions, proving what a start did or did not
/// add.
fn sessions_ids(setup: &Setup, hub: &str) -> Vec<String> {
    let output = run(setup, &["sessions", "--hub", hub, "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Hub start attempts `world_with` makes, each on a port from `ports`.
const PORT_ATTEMPTS: usize = 5;
/// How long one connect waits before the hub's exit is looked at again.
const HELLO_SLICE: Duration = Duration::from_secs(2);
/// What a hello wait leaves of the deadline for its report and cleanup.
const REPORT_MARGIN: Duration = Duration::from_millis(250);
/// The `hub.log` lines a failure report shows.
const LOG_TAIL: usize = 20;

/// Whether `log` holds the hub's failure to bind `port`: the line its start
/// error writes before the hub exits (`127.0.0.1:<port>: <why>`).
fn port_taken(log: &str, port: u16) -> bool {
    log.contains(&format!("127.0.0.1:{port}: "))
}

/// What a hub that did not say hello left behind: its exit `status`
/// (`None` for a hub still running) and the tail of `hub.log`.
fn report(setup: &Setup, status: Option<ExitStatus>) -> String {
    let log = setup.hub_log();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
    let state = match status {
        Some(status) => format!("exited ({status})"),
        None => "was still running".to_owned(),
    };
    format!("{state}; hub.log tail (last {LOG_TAIL} lines):\n{tail}")
}

/// Stops waiting on `process`: snapshots its status, reaps it (killing a
/// hub still running), and panics with `why` and the report.
fn give_up(setup: &Setup, mut process: HubProc, why: &str) -> ! {
    let status = process.child.try_wait().unwrap();
    match status {
        Some(_) => drop(process.wait()),
        None => drop(process.kill_and_wait()),
    }
    panic!("{why}: the hub {}", report(setup, status));
}

/// Waits for the hub's `hub_hello` in slices of [`HELLO_SLICE`], looking at
/// the hub's exit between them. `Ok` is the hello and the live hub; `Err`
/// is the status of a hub that exited, already reaped. The deadline is the
/// test's own, never renewed: a slice ends [`REPORT_MARGIN`] before it, so
/// the report and the cleanup run first.
fn await_hello(
    setup: &Setup,
    mut process: HubProc,
) -> Result<(doors::hub::Hub, HubProc), ExitStatus> {
    loop {
        let left = setup.deadline.left();
        if left <= REPORT_MARGIN {
            give_up(setup, process, "waited for the installed hub's hello");
        }
        let until = SystemClock.now() + left.saturating_sub(REPORT_MARGIN).min(HELLO_SLICE);
        let home = setup.home();
        let slice = bounded(setup.deadline, "the installed hub's hello", move || {
            doors::hub::connect_until(&home, &mut || Ok(()), &SystemClock, until, &mut || {})
        });
        let error = match slice {
            Ok(hub) => return Ok((hub, process)),
            Err(error) => error,
        };
        if process.child.try_wait().unwrap().is_some() {
            return Err(process.wait());
        }
        // Only a slice's own end loops; any other error would repeat at once.
        if !matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::UnexpectedEof
        ) {
            give_up(
                setup,
                process,
                &format!("the installed hub's hello failed with {error}"),
            );
        }
    }
}

/// The paired world the cases share: `a` serves its hub on a free port,
/// `b` is paired with it, holds its own git repository with its own
/// file, and configures its own model. The hub connection is held so the
/// hub never idles out mid-case.
struct World {
    a: Setup,
    b: Setup,
    process: HubProc,
    hub: doors::hub::Hub,
    #[allow(dead_code, reason = "held only for its drop")]
    server: ProviderServer,
    port: u16,
}

fn world(default: bool) -> World {
    world_with(default, &mut free_port)
}

/// [`world`], with the hub's port drawn from `ports`.
fn world_with(default: bool, ports: &mut dyn FnMut() -> u16) -> World {
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
    // files, so the workspace is a git repository with the file added.
    fs::write(a.workspace().join("only-on-a.txt"), "hub only").unwrap();
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
    let mut git = std::process::Command::new("git");
    git.args([
        "-C",
        &a.workspace().display().to_string(),
        "add",
        "only-on-a.txt",
    ])
    .env_clear()
    .envs(fakes::check_run())
    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
    .env("HOME", a.root.path())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .process_group(0);
    succeeds(&run_to_exit(a.deadline, "the hub's tracked file", git));
    let row = json!({"session_id":"s_remote", "ts":1, "project":"remote-project", "workspace":a.workspace().display().to_string(), "name":"remote session", "how":"exited"});
    fs::write(a.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    fs::create_dir_all(a.home().join("projects/remote-project/sessions/s_remote")).unwrap();
    let mut port = ports();
    let server = ProviderServer::start([support::hello()]).unwrap();
    provider_with_port(&a, &server, port);
    // The hub machine already imported: the first-run offer reads the
    // hub's receipts, and these cases cover the picker and resume
    // flows with the offer down, never the offer itself.
    for receipt in ["import-claude-code.json", "import-codex.json"] {
        write_json(
            &a.home().join(receipt),
            &json!({"source": receipt, "imported_at": 1}),
        );
    }
    // `~/` on the hub expands against the inherited `HOME`, which the
    // harness otherwise points at the setup root: point it at the hub's
    // home, as production has it, so the picker browses the hub's home.
    // A port another process took between the pick and the hub's bind
    // fails that hub at once: the next attempt serves on a fresh port.
    let mut attempt = 1;
    let (hub, process) = loop {
        if attempt > 1 {
            port = ports();
            serve_on(&a, port);
        }
        let logged = a.hub_log().len();
        let mut serve = a.fiber(&["hub", "serve", "--installed"]);
        serve.env("HOME", a.home());
        let process = HubProc::spawn_command(a.deadline, &mut serve);
        match await_hello(&a, process) {
            Ok(hello) => break hello,
            Err(status) => {
                let log = a.hub_log();
                let added = log.get(logged..).unwrap_or_default();
                if attempt < PORT_ATTEMPTS && port_taken(added, port) {
                    attempt += 1;
                    continue;
                }
                panic!(
                    "the installed hub's hello on 127.0.0.1:{port}, attempt {attempt} of {PORT_ATTEMPTS}: the hub {}",
                    report(&a, Some(status))
                );
            }
        }
    };
    assert_eq!(hub.1.kind, "hub_hello");
    let address = format!("ws://127.0.0.1:{port}");
    let code = run(&a, &["hub", "pair", "laptop"]);
    succeeds(&code);
    let code = String::from_utf8(code.stdout).unwrap();
    succeeds(&add(&b, "box", &address, code.trim(), default));
    World {
        a,
        b,
        process,
        hub,
        server,
        port,
    }
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

/// Reaps a world whose cases started nothing: the held hub connection
/// drops and the hub exits on `TERM`.
fn reap_idle(world: World) {
    drop(world.hub);
    world.process.kill("TERM");
    assert_eq!(world.process.wait().code(), Some(143));
}

/// Spawns bare `fiber` with `args` on the client's terminal.
fn client(world: &World, args: &[&str]) -> Run {
    Run::spawn(
        &world.b,
        120,
        32,
        args,
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    )
}

/// Quits a terminal with no live session, as `terminal.rs` does: no
/// resume line follows, so the restored primary screen is the
/// assertion. Returns what the run drained, for the notice cases.
fn quit_idle(run: Run) -> support::pty::Exited {
    let mut run = run;
    run.write(b"\x03\x03");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    finished
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

#[test]
fn bare_fiber_on_a_listed_hub_opens_the_hubs_home() {
    let world = world(true);
    // Bare `fiber --hub box` opens the hub's home on the client's
    // terminal: the picker lists the hub's home and its recent
    // workspace, never the client's launch directory, with no workspace
    // chosen, the hub's model on the chip, and a footer that offers no
    // Esc.
    let mut run = client(&world, &["--hub", "box"]);
    run.ready();
    // The loop pushes kitty's flags only after it processes the
    // harness's reply, so the Esc below goes out kitty-encoded and never
    // reads as Alt with the keystrokes after it.
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    let picker = run.wait_screen("the hub home picker", |screen| {
        shows(screen, "Workspaces")
            && shows(screen, "› ~/")
            && shows(screen, "projects")
            && shows(screen, "── recents ──")
            && recents_row(screen)
            && shows(screen, "remote session")
            && shows(screen, "[no workspace]")
            && shows(screen, "[fake/m]")
            && shows(screen, "↑↓ move · Enter open")
    });
    assert!(
        !shows(&picker, "blocal"),
        "the picker lists the hub's home, not the client's launch directory"
    );
    assert!(
        !picker
            .rows
            .iter()
            .any(|row| row.contains(&world.b.workspace().display().to_string())),
        "the client's workspace path never shows on the remote home"
    );
    assert!(
        !shows(&picker, "new worktree"),
        "the remote home ignores the client's repository"
    );
    assert!(
        !shows(&picker, "client/other"),
        "the chip shows the hub's model, never the client's"
    );
    assert!(
        picker
            .rows
            .iter()
            .filter(|row| row.contains("Enter open"))
            .all(|row| !row.contains("Esc")),
        "the remote picker footer offers no Esc"
    );
    // Esc cannot close the picker with no workspace chosen: the prompt
    // typed next still lands in the picker's typed row.
    run.write(b"\x1b[27u");
    run.write(b"say hi");
    run.wait_screen("the typed prompt in the still-open picker", |screen| {
        shows(screen, "› ~/say hi") && shows(screen, "Workspaces")
    });
    // Enter on the typed non-directory resolves to nothing: the picker
    // stays open with its missing row, and the hub starts no session.
    run.write(b"\r");
    run.wait_screen("the missing directory row", |screen| {
        shows(screen, "no such directory") && shows(screen, "Workspaces")
    });
    assert_eq!(
        sessions_ids(&world.b, "box"),
        ["s_remote"],
        "Enter on a typed non-directory starts nothing on the hub"
    );
    // Choosing the recent row, typing a prompt, then Enter starts the
    // session on the hub in that workspace: emptying the typed row
    // returns to the recents alone, and Enter chooses the first one.
    run.write(&[0x7f; 64]);
    run.write(b"\r");
    run.wait_screen("the chosen hub workspace", |screen| {
        shows(screen, "[w]") && !shows(screen, "Workspaces")
    });
    let ws = world.a.workspace().display().to_string();
    let guard = support::SessionGuard::arm(world.a.deadline, &ws);
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hello."));
    run.turn_finished(from);
    // The hub's run directory gains the session's socket.
    let sockets: Vec<String> = fs::read_dir(world.a.home().join("run"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "hub")
        .collect();
    assert_eq!(sockets.len(), 1, "the hub started one session");
    // In that session `@` shows the file that exists only on the hub,
    // never the client's own.
    run.write(b"@only");
    run.wait_screen("the hub-only file", |screen| {
        shows(screen, "only-on-a.txt") && !shows(screen, "blocal")
    });
    run.write(&[0x7f; 6]);
    // `/settings` shows the hub machine's configuration, never the
    // client's: the model row names the hub's model in quotes, which the
    // chip's brackets never match.
    run.write(b"/settings\r");
    run.wait_screen("the hub settings", |screen| {
        shows(screen, "Settings") && shows(screen, "\"fake/m\"") && !shows(screen, "client/other")
    });
    run.write(b"\x1b[27u");
    run.wait_screen("the closed settings", |screen| !shows(screen, "Settings"));
    // Quitting is taken in any state; the restored grid above proves the
    // terminal drew before the quit goes out, and one resume line per
    // live session is on it, naming the hub.
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen
            && !grid.hide_cursor
            && grid
                .contents
                .contains(&format!("fiber resume {id} --hub box", id = sockets[0]))
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    assert!(
        !String::from_utf8_lossy(&finished.terminal).contains("ignored `hub.default`"),
        "no ignored-key notice without a repository key"
    );
    assert!(
        !world.b.hub_socket().exists(),
        "no hub starts on the client"
    );
    // The `start` command's workspace is the chosen hub workspace, on the
    // hub's machine.
    let logged = fs::canonicalize(started_workspace(&world.a.home())).unwrap();
    assert_eq!(logged, fs::canonicalize(world.a.workspace()).unwrap());
    reap(world, &ws, guard);
}

/// Opens a session on the hub through the client's terminal and leaves
/// the client, returning the live session's id: the start of the resume
/// journey, which quits and reopens it.
fn start_on_hub(world: &World) -> (String, SessionGuard) {
    let mut run = client(world, &["--hub", "box"]);
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
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hello."));
    run.turn_finished(from);
    let sockets: Vec<String> = fs::read_dir(world.a.home().join("run"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "hub")
        .collect();
    assert_eq!(sockets.len(), 1, "the hub started one session");
    let id = sockets[0].clone();
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen
            && !grid.hide_cursor
            && grid
                .contents
                .contains(&format!("fiber resume {id} --hub box"))
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    (id, guard)
}

#[test]
fn resume_on_a_listed_hub_reopens_the_session() {
    let world = world(true);
    let (id, guard) = start_on_hub(&world);
    let ws = world.a.workspace().display().to_string();
    let prefix = id.get(..8).unwrap_or(&id).to_owned();
    // `fiber resume --hub box <prefix>` reopens that session: its reply
    // is on screen.
    let mut run = client(&world, &["resume", "--hub", "box", &prefix]);
    run.ready();
    run.wait_screen("the resumed session", |screen| {
        screen.alternate_screen && shows(screen, "Hello.")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen
            && !grid.hide_cursor
            && grid
                .contents
                .contains(&format!("fiber resume {id} --hub box"))
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    // A prefix matching nothing exits 1 with the not-found sentence and
    // starts no terminal: the bytes hold no alternate-screen switch.
    let run = client(&world, &["resume", "--hub", "box", "zzz-nope"]);
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(1));
    let terminal = String::from_utf8_lossy(&finished.terminal).into_owned();
    assert!(
        terminal.contains("no session matches \"zzz-nope\" on hub `box`"),
        "{terminal}"
    );
    assert!(
        !terminal.contains("1049h"),
        "a failed resolve starts no terminal"
    );
    // `fiber resume --hub box` with no id shows the session in the list.
    let mut run = client(&world, &["resume", "--hub", "box"]);
    run.ready();
    run.wait_screen("the hub session list", |screen| shows(screen, &id));
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    assert!(
        !world.b.hub_socket().exists(),
        "no hub starts on the client"
    );
    reap(world, &ws, guard);
}

#[test]
fn unknown_hub_and_run_layer_default() {
    let world = world(true);
    // An unknown hub is a usage error before any terminal starts, and
    // starts no hub on the client.
    let pty = client(&world, &["--hub", "nope"]);
    let finished = pty.wait();
    assert_eq!(finished.status.code(), Some(2));
    let terminal = String::from_utf8_lossy(&finished.terminal).into_owned();
    assert!(terminal.contains("No hub named `nope`"), "{terminal}");
    assert!(
        !world.b.hub_socket().exists(),
        "no hub starts on the client"
    );
    // `-c hub.default=box` opens the same remote home with no global
    // default set: a second client pairs without one.
    let c = Setup::new();
    let code = run(&world.a, &["hub", "pair", "phone"]);
    succeeds(&code);
    let code = String::from_utf8(code.stdout).unwrap();
    let address = format!("ws://127.0.0.1:{port}", port = world.port);
    succeeds(&add(&c, "box", &address, code.trim(), false));
    let mut run = Run::spawn(
        &c,
        120,
        32,
        &["-c", "hub.default=box"],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.wait_screen("the hub home picker", |screen| {
        shows(screen, "Workspaces")
            && shows(screen, "› ~/")
            && shows(screen, "── recents ──")
            && recents_row(screen)
            && shows(screen, "remote session")
            && shows(screen, "[no workspace]")
            && shows(screen, "[fake/m]")
    });
    quit_idle(run);
    assert_eq!(
        sessions_ids(&c, "box"),
        ["s_remote"],
        "the run layer default starts nothing on the hub"
    );
    assert!(
        !c.hub_socket().exists(),
        "no hub starts on the second client"
    );
    reap_idle(world);
}

#[test]
fn forwarded_hub_opens_the_same_remote_home() {
    let world = world(true);
    let forwarded = format!("unix:{}", world.a.hub_socket().display());
    let added = run(&world.b, &["hub", "add", "fwd", &forwarded]);
    succeeds(&added);
    assert!(added.stdout.is_empty());
    assert!(
        !world.b.home().join("credentials/hubs/fwd").exists(),
        "a forwarded hub keeps no token"
    );
    let mut run = client(&world, &["--hub", "fwd"]);
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.wait_screen("the hub home picker", |screen| {
        shows(screen, "Workspaces")
            && shows(screen, "› ~/")
            && shows(screen, "── recents ──")
            && recents_row(screen)
            && shows(screen, "remote session")
            && shows(screen, "[no workspace]")
    });
    quit_idle(run);
    assert!(
        !world.b.hub_socket().exists(),
        "no hub starts on the client"
    );
    reap_idle(world);
}

#[test]
fn repository_hub_default_is_ignored_with_a_notice() {
    let world = world(true);
    fs::create_dir_all(world.b.workspace().join(".fiber")).unwrap();
    fs::write(
        world.b.workspace().join(".fiber/config.json"),
        r#"{"hub": {"default": "elsewhere"}}"#,
    )
    .unwrap();
    // The repository's person-only key cannot redirect the target: the
    // notice names the key and the file exactly once, before the picker
    // opens the hub's home.
    let mut run = client(&world, &["--hub", "box"]);
    run.ready();
    run.wait_bytes(0, KITTY_PUSH, "kitty keyboard flags pushed");
    run.wait_screen("the hub home picker", |screen| {
        shows(screen, "Workspaces")
            && shows(screen, "── recents ──")
            && recents_row(screen)
            && shows(screen, "remote session")
            && shows(screen, "[no workspace]")
    });
    let finished = quit_idle(run);
    let terminal = String::from_utf8_lossy(&finished.terminal).into_owned();
    assert_eq!(
        terminal.matches("ignored `hub.default`").count(),
        1,
        "the notice prints exactly once: {terminal}"
    );
    assert!(
        terminal.contains(".fiber/config.json"),
        "the notice names the file: {terminal}"
    );
    let notice_at = terminal.find("ignored `hub.default`").unwrap();
    let picker_at = terminal.find("Workspaces").expect("the picker opened");
    assert!(
        notice_at < picker_at,
        "the notice prints before the picker: {terminal}"
    );
    reap_idle(world);
}

#[test]
fn a_taken_port_is_retried_with_a_fresh_one() {
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let mut draws = 0;
    let world = world_with(true, &mut || {
        draws += 1;
        if draws == 1 { taken } else { free_port() }
    });
    assert_ne!(world.port, taken, "the hub that said hello is the retry");
    assert!(
        world.a.hub_log().contains(&format!("127.0.0.1:{taken}: ")),
        "the first hub failed on the taken port: {}",
        world.a.hub_log()
    );
    reap_idle(world);
    drop(held);
}

#[test]
fn a_hub_that_never_gets_a_port_fails_naming_the_port_and_the_log() {
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    let mut draws = 0;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        world_with(true, &mut || {
            draws += 1;
            taken
        })
    }));
    let payload = failed
        .err()
        .expect("a hub that cannot bind fails the world");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .expect("the panic carries text");
    for needle in [
        format!("127.0.0.1:{taken}"),
        "exited".to_owned(),
        "Address already in use".to_owned(),
        format!("attempt {PORT_ATTEMPTS} of {PORT_ATTEMPTS}"),
    ] {
        assert!(message.contains(&needle), "{needle:?} missing: {message}");
    }
    assert_eq!(draws, PORT_ATTEMPTS, "one port is drawn per attempt");
    drop(held);
}
