//! Binary-level tests of a repository's servers through `fiber ask`
//! (`docs/mcp.md`, "A repository's servers"): a declared server starts
//! only when its current content is approved, runs from its pinned copy
//! in Fiber home, and a changed declaration or file needs a new approval.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, HubProc, KillGroup, Run, function_call, hello, is_status, spawn_watched, stream,
};

/// The stdout kinds of one `fiber ask` text turn, in order: the
/// complete list, so a duplicated, missing or reordered event fails
/// (`docs/testing.md`, "Event streams").
const ASK_TEXT_TURN: [&str; 15] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// [`ASK_TEXT_TURN`] with the headless skip notice before the preamble:
/// an unapproved server is skipped with a `notice`.
const ASK_SKIPPED_TURN: [&str; 16] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "notice",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// [`ASK_TEXT_TURN`] for a turn that calls a tool: the key check drops
/// the repository's hints with a `notice`, and the call's lines sit
/// between the turn's.
const ASK_TOOL_TURN: [&str; 23] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "notice",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
    /// Kills every process whose command line names the test's root,
    /// and their groups, when the setup drops: every fixture server runs
    /// with its directory under the root as its argument, so a hang or a
    /// failing exit still kills the server and its grandchild.
    #[allow(dead_code, reason = "held only for its drop")]
    guard: Watchdog,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fa");
        // Armed before any start or run: the root is known here, while
        // no server pid is.
        let guard = Watchdog::matching(&root.path().display().to_string());
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline,
            root,
            guard,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Writes the fixture server's directory: `tools.json` and one result
    /// file per `(name, body)` pair. Returns the directory.
    fn fixture(&self, tools: &Value, calls: &[(&str, &str)]) -> PathBuf {
        let dir = self.root.path().join("fx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
        for (name, body) in calls {
            fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    /// Declares the repository server `db` in `workspace`: `/bin/bash`
    /// runs `srv/run.sh` on the fixture `dir`. The script writes the path
    /// it ran as to `argv0.txt` in the fixture directory, then runs the
    /// MCP fixture there: the copy is what an approval pins.
    fn declare(&self, workspace: &Path, dir: &Path, extra: &Value, more_args: &[String]) {
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(workspace)
            .status()
            .unwrap();
        assert!(status.success());
        let srv = workspace.join("srv");
        fs::create_dir_all(&srv).unwrap();
        fs::write(
            srv.join("run.sh"),
            format!(
                "#!/bin/bash\nprintf '%s\\n' \"$0\" > \"{}/argv0.txt\"\nexec /bin/bash \"{}\" \"$1\"\n",
                dir.display(),
                fakes::mcp_fixture().display(),
            ),
        )
        .unwrap();
        let mut args = vec![
            Value::String("srv/run.sh".to_owned()),
            Value::String(dir.display().to_string()),
        ];
        args.extend(more_args.iter().cloned().map(Value::String));
        let mut entry = json!({
            "command": "/bin/bash",
            "args": Value::Array(args),
        });
        for (key, value) in extra.as_object().unwrap() {
            entry[key] = value.clone();
        }
        write(
            &workspace.join(".fiber/config.json"),
            &json!({"mcp": {"servers": {"db": entry}}}),
        );
    }

    /// Approves everything the workspace declares, as `fiber approve`
    /// does in the repository.
    fn approve(&self, workspace: &Path) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["approve", "--yes"])
            .current_dir(workspace)
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = support::run_to_exit(self.deadline, "fiber approve", command);
        assert!(output.status.success(), "{output:?}");
    }

    /// The one approval file name: the approved content's hash.
    fn approval_hash(&self) -> String {
        let mut names: Vec<String> = fs::read_dir(self.home().join("approvals"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        names.pop().unwrap()
    }

    /// Runs `fiber` in `dir` in its own process group, waits for it under
    /// the test's [`Deadline`], and asserts that nothing it started is left
    /// in the group, after a timeout too (`docs/testing.md`, "Running
    /// tests"). A watchdog beside it kills that group if this process dies
    /// first.
    #[track_caller]
    fn run_in(&self, dir: &Path, args: &[&str]) -> Run {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(dir)
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            fakes::group_empties(group, self.deadline),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Run::new(output)
    }

    /// Runs `fiber` in the workspace.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        let workspace = self.workspace();
        self.run_in(&workspace, args)
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

impl Run {
    fn new(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    /// Whether the stdout lines declare the tool `name`, by membership:
    /// the built-in list is not pinned, so #578's new tool breaks no test.
    fn declares(&self, name: &str) -> bool {
        self.lines
            .iter()
            .filter(|line| line["kind"] == "preamble_built")
            .flat_map(|line| line["payload"]["tools"].as_array().unwrap().to_vec())
            .any(|tool| tool["name"] == name)
    }

    /// Whether the stdout lines hold a notice with `code` naming `name`.
    fn notice(&self, code: &str, name: &str) -> bool {
        self.lines.iter().any(|line| {
            line["kind"] == "notice"
                && line["payload"]["code"] == code
                && line["payload"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(name)
        })
    }
}

/// The echo tool: reads-only and offline, so its calls take the permission
/// fast path.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"zebra": {"type": "string"}, "apple": {"type": "string"}},
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

#[test]
fn an_approved_server_runs_from_its_pinned_copy() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({}), &[]);
    setup.approve(&workspace);
    let hash = setup.approval_hash();

    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.notice("repository_code_skipped", "db"), "no notice");
    assert!(
        run.declares("mcp__db__echo"),
        "the first request declares the server's tools"
    );
    // The session runs the copy, never the repository's files, in the
    // session's workspace.
    assert_eq!(
        fs::read_to_string(dir.join("argv0.txt")).unwrap().trim(),
        setup
            .home()
            .join("pinned")
            .join(&hash)
            .join("srv/run.sh")
            .display()
            .to_string()
            .as_str()
    );
    assert_eq!(
        fs::read_to_string(dir.join("cwd.txt")).unwrap().trim(),
        fs::canonicalize(&workspace)
            .unwrap()
            .display()
            .to_string()
            .as_str()
    );
    assert_eq!(support::kinds(&run.lines), ASK_TEXT_TURN);
}

#[test]
fn a_changed_pinned_file_needs_a_new_approval() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({}), &[]);
    setup.approve(&workspace);
    let hash = setup.approval_hash();

    fs::write(workspace.join("srv/run.sh"), "# changed\n").unwrap();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.notice("repository_code_skipped", "`db`"),
        "skipped with a notice"
    );
    assert!(
        !run.declares("mcp__db__echo"),
        "the changed server's tools are not declared"
    );
    // The old copy is still in `pinned/`.
    assert!(
        setup
            .home()
            .join("pinned")
            .join(&hash)
            .join("srv/run.sh")
            .is_file()
    );
    assert_eq!(support::kinds(&run.lines), ASK_SKIPPED_TURN);
}

