//! Binary-level pins for the first-party `hooks` extension
//! (`docs/extensions.md`, "Hooks declared in configuration";
//! `docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, a fake provider, and the real `hooks`
//! package installed from the repository. No test here waits on a hook's
//! timeout; those run on the fake clock in `extensions` and in the
//! package's own session cases.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use extension_harness::{
    send as session_send, until_session as session_until, until_session_close,
};
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{KillGroup, Run, Setup, function_call, hello, is_status, spawn_watched, stream};

/// The first-party package under test.
const HOOKS_NAME: &str = "github.com/aakshintala/fiber/extensions/hooks";

/// The hook script every entry runs: it appends its standard input to the
/// capture file named by its first argument, then answers per its second.
const CAPTURE_SH: &str = "#!/bin/sh\ncat >> \"$1\"\ncase \"$2\" in\n  json) printf '{\"content\":\"hook replaced\"}\\n' ;;\n  text) printf 'plain hook context\\n' ;;\n  refuse) printf 'no writes here\\n' >&2; exit 2 ;;\nesac\n";

fn entry(point: &str, command: &str, args: Value, timeout: u64, on_failure: &str) -> Value {
    json!({
        "point": point,
        "command": command,
        "args": args,
        "timeout": timeout,
        "on_failure": on_failure,
    })
}

