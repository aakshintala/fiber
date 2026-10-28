//! Binary-level tests of a workspace move (`docs/events.md`, "Change
//! workspace" and `docs/invocation.md`, "Change workspace"): the built
//! `fiber` runs `hub serve` in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server. A `change_workspace` closes the old session with `moved`, the
//! hub starts the named child in the old workspace, and the child process
//! itself moves to the recorded target before running through the
//! new-session path there. These run only after the option-B fold lands:
//! the grant-reset assertions need it, while the startup, loader,
//! conversation and recovery assertions hold on this task's plumbing.

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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::hello_item_done as hello;
use support::*;

/// A command for `session` on the hub socket, with its own id.
fn command(id: &str, session: Option<&str>, command: &str, args: Value) -> String {
    let mut line = json!({"id": id, "command": command, "args": args});
    if let Some(session) = session {
        line["session_id"] = session.into();
    }
    serde_json::to_string(&line).unwrap()
}

fn subscribe(id: &str, session: &str) -> String {
    command(id, Some(session), "subscribe", json!({"level": "full"}))
}

fn prompt(id: &str, session: &str, text: &str) -> String {
    command(
        id,
        Some(session),
        "prompt",
        json!({"content": [{"type": "text", "text": text}]}),
    )
}

fn change_workspace(id: &str, session: &str, path: &str) -> String {
    command(id, Some(session), "change_workspace", json!({"path": path}))
}

