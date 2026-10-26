//! Binary-level tests for the hooks install offer (`docs/extensions.md`,
//! "Hooks declared in configuration"; `docs/testing.md`, "Levels"): the
//! built terminal runs on a PTY against a real hub with production
//! query/install, and the journeys assert the offer, the retry and the
//! failure boundaries through what the terminal shows and what it stores.
//! No journey touches the network: the first-party `hooks` package comes
//! from a local tagged Git fixture rewritten over `url.insteadOf`, and the
//! install-failure journey points that rewrite at a directory that holds
//! no repository.

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
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::pty::Run;
use support::{
    HubProc, SessionGuard, connect_hub, is_status, run_to_exit, session_dir, write_json,
};

/// The pty size the journeys draw on.
const COLS: u16 = 160;
const ROWS: u16 = 48;

/// The first-party package under test.
const HOOKS_NAME: &str = "github.com/aakshintala/fiber/extensions/hooks";

/// A one-line model reply.
fn text_reply(text: &str) -> fakes::Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// Declares personal blocking or watching `hooks` entries.
fn declare_hooks(setup: &support::Setup, entries: &Value) {
    write_json(
        &setup.home().join("config").join("hooks.json"),
        &json!({"hooks": entries}),
    );
}

/// A `session_start` entry that appends one line to `marker`.
fn marker_entry(marker: &Path, on_failure: &str) -> Value {
    json!({
        "point": "session_start",
        "command": "/bin/sh",
        "args": ["-c", format!("echo marked >> {}", marker.display())],
        "timeout": 30000,
        "on_failure": on_failure,
    })
}

/// The ids of the sessions stored under the workspace, or `None` while
/// the first session directory is still being created.
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

/// Whether `id`'s stored events carry its `session_started` line: the
/// completion signal proving the session started.
fn has_started(dir: &Path, id: &str) -> bool {
    std::fs::read_to_string(dir.join(id).join("events.jsonl"))
        .is_ok_and(|events| events.lines().any(|line| line.contains("session_started")))
}

/// Waits for exactly `count` stored sessions, each carrying its
/// `session_started` event, before reading their ids: the event proves
/// the session started, where the directory alone does not.
fn wait_stored(setup: &support::Setup, count: usize) -> Vec<String> {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let waiting = dir.clone();
    fakes::wait_until("the stored session start", setup.deadline, move || {
        stored_at(&waiting)
            .is_some_and(|ids| ids.len() == count && ids.iter().all(|id| has_started(&waiting, id)))
    });
    stored_at(&dir).unwrap()
}

/// Asserts no session directory stands yet: the journey started nothing.
fn assert_no_sessions(setup: &support::Setup, what: &str) {
    let dir = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    assert!(
        stored_at(&dir).is_none_or(|ids| ids.is_empty()),
        "{what}: a session started unexpectedly"
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
/// processes remain.
fn finish(hub: Arc<Mutex<Option<HubProc>>>, guard: SessionGuard, run: Run) {
    quit(run);
    guard.wait_gone();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// Tears the hub down from the home screen, which prints no resume line:
/// quits and waits for a clean exit, then reaps whatever remains. The
/// first Ctrl+C clears the restored draft, so quitting takes three: the
/// quit arm only starts from an empty draft.
fn finish_home(hub: Arc<Mutex<Option<HubProc>>>, guard: SessionGuard, mut run: Run) {
    run.write(b"\x03\x03\x03\r");
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    guard.wait_gone();
    let hub = hub.lock().unwrap().take().expect("the starter ran");
    hub.wait();
}

/// Lets the test's hub exit once its last client leaves, so the run's
/// wait does not outlive the terminal: merged into the provider config.
fn idle_hub(setup: &support::Setup) {
    let file = setup.home().join("config.json");
    let mut config: Value =
        serde_json::from_slice(&fs::read(&file).unwrap_or_default()).unwrap_or(Value::Null);
    if !config.is_object() {
        config = json!({});
    }
    config["hub"] = json!({"idle_exit_ms": 200});
    write_json(&file, &config);
}

/// Starts the local hub behind the terminal and arms the session guard:
/// the control socket and the guard hold the hub up while the test moves.
fn local_world(setup: &support::Setup) -> (Arc<Mutex<Option<HubProc>>>, SessionGuard) {
    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (_, _) = connect_hub(setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    (hub, guard)
}

/// A local tagged Git fixture holding the REAL first-party `hooks`
/// package: the manifest names the released version and fiber line, and
/// `init.lua` is the shipped entry registrar, so installing it registers
/// entries the way the released package does. Returns the `url.insteadOf`
/// key the child rewrites `https://github.com/` to.
fn hooks_fixture(setup: &support::Setup) -> String {
    let base = setup.root.path().join("gh");
    let repo = base.join("aakshintala/fiber");
    fs::create_dir_all(repo.join("extensions/hooks")).unwrap();
    fakes::git::git(&repo, &["init", "-q"]);
    write_json(
        &repo.join("extensions/hooks/extension.json"),
        &json!({"name":HOOKS_NAME,"version":"0.1.0","fiber":env!("CARGO_PKG_VERSION"),"api":1,"repo_settings":["hooks"]}),
    );
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/hooks/init.lua"),
        repo.join("extensions/hooks/init.lua"),
    )
    .unwrap();
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "-qm", "hooks fixture"]);
    fakes::git::git(&repo, &["tag", "v0.1.0"]);
    format!("url.file://{}/.insteadOf", base.display())
}

/// The child environment rewriting `https://github.com/` at the local
/// fixture, plus the fake provider key every terminal run needs.
fn install_env(key: &str) -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()),
        ("GIT_CONFIG_KEY_0".to_owned(), key.to_owned()),
        (
            "GIT_CONFIG_VALUE_0".to_owned(),
            "https://github.com/".to_owned(),
        ),
        ("FIBER_TEST_FAKE_KEY".to_owned(), "sk-test".to_owned()),
    ]
}

