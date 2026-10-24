//! Binary-level tests of repository extensions loading into a session
//! (`docs/extensions.md`, "Code a repository ships"): an approved package
//! loads from its pinned copy on `fiber ask` and on resume; its text
//! reaches the model request; approvals stay in their project; the offer
//! in a `full` session installs the package late; a missing required
//! package rejects the run; dependencies arrive through git.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    KillGroup, Setup, Socket, first_line, hello, run_to_exit, session_dir, spawn_watched, until,
    until_close, write_json,
};

/// The package manifest `name` at `version`, with a `prompt.md` text when
/// `prompt` holds it.
fn manifest(name: &str, version: &str, prompt: Option<&str>) -> Value {
    let mut manifest = json!({"name": name, "version": version, "fiber": "0.1.0", "api": 1});
    if prompt.is_some() {
        manifest["prompt"] = json!("prompt.md");
    }
    manifest
}

/// Writes these repository packages into the directory: each `(dir,
/// manifest, init.lua text, required)`. The directory becomes a git
/// repository, as `repository_offer.rs` declares one.
fn declare(dir: &Path, pkgs: &[(&str, Value, &str, bool)]) {
    fakes::git::git(dir, &["init", "--quiet"]);
    let mut declared = Vec::new();
    for (pkg, manifest, init, required) in pkgs {
        let root = dir.join(pkg);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("extension.json"), manifest.to_string()).unwrap();
        fs::write(root.join("init.lua"), init).unwrap();
        if let Some(prompt) = manifest.get("prompt") {
            fs::write(root.join(prompt.as_str().unwrap()), "PROMPTMARKER-seven\n").unwrap();
        }
        declared.push(json!({"path": pkg, "required": required}));
    }
    write_json(
        &dir.join(".fiber/config.json"),
        &json!({"repository_extensions": declared}),
    );
}

/// Writes the global configuration, as `reload_tests` rewrites its rig's.
fn write_global(setup: &Setup, value: &Value) {
    fs::write(setup.home().join("config.json"), value.to_string()).unwrap();
}

/// `fiber approve --yes` in `dir`: the approvals print on stderr.
fn approve(setup: &Setup, dir: &Path, env: &[(&str, &str)]) -> String {
    let mut command = setup.fiber(&["approve", "--yes"]);
    command.current_dir(dir);
    command.envs(env.iter().copied());
    let output = run_to_exit(setup.deadline, "fiber approve", command);
    assert!(output.status.success(), "fiber approve: {output:?}");
    String::from_utf8(output.stderr).unwrap()
}

/// `fiber ask` with `args` in `dir`: stdout as text and parsed lines.
fn ask_in(setup: &Setup, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> support::StdoutRun {
    let mut command = setup.fiber(args);
    command.current_dir(dir);
    command.envs(env.iter().copied());
    support::StdoutRun::from(run_to_exit(setup.deadline, "fiber ask", command))
}

/// `fiber ask hi` in `dir`.
fn ask(setup: &Setup, dir: &Path) -> support::StdoutRun {
    ask_in(setup, dir, &["ask", "hi"], &[])
}

/// Installs the personal extension `name` at `version` with this
/// `init.lua`, as `reload.rs` installs its demo. nextest runs each test in
/// its own process, so no other test's child can hold this lock
/// (`docs/testing/running.md`).
fn personal(setup: &Setup, name: &str, version: &str, init: &str) {
    let source = setup
        .root
        .path()
        .join(format!("src-{}", name.replace('/', "-")));
    write_json(
        &source.join("extension.json"),
        &json!({"name": name, "version": version, "fiber": "0.1.0", "api": 1}),
    );
    fs::write(source.join("init.lua"), init).unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .map(|_| ())
    .unwrap();
}

fn of<'a>(lines: &'a [Value], kind: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| line["kind"] == kind).collect()
}

/// The kinds of `fiber ask`'s stdout `lines`, in order, without
/// `session_status`: an observer thread writes it, so where it falls among
/// the loop's own lines is not what these tests pin.
fn stdout_kinds(lines: &[Value]) -> Vec<&str> {
    lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .map(|line| line["kind"].as_str().unwrap())
        .collect()
}

