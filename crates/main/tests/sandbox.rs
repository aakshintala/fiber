//! Binary-level sandbox key wiring (`docs/configuration.md`, "Keys"): the
//! built `fiber` reads person-only `sandbox.*` keys through the binary
//! boundary. Observable JSON events (`confined: true`) and fail-closed spawn
//! are covered through the public shell API in `crates/tools/tests/shell.rs`
//! and the wire shape in `crates/contract/src/events_tests.rs`; the existing
//! `resume.rs` binary tests cover `confined: false` events.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use support::{KillGroup, Output, Setup, spawn_watched};

impl Setup {
    fn new_with_sandbox_root() -> Self {
        // Root the session under the owner's home: the sandbox grants the
        // temp dir as writable, so a fixture beneath it would overlap and
        // report unavailable instead of confining.
        let home_base = std::env::var_os("HOME").expect("HOME names the owner's home");
        let root = fakes::TempDir::under(std::path::Path::new(&home_base), "fiber-sandbox-binary");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline: support::Deadline::start(),
            root,
        }
    }

    fn fiber_run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.root.path().join("w"))
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
        assert!(!fakes::group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Output::from(output)
    }
}

#[test]
fn sandbox_keys_round_trip_through_the_binary() {
    let setup = Setup::new_with_sandbox_root();
    let out = setup.fiber_run(&["config", "set", "sandbox.enabled", "true"]);
    assert_eq!(out.code, Some(0));
    let out = setup.fiber_run(&["config", "get", "sandbox.enabled"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains("true"));
    let out = setup.fiber_run(&["config", "set", "sandbox.allow_write", r#"["~/.pi/agent"]"#]);
    assert_eq!(out.code, Some(0));
    let out = setup.fiber_run(&["config", "get", "sandbox.allow_write"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains(".pi/agent"));
}

/// A session confined through the binary denies the configured path: the
/// model reads the secret and the sibling, and the log shows the ordered
/// events with `confined: true`, no denied secret and a readable sibling.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_session_denies_the_configured_path() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    // The denied home sits beside the session workspace under the same
    // owner-home root: a deny beneath the workspace would overlap it.
    let fixture = setup.root.path().join("fh");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(fixture.join("secret"), "shh\n").unwrap();
    fs::write(fixture.join("sibling"), "ok\n").unwrap();
    let secret = fixture.join("secret");
    let sibling = fixture.join("sibling");

    let server = ProviderServer::start([
        stream(&[function_call(
            "secret_1",
            "shell",
            &json!({"command": format!("cat {}", secret.display())}),
        )]),
        stream(&[function_call(
            "sibling_1",
            "shell",
            &json!({"command": format!("cat {}", sibling.display())}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // The person-only sandbox config enables confinement with the deny.
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "sandbox": {"enabled": true, "deny_read": [secret.display().to_string()]}
        }))
        .unwrap(),
    )
    .unwrap();
    // A standing rule allows the shell command, so review approves it.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "cat "})
        ),
    )
    .unwrap();

    let run = setup.fiber_run(&["ask", "read both files"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!lines.is_empty(), "the session writes event lines");
    // The action ids are minted per call: tell the calls apart by the
    // requested command, then follow each id through its events.
    let action = |command: &str| {
        lines
            .iter()
            .find(|line| {
                line["kind"] == "tool_call_requested"
                    && line["payload"]["arguments"].to_string().contains(command)
            })
            .and_then(|line| line["action_id"].as_str())
            .unwrap_or_else(|| panic!("a request for {command}"))
            .to_owned()
    };
    let secret_id = action(secret.display().to_string().as_str());
    let sibling_id = action(sibling.display().to_string().as_str());
    // The two calls run in order, each requested before it starts and
    // completed after.
    let positions = |kind: &str, id: &str| {
        lines
            .iter()
            .position(|line| line["kind"] == kind && line["action_id"] == id)
            .unwrap_or_else(|| panic!("a {kind} line for {id}"))
    };
    let order = [
        positions("tool_call_requested", &secret_id),
        positions("tool_call_started", &secret_id),
        positions("tool_call_completed", &secret_id),
        positions("tool_call_requested", &sibling_id),
        positions("tool_call_started", &sibling_id),
        positions("tool_call_completed", &sibling_id),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "ordered events"
    );
    for (name, id) in [("secret", &secret_id), ("sibling", &sibling_id)] {
        let started = &lines[positions("tool_call_started", id)];
        assert_eq!(
            started["payload"]["confined"], true,
            "{name} starts confined"
        );
    }
    let secret_end = &lines[positions("tool_call_completed", &secret_id)];
    let secret_text = secret_end["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    // Seatbelt reports EPERM where Landlock reports EACCES: the confined
    // read fails on both, with the platform's refusal word.
    #[cfg(target_os = "macos")]
    let refused = "Operation not permitted";
    #[cfg(not(target_os = "macos"))]
    let refused = "Permission denied";
    assert!(
        secret_text.contains(refused),
        "the denied read fails: {secret_text}"
    );
    assert!(
        !secret_text.contains("shh"),
        "no denied secret in the output: {secret_text}"
    );
    let sibling_end = &lines[positions("tool_call_completed", &sibling_id)];
    let sibling_text = sibling_end["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    assert!(
        sibling_text.contains("ok"),
        "the sibling stays readable: {sibling_text}"
    );
}

/// A confined denial followed by the model's explicit unconfined retry:
/// the first call fails confined with no automatic retry, and only the
/// next model response requests the write-plus-read call with
/// `unconfined: true`. The retry is reviewed, starts without `confined`,
/// and returns the secret.
#[test]
fn an_explicit_unconfined_retry_reads_the_denied_path() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    // The denied home sits beside the session workspace under the same
    // owner-home root: a deny beneath the workspace would overlap it.
    let fixture = setup.root.path().join("fh");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(fixture.join("secret"), "shh\n").unwrap();
    let secret = fixture.join("secret");
    // The retry writes its marker into the workspace, which stays writable.
    // It is one simple command on purpose: a script of several commands
    // declares no subject, and no standing rule can match a call with none
    // (`docs/permissions.md`, "What a rule matches"). `tee` prints the
    // secret to both its output and the marker file.
    let marker = setup.root.path().join("w").join("marker");
    let retry_command = format!("tee {} < {}", marker.display(), secret.display());

    let server = ProviderServer::start([
        stream(&[function_call(
            "secret_1",
            "shell",
            &json!({"command": format!("cat {}", secret.display())}),
        )]),
        stream(&[function_call(
            "retry_1",
            "shell",
            &json!({"command": retry_command, "unconfined": true}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // The person-only sandbox config enables confinement with the deny.
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "sandbox": {"enabled": true, "deny_read": [secret.display().to_string()]}
        }))
        .unwrap(),
    )
    .unwrap();
    // Standing rules allow both commands, so review approves them: a rule
    // prefix matches a subject equal to it or going on with a space
    // (`docs/permissions.md`, "What a rule matches"), so the prefixes carry
    // no trailing space.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "cat"}),
            json!({"decision": "allow", "tool": "shell", "prefix": "tee"}),
        ),
    )
    .unwrap();

    let run = setup.fiber_run(&["ask", "read both files"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!lines.is_empty(), "the session writes event lines");
    // The calls are told apart by their arguments: only the retry carries
    // `unconfined`.
    let action = |want_unconfined: bool| {
        lines
            .iter()
            .find(|line| {
                line["kind"] == "tool_call_requested"
                    && line["payload"]["arguments"]
                        .get("unconfined")
                        .is_some_and(|flag| flag == &json!(true))
                        == want_unconfined
            })
            .and_then(|line| line["action_id"].as_str())
            .unwrap_or_else(|| panic!("a request with unconfined:{want_unconfined}"))
            .to_owned()
    };
    let secret_id = action(false);
    let retry_id = action(true);
    // The complete ordered event kinds (`docs/testing.md`, "Event streams"):
    // `support::kinds` sets aside the independently triggered
    // `session_status` lines. `tool_call_delta` is ephemeral with a
    // pacing-dependent count (`docs/tools.md`, "Progress"), so each shell
    // call's `tool_call_started` to `tool_call_completed` segment holds its
    // variable delta run while every other kind stays pinned in order. Every
    // delta carries its call's id between that call's start and end
    // (`docs/events.md`, "`tool_call_delta`"), so a duplicated or misplaced
    // delta fails. `assistant_message_delta` is ephemeral with a
    // chunking-dependent count (`docs/events.md`,
    // "`assistant_message_delta`": the payload is "the text added since the
    // last delta" with no count promised; `docs/testing.md`, "Event
    // streams": a test asserts a count only where a doc promises it), so the
    // final reply's `assistant_message_started` to `text_completed` segment
    // holds its variable delta run the same way. The reply's items follow
    // its `assistant_message_started` in order and its
    // `assistant_message_completed` closes it (`docs/events.md`,
    // "`text_completed`"), so a duplicated or misplaced delta fails.
    let secret_started_at = lines
        .iter()
        .position(|line| line["kind"] == "tool_call_started" && line["action_id"] == secret_id)
        .expect("a tool_call_started line for the first call");
    let secret_completed_at = lines
        .iter()
        .position(|line| line["kind"] == "tool_call_completed" && line["action_id"] == secret_id)
        .expect("a tool_call_completed line for the first call");
    let retry_started_at = lines
        .iter()
        .position(|line| line["kind"] == "tool_call_started" && line["action_id"] == retry_id)
        .expect("a tool_call_started line for the retry");
    let retry_completed_at = lines
        .iter()
        .position(|line| line["kind"] == "tool_call_completed" && line["action_id"] == retry_id)
        .expect("a tool_call_completed line for the retry");
    let secret_deltas = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_delta" && line["action_id"] == secret_id)
        .count();
    let retry_deltas = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_delta" && line["action_id"] == retry_id)
        .count();
    let delta_total = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_delta")
        .count();
    assert!(retry_deltas >= 1, "a running shell streams deltas");
    assert_eq!(
        delta_total,
        secret_deltas + retry_deltas,
        "every delta belongs to one of the two calls"
    );
    for (index, line) in lines.iter().enumerate() {
        if line["kind"] != "tool_call_delta" {
            continue;
        }
        if line["action_id"] == secret_id {
            assert!(
                secret_started_at < index && index < secret_completed_at,
                "the first call's deltas stream while it runs"
            );
        } else {
            assert!(
                retry_started_at < index && index < retry_completed_at,
                "the retry's deltas stream while it runs"
            );
        }
    }
    let final_started_at = lines
        .iter()
        .rposition(|line| line["kind"] == "assistant_message_started")
        .expect("a final assistant_message_started line");
    let final_completed_at = lines
        .iter()
        .rposition(|line| line["kind"] == "assistant_message_completed")
        .expect("a final assistant_message_completed line");
    let final_id = lines[final_started_at]["action_id"]
        .as_str()
        .expect("an id for the final reply")
        .to_owned();
    let message_deltas = lines
        .iter()
        .filter(|line| line["kind"] == "assistant_message_delta")
        .count();
    for (index, line) in lines.iter().enumerate() {
        if line["kind"] != "assistant_message_delta" {
            continue;
        }
        assert_eq!(
            line["action_id"], final_id,
            "every text delta belongs to the final reply"
        );
        assert!(
            final_started_at < index && index < final_completed_at,
            "the final reply's deltas stream while it runs"
        );
    }
    let delta_text = lines
        .iter()
        .filter(|line| line["kind"] == "assistant_message_delta")
        .filter_map(|line| line["payload"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let completed_text = lines
        .iter()
        .find(|line| line["kind"] == "text_completed" && line["action_id"] == final_id)
        .and_then(|line| line["payload"]["text"].as_str())
        .unwrap_or_default();
    assert_eq!(
        delta_text, completed_text,
        "the deltas add up to the completed text"
    );
    let kinds = support::kinds(&lines);
    let mut expected: Vec<&str> = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "tool_call_started",
    ];
    expected.extend(std::iter::repeat_n("tool_call_delta", secret_deltas));
    expected.extend([
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "tool_call_requested",
        "usage_recorded",
        "assistant_message_completed",
        "permission_resolved",
        "tool_call_started",
    ]);
    expected.extend(std::iter::repeat_n("tool_call_delta", retry_deltas));
    expected.extend([
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
    ]);
    expected.extend(std::iter::repeat_n(
        "assistant_message_delta",
        message_deltas,
    ));
    expected.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(kinds, expected);
    // No automatic retry: exactly one request per model response
    // (`docs/events.md`, "Declared effects": a failed confined call is never
    // rerun unconfined; the fixture serves two model responses with one call
    // each).
    let requested = lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_requested")
        .count();
    assert_eq!(requested, 2, "one request per model response");
    // The two calls run in order, each requested before it starts and
    // completed after (`docs/loop.md`, "One step": each step streams its
    // reply, decides permission, then runs the approved calls, and returns
    // every result before the next step; `docs/events.md`, "Declared
    // effects": `tool_call_started` remains before execution and the call
    // ends through the normal `tool_call_*` end event).
    let positions = |kind: &str, id: &str| {
        lines
            .iter()
            .position(|line| line["kind"] == kind && line["action_id"] == id)
            .unwrap_or_else(|| panic!("a {kind} line for {id}"))
    };
    let order = [
        positions("tool_call_requested", &secret_id),
        positions("tool_call_started", &secret_id),
        positions("tool_call_completed", &secret_id),
        positions("tool_call_requested", &retry_id),
        positions("tool_call_started", &retry_id),
        positions("tool_call_completed", &retry_id),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "ordered events"
    );
    let secret_started = &lines[positions("tool_call_started", &secret_id)];
    assert_eq!(
        secret_started["payload"]["confined"], true,
        "the first call starts confined"
    );
    let retry_started = &lines[positions("tool_call_started", &retry_id)];
    assert!(
        retry_started["payload"].get("confined").is_none(),
        "the retry starts without confined: {}",
        retry_started["payload"]
    );
    let content_text = |id: &str| {
        lines[positions("tool_call_completed", id)]["payload"]["content"]
            .as_array()
            .map(|content| {
                content
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    };
    let secret_end = &lines[positions("tool_call_completed", &secret_id)];
    assert_eq!(
        secret_end["payload"]["status"], "failed",
        "the first call fails"
    );
    let secret_text = content_text(&secret_id);
    // Seatbelt reports EPERM where Landlock reports EACCES: the confined
    // read fails on both, with the platform's refusal word.
    #[cfg(target_os = "macos")]
    let refused = "Operation not permitted";
    #[cfg(not(target_os = "macos"))]
    let refused = "Permission denied";
    assert!(
        secret_text.contains(refused),
        "the denied read fails: {secret_text}"
    );
    assert!(
        !secret_text.contains("shh"),
        "no denied secret in the output: {secret_text}"
    );
    let retry_end = &lines[positions("tool_call_completed", &retry_id)];
    assert_eq!(
        retry_end["payload"]["status"], "completed",
        "the retry succeeds"
    );
    assert!(
        content_text(&retry_id).contains("shh"),
        "the retry returns the secret"
    );
    assert!(marker.is_file(), "the retry ran its write");
    // The retry reached review: its resolution allows it after its request
    // and before its start (`docs/loop.md`, "One step": permission is decided
    // after the reply streams its `tool_call_requested`; `docs/permissions.md`,
    // "What the log records": a reply's `permission_resolved` lines are written
    // in request order, before any of that reply's `tool_call_started`).
    let resolved = lines
        .iter()
        .position(|line| line["kind"] == "permission_resolved" && line["action_id"] == retry_id)
        .expect("a resolution for the retry");
    assert!(resolved > positions("tool_call_requested", &retry_id));
    assert!(resolved < positions("tool_call_started", &retry_id));
    assert_eq!(lines[resolved]["payload"]["decision"], "allow");
}

/// A confined call that changes a protected path restores it by the
/// call's end: the completion carries the report, and headless names
/// the path on stderr with the call's other lines.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_call_restores_the_protected_path_it_changed() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    let workspace = setup.root.path().join("w");
    fakes::git::git(&workspace, &["init", "-q"]);
    fakes::git::git(
        &workspace,
        &["commit", "-q", "--allow-empty", "-m", "baseline"],
    );
    let linked = setup.root.path().join("linked");
    let linked_arg = linked.to_string_lossy().into_owned();
    fakes::git::git(&workspace, &["worktree", "add", "--detach", &linked_arg]);
    let fiber = workspace.join(".fiber");
    fs::create_dir_all(&fiber).unwrap();
    fs::create_dir_all(linked.join(".fiber")).unwrap();
    fs::write(fiber.join("note.txt"), "original\n").unwrap();
    fs::write(linked.join(".fiber/note.txt"), "linked original\n").unwrap();
    fs::write(linked.join("mutation.txt"), "gitdir: /evil\n").unwrap();
    let linked_pointer = fs::read(linked.join(".git")).unwrap();
    // The command keeps a subject the standing rule can match
    // (`docs/permissions.md`, "What a rule matches") while changing the
    // linked pointer's gitdir target and both worktrees' `.fiber/` paths.
    let command =
        "tee .fiber/note.txt ../linked/.git ../linked/.fiber/note.txt < ../linked/mutation.txt";

    let server = ProviderServer::start([
        stream(&[function_call(
            "change_1",
            "shell",
            &json!({"command": command}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "sandbox": {
                "enabled": true,
                "allow_write": [linked.display().to_string()]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    // A standing rule allows the shell command, so review approves it.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "tee"})
        ),
    )
    .unwrap();

    let run = setup.fiber_run(&["ask", "change the note"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(!lines.is_empty(), "the session writes event lines");
    let completed = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_completed"
                && line["payload"]["details"]["command"] == command
        })
        .unwrap_or_else(|| panic!("a completion for the change"));
    let action = completed["action_id"]
        .as_str()
        .expect("a completion names its call");
    let started = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == action)
        .unwrap_or_else(|| panic!("a start for the change"));
    assert_eq!(
        started["payload"]["confined"], true,
        "the change runs confined"
    );
    // `docs/sandbox.md:64-73` promises restore and report of each changed
    // protected path; `docs/events.md:683` defines restored paths as
    // workspace-relative. Both worktrees' `.fiber/` paths must remain distinct.
    let restored: std::collections::BTreeSet<&str> =
        completed["payload"]["details"]["protected_paths"]["restored"]
            .as_array()
            .expect("the completion carries restored paths")
            .iter()
            .filter_map(Value::as_str)
            .collect();
    assert_eq!(
        restored,
        std::collections::BTreeSet::from_iter([".fiber", "../linked/.git", "../linked/.fiber",]),
        "the completion carries the worktree pointer and `.fiber/` report; pointer after call: {:?}",
        fs::read(linked.join(".git"))
    );
    let text = completed["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    // `docs/events.md:697-701` promises the completion report's warning
    // line reaches the model through result text; the exact wording is not
    // specified by that contract.
    assert!(
        text.contains(".fiber"),
        "the result names the change: {text}"
    );
    // `docs/sandbox.md:102-106` promises this warning wording and its
    // headless stderr destination.
    assert!(
        run.stderr.contains(
            "fiber: Protected path .fiber changed while a confined call ran; Fiber restored it."
        ),
        "headless warns on stderr: {}",
        run.stderr
    );
    // `docs/sandbox.md:64-73` promises each changed protected path is
    // restored to its recorded contents after the confined call.
    assert_eq!(
        fs::read(fiber.join("note.txt")).unwrap(),
        b"original\n",
        "the call's end restores the change"
    );
    assert_eq!(fs::read(linked.join(".git")).unwrap(), linked_pointer);
    assert_eq!(
        fs::read(linked.join(".fiber/note.txt")).unwrap(),
        b"linked original\n"
    );
    // `docs/sandbox.md:102-106` promises the warning wording and its
    // headless stderr destination for each changed path.
    assert!(
        run.stderr
            .contains("fiber: Protected path ../linked/.git changed while a confined call ran;"),
        "headless warns about the linked pointer: {}",
        run.stderr
    );
}

/// A session killed mid-call leaves its baseline stale: the next session
/// start restores the abandoned change and reports it.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_call_leaves_a_stale_baseline_the_next_session_recovers() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use std::io::BufRead;
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    let workspace = setup.root.path().join("w");
    let fiber = workspace.join(".fiber");
    fs::create_dir_all(&fiber).unwrap();
    fs::write(fiber.join("note.txt"), "original\n").unwrap();
    // The child signals its own start, then blocks to be killed: `tee`
    // creates the signal file first and reads the FIFO the test holds
    // open, so the open blocks until the kill. A single simple stage with
    // an input redirect keeps the call rule-matchable: a chained command
    // or an output-to-file redirect names no subject, so no allow rule
    // could match it (`crates/tools/src/shell/classify.rs`,
    // `subject_and_prefix`; `crates/tools/src/shell/syntax.rs`,
    // `Redirect`). Spawn follows the baseline, so the signal proves the
    // child runs with the baseline held. The note changes only after that
    // signal, while the call runs, and the guard restores whatever changed
    // it without naming a writer.
    let command = "tee ready.signal < input.fifo";
    std::process::Command::new("mkfifo")
        .arg(workspace.join("input.fifo"))
        .status()
        .expect("the signal FIFO is created");
    // Opened read-write, the FIFO never blocks on open; held until the
    // end, the writer keeps `tee` blocked reading.
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.join("input.fifo"))
        .expect("the signal FIFO opens");

    let server = ProviderServer::start([
        stream(&[function_call(
            "sleep_1",
            "shell",
            &json!({"command": command}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "sandbox": {"enabled": true}
        }))
        .unwrap(),
    )
    .unwrap();
    // A standing rule allows the shell command, so review approves it.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "tee"})
        ),
    )
    .unwrap();

    // Spawn the session by hand: the turn never ends on its own, so the
    // test reads for the confined start, then kills the session dead.
    let mut spawned = Command::new(env!("CARGO_BIN_EXE_fiber"));
    spawned
        .args(["ask", "take a nap"])
        .current_dir(&workspace)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", setup.root.path())
        .env("FIBER_HOME", setup.home())
        .env("FIBER_TEST_FAKE_KEY", "sk-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, watchdog) = spawn_watched(&mut spawned);
    let group = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = support::Deadline::start();
    loop {
        let line = deadline.recv_or_fail(&lines_rx, "the confined start");
        let value: Value = serde_json::from_str(&line).unwrap_or(json!({}));
        if value["kind"] == "tool_call_started" && value["payload"]["confined"] == true {
            break;
        }
    }
    // The note changes only once the confined child proves it started: the
    // child's own signal lands after the baseline exists, since the call
    // writes the baseline before spawn. Neither `tool_call_started` nor the
    // baseline file proves the child runs; this file does.
    fakes::within("the child signals", support::Deadline::start(), move || {
        while fs::read(workspace.join("ready.signal")).is_err() {}
    });
    fs::write(fiber.join("note.txt"), "changed\n").unwrap();
    // SIGKILL the whole session, then reap it on every path past this point.
    fakes::kill_group(group, "KILL").expect("the session dies");
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || done_tx.send(child.wait()).unwrap());
    match deadline.recv(&done_rx) {
        Ok(exit) => {
            let status = exit.expect("the killed session reaps");
            assert!(!status.success(), "the kill ends the session: {status:?}");
        }
        Err(_) => support::expired(deadline, group, &done_rx, "`fiber ask` to die"),
    }
    assert!(!fakes::group_alive(group), "`fiber` left a process behind");
    watchdog.stand_down(deadline.cleanup_phase());

    // The next session start recovers the abandoned change and reports it.
    let run = setup.fiber_run(&["ask", "hello"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        fs::read(fiber.join("note.txt")).unwrap(),
        b"original\n",
        "startup recovery restores the abandoned change"
    );
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let notice = lines
        .iter()
        .find(|line| {
            line["kind"] == "notice" && line["payload"]["code"] == "protected_paths_restored"
        })
        .unwrap_or_else(|| panic!("a startup recovery notice"));
    assert!(
        notice["payload"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(".fiber"),
        "the notice names the path: {}",
        notice["payload"]
    );
}

/// An `openai-responses` stream answering `allow` in one fragment: the
/// reviewer's stage-1 verdict, so a call with no matchable subject still
/// runs reviewed.
#[cfg(target_os = "linux")]
fn allow_verdict() -> fakes::Response {
    use serde_json::json;
    let events = [
        json!({"type": "response.output_text.delta", "delta": "allow"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "allow"}]}}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}}}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    fakes::Response::stream(body)
}

/// A confined call cannot open a socket: bash's `/dev/tcp` needs
/// `socket`, which the filter denies with EPERM beside the supervisor
/// signal rules, on every Landlock ABI.
#[cfg(target_os = "linux")]
#[test]
fn a_confined_call_cannot_open_a_socket() {
    use fakes::ProviderServer;
    use serde_json::{Value, json};
    use support::{function_call, hello, stream};

    let setup = Setup::new_with_sandbox_root();
    let command = "cat < /dev/tcp/127.0.0.1/9";

    let server = ProviderServer::start([
        stream(&[function_call(
            "socket_1",
            "shell",
            &json!({"command": command}),
        )]),
        allow_verdict(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // The reviewer allows the call, which names no subject for a rule.
    fs::write(
        setup.home().join("config.json"),
        serde_json::to_string(&json!({
            "model": "fake/m",
            "reviewer": {"model": "fake/m"},
            "sandbox": {"enabled": true}
        }))
        .unwrap(),
    )
    .unwrap();

    let run = setup.fiber_run(&["ask", "open a socket"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines: Vec<Value> = run
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let resolved = lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the socket call resolves");
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    assert_eq!(resolved["payload"]["decision"], "allow");
    let completed = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_completed"
                && line["payload"]["details"]["command"] == command
        })
        .unwrap_or_else(|| panic!("a completion for the socket call"));
    let text = completed["payload"]["content"]
        .as_array()
        .map(|content| {
            content
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    // The filter refuses with EPERM, which bash reports as its word for
    // the denied socket, not a refused connection.
    assert!(
        text.contains("Operation not permitted"),
        "the socket stays denied: {text}"
    );
}

#[test]
fn a_repository_sandbox_key_is_ignored_through_the_binary() {
    let setup = Setup::new_with_sandbox_root();
    fs::create_dir_all(setup.root.path().join("w/.fiber")).unwrap();
    fs::write(
        setup.root.path().join("w/.fiber/config.json"),
        r#"{"sandbox": {"enabled": true}}"#,
    )
    .unwrap();
    let out = setup.fiber_run(&["config", "get", "sandbox.enabled"]);
    assert_eq!(out.code, Some(0));
    assert!(out.stdout.contains("false"));
}