/// A terminal run with `args` and `env` on the pty.
fn spawn(setup: &support::Setup, args: &[&str], env: &[(String, String)]) -> Run {
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    Run::spawn(setup, COLS, ROWS, args, &env)
}

/// Asserts the global install record stands: the package directory with
/// its manifest and registrar.
fn assert_installed(setup: &support::Setup) {
    let dir = setup.home().join("extensions/hooks");
    assert!(dir.join("extension.json").is_file(), "the install record");
    assert!(dir.join("init.lua").is_file(), "the shipped registrar");
}

/// Asserts no partial install stands: the package directory is absent.
fn assert_not_installed(setup: &support::Setup, what: &str) {
    assert!(
        !setup.home().join("extensions/hooks").exists(),
        "{what}: a partial hooks package stands"
    );
}

/// Runs one headless `ask` turn in `setup` and returns its JSON lines: the
/// session startup's `notice` lines are ephemeral, so the wire — not the
/// stored events — names the missing extension.
fn ask_lines(setup: &support::Setup, args: &[&str]) -> Vec<Value> {
    let output = run_to_exit(setup.deadline, "the headless ask", setup.fiber(args));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !is_status(line))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Asserts the headless session's startup `notice` names the missing
/// extension: the wire-level `extension_missing` the terminal's ephemeral
/// notice store also folds.
fn assert_hooks_notice(lines: &[Value]) {
    let notice = lines
        .iter()
        .find(|line| line["kind"] == "notice")
        .expect("a startup notice");
    assert_eq!(notice["payload"]["code"], "extension_missing");
    assert_eq!(
        notice["payload"]["message"],
        "The `hooks` extension is not installed, so its declared hooks will not run. Run `fiber extension install hooks`.",
    );
}