/// The kinds of every durable line in the session `id`'s log, in order.
fn durable_kinds(setup: &Setup, id: &str) -> Vec<String> {
    let text = fs::read_to_string(session_dir(setup, id).join("events.jsonl")).unwrap();
    text.lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
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

/// The request bodies the fake model server saw, as text.
fn bodies(server: &ProviderServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

/// The one directory under `home/pinned/`: the test approves one package.
fn pinned_copy(setup: &Setup) -> PathBuf {
    let mut copies: Vec<_> = fs::read_dir(setup.home().join("pinned"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    copies.pop().unwrap()
}

#[test]
fn approved_package_loads_from_its_copy_with_its_prompt_text() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", Some("prompt.md")),
            "fiber.command('lint',{timeout=5000,run=function() return 'lint' end})",
            false,
        )],
    );
    approve(&setup, &setup.workspace(), &[]);
    let run = ask(&setup, &setup.workspace());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let loaded = of(&run.lines, "extensions_loaded");
    assert_eq!(loaded.len(), 1, "{:?}", run.lines);
    assert!(
        loaded_set(loaded[0]).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        loaded[0]
    );
    assert_eq!(server.requests().len(), 1);
    assert!(
        bodies(&server)[0].contains("PROMPTMARKER-seven"),
        "the request carries the package's prompt text"
    );
    assert_eq!(
        stdout_kinds(&run.lines),
        [
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
        ]
    );
    // The load reads the pinned copy, not the repository: editing the
    // copy's prompt file changes the next run's request, while the
    // repository's file is left alone.
    fs::write(pinned_copy(&setup).join("prompt.md"), "COPYMARKER-nine\n").unwrap();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let rerun = ask(&setup, &setup.workspace());
    assert_eq!(rerun.code, Some(0), "stderr: {}", rerun.stderr);
    assert_eq!(server.requests().len(), 1);
    let body = &bodies(&server)[0];
    assert!(
        body.contains("COPYMARKER-nine"),
        "the rerun carries the copy's text"
    );
    assert!(!body.contains("PROMPTMARKER-seven"), "{body}");
}

#[test]
fn each_worktree_loads_its_own_approved_version() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let first = setup.workspace();
    declare(
        &first,
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", None),
            "-- entry\n",
            false,
        )],
    );
    fakes::git::git(&first, &["add", "."]);
    fakes::git::git(&first, &["commit", "--quiet", "-m", "v1"]);
    let second = setup.root.path().join("w2");
    fakes::git::git(
        &first,
        &["worktree", "add", "-b", "other", second.to_str().unwrap()],
    );
    fs::write(
        second.join("pkg/extension.json"),
        manifest("fiber.test/pkg", "v2.0.0", None).to_string(),
    )
    .unwrap();
    approve(&setup, &first, &[]);
    approve(&setup, &second, &[]);
    let one = ask(&setup, &first);
    assert_eq!(one.code, Some(0), "stderr: {}", one.stderr);
    assert!(
        loaded_set(of(&one.lines, "extensions_loaded")[0])
            .contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        one.lines
    );
    let two = ask(&setup, &second);
    assert_eq!(two.code, Some(0), "stderr: {}", two.stderr);
    assert!(
        loaded_set(of(&two.lines, "extensions_loaded")[0])
            .contains(&("fiber.test/pkg".to_owned(), "v2.0.0".to_owned())),
        "{:?}",
        two.lines
    );
    // One project key for both worktrees, holding both approvals.
    let mut keys: Vec<_> = fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    let approvals: Vec<_> = fs::read_dir(keys.pop().unwrap().join("approvals"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(approvals.len(), 2, "{approvals:?}");
}

#[test]
fn approval_in_one_project_does_not_load_in_another() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let pkgs = [(
        "pkg",
        manifest("fiber.test/pkg", "v1.0.0", None),
        "-- entry\n",
        false,
    )];
    declare(&setup.workspace(), &pkgs);
    approve(&setup, &setup.workspace(), &[]);
    let other = setup.root.path().join("sibling");
    fs::create_dir_all(&other).unwrap();
    declare(&other, &pkgs);
    let one = ask(&setup, &setup.workspace());
    assert_eq!(one.code, Some(0), "stderr: {}", one.stderr);
    assert!(
        loaded_set(of(&one.lines, "extensions_loaded")[0])
            .contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        one.lines
    );
    // The sibling declares the same package but never approved it: the
    // first project's approval does not cross over.
    let two = ask(&setup, &other);
    assert_eq!(two.code, Some(0), "stderr: {}", two.stderr);
    assert!(
        !loaded_set(of(&two.lines, "extensions_loaded")[0])
            .iter()
            .any(|(name, _)| name == "fiber.test/pkg"),
        "{:?}",
        two.lines
    );
}