#[test]
fn a_changed_declaration_needs_a_new_approval() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({}), &[]);
    setup.approve(&workspace);

    let file = workspace.join(".fiber/config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    config["mcp"]["servers"]["db"]["env"] = json!({"MODE": "fast"});
    fs::write(&file, config.to_string()).unwrap();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        run.notice("repository_code_skipped", "`db`"),
        "skipped with a notice"
    );
    assert!(
        !run.declares("mcp__db__echo"),
        "the changed server's tools are not declared"
    );
    assert_eq!(support::kinds(&run.lines), ASK_SKIPPED_TURN);
}

#[test]
fn an_approval_in_one_repository_covers_another_on_this_machine() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let first = setup.workspace();
    setup.declare(&first, &dir, &json!({}), &[]);
    setup.approve(&first);

    // The same declaration and file bytes elsewhere: no new approval.
    let second = setup.root.path().join("w2");
    fs::create_dir_all(&second).unwrap();
    setup.declare(&second, &dir, &json!({}), &[]);
    let run = setup.run_in(&second, &["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!run.notice("repository_code_skipped", "db"), "no notice");
    assert!(run.declares("mcp__db__echo"), "the tools are declared");
    assert_eq!(support::kinds(&run.lines), ASK_TEXT_TURN);
}

#[test]
fn a_pruned_copy_is_rebuilt() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({}), &[]);
    setup.approve(&workspace);
    let hash = setup.approval_hash();

    fs::remove_dir_all(setup.home().join("pinned").join(&hash)).unwrap();
    fs::remove_file(setup.home().join("pinned").join(format!("{hash}.ready"))).unwrap();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(run.declares("mcp__db__echo"), "the rebuilt copy runs");
    assert_eq!(
        fs::read_to_string(dir.join("argv0.txt")).unwrap().trim(),
        setup
            .home()
            .join("pinned")
            .join(&hash)
            .join("srv/run.sh")
            .display()
            .to_string()
            .as_str()
    );
    assert_eq!(support::kinds(&run.lines), ASK_TEXT_TURN);
}