impl Setup {
    /// Installs the real `hooks` package from the repository.
    fn install_hooks(&self) {
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/hooks"),
            ),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Removes the installed `hooks` package, leaving its settings behind.
    fn remove_hooks(&self) {
        fs::remove_dir_all(config::extensions_dir(&self.home()).join(config::dir_name(HOOKS_NAME)))
            .unwrap();
    }

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's deadline, and asserts that nothing it started is left in the
    /// group. A watchdog beside it kills that group if this process dies
    /// first.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
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
            !fakes::group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Run::from(output)
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

impl From<std::process::Output> for Run {
    fn from(output: std::process::Output) -> Self {
        let lines = String::from_utf8(output.stdout)
            .unwrap()
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
}

/// Writes the executable hook script into the workspace's `hooks/`.
fn write_hook_script(setup: &Setup) {
    let dir = setup.workspace().join("hooks");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("capture.sh");
    fs::write(&path, CAPTURE_SH).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A setup with the fake provider serving `responses`, the `hooks`
/// package, and `hooks` settings holding `entries`. The caller holds the
/// server so it stays up for the run.
fn hooked(responses: Vec<fakes::Response>, entries: &Value) -> (Setup, ProviderServer) {
    let setup = Setup::new();
    let server = ProviderServer::start(responses).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    write(
        &setup.home().join("config").join("hooks.json"),
        &json!({"hooks": entries}).to_string(),
    );
    write_hook_script(&setup);
    (setup, server)
}

/// Reads the capture file's lines: one JSON line per run of the entry.
fn captured(setup: &Setup, name: &str) -> Vec<String> {
    let text = fs::read_to_string(setup.workspace().join(name)).unwrap();
    assert!(text.ends_with('\n'), "the capture holds whole lines");
    text.lines().map(str::to_owned).collect()
}

/// The loaded extensions' names, from `extensions_loaded`.
fn loaded_names(run: &Run) -> Vec<String> {
    run.first("extensions_loaded")["payload"]["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The count rule each kind-multiset assertion cites.
const COUNT_DOC: &str = "docs/testing.md:95: - the event kinds as a set with counts, so a duplicated or missing event fails";

/// The exclusion each kind-multiset assertion cites for the fake
/// stream's text deltas.
const IGNORE_DELTA_DOC: &str = "docs/testing.md, Event streams: A documented variable-progress kind, such as `job_delta` (`docs/events.md`, \"Jobs\"), may be excluded from the multiset only when its count is not under test.; docs/events.md:116: - **Ephemeral** means a later durable line makes it obsolete: text deltas,";

/// The reply order each promised-order assertion cites.
const REPLY_DOC: &str = "docs/events.md:582-586: A reply's items are logged in the order the model produced them: its `text_completed`, `reasoning_completed` and `tool_call_requested` lines follow its `assistant_message_started` in that order, and its `assistant_message_completed` closes it. Replay rebuilds the reply from them in that order.";

/// The reload order the reload test's first promised-order assertion cites.
const RELOAD_PREAMBLE_DOC: &str = "docs/events.md:1154-1155: `reloaded` is written once the new tool set is declared, and `preamble_built` follows it. The next model request misses the prompt cache.";

/// The reload order the reload test's second promised-order assertion cites.
const RELOAD_EXTENSIONS_DOC: &str = "docs/events.md:1163-1168: Durable. The full set of extensions the session loaded. Written at each process start, a resume included, before the first model request; again before the process's first `preamble_built` when its offer approved an extension, which then loads in that process, with no `reloaded` (`docs/extensions.md`, \"Code a repository ships\"); and again after every `reload`, following `reloaded`. It always carries the whole set, never a";

/// Asserts `actual` holds exactly `expected` as a multiset: the sorted
/// kinds must match, so a duplicated or missing event fails. The fake
/// provider scripts its text in two chunks, but chunking is not under
/// test: `assistant_message_delta` is ephemeral, made obsolete by the
/// durable `text_completed` that follows it, so deltas are set aside from
/// the actual kinds while `text_completed` stays counted. `expected` must
/// not name the set-aside kind. Each call cites the count rule and the
/// exclusion.
#[track_caller]
fn assert_kinds_without_deltas(
    actual: &[&str],
    expected: &[&str],
    ignore_delta_doc: &str,
    count_doc: &str,
) {
    assert!(
        !expected.contains(&"assistant_message_delta"),
        "contradictory policy: `assistant_message_delta` is expected yet excluded ({ignore_delta_doc})"
    );
    let mut actual: Vec<&str> = actual
        .iter()
        .copied()
        .filter(|kind| *kind != "assistant_message_delta")
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "event kinds as a set with counts ({count_doc})"
    );
}

/// Asserts each `first` line is followed by a `second` line: both kinds
/// occur, and no `first` sits at or after the last `second`. `doc` is the
/// verbatim doc line promising the order, so the order and its reason sit
/// together.
#[track_caller]
fn assert_each_followed_by(kinds: &[&str], first: &str, second: &str, doc: &str) {
    assert!(kinds.contains(&first), "no `{first}` line ({doc})");
    let last_second = kinds
        .iter()
        .rposition(|kind| *kind == second)
        .unwrap_or_else(|| panic!("no `{second}` line ({doc})"));
    for (at, kind) in kinds.iter().enumerate() {
        if *kind == first {
            assert!(
                at < last_second,
                "`{first}` at {at} has no later `{second}` in {kinds:?} ({doc})"
            );
        }
    }
}

#[test]
fn the_workspace_survives_a_reload_and_entries_still_run_rooted() {
    let setup = Setup::new_with_fm_root();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read_1",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
        stream(&[function_call(
            "call_read_2",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    setup.install_hooks();
    write(
        &setup.home().join("config").join("hooks.json"),
        &json!({"hooks": {
            "start": entry("session_start", "/bin/sh", json!(["hooks/capture.sh", "start.log"]), 30000, "blocking"),
            "mark": entry("after_tool", "/bin/sh", json!(["hooks/capture.sh", "mark.log"]), 30000, "blocking"),
        }})
        .to_string(),
    );
    write_hook_script(&setup);
    fs::write(setup.workspace().join("note.txt"), "note text\n").unwrap();

    let id = doors::mint("s_");
    let mut running = setup.start_session(&id, &["--prompt", "read the note"]);
    let client = running.connect(&setup.socket(&id));
    running.wait_for("extensions_loaded");
    session_send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    let mut lines = session_until(&client, "the first turn to complete", |line| {
        line["kind"] == "turn_completed"
    });
    session_send(&client, r#"{"id":"c_reload","command":"reload"}"#);
    lines.extend(session_until(&client, "the reload to land", |line| {
        line["kind"] == "reloaded"
    }));
    session_send(
        &client,
        r#"{"id":"c_prompt","command":"prompt","args":{"content":[{"type":"text","text":"read again"}]}}"#,
    );
    lines.extend(session_until(
        &client,
        "the second turn to complete",
        |line| line["kind"] == "turn_completed",
    ));
    session_send(&client, r#"{"id":"c_close","command":"close"}"#);
    lines.extend(until_session_close(&client));
    drop(client);
    let (status, _, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr} lines: {lines:?}");

    let starts = captured(&setup, "start.log");
    assert_eq!(starts.len(), 1);
    // `clients` is set aside: written when the connection attaches, with
    // no position against the session's other lines (`docs/testing.md`,
    // "Event streams"); one attach writes exactly one.
    let kinds = support::kinds(&lines);
    assert_kinds_without_deltas(
        &kinds,
        &[
            "command_accepted",
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "reloaded",
            "command_accepted",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "command_accepted",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "tool_call_requested",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(&kinds, "reloaded", "preamble_built", RELOAD_PREAMBLE_DOC);
    assert_each_followed_by(
        &kinds,
        "reloaded",
        "extensions_loaded",
        RELOAD_EXTENSIONS_DOC,
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count(),
        1
    );
    let first: Value = serde_json::from_str(&starts[0]).unwrap();
    assert_eq!(first["reason"], "start");
    let marks = captured(&setup, "mark.log");
    assert_eq!(marks.len(), 2);
    for line in &marks {
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["point"], "after_tool");
        assert_eq!(value["tool"], "read");
    }
}

#[test]
fn an_entry_with_empty_tools_is_not_registered_and_names_the_field() {
    let (setup, _server) = hooked(
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({
            "quiet": json!({
                "point": "after_tool",
                "tools": [],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "quiet.log"],
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
        }),
    );
    fs::write(setup.workspace().join("note.txt"), "note text\n").unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
            "preamble_built",
            "opening_message",
            "extension_state_set",
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
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "tool_call_requested",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(loaded_names(&run).contains(&HOOKS_NAME.to_owned()));

    let notices = run.all("notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    let failure = &notices[0];
    assert_eq!(failure["payload"]["code"], "extension_failed");
    let message = failure["payload"]["message"].as_str().unwrap();
    assert!(message.contains("quiet"), "{message}");
    assert!(message.contains("tools"), "{message}");
    assert!(
        !setup.workspace().join("quiet.log").exists(),
        "the entry never ran"
    );
}

#[test]
fn an_on_failure_less_entry_beside_a_blocking_one_fails_closed_when_removed() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "loose": json!({
                "point": "after_tool",
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "loose.log"],
                "timeout": 1000,
            }),
            "guard": entry("before_tool", "/bin/sh", json!(["hooks/capture.sh", "guard.log"]), 1000, "blocking"),
        }),
    );
    setup.remove_hooks();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(1));
    assert_kinds_without_deltas(&run.kinds(), &["fiber_exited"], IGNORE_DELTA_DOC, COUNT_DOC);
    assert_eq!(
        run.first("fiber_exited")["payload"]["error"]["code"],
        "extension_missing"
    );
    let message = run.first("fiber_exited")["payload"]["error"]["message"]
        .as_str()
        .unwrap();
    assert_eq!(
        message,
        "The `hooks` extension is not installed. Run `fiber extension install hooks`.",
    );
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
}

#[test]
fn no_hooks_key_loads_no_hooks_vm() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.install_hooks();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
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
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(!loaded_names(&run).contains(&HOOKS_NAME.to_owned()));
    assert!(run.all("notice").is_empty());
}

#[test]
fn empty_hooks_settings_load_silently() {
    let (setup, _server) = hooked(vec![hello()], &json!({}));
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(loaded_names(&run).contains(&HOOKS_NAME.to_owned()));
    assert!(run.all("notice").is_empty());
}

#[test]
fn a_removed_package_with_a_blocking_entry_fails_closed() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "guard": entry("before_tool", "/bin/sh", json!(["hooks/capture.sh", "guard.log"]), 1000, "blocking"),
        }),
    );
    setup.remove_hooks();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(1));
    assert_kinds_without_deltas(&run.kinds(), &["fiber_exited"], IGNORE_DELTA_DOC, COUNT_DOC);
    assert_eq!(
        run.first("fiber_exited")["payload"]["error"]["code"],
        "extension_missing"
    );
}