/// The acknowledgement of command `id`: every line until it, the last
/// accepted.
fn until_ack(client: &Socket, what: &str, id: &str) -> Vec<Value> {
    let lines = until(client, what, |line| {
        line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    assert_eq!(
        lines.last().unwrap()["kind"],
        "command_accepted",
        "{lines:?}"
    );
    lines
}

/// The rejection of command `id`.
fn until_rejected(client: &Socket, what: &str, id: &str) -> Value {
    let lines = until(client, what, |line| {
        line["kind"] == "command_rejected"
            && line["payload"].get("command_id").and_then(Value::as_str) == Some(id)
    });
    lines.last().unwrap().clone()
}

/// The log of session `id`, wherever its project is.
fn session_log(setup: &Setup, id: &str) -> Vec<Value> {
    fs::read_to_string(session_log_path(setup, id))
        .map(|text| complete_lines(&text))
        .unwrap()
}

fn session_log_path(setup: &Setup, id: &str) -> PathBuf {
    fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .expect("the session's log exists")
}

/// The project key of `dir`: the git common directory when `dir` is in a
/// repository, else `dir` itself, canonicalized with slashes replaced, as
/// `worktree::project` plus `log::project_key` compute it.
fn project_key_of(dir: &Path) -> String {
    let mut git = Command::new("git");
    git.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let path = match git.output() {
        Ok(out) if out.status.success() => {
            PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        _ => dir.to_path_buf(),
    };
    let path = fs::canonicalize(&path).unwrap_or(path);
    path.to_string_lossy().replace('/', "-")
}

/// Two folders sharing one git project, plus two unrelated git projects:
/// every move below stays inside this layout.
struct Layout {
    shared_a: PathBuf,
    shared_b: PathBuf,
    repo_a: PathBuf,
    repo_b: PathBuf,
}

impl Layout {
    fn new(setup: &Setup) -> Self {
        let root = setup.root.path();
        let repo = root.join("shared");
        fs::create_dir_all(repo.join("a")).unwrap();
        fs::create_dir_all(repo.join("b")).unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        let repo_a = root.join("repo-a");
        fs::create_dir_all(&repo_a).unwrap();
        fakes::git::git(&repo_a, &["init", "--quiet"]);
        fakes::git::git(&repo_a, &["add", "."]);
        let repo_b = root.join("repo-b");
        fs::create_dir_all(&repo_b).unwrap();
        fakes::git::git(&repo_b, &["init", "--quiet"]);
        fakes::git::git(&repo_b, &["add", "."]);
        Self {
            shared_a: repo.join("a"),
            shared_b: repo.join("b"),
            repo_a,
            repo_b,
        }
    }
}

/// Starts the hub, one client and a session guard per workspace: the
/// caller runs the turns in any of the workspaces.
struct Hubbed {
    hub: Arc<Mutex<Option<HubProc>>>,
    client: Option<Socket>,
    guards: Vec<SessionGuard>,
    home: PathBuf,
}

impl Hubbed {
    fn new(setup: &Setup, workspaces: &[PathBuf]) -> Self {
        let hub = Arc::new(Mutex::new(None));
        let (client, hello) = connect_hub(setup, &hub);
        assert_eq!(hello["kind"], "hub_hello");
        let guards = workspaces
            .iter()
            .map(|workspace| SessionGuard::arm(setup.deadline, &workspace.to_string_lossy()))
            .collect();
        Self {
            hub,
            client: Some(client),
            guards,
            home: setup.home(),
        }
    }

    fn client(&self) -> &Socket {
        self.client.as_ref().expect("the client is connected")
    }

    /// Starts a session in `workspace` with `content` under command `id`,
    /// subscribes `sub_id` full to it, and waits for its first turn to
    /// complete.
    fn start_in(&self, id: &str, sub_id: &str, workspace: &Path, content: &str) -> String {
        self.client().send(&command(
            id,
            None,
            "start",
            json!({"workspace": workspace.to_string_lossy(), "content": [{"type": "text", "text": content}]}),
        ));
        let ack = until_ack(self.client(), "the start acknowledgement", id);
        let session = ack.last().unwrap()["payload"]["result"]["session_id"]
            .as_str()
            .expect("the start answers with a session id")
            .to_owned();
        self.client().send(&subscribe(sub_id, &session));
        until_ack(self.client(), "the subscribe acknowledgement", sub_id);
        until(self.client(), "the first turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
        session
    }

    /// Prompts `session` with `text` under command `id`, and waits for the
    /// acknowledgement and the turn to complete.
    fn prompt(&self, id: &str, session: &str, text: &str) {
        self.client().send(&prompt(id, session, text));
        until_ack(self.client(), "the prompt acknowledgement", id);
        until(self.client(), "the turn_completed", |line| {
            line["kind"] == "turn_completed" && line["session_id"] == session
        });
    }

    /// Moves `session` to `target` under command `id`, and answers with
    /// the new session id.
    fn move_to(&self, id: &str, session: &str, target: &Path) -> String {
        self.client()
            .send(&change_workspace(id, session, &target.to_string_lossy()));
        let ack = until_ack(self.client(), "the move acknowledgement", id);
        ack.last().unwrap()["payload"]["result"]["new_session_id"]
            .as_str()
            .expect("the move answers with a session id")
            .to_owned()
    }

    /// Closes `session` under command `id`, and waits for its `fiber_exited`.
    fn close(&self, id: &str, session: &str) {
        self.client()
            .send(&command(id, Some(session), "close", json!({})));
        until(self.client(), "the fiber_exited", |line| {
            line["kind"] == "fiber_exited" && line["session_id"] == session
        });
    }

    fn finish(self) {
        let Self {
            hub, guards, home, ..
        } = self;
        for guard in guards {
            // A holder that outlives the wait is a session the hub
            // started and the test never closed through the hub: an
            // idle one hears no close once the hub below is dead, so
            // the wait names each session's last line with the timeout
            // instead of failing on the bare wait.
            let workspace = guard.workspace.clone();
            let waited =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.wait_gone()));
            if let Err(payload) = waited {
                hub.lock()
                    .unwrap()
                    .take()
                    .expect("the hub ran")
                    .kill_and_wait();
                panic!(
                    "{}\nworkspace {workspace} holders, last session lines:\n{}",
                    payload_message(&payload),
                    session_log_tails(&home).join("\n"),
                );
            }
        }
        hub.lock()
            .unwrap()
            .take()
            .expect("the hub ran")
            .kill_and_wait();
    }
}

/// A caught panic's message: the workspace wait panics with a formatted
/// string, so either string shape reads.
fn payload_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "the wait failed without a message".to_owned()
    }
}

/// The last complete line of every session log under `home`, oldest
/// session first: the workspace wait already names its holders' pids
/// and command lines, and these tails name what each session was doing
/// when it stopped answering.
fn session_log_tails(home: &Path) -> Vec<String> {
    let mut tails = Vec::new();
    let Ok(projects) = fs::read_dir(home.join("projects")) else {
        return tails;
    };
    for project in projects.flatten() {
        let Ok(sessions) = fs::read_dir(project.path().join("sessions")) else {
            continue;
        };
        for session in sessions.flatten() {
            let path = session.path().join("events.jsonl");
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(last) = complete_lines(&text).pop() {
                let id = session.file_name().to_string_lossy().into_owned();
                tails.push(format!("session {id}: {last}"));
            }
        }
    }
    tails.sort();
    tails
}