#[test]
fn the_workspace_load_shadows_the_personal_install() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello(), hello()]).unwrap();
    setup.provider(&server);
    personal(&setup, "fiber.test/pkg", "v0.9.0", "-- entry\n");
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", None),
            "-- entry\n",
            false,
        )],
    );
    approve(&setup, &setup.workspace(), &[]);
    let run = ask(&setup, &setup.workspace());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let loaded = of(&run.lines, "extensions_loaded");
    assert!(
        loaded_set(loaded[0]).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        loaded[0]
    );
    let notices = of(&run.lines, "notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0]["payload"]["code"], "extension_shadowed",
        "{notices:?}"
    );
    let message = notices[0]["payload"]["message"].as_str().unwrap();
    assert!(message.contains("v1.0.0"), "{message}");
    assert!(message.contains("v0.9.0"), "{message}");
    assert_eq!(
        stdout_kinds(&run.lines),
        [
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
        ]
    );
    // The `enabled` gate is personal: disabling the name leaves the
    // repository copy loading, with the same notice.
    write_global(
        &setup,
        &json!({"model":"fake/m", "extensions":{"fiber.test/pkg":{"enabled":false}}}),
    );
    let disabled = ask(&setup, &setup.workspace());
    assert_eq!(disabled.code, Some(0), "stderr: {}", disabled.stderr);
    assert!(
        loaded_set(of(&disabled.lines, "extensions_loaded")[0])
            .contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        disabled.lines
    );
    assert_eq!(
        of(&disabled.lines, "notice").len(),
        1,
        "{:?}",
        disabled.lines
    );
    // The sibling repository declares nothing: the personal install
    // loads, with no notice.
    write_global(&setup, &json!({"model":"fake/m"}));
    let other = setup.root.path().join("sibling");
    fs::create_dir_all(&other).unwrap();
    fakes::git::git(&other, &["init", "--quiet"]);
    let personal = ask(&setup, &other);
    assert_eq!(personal.code, Some(0), "stderr: {}", personal.stderr);
    assert!(
        loaded_set(of(&personal.lines, "extensions_loaded")[0])
            .contains(&("fiber.test/pkg".to_owned(), "v0.9.0".to_owned())),
        "{:?}",
        personal.lines
    );
    assert!(
        of(&personal.lines, "notice").is_empty(),
        "{:?}",
        personal.lines
    );
}

#[test]
fn remove_records_never_and_the_next_ask_loads_nothing() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", None),
            "-- entry\n",
            false,
        )],
    );
    let approved = approve(&setup, &setup.workspace(), &[]);
    assert!(
        approved.contains("approved extension fiber.test/pkg "),
        "{approved}"
    );
    let mut command = setup.fiber(&["extension", "remove", "fiber.test/pkg"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber extension remove", command);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        "fiber: removed fiber.test/pkg from this project; it is not offered again until its content changes",
        "{stderr}"
    );
    let run = ask(&setup, &setup.workspace());
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        !loaded_set(of(&run.lines, "extensions_loaded")[0])
            .iter()
            .any(|(name, _)| name == "fiber.test/pkg"),
        "{:?}",
        run.lines
    );
    let approvals: Vec<_> = fs::read_dir(setup.home().join("projects"))
        .unwrap()
        .flat_map(|entry| fs::read_dir(entry.unwrap().path().join("approvals")).unwrap())
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(approvals.len(), 1, "{approvals:?}");
    let record = fs::read_to_string(&approvals[0]).unwrap();
    assert!(record.contains("\"decision\":\"never\""), "{record}");
}