/// The `session_started` payload of the stored session's durable events.
fn started_payload(setup: &support::Setup, id: &str) -> Value {
    fs::read_to_string(session_dir(setup, id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .find(|line: &Value| line["kind"] == "session_started")
        .expect("the stored session started")
        .get("payload")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Removed `hooks` plus a well-formed blocking declaration: the terminal
/// shows the install offer before any session exists. Accepting installs
/// `hooks` and the session starts with the entry registered: the stored
/// session started in this workspace, and the `session_start` hook left
/// its marker.
#[test]
fn removed_hooks_with_a_blocking_entry_offers_then_installs_and_starts() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("marker.log");
    declare_hooks(
        &setup,
        &json!({"marker": marker_entry(&marker, "blocking")}),
    );
    let key = hooks_fixture(&setup);
    let (hub, guard) = local_world(&setup);
    let mut run = spawn(&setup, &[], &install_env(&key));
    run.ready();
    run.write(b"hi\r");
    run.wait_screen("the install offer", |grid| {
        grid.contents.contains("Install hooks?") && grid.contents.contains("marker")
    });
    run.write(b"\r");
    let marked = marker.clone();
    fakes::wait_until("the hook marker", setup.deadline, move || marked.is_file());
    let id = wait_stored(&setup, 1).into_iter().next().unwrap();
    run.wait_screen("the session reply", |grid| grid.contents.contains("done"));
    assert_installed(&setup);
    let started = started_payload(&setup, &id);
    // The hub stores the canonical workspace: on macOS that resolves
    // the fixture's `/var/...` temporary path to `/private/var/...`,
    // so both sides canonicalize before the identity comparison.
    assert_eq!(
        fs::canonicalize(started["workspace"].as_str().unwrap()).unwrap(),
        fs::canonicalize(setup.workspace()).unwrap()
    );
    assert!(fs::read_to_string(&marker).unwrap().contains("marked"));
    drop(server);
    finish(hub, guard, run);
}

/// The same refusal declined: Esc starts no session and runs no hook
/// point, showing the terminal's message without the offer.
#[test]
fn declining_the_offer_starts_no_session_and_runs_no_hook() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("marker.log");
    declare_hooks(
        &setup,
        &json!({"marker": marker_entry(&marker, "blocking")}),
    );
    let (hub, guard) = local_world(&setup);
    let mut run = spawn(
        &setup,
        &[],
        &[("FIBER_TEST_FAKE_KEY".to_owned(), "sk-test".to_owned())],
    );
    run.ready();
    run.write(b"hi\r");
    run.wait_screen("the install offer", |grid| {
        grid.contents.contains("Install hooks?")
    });
    run.write(b"\x1b[27u");
    run.wait_screen("the refusal without the offer", |grid| {
        grid.contents.contains("not installed")
            && grid.contents.contains("fiber extension install hooks")
            && !grid.contents.contains("Install hooks?")
    });
    assert!(!marker.exists(), "no hook point ran");
    assert_no_sessions(&setup, "the decline");
    assert_not_installed(&setup, "the decline");
    drop(server);
    finish_home(hub, guard, run);
}

/// A blocking entry declared only by an override: no blocker in file
/// configuration, yet the offer names it, and accepting installs and
/// starts with the original arguments intact.
#[test]
fn a_blocking_entry_from_overrides_only_offers_and_starts() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("override.log");
    let entry = marker_entry(&marker, "blocking");
    let key = hooks_fixture(&setup);
    let (hub, guard) = local_world(&setup);
    let flag = format!("extensions.\"{HOOKS_NAME}\".settings.hooks.only_override={entry}");
    let env = install_env(&key);
    let mut run = spawn(&setup, &["-c", &flag], &env);
    run.ready();
    run.write(b"hi\r");
    run.wait_screen("the install offer naming the override entry", |grid| {
        grid.contents.contains("Install hooks?") && grid.contents.contains("only_override")
    });
    run.write(b"\r");
    let marked = marker.clone();
    fakes::wait_until("the hook marker", setup.deadline, move || marked.is_file());
    let id = wait_stored(&setup, 1).into_iter().next().unwrap();
    run.wait_screen("the session reply", |grid| grid.contents.contains("done"));
    assert_installed(&setup);
    let started = started_payload(&setup, &id);
    // The hub stores the canonical workspace: on macOS that resolves
    // the fixture's `/var/...` temporary path to `/private/var/...`,
    // so both sides canonicalize before the identity comparison.
    assert_eq!(
        fs::canonicalize(started["workspace"].as_str().unwrap()).unwrap(),
        fs::canonicalize(setup.workspace()).unwrap()
    );
    drop(server);
    finish(hub, guard, run);
}