#[test]
fn finish_names_session_tails_when_a_holder_outlives_the_wait() {
    let setup = Setup::new();
    // A holder whose command line carries the workspace: `exec -a`
    // names argv[0], which the workspace wait matches. It blocks
    // copying its stdin pipe, which the test holds open, so the
    // guard's timeout is decided by the pipe staying open, never by
    // the clock.
    let held = setup.root.path().join("held");
    let held_text = held.to_string_lossy().into_owned();
    let mut spawned = Command::new("bash");
    spawned
        .args(["-c", &format!("exec -a 'holder {held_text}' cat")])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // The guarded group helper: its own process group, reaped below,
    // with a watchdog killing the group if the test dies first.
    let (mut holder, watchdog) = spawn_watched(&mut spawned);
    let stdin = holder.stdin.take().expect("the holder keeps stdin piped");
    let group = holder.id();
    // The guard's first listing must see the holder: a short wait armed
    // before the holder appears would pass with nothing to wait on,
    // instead of timing out on the live holder.
    loop {
        if fakes::matching(&held_text)
            .unwrap_or_default()
            .contains(&group)
        {
            break;
        }
        assert!(
            !setup.deadline.left().is_zero(),
            "waited until the deadline for the holder to appear"
        );
        thread::yield_now();
    }
    // One planted session log, ending `moved`: the failure names its
    // last line with the wait.
    let dir = setup
        .home()
        .join("projects")
        .join("p1")
        .join("sessions")
        .join("s_old");
    fs::create_dir_all(&dir).unwrap();
    let first = serde_json::to_string(&json!({"kind": "session_started"})).unwrap();
    let moved = serde_json::to_string(&json!({"kind": "moved"})).unwrap();
    fs::write(dir.join("events.jsonl"), format!("{first}\n{moved}\n")).unwrap();
    let mut hubbed = Hubbed::new(&setup, &[]);
    // The holder has been confirmed above; an expired deadline forces
    // the diagnostic path without waiting for wall-clock time.
    hubbed.guards.push(SessionGuard::arm(
        Deadline::after(std::time::Duration::ZERO),
        &held_text,
    ));
    let finished = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hubbed.finish()));
    // Release the pipe the holder was blocked on, then reap it and its
    // group under the cleanup bound before asserting on the message, so
    // no zombie outlives the test.
    drop(stdin);
    let cleanup = setup.deadline.cleanup_phase();
    let (done, reaped) = mpsc::channel();
    thread::spawn(move || done.send(holder.wait()).unwrap());
    match cleanup.recv(&reaped) {
        Ok(status) => status.unwrap(),
        Err(_) => panic!("waited until the cleanup deadline to reap the holder"),
    };
    assert!(
        fakes::group_empties(group, cleanup),
        "the holder left a process in its group behind"
    );
    watchdog.stand_down(cleanup);
    let message = payload_message(&finished.expect_err("a live holder outlives the short wait"));
    assert!(message.contains(&held_text), "{message}");
    assert!(message.contains("holding the workspace path"), "{message}");
    assert!(message.contains("session s_old"), "{message}");
    assert!(message.contains("\"moved\""), "{message}");
}

/// Waits for `child`'s `session_started` on the client: the redirect
/// replays the new session's lines once it runs.
fn until_child_started(client: &Socket, child: &str) -> Value {
    until(client, "the moved child's session_started", |line| {
        line["session_id"] == child && line["kind"] == "session_started"
    })
    .into_iter()
    .find(|line| line["kind"] == "session_started")
    .unwrap()
}