#[test]
fn list_prints_the_repository_line_with_the_project() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", None),
            "-- entry\n",
            false,
        )],
    );
    approve(&setup, &setup.workspace(), &[]);
    let mut command = setup.fiber(&["extension", "list"]);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber extension list", command);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .find(|line| line.starts_with("fiber.test/pkg "))
        .unwrap_or_else(|| panic!("no repository line in {stdout}"));
    let words: Vec<_> = line.split(' ').collect();
    assert_eq!(words.len(), 6, "{line}");
    assert_eq!(&words[1..3], &["v1.0.0", "repository"]);
    assert!(
        words[3].contains("fiber-repository-load") || words[3].contains("fh-"),
        "the project identity: {line}"
    );
    assert!(
        words[3].ends_with(".git"),
        "the project is git's shared directory, not the workspace: {line}"
    );
    assert_eq!(words[4], "pkg");
    assert_eq!(words[5].len(), 64, "{line}");
    assert!(words[5].chars().all(|c| c.is_ascii_hexdigit()), "{line}");
}

#[test]
fn resume_in_another_worktree_keeps_the_folded_version() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let first = setup.workspace();
    // `other` stays unapproved everywhere: the first run skips it, and
    // the fold carries the skip into the resume. The approval below
    // names only `pkg`, so `other` is declared after it.
    declare(
        &first,
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", None),
            "-- entry\n",
            false,
        )],
    );
    let approved = approve(&setup, &first, &[]);
    assert!(
        approved.contains("approved extension fiber.test/pkg "),
        "{approved}"
    );
    declare(
        &first,
        &[
            (
                "pkg",
                manifest("fiber.test/pkg", "v1.0.0", None),
                "-- entry\n",
                false,
            ),
            (
                "other",
                manifest("fiber.test/other", "v1.0.0", None),
                "-- entry\n",
                false,
            ),
        ],
    );
    // The first run is a `full` session: it approves `pkg` and skips
    // `other` through the offer, so the fold records the skip.
    let (id, setup) = {
        let mut session = Session::start(setup);
        session
            .client
            .send(r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#);
        let offered = session.wait("the offer", "repository_code_offered");
        let request = offered["payload"]["request_id"].as_str().unwrap();
        let items = offered["payload"]["items"].as_array().unwrap();
        // `pkg` is already approved, so the offer raises only `other`:
        // skip it, and the fold records the skip.
        assert_eq!(items.len(), 1, "{offered}");
        assert_eq!(items[0]["name"], "fiber.test/other", "{offered}");
        session.client.send(
            &json!({"id":"c_reply","command":"reply","args":{"request_id":request,"decisions":["skip"]}})
                .to_string(),
        );
        session.wait("the resolution", "repository_code_resolved");
        session.wait("the turn", "turn_completed");
        let (setup, stream, _stdout, stderr, success) = session.finish();
        assert!(success, "{stderr}");
        let kinds: Vec<&str> = stream
            .iter()
            .filter(|line| line["kind"] != "clients")
            .map(|line| line["kind"].as_str().unwrap())
            .collect();
        let at = |kind: &str| {
            kinds
                .iter()
                .position(|seen| seen == &kind)
                .unwrap_or_else(|| panic!("no {kind} in {kinds:?}"))
        };
        assert!(at("repository_code_offered") < at("repository_code_resolved"));
        let loaded: Vec<_> = stream
            .iter()
            .filter(|line| line["kind"] == "extensions_loaded")
            .collect();
        // The offer approves nothing new, so there is no late load:
        // the one line is the startup load of the approved `pkg`.
        assert_eq!(loaded.len(), 1, "{kinds:?}");
        let set = loaded_set(loaded[0]);
        assert!(
            set.contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
            "{set:?}"
        );
        assert!(
            !set.iter().any(|(name, _)| name == "fiber.test/other"),
            "the skipped package stays out: {set:?}"
        );
        let id = stream
            .iter()
            .find(|line| line["kind"] == "session_started")
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let durable = durable_kinds(&setup, &id);
        assert!(
            durable.contains(&"repository_code_resolved".to_owned()),
            "{durable:?}"
        );
        (id, setup)
    };
    fakes::git::git(&first, &["add", "."]);
    fakes::git::git(&first, &["commit", "--quiet", "-m", "v1"]);
    let second = setup.root.path().join("w2");
    fakes::git::git(
        &first,
        &["worktree", "add", "-b", "other", second.to_str().unwrap()],
    );
    fs::write(
        second.join("pkg/extension.json"),
        manifest("fiber.test/pkg", "v2.0.0", None).to_string(),
    )
    .unwrap();
    // The second worktree approves `pkg` at v2 and `other`: both
    // approvals sit in the shared project. The resume must still load
    // v1 and keep `other` out, on the fold alone.
    // (`declare` rewrites the worktree's repository file.)
    declare(
        &second,
        &[
            (
                "pkg",
                manifest("fiber.test/pkg", "v2.0.0", None),
                "-- entry\n",
                false,
            ),
            (
                "other",
                manifest("fiber.test/other", "v1.0.0", None),
                "-- entry\n",
                false,
            ),
        ],
    );
    let approved = approve(&setup, &second, &[]);
    assert!(
        approved.contains("approved extension fiber.test/pkg "),
        "{approved}"
    );
    assert!(
        approved.contains("approved extension fiber.test/other "),
        "{approved}"
    );
    // The resume runs in the other worktree, whose package is v2: the
    // folded session keeps v1, and the folded skip keeps `other` out.
    let two = ask_in(&setup, &second, &["ask", "--resume", &id[..8], "two"], &[]);
    assert_eq!(two.code, Some(0), "stderr: {}", two.stderr);
    assert_eq!(two.session_id(), id);
    let resumed = loaded_set(of(&two.lines, "extensions_loaded")[0]);
    assert!(
        resumed.contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{resumed:?}"
    );
    assert!(
        !resumed.iter().any(|(name, _)| name == "fiber.test/other"),
        "the folded skip holds on resume: {resumed:?}"
    );
}