/// Only `non-blocking` entries: the session starts with a `notice`
/// naming `hooks`, no offer, and no hook marker while the package is
/// absent.
#[test]
fn non_blocking_entries_start_with_a_notice_and_no_offer() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done"), text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("soft.log");
    declare_hooks(
        &setup,
        &json!({"soft": marker_entry(&marker, "non-blocking")}),
    );
    let (hub, guard) = local_world(&setup);
    let mut run = spawn(
        &setup,
        &[],
        &[("FIBER_TEST_FAKE_KEY".to_owned(), "sk-test".to_owned())],
    );
    run.ready();
    run.write(b"hi\r");
    run.wait_screen("the session reply", |grid| grid.contents.contains("done"));
    wait_stored(&setup, 1);
    run.wait_screen("the session without the offer", |grid| {
        !grid.contents.contains("Install hooks?")
    });
    assert!(!marker.exists(), "no hook point ran without the package");
    assert_not_installed(&setup, "the soft session");
    finish(hub, guard, run);
    // The same fixture over the wire: the headless session starts with
    // the `notice` naming the missing extension.
    assert_hooks_notice(&ask_lines(&setup, &["ask", "hi"]));
    drop(server);
}

/// A failing install: the `insteadOf` rewrite points at a directory that
/// holds no repository, so the clone fails deterministically with no
/// network. The failure replaces the offer, naming the install command,
/// and leaves no session, no marker and no partial package.
#[test]
fn a_failing_install_replaces_the_offer_and_starts_nothing() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("failed.log");
    declare_hooks(
        &setup,
        &json!({"marker": marker_entry(&marker, "blocking")}),
    );
    let barren = setup.root.path().join("barren");
    fs::create_dir_all(&barren).unwrap();
    let key = format!("url.file://{}/.insteadOf", barren.display());
    let (hub, guard) = local_world(&setup);
    let mut run = spawn(&setup, &[], &install_env(&key));
    run.ready();
    run.write(b"hi\r");
    run.wait_screen("the install offer", |grid| {
        grid.contents.contains("Install hooks?")
    });
    run.write(b"\r");
    run.wait_screen("the install failure", |grid| {
        grid.contents.contains("fiber extension install hooks")
            && !grid.contents.contains("Install hooks?")
    });
    assert!(!marker.exists(), "no hook point ran");
    assert_no_sessions(&setup, "the failed install");
    assert_not_installed(&setup, "the failed install");
    drop(server);
    finish_home(hub, guard, run);
}

/// Runs `fiber` with `args` in `setup` to completion.
fn fiber_run(setup: &support::Setup, args: &[&str]) -> std::process::Output {
    run_to_exit(setup.deadline, "the fiber command", setup.fiber(args))
}

/// Pairs `client` with the hub serving `port` on `hub`: the pairing code
/// round-trips through files, never the network beyond loopback.
fn pair_client(hub: &support::Setup, client: &support::Setup, port: u16) {
    let code = fiber_run(hub, &["hub", "pair", "laptop"]);
    assert!(
        code.status.success(),
        "{}",
        String::from_utf8_lossy(&code.stderr)
    );
    let input = client.root.path().join("pairing-input");
    fs::write(
        &input,
        format!("{}\n", String::from_utf8(code.stdout).unwrap().trim()),
    )
    .unwrap();
    let mut command = client.fiber(&[
        "hub",
        "add",
        "box",
        &format!("ws://127.0.0.1:{port}"),
        "--default",
    ]);
    command.stdin(fs::File::open(input).unwrap());
    let added = run_to_exit(
        client.deadline,
        "hub add with a pairing code on stdin",
        command,
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
}

/// The `git` on this test's own `PATH`: the gate wrapper delegates to it
/// once released, so the path is absolute and never the wrapper itself.
fn real_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .expect("a real git on PATH")
}

/// A `git` wrapper that gates only the install's external `clone`: the
/// clone's full arguments go to `signal` and the call blocks reading
/// `fifo`, while every other invocation — the hub's boot-time probes —
/// delegates to the absolute real `git` at once. The `signal` file only
/// records the gated arguments; the wait that proves the clone is held
/// is the test's fifo rendezvous below, never the file.
fn write_git_gate(dir: &Path, signal: &Path, fifo: &Path, real: &Path) {
    let script = format!(
        "#!/bin/sh\ncase \" $* \" in\n*\" clone \"*)\nprintf '%s\\n' \"$*\" >> '{}'\nread -r _ < '{}'\n;;\nesac\nexec '{}' \"$@\"\n",
        signal.display(),
        fifo.display(),
        real.display(),
    );
    fs::write(dir.join("git"), script).unwrap();
    fs::set_permissions(dir.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
}

/// Killing the hub's group ends the wrapper's blocked read, so the FIFO
/// needs no writer: dropping is the backstop behind the explicit
/// kill-and-wait below, and the watchdog reaps the rest on failure.
struct GateCleanup {
    process: Option<HubProc>,
}

impl Drop for GateCleanup {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            process.kill("TERM");
        }
    }
}