#[test]
fn the_door_stays_closed_for_a_removed_blocking_guard() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "guard": entry("before_tool", "/bin/sh", json!(["hooks/capture.sh", "guard.log"]), 1000, "blocking"),
        }),
    );
    setup.remove_hooks();
    let id = doors::mint("s_");
    let output = support::run_to_exit(
        setup.deadline,
        "fiber session",
        setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            setup.workspace().to_str().unwrap(),
            "--prompt",
            "hi",
        ]),
    );
    let run = Run::from(output);
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_kinds_without_deltas(&run.kinds(), &["fiber_exited"], IGNORE_DELTA_DOC, COUNT_DOC);
    assert_eq!(
        run.first("fiber_exited")["payload"]["error"]["code"],
        "extension_missing"
    );
    let message = run.first("fiber_exited")["payload"]["error"]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.contains("fiber extension install hooks"),
        "{message}"
    );
}

#[test]
fn a_removed_package_with_only_non_blocking_entries_warns_and_runs() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "note": entry("after_tool", "/bin/sh", json!(["hooks/capture.sh", "note.log"]), 1000, "non-blocking"),
        }),
    );
    setup.remove_hooks();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );

    let notices = run.all("notice");
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["payload"]["code"], "extension_missing");
    assert_eq!(
        notices[0]["payload"]["message"],
        "The `hooks` extension is not installed, so its declared hooks will not run. Run `fiber extension install hooks`.",
    );
    assert_eq!(notices[0]["payload"]["extension"], HOOKS_NAME);
}