/// A session with a `full` client, driving one offer and the turn after.
struct Session {
    child: std::process::Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    client: Socket,
    setup: Setup,
    stream: Vec<Value>,
    stdout: mpsc::Receiver<String>,
    reader: thread::JoinHandle<()>,
}

impl Session {
    fn start(setup: Setup) -> Self {
        let id = doors::mint("s_");
        let workspace = setup.workspace();
        let (mut child, watchdog) = spawn_watched(&mut setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            workspace.to_str().unwrap(),
        ]));
        let guard = KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            stderr_pipe
                .take(u64::MAX)
                .read_to_string(&mut text)
                .unwrap();
            tx.send(text).unwrap();
        });
        first_line(setup.deadline, &lines);
        let client = Socket::connect(setup.deadline, &setup.session_socket(&id));
        let stdout = lines;
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // `clients` is written when the `full` connection attaches, with no
        // order against the startup events: the wait returns once both
        // `clients` and `extensions_loaded` have arrived.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        let stream = until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        Self {
            child,
            guard,
            watchdog,
            stderr,
            client,
            setup,
            stream,
            stdout,
            reader,
        }
    }

    fn wait(&mut self, what: &str, kind: &str) -> Value {
        let lines = until(&self.client, what, |line| line["kind"] == kind);
        let found = lines.last().unwrap().clone();
        self.stream.extend(lines);
        found
    }

    fn finish(self) -> (Setup, Vec<Value>, Vec<Value>, String, bool) {
        let Self {
            mut child,
            guard,
            watchdog,
            stderr,
            client,
            setup,
            stdout,
            reader,
            ..
        } = self;
        client.send(r#"{"id":"close","command":"close"}"#);
        let mut stream = self.stream;
        stream.extend(until_close(&client));
        let group = child.id();
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(child.wait()).unwrap();
        });
        let status = setup.deadline.recv(&done).unwrap().unwrap();
        let err = setup.deadline.recv(&stderr).unwrap();
        assert!(!fakes::group_alive(group));
        std::mem::forget(guard);
        watchdog.stand_down(setup.deadline.cleanup_phase());
        // The child's stdout is closed, so the reader ended: every line
        // is here, including the ephemeral notices the durable log and
        // a late-subscribed client never see.
        reader.join().unwrap();
        let stdout: Vec<Value> = stdout
            .try_iter()
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect();
        (setup, stream, stdout, err, status.success())
    }
}