/// Quits a terminal with no live session: no resume line follows, and the
/// drained run proves the failure cases had nothing more to say. The
/// first Ctrl+C clears the typed draft, so quitting takes three.
fn quit_idle(run: Run) {
    let mut run = run;
    run.write(b"\x03\x03\x03");
    run.wait_screen("the restored primary screen", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// A failing names query: with the extensions lock a directory, the
/// production query fails while the hub still refuses the start for
/// missing blocking hooks. The terminal shows the exact refusal with no
/// offer, and starts no session, leaving no marker.
#[test]
fn an_unreadable_declaration_faults_the_query_leaving_the_refusal() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([text_reply("done")]).unwrap();
    setup.provider(&server);
    idle_hub(&setup);
    let marker = setup.workspace().join("locked.log");
    declare_hooks(
        &setup,
        &json!({"marker": marker_entry(&marker, "blocking")}),
    );
    let (hub, guard) = local_world(&setup);
    let mut run = spawn(
        &setup,
        &[],
        &[("FIBER_TEST_FAKE_KEY".to_owned(), "sk-test".to_owned())],
    );
    run.ready();
    // The fault goes in after the pty and hub are ready and before the
    // start, and stays until the refusal shows.
    let lock = setup.home().join("extensions/.lock");
    fs::remove_file(&lock).unwrap_or(());
    fs::create_dir_all(&lock).unwrap();
    run.write(b"hi\r");
    run.wait_screen("the refusal without the offer", |grid| {
        grid.contents.contains("not installed")
            && grid.contents.contains("fiber extension install hooks")
            && !grid.contents.contains("Install hooks?")
    });
    // The refusal precedes the asynchronous names-query result. Keep
    // the fault in place through shutdown so a late query cannot succeed
    // after the screen predicate returns and open the offer.
    assert!(!marker.exists(), "no hook point ran");
    assert_no_sessions(&setup, "the faulted query");
    assert_not_installed(&setup, "the faulted query");
    drop(server);
    finish_home(hub, guard, run);
}

/// Losing the hub while the install runs: the external clone enters the
/// gate, the test drops the hub, and the terminal's failure names the
/// install command with the connection lost. No start, session or hook
/// marker lands on either machine, and no late worker result retries.
#[test]
fn losing_the_hub_mid_install_fails_without_retry_or_session() {
    let a = support::Setup::new();
    let b = support::Setup::new();
    // The hub machine already imported: the first-run offer reads the
    // hub's receipts, and this journey covers the picker and the offer,
    // never the import itself.
    for receipt in ["import-claude-code.json", "import-codex.json"] {
        write_json(
            &a.home().join(receipt),
            &json!({"source": receipt, "imported_at": 1}),
        );
    }
    fakes::git::git(&a.workspace(), &["init", "-q"]);
    let row = json!({"session_id":"s_remote","ts":1,"project":"remote-project","workspace":a.workspace().display().to_string(),"name":"remote session","how":"exited"});
    fs::write(a.home().join("recent.jsonl"), format!("{row}\n")).unwrap();
    fs::create_dir_all(a.home().join("projects/remote-project/sessions/s_remote")).unwrap();
    let server = ProviderServer::start([support::hello()]).unwrap();
    a.provider(&server);
    let marker = a.workspace().join("remote.log");
    declare_hooks(&a, &json!({"marker": marker_entry(&marker, "blocking")}));
    let key = hooks_fixture(&a);
    // The gate the hub's clone cannot pass until released.
    let gate = a.root.path().join("gate");
    let wbin = gate.join("wbin");
    fs::create_dir_all(&wbin).unwrap();
    let fifo = gate.join("clone.fifo");
    fakes::fifo(&fifo);
    let signal = gate.join("entered");
    write_git_gate(&wbin, &signal, &fifo, &real_git());
    let path = format!(
        "{}:{}",
        wbin.display(),
        std::env::var_os("PATH").unwrap().to_string_lossy()
    );
    write_json(
        &b.home().join("config.json"),
        &json!({"model": "client/other", "tui": {"hover": false}}),
    );
    let served = hub_start::serve_installed(
        &a,
        hub_start::ATTEMPTS,
        &mut hub_start::free_port,
        &mut |port| {
            write_json(
                &a.home().join("config.json"),
                &json!({"model": "fake/m", "hub": {"port": port, "idle_exit_ms": 1000}}),
            );
            let mut command = a.fiber(&["hub", "serve", "--installed"]);
            command.env("HOME", a.home());
            command.envs(install_env(&key));
            command.env("PATH", &path);
            command
        },
    );
    let port = served.port;
    let mut cleanup = GateCleanup {
        process: Some(served.process),
    };
    pair_client(&a, &b, port);
    let ws_a = a.workspace().to_string_lossy().into_owned();
    let hub_guard = SessionGuard::arm(a.deadline, &ws_a);
    let ws_b = b.workspace().to_string_lossy().into_owned();
    let client_guard = SessionGuard::arm(b.deadline, &ws_b);
    let mut run = Run::spawn(
        &b,
        120,
        32,
        &["--hub", "box"],
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    );
    run.ready();
    run.wait_screen("the hub home picker", |grid| {
        grid.contents.contains("Workspaces") && grid.contents.contains("remote session")
    });
    run.write(&[0x7f; 64]);
    run.write(b"\r");
    run.wait_screen("the chosen hub workspace", |grid| {
        !grid.contents.contains("Workspaces")
    });
    run.write(b"say hi\r");
    run.wait_screen("the install offer", |grid| {
        grid.contents.contains("Install hooks?") && grid.contents.contains("marker")
    });
    run.write(b"\r");
    // The install is in flight once the hub's clone reaches the gate:
    // opening the fifo for writing rendezvouses with the gate's blocked
    // read, so the open returning proves install_hooks holds the clone
    // there. The held writer keeps the read blocked until the hub dies;
    // a dropped `release` (the test failed) frees the holder too.
    let (held, opened) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let gate_fifo = fifo.clone();
    let holder = std::thread::spawn(move || {
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&gate_fifo)
            .expect("the gate opens its fifo");
        held.send(()).unwrap();
        // Hold the write end open until the test releases it: closing
        // it would release the gate's blocked read.
        released.recv().unwrap_or(());
        drop(writer);
    });
    a.deadline
        .recv_or_fail(&opened, "the clone to enter the gate");
    assert!(
        fs::read_to_string(&signal).unwrap().contains("clone"),
        "the gate held the install's clone"
    );
    // Drop the hub with the install gated: the failure must name the
    // install command with the connection lost. The kill takes the
    // whole group, but the gated clone stays blocked on the fifo until
    // the release below, so the group cannot be empty yet: wait for
    // the hub leader's exit first, assert while the gate is held, then
    // release and drain the group under the deadline.
    let process = cleanup.process.take().expect("the hub serves");
    process.kill("TERM");
    let HubProc {
        mut child,
        watchdog,
        group,
        deadline,
    } = process;
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(child.wait().expect("the hub leader reaped"))
            .unwrap();
    });
    let status = deadline.recv_or_fail(&finished, "the hub leader to exit");
    assert_eq!(status.code(), Some(143));
    run.wait_screen("the install failure", |grid| {
        grid.contents.contains("installing hooks")
            && grid.contents.contains("fiber extension install hooks")
    });
    assert!(!marker.exists(), "no hook point ran on the hub");
    assert_no_sessions(&a, "the hub after the loss");
    assert_no_sessions(&b, "the client after the loss");
    assert_not_installed(&a, "the hub after the loss");
    assert_not_installed(&b, "the client after the loss");
    drop(release);
    holder.join().expect("the gate holder joins");
    fs::remove_file(&fifo).unwrap_or(());
    // The release lets the gated clone exit: the group must drain under
    // the deadline before the watchdog stands down, so the reaping
    // assertion still holds and no process group escapes.
    fakes::wait_until("the hub group to drain", a.deadline, move || {
        !fakes::group_alive(group)
    });
    assert!(
        !fakes::group_alive(group),
        "the hub left a process in its group behind"
    );
    watchdog.stand_down(deadline.cleanup_phase());
    quit_idle(run);
    hub_guard.wait_gone();
    client_guard.wait_gone();
    drop((server, served.hub, cleanup));
}