#[test]
fn a_move_before_any_model_turn_starts_the_child_in_the_target() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let client = hubbed.client();
    // No content: the session starts with no model turn.
    client.send(&command(
        "c_start",
        None,
        "start",
        json!({"workspace": layout.repo_a}),
    ));
    let ack = until_ack(client, "the start acknowledgement", "c_start");
    let old = ack.last().unwrap()["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    client.send(&subscribe("c_sub", &old));
    until_ack(client, "the subscribe acknowledgement", "c_sub");
    // The old session was never prompted: ended moves are retained even
    // with no model turn, so its `moved` record is still written.
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(client, &child);
    // The old log ends `moved`, naming the target and the child.
    let old_log = session_log(&setup, &old);
    let moved = old_log.last().unwrap();
    assert_eq!(moved["kind"], "moved");
    assert_eq!(moved["payload"]["new_session_id"], child);
    let target = fs::canonicalize(&layout.repo_b).unwrap();
    assert_eq!(
        moved["payload"]["workspace"],
        target.to_string_lossy().as_ref()
    );
    // The child starts in the target, continuing the old session at its
    // only durable line, with the parent's project recorded.
    let child_log = session_log(&setup, &child);
    let started = &child_log[0];
    assert_eq!(started["kind"], "session_started");
    assert_eq!(
        started["payload"]["workspace"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(started["payload"]["forked_from"]["session_id"], old);
    assert_eq!(
        started["payload"]["forked_from"]["seq"],
        moved["payload"]["seq"]
    );
    assert_eq!(
        started["payload"]["forked_from"]["project"],
        project_key_of(&layout.repo_a)
    );
    assert!(started["payload"]["rewind"]["summary"].is_null());
    assert_eq!(started["payload"]["rewind"]["note"], "");
    assert!(started["payload"].get("worktree").is_none());
    assert!(
        child_log.iter().all(|line| line["kind"] != "session_named"),
        "no name to copy"
    );
    assert!(server.requests().is_empty(), "no model call yet");
    hubbed.close("c_close", &child);
    hubbed.finish();
}

#[test]
fn moving_between_two_folders_of_one_git_project_records_no_project() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.shared_a.clone(), layout.shared_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.shared_a, "one");
    let child = hubbed.move_to("c_mv", &old, &layout.shared_b);
    until_child_started(hubbed.client(), &child);
    let child_log = session_log(&setup, &child);
    let started = &child_log[0];
    assert_eq!(started["kind"], "session_started");
    assert_eq!(started["payload"]["forked_from"]["session_id"], old);
    assert!(
        started["payload"]["forked_from"].get("project").is_none(),
        "same project records nothing: {:?}",
        started["payload"]["forked_from"]
    );
    // Both sessions live under one project key.
    let key = project_key_of(&layout.shared_a);
    assert_eq!(key, project_key_of(&layout.shared_b));
    assert!(
        session_log_path(&setup, &old)
            .to_string_lossy()
            .contains(&key)
            && session_log_path(&setup, &child)
                .to_string_lossy()
                .contains(&key)
    );
    hubbed.close("c_close", &child);
    hubbed.finish();
}

#[test]
fn moving_between_two_real_projects_records_the_parent_project() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    let child_log = session_log(&setup, &child);
    let started = &child_log[0];
    assert_eq!(
        started["payload"]["forked_from"]["project"],
        project_key_of(&layout.repo_a)
    );
    assert!(
        session_log_path(&setup, &child)
            .to_string_lossy()
            .contains(&project_key_of(&layout.repo_b))
    );
    hubbed.close("c_close", &child);
    hubbed.finish();
}

/// A finished `function_call` for `name_session` with `arguments`.
fn name_call(call_id: &str, arguments: &Value) -> Value {
    function_call(call_id, "name_session", arguments)
}

#[test]
fn the_moved_child_inherits_the_conversation_without_a_summary() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "Fix the parser"}))]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let old_log = session_log(&setup, &old);
    let named = old_log
        .iter()
        .find(|line| line["kind"] == "session_named")
        .expect("the first turn names the session");
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    // The child's record carries the point, no summary, no note, no
    // worktree, and the copied name right after the start.
    let child_log = session_log(&setup, &child);
    let started = &child_log[0];
    assert_eq!(started["payload"]["forked_from"]["session_id"], old);
    assert!(started["payload"]["rewind"]["summary"].is_null());
    assert_eq!(started["payload"]["rewind"]["note"], "");
    assert!(started["payload"].get("worktree").is_none());
    assert_eq!(child_log[1]["kind"], "session_named");
    assert_eq!(child_log[1]["payload"], named["payload"]);
    // The next prompt's request still carries the parent's conversation.
    hubbed.prompt("c_two", &child, "two");
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert!(
        serde_json::to_string(&body).unwrap().contains("one"),
        "the parent's prompt reaches the child's request"
    );
    hubbed.close("c_close", &child);
    hubbed.finish();
}

/// The package manifest `name` at `version`.
fn manifest(name: &str, version: &str) -> Value {
    json!({"name": name, "version": version, "fiber": "0.1.0", "api": 1})
}

/// Writes these repository packages into `dir`, which becomes a git
/// repository: each `(dir, manifest, init.lua text, required)`.
fn declare(dir: &Path, pkgs: &[(&str, Value, &str, bool)]) {
    fakes::git::git(dir, &["init", "--quiet"]);
    let mut declared = Vec::new();
    for (pkg, manifest, init, required) in pkgs {
        let root = dir.join(pkg);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("extension.json"), manifest.to_string()).unwrap();
        fs::write(root.join("init.lua"), init).unwrap();
        declared.push(json!({"path": pkg, "required": required}));
    }
    write_json(
        &dir.join(".fiber/config.json"),
        &json!({"repository_extensions": declared}),
    );
}

/// `fiber approve --yes` in `dir`.
fn approve(setup: &Setup, dir: &Path) {
    let mut command = setup.fiber(&["approve", "--yes"]);
    command.current_dir(dir);
    let output = run_to_exit(setup.deadline, "fiber approve", command);
    assert!(output.status.success(), "fiber approve: {output:?}");
}