#[test]
fn an_approved_offer_loads_before_the_first_preamble() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", Some("prompt.md")),
            "fiber.command('lint',{timeout=5000,run=function() return 'lint' end})",
            false,
        )],
    );
    let mut session = Session::start(setup);
    session.client.send(
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let offered = session.wait("the offer", "repository_code_offered");
    let request = offered["payload"]["request_id"].as_str().unwrap();
    session.client.send(
        &json!({"id":"c_reply","command":"reply","args":{"request_id":request,"decisions":["approve"]}})
            .to_string(),
    );
    session.wait("the resolution", "repository_code_resolved");
    session.wait("the turn", "turn_completed");
    let (setup, stream, _stdout, stderr, success) = session.finish();
    assert!(success, "{stderr}");
    let id = stream
        .iter()
        .find(|line| line["kind"] == "session_started")
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(server.requests().len(), 1);
    assert!(
        bodies(&server)[0].contains("PROMPTMARKER-seven"),
        "the first request carries the package's prompt text"
    );
    let kinds: Vec<&str> = stream
        .iter()
        .filter(|line| line["kind"] != "clients")
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let at = |kind: &str| {
        kinds
            .iter()
            .position(|seen| seen == &kind)
            .unwrap_or_else(|| panic!("no {kind} in {kinds:?}"))
    };
    assert!(at("repository_code_offered") < at("repository_code_resolved"));
    let loaded_at: Vec<_> = kinds
        .iter()
        .enumerate()
        .filter(|(_, seen)| **seen == "extensions_loaded")
        .map(|(at, _)| at)
        .collect();
    assert_eq!(loaded_at.len(), 2, "{kinds:?}");
    assert!(at("repository_code_resolved") < loaded_at[1]);
    assert!(loaded_at[1] < at("preamble_built"));
    let loaded: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "extensions_loaded")
        .collect();
    assert_eq!(loaded.len(), 2, "{kinds:?}");
    assert!(
        loaded_set(loaded[1]).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        loaded[1]
    );
    assert!(!kinds.contains(&"reloaded"), "{kinds:?}");
    let durable = durable_kinds(&setup, &id);
    let durable_at = |kind: &str| {
        durable
            .iter()
            .enumerate()
            .filter(|(_, seen)| seen == &kind)
            .map(|(at, _)| at)
            .collect::<Vec<_>>()
    };
    let resolved = durable_at("repository_code_resolved");
    let loaded = durable_at("extensions_loaded");
    let built = durable_at("preamble_built");
    assert_eq!(resolved.len(), 1, "{durable:?}");
    assert_eq!(loaded.len(), 2, "{durable:?}");
    assert_eq!(built.len(), 1, "{durable:?}");
    assert!(resolved[0] < loaded[1]);
    assert!(loaded[1] < built[0]);
    assert!(!durable.contains(&"reloaded".to_owned()), "{durable:?}");
}

/// A startup notice is written once when the offer loads late: the late
/// preparer reads every notice the startup wrote, including the ones
/// `mcp_servers_started` writes after that read (`session_command.rs`
/// and `resume.rs` pass them through `startup_written`).
#[test]
fn a_startup_notice_is_not_written_twice_by_the_late_load() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    write_global(&setup, &json!({"model": "fake/m", "bogus": 1}));
    declare(
        &setup.workspace(),
        &[(
            "pkg",
            manifest("fiber.test/pkg", "v1.0.0", Some("prompt.md")),
            "fiber.command('lint',{timeout=5000,run=function() return 'lint' end})",
            false,
        )],
    );
    let mut session = Session::start(setup);
    session.client.send(
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    let offered = session.wait("the offer", "repository_code_offered");
    let request = offered["payload"]["request_id"].as_str().unwrap();
    session.client.send(
        &json!({"id":"c_reply","command":"reply","args":{"request_id":request,"decisions":["approve"]}})
            .to_string(),
    );
    session.wait("the resolution", "repository_code_resolved");
    session.wait("the turn", "turn_completed");
    let (_setup, stream, stdout, stderr, success) = session.finish();
    assert!(success, "{stderr}");
    // The offer approved `pkg`, so the late preparer ran: the second
    // load carries it. Without the `startup_written` read the unknown
    // key below would surface twice, once per load.
    let loaded: Vec<_> = stream
        .iter()
        .filter(|line| line["kind"] == "extensions_loaded")
        .collect();
    assert_eq!(loaded.len(), 2);
    assert!(
        loaded_set(loaded[1]).contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{:?}",
        loaded[1]
    );
    // Notices are ephemeral: they reach the session's stdout, never the
    // durable log, and a client subscribing after the startup write
    // misses them. The startup wrote the unknown key once; the late
    // load must not write it again.
    let ignored: Vec<_> = stdout
        .iter()
        .filter(|line| line["kind"] == "notice" && line["payload"]["code"] == "config_key_ignored")
        .collect();
    assert_eq!(ignored.len(), 1, "{ignored:?}");
}