#[test]
fn a_required_server_that_fails_to_start_exits_before_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start(Vec::<fakes::Response>::new()).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    fs::write(dir.join("fail-start"), "").unwrap();
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({"required": true}), &[]);
    setup.approve(&workspace);

    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(server.requests().is_empty(), "no model request");
    let exited = run
        .lines
        .iter()
        .find(|line| line["kind"] == "fiber_exited")
        .unwrap();
    assert_eq!(
        exited["payload"]["error"]["code"],
        "mcp_required_server_failed"
    );
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "preamble_built"),
        "no session line"
    );
    // A `required` failure stops the session before any line: the only
    // stdout line is the exit.
    assert_eq!(support::kinds(&run.lines), ["fiber_exited"]);
}

#[test]
fn a_reapproved_file_change_keeps_the_cached_tool_list() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    setup.declare(&workspace, &dir, &json!({}), &[]);
    setup.approve(&workspace);

    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert!(
        dir.join("pid.txt").exists(),
        "the first run starts the server"
    );
    assert_eq!(support::kinds(&first.lines), ASK_TEXT_TURN);
    // The declaration is unchanged, so the tool-list cache stays keyed to
    // it across the re-approval of the changed file.
    fs::write(workspace.join("srv/run.sh"), "# changed\n").unwrap();
    setup.approve(&workspace);
    fs::remove_file(dir.join("pid.txt")).unwrap();
    let second = setup.run(&["ask", "hi"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert!(
        second.declares("mcp__db__echo"),
        "the cached list still declares"
    );
    assert!(
        !dir.join("pid.txt").exists(),
        "the cached server never spawns"
    );
    assert_eq!(support::kinds(&second.lines), ASK_TEXT_TURN);
}

#[test]
fn a_repositorys_hints_do_not_change_the_declared_effects() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call("call_echo", "mcp__db__echo", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let workspace = setup.workspace();
    setup.declare(
        &workspace,
        &dir,
        &json!({"tools": {"echo": {"hints": {"destructiveHint": true}}}}),
        &[],
    );
    setup.approve(&workspace);

    let run = setup.run(&["ask", "echo hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let started = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started")
        .unwrap();
    // The server's own hints stand: a repository that marked the tool
    // destructive would not run it past the person.
    assert_eq!(started["payload"]["effects"], json!(["reads"]));
    assert_eq!(started["payload"]["reversible"], true);
    assert_eq!(support::kinds(&run.lines), ASK_TOOL_TURN);
}

/// The session process for `id`, with its stdout lines as they come.
fn spawn_session(setup: &Setup, id: &str, extra: &[&str]) -> (HubProc, mpsc::Receiver<Value>) {
    let workspace = setup.workspace();
    let mut args = vec!["session", "--id", id, "--workspace"];
    args.push(workspace.to_str().unwrap());
    args.extend(extra);
    let home = setup.home();
    let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
    command
        .args(&args)
        .current_dir(&workspace)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", home)
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let parsed = serde_json::from_str(&line).unwrap_or(Value::String(line));
            if tx.send(parsed).is_err() {
                break;
            }
        }
    });
    (
        HubProc {
            child,
            watchdog,
            group,
            deadline: setup.deadline,
        },
        lines,
    )
}