/// The entries of the loaded set the `extensions_loaded` line carries:
/// each `(name, version)`.
fn loaded_set(line: &Value) -> Vec<(String, String)> {
    line["payload"]["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["name"].as_str().unwrap().to_owned(),
                entry["version"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn target_repository_code_loads_fresh_with_its_own_version() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    declare(
        &layout.repo_a,
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0"),
            "-- one\n",
            false,
        )],
    );
    declare(
        &layout.repo_b,
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v2.0.0"),
            "-- two\n",
            false,
        )],
    );
    approve(&setup, &layout.repo_a);
    approve(&setup, &layout.repo_b);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let old_loaded = session_log(&setup, &old)
        .into_iter()
        .find(|line| line["kind"] == "extensions_loaded")
        .unwrap();
    assert!(
        loaded_set(&old_loaded).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{old_loaded:?}"
    );
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    until(hubbed.client(), "the moved child's extensions", |line| {
        line["session_id"] == child && line["kind"] == "extensions_loaded"
    });
    // The child loads the target's set fresh: its version, never the
    // source's, with the target project's own approvals behind it.
    let child_loaded = session_log(&setup, &child)
        .into_iter()
        .find(|line| line["kind"] == "extensions_loaded")
        .unwrap();
    let set = loaded_set(&child_loaded);
    assert!(
        set.contains(&("fiber.test/pkg".to_owned(), "v2.0.0".to_owned())),
        "{child_loaded:?}"
    );
    assert!(
        !set.contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "no source pinned copy leaks across: {child_loaded:?}"
    );
    for repo in [&layout.repo_a, &layout.repo_b] {
        let approvals = setup
            .home()
            .join("projects")
            .join(project_key_of(repo))
            .join("approvals");
        assert!(
            fs::read_dir(&approvals).unwrap().count() > 0,
            "each project holds its own approvals: {approvals:?}"
        );
    }
    hubbed.close("c_close", &child);
    hubbed.finish();
}

#[test]
fn two_successive_target_requests_keep_a_stable_prefix() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    hubbed.prompt("c_two", &child, "two");
    hubbed.prompt("c_three", &child, "three");
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let first: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let second: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(first["instructions"], second["instructions"]);
    assert_eq!(first["tools"], second["tools"]);
    hubbed.close("c_close", &child);
    hubbed.finish();
}

/// Runs `fiber sessions delete --yes <id>`: stdin null, in `workspace`.
fn delete(setup: &Setup, workspace: &Path, id: &str) -> std::process::Output {
    let mut command = setup.fiber(&["sessions", "delete", "--yes", id]);
    command.current_dir(workspace);
    run_to_exit(setup.deadline, "fiber sessions delete", command)
}

/// Runs `fiber sessions prune --yes`: stdin null, in `workspace`. With no
/// `--older-than` no session is selected: only worktrees go.
fn prune(setup: &Setup, workspace: &Path) -> std::process::Output {
    let mut command = setup.fiber(&["sessions", "prune", "--yes"]);
    command.current_dir(workspace);
    run_to_exit(setup.deadline, "fiber sessions prune", command)
}