#[test]
fn a_required_package_that_fails_to_start_rejects_the_run() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let pkgs = [(
        "pkg",
        manifest("fiber.test/pkg", "v1.0.0", None),
        "error(\"boom\")\n",
        true,
    )];
    declare(&setup.workspace(), &pkgs);
    approve(&setup, &setup.workspace(), &[]);
    let run = ask(&setup, &setup.workspace());
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(server.requests().is_empty(), "the run makes no request");
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "session_started"),
        "no session_started: {:?}",
        run.lines
    );
    let exited = run.lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(
        exited["payload"]["error"]["code"], "extension_required_failed",
        "{exited}"
    );
    assert!(
        run.stderr.contains("The repository requires the extension"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("fiber.test/pkg"), "{}", run.stderr);
    // The same package, not required, loads nothing and the run goes on
    // with its `extension_failed` notice.
    let pkgs = [(
        "pkg",
        manifest("fiber.test/pkg", "v1.0.0", None),
        "error(\"boom\")\n",
        false,
    )];
    declare(&setup.workspace(), &pkgs);
    let rerun = ask(&setup, &setup.workspace());
    assert_eq!(rerun.code, Some(0), "stderr: {}", rerun.stderr);
    assert!(
        !loaded_set(of(&rerun.lines, "extensions_loaded")[0])
            .iter()
            .any(|(name, _)| name == "fiber.test/pkg"),
        "{:?}",
        rerun.lines
    );
    let notices = of(&rerun.lines, "notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0]["payload"]["code"], "extension_failed",
        "{notices:?}"
    );
    assert_eq!(server.requests().len(), 1);
}

/// A local git repository standing in for `https://github.com/`: this is
/// git's own `url.<base>.insteadOf`, so the shipped binary needs no test
/// switch, as `ask.rs` reaches its dependencies.
struct Github {
    base: PathBuf,
    key: String,
}

impl Github {
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        fs::create_dir_all(&base).unwrap();
        let key = format!("url.file://{}/.insteadOf", base.display());
        Self { base, key }
    }

    fn env(&self) -> [(&str, &str); 3] {
        [
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", self.key.as_str()),
            ("GIT_CONFIG_VALUE_0", "https://github.com/"),
        ]
    }

    /// Commits the extension `name` at the repository's root and tags it.
    fn release(&self, repo: &str, name: &str, tag: &str) {
        let dir = self.base.join(repo);
        fs::create_dir_all(&dir).unwrap();
        fakes::git::git(&dir, &["init", "--quiet"]);
        fs::write(
            dir.join("extension.json"),
            // The binary under test is version 0.0.0: a newer `fiber`
            // minimum would refuse the dependency at approval.
            json!({"name": name, "version": tag, "fiber": "0.0.0", "api": 1}).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("init.lua"),
            "fiber.command('h',{timeout=1000,run=function() return 'h' end})\n",
        )
        .unwrap();
        fakes::git::git(&dir, &["add", "."]);
        fakes::git::git(&dir, &["commit", "--quiet", "-m", tag]);
        fakes::git::git(&dir, &["tag", tag]);
    }
}

#[test]
fn approve_installs_a_dependency_reached_through_git() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let github = Github::new(&setup);
    github.release("test/helper", "github.com/test/helper", "v1.4.0");
    let mut pkg = manifest("fiber.test/pkg", "v1.0.0", None);
    pkg["depends"] = json!({"github.com/test/helper": "1.4"});
    declare(&setup.workspace(), &[("pkg", pkg, "-- entry\n", false)]);
    let env: Vec<(&str, &str)> = github.env().into_iter().collect();
    approve(&setup, &setup.workspace(), &env);
    // The dependency sits in the copy, under `.fiber-depends/`.
    let depends = pinned_copy(&setup).join(".fiber-depends/extensions/github.com-test-helper");
    assert!(depends.join("init.lua").is_file(), "{depends:?}");
    let run = ask_in(&setup, &setup.workspace(), &["ask", "hi"], &env);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let set = loaded_set(of(&run.lines, "extensions_loaded")[0]);
    assert!(
        set.contains(&("fiber.test/pkg".to_owned(), "v1.0.0".to_owned())),
        "{set:?}"
    );
    assert!(
        set.contains(&("github.com/test/helper".to_owned(), "v1.4.0".to_owned())),
        "{set:?}"
    );
}