/// Waits for the session's `extensions_loaded` on stdout: every startup
/// line is written and the socket is listening.
fn started(deadline: Deadline, lines: &mpsc::Receiver<Value>) {
    while deadline.left() > std::time::Duration::ZERO {
        if let Ok(line) = deadline.recv(lines)
            && line["kind"] == "extensions_loaded"
        {
            return;
        }
    }
    panic!("no extensions_loaded within the deadline");
}

/// A `full` client on the session `id`.
fn attach(setup: &Setup, id: &str) -> fakes::Client {
    let socket = setup.home().join("run").join(id);
    let client = fakes::Client::connect(&socket).unwrap();
    client
        .send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#)
        .unwrap();
    client
        .recv_until(setup.deadline, |line| {
            line["kind"] == "clients" && line["payload"]["count"] == 1
        })
        .expect("the session counts the client within the deadline");
    client
}

fn prompt(client: &fakes::Client) {
    client
        .send(r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#)
        .unwrap();
}

#[test]
fn a_file_created_after_gathering_is_refused_on_a_lazy_start() {
    let setup = Setup::new();
    // The first ask's reply, then the hub turn's call and follow-up.
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call("call_echo", "mcp__db__echo", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let workspace = setup.workspace();
    // The declaration names `late.sh`, absent: nothing to freeze.
    setup.declare(&workspace, &dir, &json!({}), &["late.sh".to_owned()]);
    setup.approve(&workspace);

    // The first run populates the tool-list cache, spawning while the
    // guard still passes.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert!(first.declares("mcp__db__echo"));
    assert!(
        dir.join("pid.txt").exists(),
        "the first run starts the server"
    );
    assert_eq!(support::kinds(&first.lines), ASK_TEXT_TURN);
    fs::remove_file(dir.join("pid.txt")).unwrap();

    // The hub session declares the server from the cache and spawns nothing.
    let id = doors::mint("s_");
    let (session, stdout) = spawn_session(&setup, &id, &[]);
    started(setup.deadline, &stdout);
    assert!(
        !dir.join("pid.txt").exists(),
        "a cached server starts on its first call"
    );
    let client = attach(&setup, &id);

    // Created after gathering: the approval does not cover it. The prompt
    // first raises a fresh offer for the new content, which the client
    // skips; the load then drops it with no line, and the guard refuses
    // the lazy spawn.
    fs::write(workspace.join("late.sh"), "#!/bin/bash\ntouch \"marker\"\n").unwrap();
    prompt(&client);
    let mut lines = Vec::new();
    while setup.deadline.left() > std::time::Duration::ZERO {
        if let Ok(line) = setup.deadline.recv(&stdout) {
            if line["kind"] == "repository_code_offered" {
                let request = line["payload"]["request_id"].as_str().unwrap().to_owned();
                client
                    .send(&format!(
                        "{{\"id\":\"c_skip\",\"command\":\"reply\",\"args\":{{\"request_id\":\"{request}\",\"decisions\":[\"skip\"]}}}}"
                    ))
                    .unwrap();
            }
            let done = line["kind"] == "turn_completed";
            lines.push(line);
            if done {
                break;
            }
        }
    }
    assert!(
        lines.iter().any(|line| line["kind"] == "turn_completed"),
        "the turn completes"
    );
    // The complete ordered list from the attach: the fresh offer is
    // skipped and resolved, the load drops it with no line, and the
    // guard's refusal is recorded as `mcp_server_failed`, so a
    // duplicated, missing or reordered event fails (`docs/testing.md`,
    // "Event streams").
    assert_eq!(
        support::kinds_without_attention(&lines),
        [
            "clients",
            "repository_code_offered",
            "repository_code_resolved",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "mcp_server_failed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count(),
        1,
        "one clients line for the attach"
    );
    let failed = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_completed"
                && line["payload"]["error"]["code"] == "mcp_server_unavailable"
        })
        .unwrap_or_else(|| panic!("a refused call completes the tool call"));
    assert!(
        failed["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("late.sh"),
        "{failed}"
    );
    assert!(!workspace.join("marker").exists(), "late.sh never runs");
    assert!(
        !dir.join("pid.txt").exists(),
        "a refused spawn starts nothing"
    );

    client
        .send(r#"{"id":"c_close","command":"close"}"#)
        .unwrap();
    session.kill("TERM");
    session.wait();
}