#[test]
fn a_missing_required_extension_fails_the_start_and_a_fixed_target_retries() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    // The target requires a package whose startup fails.
    declare(
        &layout.repo_b,
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0"),
            "error(\"boom\")\n",
            true,
        )],
    );
    approve(&setup, &layout.repo_b);
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let client = hubbed.client();
    client.send(&change_workspace(
        "c_mv",
        &old,
        &layout.repo_b.to_string_lossy(),
    ));
    // The session accepts the move, then the hub answers the start's own
    // failure instead: the old session stays ended `moved`.
    let rejected = until_rejected(client, "the start failure", "c_mv");
    assert_eq!(rejected["payload"]["code"], "extension_required_failed");
    let old_log = session_log(&setup, &old);
    let moved = old_log.last().unwrap();
    assert_eq!(moved["kind"], "moved");
    let same_child = moved["payload"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Fix the package and approve its new content. The hub never retries
    // a recorded child: rerun its recorded start directly, with the same
    // id, and read its startup off its stdout.
    fs::write(layout.repo_b.join("pkg/init.lua"), "-- fixed\n").unwrap();
    approve(&setup, &layout.repo_b);
    hubbed.finish();
    fs::remove_file(setup.home().join("run").join(&same_child)).unwrap_or(());
    let mut rerun = setup.fiber(&[
        "session",
        "--workspace",
        &layout.repo_a.to_string_lossy(),
        "--id",
        &same_child,
        "--rewound-from",
        &old,
    ]);
    rerun.current_dir(&layout.repo_a);
    let (mut child, _watchdog) = spawn_watched(&mut rerun);
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let started = first_line(setup.deadline, &lines);
    assert_eq!(started["kind"], "session_started");
    assert_eq!(started["payload"]["forked_from"]["session_id"], old);
    // The retry boots fully before the log is read: startup prints its
    // lines in order, so the third is the loaded set.
    assert_eq!(first_line(setup.deadline, &lines)["kind"], "fiber_started");
    let loaded_line = first_line(setup.deadline, &lines);
    assert_eq!(loaded_line["kind"], "extensions_loaded");
    // The fixed package loads on the retry.
    assert!(
        loaded_set(&loaded_line).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{loaded_line:?}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn old_session_commands_resume_the_old_workspace() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    // The child's startup settles before the snapshot: only then is its
    // log stable while it idles.
    until(hubbed.client(), "the moved child's extensions", |line| {
        line["session_id"] == child && line["kind"] == "extensions_loaded"
    });
    let child_len = session_log(&setup, &child).len();
    // An old-id command resumes the old workspace; nothing redirects it
    // onto the child, and the child's log gains nothing.
    hubbed.prompt("c_old_two", &old, "two again");
    let old_log = session_log(&setup, &old);
    assert_eq!(old_log.last().unwrap()["kind"], "turn_completed");
    assert_eq!(old_log.last().unwrap()["session_id"], old);
    assert_eq!(session_log(&setup, &child).len(), child_len);
    hubbed.close("c_close_old", &old);
    hubbed.close("c_close", &child);
    hubbed.finish();
}

#[test]
fn same_workspace_rewind_records_no_project() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, std::slice::from_ref(&layout.repo_a));
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    hubbed.prompt("c_two", &old, "two");
    hubbed
        .client()
        .send(&command("c_rw", Some(&old), "rewind", json!({})));
    let ack = until_ack(hubbed.client(), "the rewind acknowledgement", "c_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until_child_started(hubbed.client(), &next);
    let child_log = session_log(&setup, &next);
    assert!(
        child_log[0]["payload"]["forked_from"]
            .get("project")
            .is_none(),
        "the rewind control records nothing: {:?}",
        child_log[0]["payload"]["forked_from"]
    );
    hubbed.close("c_close", &next);
    hubbed.finish();
}

#[test]
fn a_rewind_to_a_pre_move_point_reads_across_projects() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    hubbed.prompt("c_two", &old, "two");
    // The pre-move point: the old session's latest turn start.
    let point = session_log(&setup, &old)
        .into_iter()
        .filter(|line| line["kind"] == "turn_started")
        .map(|line| line["seq"].as_u64().unwrap())
        .next_back()
        .unwrap();
    let moved = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &moved);
    // Rewind the moved session to the pre-move point in the old project.
    hubbed.client().send(&command(
        "c_rw",
        Some(&moved),
        "rewind",
        json!({"from_session_id": old, "seq": point}),
    ));
    let ack = until_ack(hubbed.client(), "the cross-project rewind", "c_rw");
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until_child_started(hubbed.client(), &next);
    // The rewind child points at the old project with its project key, so
    // its history walks there.
    let child_log = session_log(&setup, &next);
    assert_eq!(child_log[0]["payload"]["forked_from"]["session_id"], old);
    assert_eq!(child_log[0]["payload"]["forked_from"]["seq"], point);
    assert_eq!(
        child_log[0]["payload"]["forked_from"]["project"],
        project_key_of(&layout.repo_a)
    );
    // Resume works on the rewind child, and the leaf deletes clean.
    hubbed.prompt("c_three", &next, "three");
    hubbed.close("c_close_next", &next);
    let log = session_log_path(&setup, &next);
    let output = delete(&setup, &layout.repo_b, &next);
    assert!(output.status.success(), "delete the leaf: {output:?}");
    assert!(!log.exists(), "the leaf's log is gone");
    hubbed.finish();
}