#[test]
fn a_removed_package_with_watcher_only_declarations_runs_quietly() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "seen": json!({
                "watch": ["turn_completed"],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "seen.log"],
                "timeout": 1000,
            }),
        }),
    );
    setup.remove_hooks();
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
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
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(
        run.all("notice").is_empty(),
        "no missing notice: {:?}",
        run.all("notice")
    );
}

#[test]
fn an_entry_with_misplaced_tools_is_not_registered_and_names_the_field() {
    let (setup, _server) = hooked(
        vec![hello()],
        &json!({
            "loud": json!({
                "point": "session_start",
                "tools": ["read"],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "loud.log"],
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
        }),
    );
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(loaded_names(&run).contains(&HOOKS_NAME.to_owned()));

    let notices = run.all("notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    let failure = &notices[0];
    assert_eq!(failure["payload"]["code"], "extension_failed");
    let message = failure["payload"]["message"].as_str().unwrap();
    assert!(message.contains("loud"), "{message}");
    assert!(message.contains("tools"), "{message}");
    assert!(
        !setup.workspace().join("loud.log").exists(),
        "the entry never ran"
    );
}

#[test]
fn a_watch_entry_is_not_registered_and_names_the_entry() {
    let (setup, _server) = hooked(
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({
            "seen": json!({
                "watch": ["turn_completed"],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "seen.log"],
            }),
            "note": json!({
                "point": "after_tool",
                "tools": ["read"],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "note.log"],
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
        }),
    );
    fs::write(setup.workspace().join("note.txt"), "note text\n").unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "tool_call_requested",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(loaded_names(&run).contains(&HOOKS_NAME.to_owned()));

    // The kinds hold as a set with counts, pinning only the reply's
    // promised order; the missing timeout must yield an entry/field
    // notice (`docs/extensions.md`, "Hooks declared in configuration").
    // The watch entry missing `timeout` left its notice and never ran;
    // the hook entry ran.
    let notices = run.all("notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["payload"]["code"], "extension_failed");
    let message = notices[0]["payload"]["message"].as_str().unwrap();
    assert!(
        message.contains("`timeout` hook not registered for entry `seen`"),
        "{message}"
    );
    assert!(
        !setup.workspace().join("seen.log").exists(),
        "the watch entry never ran"
    );
    assert_eq!(captured(&setup, "note.log").len(), 1);
}

#[test]
fn entries_without_a_string_command_are_not_registered_and_name_the_entry() {
    let (setup, _server) = hooked(
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({
            "missing": json!({
                "point": "after_tool",
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
            "shapeless": json!({
                "point": "after_tool",
                "command": 42,
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
            "note": json!({
                "point": "after_tool",
                "tools": ["read"],
                "command": "/bin/sh",
                "args": ["hooks/capture.sh", "note.log"],
                "timeout": 30000,
                "on_failure": "non-blocking",
            }),
        }),
    );
    fs::write(setup.workspace().join("note.txt"), "note text\n").unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "notice",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "tool_call_requested",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert!(loaded_names(&run).contains(&HOOKS_NAME.to_owned()));

    // Both bad entries left one grouped notice and never ran; the good
    // one ran (`docs/errors.md`, "Notices": one notice per code;
    // `docs/extensions.md`, "Hooks declared in configuration": a hook
    // entry names its `command`, and a `notice` names the entry that
    // cannot register).
    let notices = run.all("notice");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["payload"]["code"], "extension_failed");
    let message = notices[0]["payload"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("2 `extension_failed` notices: "),
        "{message}"
    );
    assert!(message.contains("command"), "{message}");
    assert!(message.contains("missing"), "{message}");
    assert!(message.contains("shapeless"), "{message}");
    assert_eq!(captured(&setup, "note.log").len(), 1);
}

#[test]
fn an_after_tool_exit_two_with_non_blocking_keeps_the_output() {
    let (setup, _server) = hooked(
        vec![
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ],
        &json!({
            "soft": entry("after_tool", "/bin/sh", json!(["hooks/capture.sh", "soft.log", "refuse"]), 30000, "non-blocking"),
        }),
    );
    fs::write(setup.workspace().join("note.txt"), "note text\n").unwrap();
    let run = setup.run(&["ask", "read the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_kinds_without_deltas(
        &kinds,
        &[
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "extension_state_set",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "notice",
            "tool_call_completed",
            "step_started",
            "extension_exec",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "assistant_message_started",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "tool_call_requested",
        "assistant_message_completed",
        REPLY_DOC,
    );
    assert_each_followed_by(
        &kinds,
        "text_completed",
        "assistant_message_completed",
        REPLY_DOC,
    );

    assert_eq!(captured(&setup, "soft.log").len(), 1);
    let failure = run
        .all("notice")
        .into_iter()
        .find(|notice| notice["payload"]["code"] == "hook_failed")
        .expect("the hook's failure notice");
    let message = failure["payload"]["message"].as_str().unwrap();
    assert!(message.contains("soft"), "{message}");
    assert!(message.contains("exited 2"), "{message}");
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    assert!(
        completed["payload"]["content"]
            .to_string()
            .contains("note text"),
        "the original output is kept: {:?}",
        completed["payload"]["content"]
    );
}

/// A non-object entry fails `config_invalid` naming the malformed entry,
/// whatever the file's key order, whether or not `hooks` is installed.
#[test]
fn a_non_object_entry_is_config_invalid() {
    let blocking_a = entry(
        "after_tool",
        "/bin/sh",
        json!(["hooks/capture.sh", "a.log"]),
        1000,
        "blocking",
    );
    let blocking_z = entry(
        "after_tool",
        "/bin/sh",
        json!(["hooks/capture.sh", "z.log"]),
        1000,
        "blocking",
    );
    // Each acceptance map in both file orders: the verdict names the
    // malformed entry (`docs/extensions.md`, "Hooks declared in
    // configuration"), `z` here and `a` below.
    let cases = [
        (json!({"a": blocking_a.clone(), "z": false}), "hooks.z"),
        (json!({"z": false, "a": blocking_a}), "hooks.z"),
        (json!({"a": false, "z": blocking_z.clone()}), "hooks.a"),
        (json!({"z": blocking_z, "a": false}), "hooks.a"),
        // Both malformed: the verdict names the first in sorted key order
        // even when the file order leads with the other one.
        (json!({"b": false, "a": false}), "hooks.a"),
        (json!({"a": false, "b": false}), "hooks.a"),
    ];
    for installed in [true, false] {
        for (entries, expected) in &cases {
            let (setup, _server) = hooked(vec![hello()], entries);
            if !installed {
                setup.remove_hooks();
            }
            let run = setup.run(&["ask", "hi"]);
            assert_eq!(
                run.code,
                Some(1),
                "installed: {installed}, entries: {entries}, stderr: {}",
                run.stderr
            );
            // One line only, so no turn starts: a failure before
            // `fiber_started` prints one `fiber_exited` line
            // (`docs/errors.md:85-91`).
            assert_kinds_without_deltas(
                &run.kinds(),
                &["fiber_exited"],
                IGNORE_DELTA_DOC,
                COUNT_DOC,
            );
            assert_eq!(
                run.first("fiber_exited")["payload"]["error"]["code"],
                "config_invalid",
                "installed: {installed}, entries: {entries}"
            );
            let message = run.first("fiber_exited")["payload"]["error"]["message"]
                .as_str()
                .unwrap();
            assert!(message.contains(expected), "{message}");
        }
    }
}

/// A non-object `hooks` value fails `config_invalid` naming the `hooks` key,
/// whether or not `hooks` is installed.
#[test]
fn a_non_object_hooks_value_is_config_invalid_naming_hooks() {
    for installed in [true, false] {
        let (setup, _server) = hooked(vec![hello()], &json!(false));
        if !installed {
            setup.remove_hooks();
        }
        let run = setup.run(&["ask", "hi"]);
        assert_eq!(
            run.code,
            Some(1),
            "installed: {installed}, stderr: {}",
            run.stderr
        );
        // One line only, so no turn starts (`docs/errors.md:85-91`); the
        // message names the `hooks` key (`docs/extensions.md`, "Hooks
        // declared in configuration").
        assert_kinds_without_deltas(&run.kinds(), &["fiber_exited"], IGNORE_DELTA_DOC, COUNT_DOC);
        assert_eq!(
            run.first("fiber_exited")["payload"]["error"]["code"],
            "config_invalid",
            "installed: {installed}"
        );
        let message = run.first("fiber_exited")["payload"]["error"]["message"]
            .as_str()
            .unwrap();
        assert!(message.contains("hooks"), "{message}");
    }
}

/// The multiset helper pins every counted kind while letting the fake
/// stream's text-delta chunking vary: the same kinds in another order,
/// with another delta count, pass; a duplicated, missing or extra counted
/// kind fails, and naming the set-aside delta kind is a contradictory
/// policy.
#[test]
fn kinds_without_deltas_hold_counts_but_not_delta_chunking() {
    let reported = [
        "turn_completed",
        "assistant_message_delta",
        "text_completed",
        "assistant_message_delta",
        "assistant_message_delta",
    ];
    assert_kinds_without_deltas(
        &reported,
        &["text_completed", "turn_completed"],
        IGNORE_DELTA_DOC,
        COUNT_DOC,
    );
    // A duplicated counted kind fails.
    assert!(
        std::panic::catch_unwind(|| assert_kinds_without_deltas(
            &reported,
            &["text_completed", "text_completed", "turn_completed"],
            IGNORE_DELTA_DOC,
            COUNT_DOC,
        ))
        .is_err()
    );
    // A counted kind the stream dropped fails: `text_completed` stays
    // counted.
    assert!(
        std::panic::catch_unwind(|| assert_kinds_without_deltas(
            &["turn_completed", "assistant_message_delta"],
            &["text_completed", "turn_completed"],
            IGNORE_DELTA_DOC,
            COUNT_DOC,
        ))
        .is_err()
    );
    // An extra counted kind fails.
    assert!(
        std::panic::catch_unwind(|| assert_kinds_without_deltas(
            &["turn_completed", "assistant_message_delta", "notice"],
            &["turn_completed"],
            IGNORE_DELTA_DOC,
            COUNT_DOC,
        ))
        .is_err()
    );
    // Naming the set-aside delta kind is a contradictory policy.
    assert!(
        std::panic::catch_unwind(|| assert_kinds_without_deltas(
            &reported,
            &["assistant_message_delta", "turn_completed"],
            IGNORE_DELTA_DOC,
            COUNT_DOC,
        ))
        .is_err()
    );
}

/// The promised-order helper pins each pair with its doc line: an
/// adjacent or separated pair passes, while a `first` with no later
/// `second`, or a missing kind, fails.
#[test]
fn each_followed_by_pins_the_promised_pair() {
    let doc = "no doc promises this test pair";
    assert_each_followed_by(
        &["reloaded", "preamble_built"],
        "reloaded",
        "preamble_built",
        doc,
    );
    assert_each_followed_by(
        &["reloaded", "extensions_loaded", "preamble_built"],
        "reloaded",
        "preamble_built",
        doc,
    );
    assert_each_followed_by(
        &["reloaded", "preamble_built", "reloaded", "preamble_built"],
        "reloaded",
        "preamble_built",
        doc,
    );
    // A `first` at the end, or with `second` only before it, fails.
    assert!(
        std::panic::catch_unwind(|| assert_each_followed_by(
            &["reloaded", "preamble_built", "reloaded"],
            "reloaded",
            "preamble_built",
            doc,
        ))
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| assert_each_followed_by(
            &["preamble_built", "reloaded"],
            "reloaded",
            "preamble_built",
            doc,
        ))
        .is_err()
    );
    // A missing kind fails on either side.
    assert!(
        std::panic::catch_unwind(|| assert_each_followed_by(
            &["reloaded"],
            "reloaded",
            "preamble_built",
            doc,
        ))
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| assert_each_followed_by(
            &["preamble_built"],
            "reloaded",
            "preamble_built",
            doc,
        ))
        .is_err()
    );
}
