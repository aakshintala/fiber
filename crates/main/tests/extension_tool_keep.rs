//! Binary-level tests of a Lua extension tool's declared `keep`
//! (`docs/tools.md`, "Bounded results"; `docs/extensions.md`,
//! "Registering"): the built `fiber` runs in its own process group with
//! its own `FIBER_HOME`, holding an ordinary provider whose base URL is
//! the fake server, and the extensions under test.
//!
//! A keep table is `{ head = 1024, tail = 1024 }`: `head` maps to
//! `Bound.start` and `tail` to `Bound.end`.

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
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{Setup, StdoutRun, function_call, hello_single_delta as hello, outputs_sent, stream};

/// One finished `fiber ask`: its exit code, its stdout lines parsed (less
/// `session_status`, which an observer thread writes), its raw stdout and
/// its stderr.
impl StdoutRun {
    fn payload(&self, kind: &str) -> &Value {
        &self
            .lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))["payload"]
    }

    /// Each notice's code, extension and message.
    fn notices(&self) -> Vec<(String, Option<String>, String)> {
        self.all("notice")
            .iter()
            .map(|line| {
                let payload = &line["payload"];
                (
                    payload["code"].as_str().unwrap().to_owned(),
                    payload["extension"].as_str().map(str::to_owned),
                    payload["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.lines[0]["session_id"].as_str().unwrap())
    }
}

/// Runs `fiber ask <prompt>` in the workspace, in its own process group,
/// waits for it under the test's deadline, and asserts that nothing it
/// started is left in the group. A watchdog kills the group if this
/// process dies first.
#[track_caller]
fn ask(setup: &Setup, prompt: &str) -> StdoutRun {
    let mut command = setup.fiber(&["ask", prompt]);
    command.current_dir(setup.workspace());
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match setup.deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, "`fiber ask` to exit"),
    };
    assert!(
        !fakes::group_alive(group),
        "`fiber` left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup_phase());
    let run = StdoutRun::from(output);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    run
}

/// A server whose model makes `calls` in one step, then answers `Hello.`.
fn calling(calls: &[Value]) -> ProviderServer {
    ProviderServer::start([stream(calls), hello()]).unwrap()
}

/// The tool names the request `index` sends.
fn sent_names(server: &ProviderServer, index: usize) -> Vec<String> {
    let request: Value = serde_json::from_slice(&server.requests()[index].body).unwrap();
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The complete event kinds of a run whose model makes one step of calls,
/// then answers: `loading` follows `extensions_loaded`, `decided` follows
/// the first step's `assistant_message_completed`, and `ran` follows that.
fn expected_kinds(
    loading: &[&'static str],
    requested: usize,
    decided: &[&'static str],
    ran: &[&'static str],
) -> Vec<&'static str> {
    let mut kinds = vec!["session_started", "fiber_started", "extensions_loaded"];
    kinds.extend(loading);
    kinds.extend([
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
    ]);
    kinds.extend(std::iter::repeat_n("tool_call_requested", requested));
    kinds.extend(["usage_recorded", "assistant_message_completed"]);
    kinds.extend(decided);
    kinds.extend(ran);
    kinds.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

/// One call that runs: started, then completed.
const RAN: &[&str] = &["tool_call_started", "tool_call_completed"];

/// A tool registration as a line of Lua: `name`, the Lua `run`, and
/// `keep` as its keep table.
fn keeping(name: &str, keep: &str, run: &str) -> String {
    format!(
        "fiber.tool(\"{name}\", {{ description = \"The {name} tool.\", input_schema = {{ type = \"object\" }}, timeout = 2000, effects = {{ effects = {{ \"reads\" }}, reversible = true }}, keep = {keep}, run = {run} }})\n"
    )
}

/// A tool registration as a line of Lua with no `keep`: `name` and the
/// Lua `run`.
fn plain(name: &str, run: &str) -> String {
    format!(
        "fiber.tool(\"{name}\", {{ description = \"The {name} tool.\", input_schema = {{ type = \"object\" }}, timeout = 2000, effects = {{ effects = {{ \"reads\" }}, reversible = true }}, run = {run} }})\n"
    )
}

/// Asserts two strings are byte-identical without printing either whole
/// on failure: only the two lengths and the first differing offset
/// (`docs/testing.md`, "What a test asserts").
#[track_caller]
fn assert_large_eq(actual: &str, expected: &str) {
    if actual.len() != expected.len() {
        let offset = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| actual.len().min(expected.len()));
        panic!(
            "large strings differ: actual {} bytes, expected {} bytes, first differing offset {offset}",
            actual.len(),
            expected.len()
        );
    }
    if let Some(offset) = actual
        .bytes()
        .zip(expected.bytes())
        .position(|(left, right)| left != right)
    {
        panic!(
            "large strings differ: actual {} bytes, expected {} bytes, first differing offset {offset}",
            actual.len(),
            expected.len()
        );
    }
}

#[test]
#[should_panic(expected = "actual 3 bytes, expected 5 bytes, first differing offset 1")]
fn assert_large_eq_reports_first_offset_when_lengths_differ() {
    assert_large_eq("axc", "aycde");
}

#[test]
#[should_panic(expected = "actual 2 bytes, expected 5 bytes, first differing offset 2")]
fn assert_large_eq_reports_shorter_length_when_one_is_a_prefix() {
    assert_large_eq("ab", "abcde");
}

/// The single text part of a `tool_call_completed`'s `content`: the JSON
/// structure is checked here, so the text itself compares byte-exact
/// through [`assert_large_eq`].
#[track_caller]
fn completed_text(completed: &Value) -> &str {
    let content = completed["content"]
        .as_array()
        .expect("content is a list of one text part");
    assert_eq!(content.len(), 1, "content holds one part");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().expect("the part holds text")
}

/// The one model-facing output the run sent: exactly one call output, so
/// its bytes compare through [`assert_large_eq`].
#[track_caller]
fn sent_output(server: &ProviderServer) -> String {
    let sent = outputs_sent(server);
    assert_eq!(sent.len(), 1, "one call output sent");
    sent.into_iter().next().unwrap()
}

/// The `tool_call_completed` of a run whose model makes one call: its
/// action's artifact path, and the artifact's absolute path in the
/// session directory.
fn artifact_of(run: &StdoutRun, setup: &Setup) -> (String, PathBuf) {
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap_or_else(|| panic!("no tool_call_completed line"));
    let action = completed["action_id"].as_str().unwrap();
    let relative = format!("artifacts/{action}.txt");
    assert_eq!(completed["payload"]["artifact"], relative.as_str());
    let path = run.session_dir(setup).join(&relative);
    (relative, path)
}

#[test]
fn a_larger_declared_cap_sends_a_result_past_the_default_uncut() {
    let setup = Setup::new_with_fm_root();
    let full = "a".repeat(20 * 1024);
    let server = calling(&[function_call("call_big", "big", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "big",
        &keeping(
            "big",
            "{ head = 32768 }",
            &format!("function() return string.rep(\"a\", {}) end", full.len()),
        ),
    );
    let run = ask(&setup, "go big");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    assert_large_eq(completed_text(completed), &full);
    assert_eq!(completed.get("artifact"), None);
    assert_eq!(completed.get("kept"), None);
    assert_large_eq(&sent_output(&server), &full);
}

#[test]
fn a_smaller_declared_cap_cuts_at_it_with_the_artifact() {
    let setup = Setup::new_with_fm_root();
    let full = "b".repeat(4 * 1024);
    let server = calling(&[function_call("call_small", "small", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "small",
        &keeping(
            "small",
            "{ head = 1024 }",
            "function() return string.rep(\"b\", 4096) end",
        ),
    );
    let run = ask(&setup, "go small");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let (_relative, path) = artifact_of(&run, &setup);
    assert_large_eq(&fs::read_to_string(&path).unwrap(), &full);
    let text = format!(
        "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]",
        &full[..1024],
        full.len() - 1024,
        path.display()
    );
    assert_large_eq(completed_text(completed), &text);
    assert_eq!(completed["kept"], json!([[0, 1024]]));
    assert_large_eq(&sent_output(&server), &text);
}

#[test]
fn a_keep_of_both_ends_sends_the_first_and_last_kib_with_the_notice_between() {
    let setup = Setup::new_with_fm_root();
    let full = format!(
        "{}{}{}",
        "H".repeat(1024),
        "M".repeat(62 * 1024),
        "T".repeat(1024)
    );
    assert_eq!(full.len(), 64 * 1024);
    let server = calling(&[function_call("call_both", "both", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "both",
        &keeping(
            "both",
            "{ head = 1024, tail = 1024 }",
            "function() return string.rep(\"H\", 1024) .. string.rep(\"M\", 62 * 1024) .. string.rep(\"T\", 1024) end",
        ),
    );
    let run = ask(&setup, "go both");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let (_relative, path) = artifact_of(&run, &setup);
    assert_large_eq(&fs::read_to_string(&path).unwrap(), &full);
    let text = format!(
        "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]\n{}",
        &full[..1024],
        full.len() - 2048,
        path.display(),
        &full[full.len() - 1024..]
    );
    assert_large_eq(completed_text(completed), &text);
    assert_eq!(completed["kept"], json!([[0, 1024], [64512, 65536]]));
    assert_large_eq(&sent_output(&server), &text);
}

#[test]
fn no_keep_cuts_at_the_default_sixteen_kib() {
    let setup = Setup::new_with_fm_root();
    let full = "c".repeat(20 * 1024);
    let server = calling(&[function_call("call_plain", "plain", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "plain",
        &plain(
            "plain",
            &format!("function() return string.rep(\"c\", {}) end", full.len()),
        ),
    );
    let run = ask(&setup, "go plain");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let (_relative, path) = artifact_of(&run, &setup);
    assert_large_eq(&fs::read_to_string(&path).unwrap(), &full);
    let text = format!(
        "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]",
        &full[..16384],
        full.len() - 16384,
        path.display()
    );
    assert_large_eq(completed_text(completed), &text);
    assert_eq!(completed["kept"], json!([[0, 16384]]));
    assert_large_eq(&sent_output(&server), &text);
}

#[test]
fn a_configured_cap_divides_in_the_declared_start_to_end_proportion() {
    let setup = Setup::new_with_fm_root();
    let full = "d".repeat(4 * 1024);
    let server = calling(&[function_call("call_capped", "capped", &json!({}))]);
    setup.provider(&server);
    fs::write(
        setup.home().join("config.json"),
        json!({"model": "fake/m", "tools": {"capped": {"max_result_bytes": 100}}}).to_string(),
    )
    .unwrap();
    setup.lua(
        "capped",
        &keeping(
            "capped",
            "{ head = 1024, tail = 1024 }",
            "function() return string.rep(\"d\", 4096) end",
        ),
    );
    let run = ask(&setup, "go capped");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let (_relative, path) = artifact_of(&run, &setup);
    assert_large_eq(&fs::read_to_string(&path).unwrap(), &full);
    // The end gets its share rounded down, and the start gets the rest:
    // 50 and 50 (`docs/tools.md`, "Bounded results").
    let text = format!(
        "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]\n{}",
        &full[..50],
        full.len() - 100,
        path.display(),
        &full[full.len() - 50..]
    );
    assert_large_eq(completed_text(completed), &text);
    assert_eq!(completed["kept"], json!([[0, 50], [4046, 4096]]));
    assert_large_eq(&sent_output(&server), &text);
}

#[test]
fn a_malformed_keep_leaves_the_tool_out_and_names_it_in_a_notice() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_kept", "kept", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "two",
        &format!(
            "{}{}",
            keeping("bad", "{ head = -1 }", "function() return \"\" end"),
            keeping("kept", "{ head = 1024 }", "function() return \"kept\" end"),
        ),
    );
    let run = ask(&setup, "use the tool");
    assert_eq!(run.kinds(), expected_kinds(&["notice"], 1, &[], RAN));
    assert_eq!(
        run.notices(),
        [(
            "extension_failed".to_owned(),
            Some("fiber.test/two".to_owned()),
            "`bad` tool not registered: `keep`: `head` must be a whole number of bytes".to_owned()
        )]
    );
    let names = sent_names(&server, 0);
    assert!(!names.contains(&"bad".to_owned()), "{names:?}");
    assert!(names.contains(&"kept".to_owned()), "{names:?}");
    assert_eq!(outputs_sent(&server), ["kept"]);
}