#[test]
fn a_rewind_names_the_ancestor_project_after_its_workspace_moves_away() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, "one");
    hubbed.prompt("c_two", &old, "two");
    // The pre-move point: the old session's latest turn start.
    let point = session_log(&setup, &old)
        .into_iter()
        .filter(|line| line["kind"] == "turn_started")
        .map(|line| line["seq"].as_u64().unwrap())
        .next_back()
        .unwrap();
    // The ancestor's project key, read while its workspace still resolves.
    let expected = project_key_of(&layout.repo_a);
    let moved = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &moved);
    // The recorded workspace moves away, but the ancestor's log stays in
    // its project: the continuation still names that project.
    let away = setup.root.path().join("repo-a-away");
    fs::rename(&layout.repo_a, &away).unwrap();
    hubbed.client().send(&command(
        "c_rw",
        Some(&moved),
        "rewind",
        json!({"from_session_id": old, "seq": point}),
    ));
    let ack = until_ack(
        hubbed.client(),
        "the cross-project rewind after the workspace moved",
        "c_rw",
    );
    let next = ack.last().unwrap()["payload"]["result"]["new_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    until_child_started(hubbed.client(), &next);
    let child_log = session_log(&setup, &next);
    assert_eq!(child_log[0]["payload"]["forked_from"]["session_id"], old);
    assert_eq!(child_log[0]["payload"]["forked_from"]["seq"], point);
    assert_eq!(
        child_log[0]["payload"]["forked_from"]["project"], expected,
        "the project comes from the ancestor's directory, not its moved workspace"
    );
    hubbed.prompt("c_three", &next, "three");
    hubbed.close("c_close_next", &next);
    let log = session_log_path(&setup, &next);
    let output = delete(&setup, &layout.repo_b, &next);
    assert!(output.status.success(), "delete the leaf: {output:?}");
    assert!(!log.exists(), "the leaf's log is gone");
    hubbed.finish();
}

/// A tiny page server with one response per fetch below: the start, the
/// old session's prompt, and the child's prompt after the move.
fn site(body: &str) -> ProviderServer {
    let page = || Response {
        status: 200,
        headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
        body: body.as_bytes().to_vec(),
        drop_connection: false,
        stall: false,
    };
    ProviderServer::start([page(), page(), page()]).unwrap()
}

fn fetch_call(url: &str) -> Value {
    function_call("call_fetch", "web_fetch", &json!({"url": url}))
}

#[test]
fn known_hosts_carry_over_the_move() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    let site = site("known host page");
    let url = format!("{}/doc?q=1", site.url());
    // One fetch turn each: the start, the old session's prompt, and the
    // child's prompt after the move.
    let server = ProviderServer::start([
        stream(&[fetch_call(&url)]),
        hello(),
        stream(&[fetch_call(&url)]),
        hello(),
        stream(&[fetch_call(&url)]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    // The prompt names the host, so the first fetch takes the fast path
    // and surfaces the host to the conversation.
    let old = hubbed.start_in("c_start", "c_sub", &layout.repo_a, &format!("read {url}"));
    hubbed.prompt("c_fetch", &old, &format!("read {url}"));
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    // The next prompt never names the host: the child's fetch takes the
    // fast path only when the known hosts carried over.
    hubbed.prompt("c_fetch_again", &child, "fetch it again");
    let child_log = session_log(&setup, &child);
    let completed = child_log
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .expect("the child's fetch completes");
    assert_eq!(completed["payload"]["status"], "completed");
    assert!(
        child_log
            .iter()
            .all(|line| line["kind"] != "permission_resolved"),
        "no permission gate on a known host"
    );
    assert_eq!(site.requests().len(), 3);
    hubbed.close("c_close", &child);
    hubbed.finish();
}

/// Polls under the deadline until some session log ends in `moved`,
/// returning the old and child ids: the gate between typing `/cd` and
/// observing the switched terminal, so no later key races the attach.
#[track_caller]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn until_moved(setup: &Setup) -> (String, String) {
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
        thread::yield_now();
    }
}

/// Writes `/pwd` until the screen names `target`, proving the terminal
/// attached to the moved child: the move's accept may land after an
/// early probe goes out, answering it with the old workspace. The
/// readiness signals are the log gate before this helper (the old
/// session ends `moved` before the next key goes out) and the screen
/// naming `target` here; between probes it waits on the terminal's own
/// output through `wait_screen` for either a changed screen or the
/// target, never a sleep and never a second deadline (`docs/testing.md`,
/// "Waits and timeouts").
#[track_caller]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn until_pwd_shows(run: &mut pty::Run, target: &str, deadline: Deadline) {
    loop {
        let before = run.screen().contents.clone();
        if before.contains(target) {
            return;
        }
        assert!(
            !deadline.left().is_zero(),
            "waited until the deadline for the target workspace {target} in a notice; screen:\n{before}"
        );
        run.write(b"/pwd\r");
        run.wait_screen("the answered `/pwd` notice", |grid| {
            grid.contents.contains(target) || grid.contents != before
        });
    }
}

/// Through the real terminal: `/cd` switches to the moved child, whose
/// workspace answers `/pwd`, and the rewind view pages the parent's
/// turn through the moved link.
#[test]
fn pty_cd_switches_to_the_moved_session_and_pages_its_parent() {
    let setup = Setup::new();
    // A short target outside the temp root: the `/pwd` notice cuts a
    // long path with an ellipsis, so the whole target must fit on one
    // notice row for the screen to name it.
    let target = std::env::temp_dir().join(format!("fiber-cd-{}", std::process::id()));
    if target.exists() {
        fs::remove_dir_all(&target).unwrap();
    }
    fs::create_dir_all(&target).unwrap();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    // The hub a client starts exits on idle: without the short idle the
    // PTY's hub would outlive the test's deadline after the terminal
    // quits, with the moved sessions still running.
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["hub"] = json!({"idle_exit_ms": 200});
    write_json(&path, &config);
    let mut run = pty::Run::spawn(&setup, 120, 32, &[], &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    // The first frame draws the input line; its title proves the input
    // reader runs before the prompt goes out.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.wait_bytes(0, pty::KITTY_PUSH, "kitty keyboard flags pushed");
    // The prompt starts the old session in the launch workspace.
    let from = run.output().len();
    run.write(b"say hi\r");
    run.turn_finished(from);
    // The move names the target; the log gate proves the old session
    // ended `moved` before the next key goes out.
    run.write(format!("/cd {}\r", target.to_string_lossy()).as_bytes());
    let (old, child) = until_moved(&setup);
    assert_ne!(old, child, "the move names a new session");
    // The switched terminal answers `/pwd` with the target workspace, as
    // the loop records it: canonicalized, so `/var` reads `/private/var`.
    let recorded = fs::canonicalize(&target)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    until_pwd_shows(&mut run, &recorded, setup.deadline);
    // The rewind view pages the parent's turn through the moved link.
    run.write(b"/rewind\r");
    run.wait_screen("the parent turn in the rewind view", |grid| {
        grid.contents.contains("say hi")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    fs::remove_dir_all(&target).unwrap();
}

#[test]
fn the_old_worktree_survives_the_move_and_a_later_prune_removes_it() {
    let setup = Setup::new();
    let layout = Layout::new(&setup);
    // The old session needs a commit to start in a worktree of it.
    fs::write(layout.repo_a.join("file.txt"), "x").unwrap();
    fakes::git::git(&layout.repo_a, &["add", "."]);
    fakes::git::git(&layout.repo_a, &["commit", "--quiet", "-m", "first"]);
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let hubbed = Hubbed::new(&setup, &[layout.repo_a.clone(), layout.repo_b.clone()]);
    // Start the old session in a Fiber-managed worktree of repo_a.
    let client = hubbed.client();
    client.send(&command(
        "c_start",
        None,
        "start",
        json!({"workspace": layout.repo_a.to_string_lossy(), "worktree": true,
            "content": [{"type": "text", "text": "one"}]}),
    ));
    let ack = until_ack(client, "the start acknowledgement", "c_start");
    let old = ack.last().unwrap()["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    client.send(&subscribe("c_sub", &old));
    until_ack(client, "the subscribe acknowledgement", "c_sub");
    until(client, "the first turn_completed", |line| {
        line["kind"] == "turn_completed" && line["session_id"] == old
    });
    let old_log = session_log(&setup, &old);
    let old_worktree = PathBuf::from(
        old_log[0]["payload"]["worktree"]["path"]
            .as_str()
            .expect("the old session runs in a worktree"),
    );
    assert!(old_worktree.is_dir(), "the old worktree exists");
    let child = hubbed.move_to("c_mv", &old, &layout.repo_b);
    until_child_started(hubbed.client(), &child);
    // The move ends the old session but removes nothing of its tree.
    assert!(old_worktree.is_dir(), "the old worktree survives the move");
    // With the leaf deleted, a later normal prune removes the clean old
    // worktree under the unchanged shared-lock and dirty rules, while the
    // plain target workspace and the young old log stay.
    hubbed.close("c_close", &child);
    let output = delete(&setup, &layout.repo_b, &child);
    assert!(output.status.success(), "delete the leaf: {output:?}");
    // Prune scopes to its working directory's project: the old worktree
    // lives in repo_a's project, so prune runs there.
    let output = prune(&setup, &layout.repo_a);
    assert!(
        output.status.success(),
        "prune the old worktree: {output:?}"
    );
    assert!(
        !old_worktree.exists(),
        "a later normal prune removes the old worktree"
    );
    assert!(layout.repo_b.is_dir(), "the target workspace stays");
    assert!(
        session_log_path(&setup, &old).is_file(),
        "the young old log stays without --older-than"
    );
    hubbed.finish();
}
