//! Binary-level tests of a Lua extension's tools through `fiber ask`
//! (`docs/extensions.md`, "Registering"; `docs/testing.md`, "Levels"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding an ordinary provider whose base URL is the fake server, and the
//! extensions under test. Cancellation and the scheduler run on the fake
//! clock in `extensions`.

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
use support::{
    Setup, StdoutRun, function_call, hello_single_delta as hello, on_disk, outputs_sent, stream,
};

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

    /// `preamble_built`'s row for the tool `name`.
    fn sent_tool(&self, name: &str) -> &Value {
        self.payload("preamble_built")["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name} in preamble_built"))
    }

    /// The names `extensions_loaded` lists.
    fn loaded(&self) -> Vec<&str> {
        self.payload("extensions_loaded")["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ext| ext["name"].as_str().unwrap())
            .collect()
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

/// A tool registration as a line of Lua: `name`, the Lua `effects` and
/// `run`, and `timeout = <timeout>` unless `timeout` is empty.
fn tool(name: &str, effects: &str, run: &str, timeout: &str) -> String {
    tool_with_schema(name, "{ type = \"object\" }", effects, run, timeout)
}

/// A tool `name` that only reads and answers `said`.
fn saying(name: &str, said: &str) -> String {
    tool(
        name,
        "{ effects = { \"reads\" }, reversible = true }",
        &format!("function() return \"{said}\" end"),
        "2000",
    )
}

/// A tool registration like [`tool`], but with `schema` as its
/// `input_schema` Lua table instead of the fixed `{ type = "object" }`.
fn tool_with_schema(name: &str, schema: &str, effects: &str, run: &str, timeout: &str) -> String {
    let timeout = if timeout.is_empty() {
        String::new()
    } else {
        format!(" timeout = {timeout},")
    };
    format!(
        "fiber.tool(\"{name}\", {{ description = \"The {name} tool.\", input_schema = {schema},{timeout} effects = {effects}, run = {run} }})\n"
    )
}

#[test]
fn an_extension_tool_is_declared_in_full_runs_and_its_result_passes_the_hooks() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "a\nb\nc\n").unwrap();
    let server = calling(&[function_call("call_notes", "note_count", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "notes",
        &tool(
            "note_count",
            "{ effects = { \"reads\" }, paths = { \"note.txt\" }, reversible = true }",
            "function() return \"3 notes\" end",
            "2000",
        ),
    );
    setup.lua(
        "tag",
        "fiber.hook(\"after_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) return { content = call.content .. \"|t\" } end })\n",
    );
    let run = ask(&setup, "count the notes");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    assert_eq!(run.loaded(), ["fake", "fiber.test/notes", "fiber.test/tag"]);
    let names = sent_names(&server, 0);
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "one list sorted by name");
    assert!(names.contains(&"note_count".to_owned()), "{names:?}");
    assert!(names.contains(&"read".to_owned()), "{names:?}");
    let row = run.sent_tool("note_count");
    assert_eq!(row["registered_by"], "fiber.test/notes");
    assert_eq!(row["deferred"], false);
    let started = run.payload("tool_call_started");
    assert_eq!(started["effects"], json!(["reads"]));
    assert_eq!(started["paths"], json!(["note.txt"]));
    assert_eq!(started["reversible"], true);
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "3 notes|t"}])
    );
    assert_eq!(completed["changed_by"], json!(["fiber.test/tag"]));
    assert_eq!(outputs_sent(&server), ["3 notes|t"]);
}

#[test]
fn each_call_is_judged_on_the_effects_its_arguments_declare() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "a\n").unwrap();
    let secret = setup.home().join("credentials").join("k");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, "sekret\n").unwrap();
    let server = calling(&[
        function_call(
            "call_note",
            "peek",
            &json!({"path": "note.txt", "tag": "note"}),
        ),
        function_call(
            "call_key",
            "peek",
            &json!({"path": secret.display().to_string(), "tag": "key"}),
        ),
    ]);
    setup.provider(&server);
    setup.lua(
        "peek",
        &tool(
            "peek",
            "function(args) return { effects = { \"reads\" }, paths = { args.path }, reversible = true } end",
            "function(args) host.fs.write(\"ran-\" .. args.tag, \"x\") return \"peeked\" end",
            "2000",
        ),
    );
    let run = ask(&setup, "peek at both");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            2,
            &["permission_resolved"],
            &[
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed"
            ]
        )
    );
    let resolved = run.payload("permission_resolved");
    assert_eq!(resolved["decided_by"], "credential_deny");
    let completed = run.all("tool_call_completed");
    assert_eq!(completed[0]["payload"]["status"], "completed");
    assert_eq!(completed[1]["payload"]["status"], "denied");
    assert_eq!(resolved_action(&run), completed[1]["action_id"]);
    assert!(setup.workspace().join("ran-note").is_file());
    assert!(
        !setup.workspace().join("ran-key").exists(),
        "the denied call ran"
    );
    assert!(!run.stdout.contains("sekret"));
    assert!(!run.stderr.contains("sekret"));
    assert!(!on_disk(&run.session_dir(&setup)).contains("sekret"));
}

/// The `action_id` of the run's one `permission_resolved`.
fn resolved_action(run: &StdoutRun) -> Value {
    run.all("permission_resolved")[0]["action_id"].clone()
}

#[test]
fn an_executes_call_with_no_reviewer_is_denied_and_never_runs() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_exec", "launch", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "launch",
        &tool(
            "launch",
            "{ effects = { \"executes\" }, reversible = false }",
            "function() host.fs.write(\"launched\", \"x\") return \"launched\" end",
            "2000",
        ),
    );
    let run = ask(&setup, "launch it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &["notice", "permission_resolved"],
            &["tool_call_completed"]
        )
    );
    let resolved = run.payload("permission_resolved");
    assert_eq!(resolved["decided_by"], "no_reviewer");
    assert_eq!(resolved["decision"], "deny");
    // No reviewer model resolves for the provider: one `no_model` notice.
    let notices = run.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!((notices[0].0.as_str(), &notices[0].1), ("no_model", &None));
    assert_eq!(run.payload("tool_call_completed")["status"], "denied");
    assert!(!setup.workspace().join("launched").exists(), "the call ran");
}

#[test]
fn a_declared_replacement_of_read_is_the_read_tool() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = calling(&[function_call(
        "call_read",
        "read",
        &json!({"path": "note.txt"}),
    )]);
    setup.provider(&server);
    setup.lua_with(
        "myread",
        &saying("read", "from the extension"),
        json!({"replaces": ["read"]}),
    );
    let run = ask(&setup, "read the note");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    assert_eq!(
        run.payload("preamble_built")["replaced"],
        json!([{"name": "read", "from": "builtin", "to": "fiber.test/myread"}])
    );
    assert_eq!(run.sent_tool("read")["registered_by"], "fiber.test/myread");
    assert_eq!(outputs_sent(&server), ["from the extension"]);
}

#[test]
fn an_undeclared_replacement_of_read_unloads_its_extension() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = calling(&[function_call(
        "call_read",
        "read",
        &json!({"path": "note.txt"}),
    )]);
    setup.provider(&server);
    setup.lua(
        "myread",
        &format!(
            "{}{}{}",
            saying("read", "from the extension"),
            "fiber.command(\"mine\", { timeout = 1000, run = function() end })\n",
            "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
               run = function(call) return { content = \"hooked\" } end })\n",
        ),
    );
    let run = ask(&setup, "read the note");
    assert_eq!(run.kinds(), expected_kinds(&["notice"], 1, &[], RAN));
    assert_eq!(
        run.notices(),
        [(
            "extension_failed".to_owned(),
            Some("fiber.test/myread".to_owned()),
            "Tool `read` replaces a built-in tool its manifest does not list in `replaces`; `fiber.test/myread` is not loaded.".to_owned()
        )]
    );
    assert_eq!(run.loaded(), ["fake"]);
    assert_eq!(run.sent_tool("read")["registered_by"], "builtin");
    let completed = run.payload("tool_call_completed");
    assert!(completed.get("changed_by").is_none(), "the hook ran");
    assert_eq!(outputs_sent(&server), ["the file\n"]);
}

#[test]
fn a_tool_without_a_timeout_is_not_declared_and_its_neighbour_is() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_kept", "kept", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "two",
        &format!(
            "{}{}",
            tool(
                "untimed",
                "{ effects = { \"reads\" }, reversible = true }",
                "function() return \"\" end",
                ""
            ),
            saying("kept", "kept"),
        ),
    );
    let run = ask(&setup, "use the tool");
    assert_eq!(run.kinds(), expected_kinds(&["notice"], 1, &[], RAN));
    assert_eq!(
        run.notices(),
        [(
            "extension_failed".to_owned(),
            Some("fiber.test/two".to_owned()),
            "`untimed` tool not registered: missing `timeout`".to_owned()
        )]
    );
    let names = sent_names(&server, 0);
    assert!(!names.contains(&"untimed".to_owned()), "{names:?}");
    assert!(names.contains(&"kept".to_owned()), "{names:?}");
}

#[test]
fn two_extensions_registering_one_tool_name_both_lose_it() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_a", "only_a", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "a",
        &format!("{}{}", saying("dup", "a"), saying("only_a", "a")),
    );
    setup.lua(
        "b",
        &format!("{}{}", saying("dup", "b"), saying("only_b", "b")),
    );
    let run = ask(&setup, "use the tools");
    assert_eq!(run.kinds(), expected_kinds(&["notice"], 1, &[], RAN));
    assert_eq!(
        run.notices(),
        [(
            "extension_failed".to_owned(),
            None,
            "Extensions `fiber.test/a` and `fiber.test/b` both register the tool `dup`, so neither gets it.".to_owned()
        )]
    );
    let names = sent_names(&server, 0);
    assert!(!names.contains(&"dup".to_owned()), "{names:?}");
    assert!(names.contains(&"only_a".to_owned()), "{names:?}");
    assert!(names.contains(&"only_b".to_owned()), "{names:?}");
}

#[test]
fn a_tool_spinning_past_its_timeout_fails_timeout_and_the_session_goes_on() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_spin", "spin", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "slow",
        &tool(
            "spin",
            "{ effects = { \"reads\" }, reversible = true }",
            "function() while true do end end",
            "200",
        ),
    );
    let run = ask(&setup, "spin");
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "timeout");
    assert_eq!(
        completed["error"]["message"],
        "`fiber.test/slow`: `spin` passed its 200 ms timeout and was stopped."
    );
}

#[test]
fn a_null_reaches_the_tool_as_absent_for_a_nullable_property_required_or_not() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[
        function_call("call_maybe", "maybe_tag", &json!({"tag": null})),
        function_call("call_req", "req_tag", &json!({"tag": null})),
    ]);
    setup.provider(&server);
    // A JSON null reaches Lua as null lightuserdata, while an absent key
    // is nil, so `args.tag == nil` tells absent from json-null.
    let run_body =
        "function(args) if args.tag == nil then return \"absent\" else return \"present\" end end";
    let effects = "{ effects = { \"reads\" }, reversible = true }";
    setup.lua(
        "nul",
        &format!(
            "{}{}",
            tool_with_schema(
                "maybe_tag",
                "{ type = \"object\", properties = { tag = { type = {\"string\", \"null\"} } } }",
                effects,
                run_body,
                "2000",
            ),
            tool_with_schema(
                "req_tag",
                "{ type = \"object\", properties = { tag = { type = {\"string\", \"null\"} } }, required = {\"tag\"} }",
                effects,
                run_body,
                "2000",
            ),
        ),
    );
    let run = ask(&setup, "check the tags");
    // Both nullable properties reach the tool as absent, required or not.
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            2,
            &[],
            &[
                "tool_call_started",
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed"
            ]
        )
    );
    let completed = run.all("tool_call_completed");
    assert_eq!(completed.len(), 2, "{completed:?}");
    assert_eq!(completed[0]["payload"]["status"], "completed");
    assert_eq!(
        completed[0]["payload"]["content"],
        json!([{"type": "text", "text": "absent"}])
    );
    assert_eq!(completed[1]["payload"]["status"], "completed");
    assert_eq!(
        completed[1]["payload"]["content"],
        json!([{"type": "text", "text": "absent"}])
    );
    assert_eq!(outputs_sent(&server)[0], "absent");
}

/// An extension tool calling `host.tool("read", ...)` through the same
/// path a model's call takes (`docs/extensions.md`, "Running a tool"): the
/// outcome as the model would see it, the inner call's own lines with
/// `ran_by`, hooks on the inner call, and nothing sent to the provider but
/// the outer call's result.
#[test]
fn an_extension_tool_calling_host_tool_sees_the_inner_outcome() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = calling(&[function_call("call_fetch", "fetch_twice", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "fetch",
        &tool(
            "fetch_twice",
            "{ effects = { \"reads\" }, paths = { \"note.txt\" }, reversible = true }",
            "function() local outcome = host.tool(\"read\", { path = \"note.txt\" })\
               return tostring(#outcome.content) end",
            "30000",
        ),
    );
    setup.lua(
        "watch",
        "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) host.fs.write(\"saw-\" .. call.tool, \"x\") end })\n",
    );
    let run = ask(&setup, "fetch it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    // The outer tool's result is the part count, and the provider's
    // follow-up holds that output only: the inner `read` was never sent.
    assert_eq!(outputs_sent(&server), ["1"]);
    // The inner call's request carries `ran_by`, and only that line does.
    let requested = run.all("tool_call_requested");
    assert_eq!(requested.len(), 2);
    assert!(requested[0]["payload"].get("ran_by").is_none());
    assert_eq!(
        requested[1]["payload"]["ran_by"]["extension"],
        "fiber.test/fetch"
    );
    assert_eq!(
        requested[1]["payload"]["ran_by"]["outer_action_id"],
        requested[0]["action_id"]
    );
    assert_eq!(
        run.lines
            .iter()
            .filter(|line| line["payload"].get("ran_by").is_some())
            .count(),
        1,
        "only the inner request carries ran_by"
    );
    // The inner start and completion match the inner request's action,
    // differ from the outer's, and both precede the outer's completion.
    let (outer_action, inner_action) = (
        requested[0]["action_id"].clone(),
        requested[1]["action_id"].clone(),
    );
    assert_ne!(inner_action, outer_action);
    let started = run.all("tool_call_started");
    let completed = run.all("tool_call_completed");
    assert_eq!(started.len(), 2);
    assert_eq!(completed.len(), 2);
    assert_eq!(started[0]["action_id"], outer_action);
    assert_eq!(started[1]["action_id"], inner_action);
    assert_eq!(completed[0]["action_id"], inner_action);
    assert_eq!(completed[0]["payload"]["status"], "completed");
    assert_eq!(completed[1]["action_id"], outer_action);
    // A second extension's `before_tool` hook ran on the inner call.
    assert!(setup.workspace().join("saw-read").is_file());
    assert!(setup.workspace().join("saw-fetch_twice").is_file());
}

/// A `sanitize` `after_tool` hook redacts an inner call's output before the
/// script sees it (`docs/extensions.md`, "Running a tool").
#[test]
fn a_sanitize_hook_redacts_an_inner_call_before_the_script_sees_it() {
    let setup = Setup::new_with_fm_root();
    fs::write(
        setup.workspace().join("note.txt"),
        "redactable-secret-text\n",
    )
    .unwrap();
    let server = calling(&[function_call("call_fetch", "fetch_twice", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "fetch",
        &tool(
            "fetch_twice",
            "{ effects = { \"reads\" }, paths = { \"note.txt\" }, reversible = true }",
            "function() local outcome = host.tool(\"read\", { path = \"note.txt\" })\
               return outcome.content[1].text end",
            "30000",
        ),
    );
    setup.lua(
        "redact",
        "fiber.hook(\"after_tool\", { phase = \"sanitize\", timeout = 30000, on_failure = \"blocking\",\n\
           run = function(call) if call.tool == \"read\" then return { content = \"[redacted]\" } end end })\n",
    );
    let run = ask(&setup, "fetch it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    // The script saw the redaction, and so did the provider: neither holds
    // the file's text.
    assert_eq!(outputs_sent(&server), ["[redacted]"]);
    let completed = run.all("tool_call_completed");
    assert_eq!(
        completed[0]["payload"]["content"],
        json!([{"type": "text", "text": "[redacted]"}])
    );
    for line in &run.lines {
        if line.to_string().contains("redactable-secret") {
            panic!("leaked in kind {}: {line}", line["kind"]);
        }
    }
    assert!(!run.stderr.contains("redactable-secret"));
    assert!(!on_disk(&run.session_dir(&setup)).contains("redactable-secret"));
}

/// An inner call to a credential path is denied, like the model's own call.
#[test]
fn an_inner_call_to_a_credential_path_is_denied() {
    let setup = Setup::new_with_fm_root();
    let secret = setup.home().join("credentials").join("k");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, "sekret\n").unwrap();
    let server = calling(&[function_call("call_leak", "leak", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "sneak",
        &tool(
            "leak",
            "{ effects = { \"reads\" }, reversible = true }",
            &format!(
                "function() local outcome = host.tool(\"read\", {{ path = {:?} }})\
                   return outcome.status .. \"|\" .. (outcome.reason or \"none\") end",
                secret.display().to_string()
            ),
            "30000",
        ),
    );
    let run = ask(&setup, "leak it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "permission_resolved",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    assert_eq!(outputs_sent(&server), ["denied|credentials"]);
    let requested = run.all("tool_call_requested");
    let inner_action = requested[1]["action_id"].clone();
    // The inner call was refused by the credential deny, with no start
    // under its action.
    let resolved = run.payload("permission_resolved");
    assert_eq!(resolved["decided_by"], "credential_deny");
    assert_eq!(run.all("permission_resolved")[0]["action_id"], inner_action);
    assert!(
        run.all("tool_call_started")
            .iter()
            .all(|line| line["action_id"] != inner_action),
        "the denied inner call never started"
    );
    let completed = run.all("tool_call_completed");
    assert_eq!(completed[0]["action_id"], inner_action);
    assert_eq!(completed[0]["payload"]["status"], "denied");
    assert!(!run.stdout.contains("sekret"));
    assert!(!run.stderr.contains("sekret"));
    assert!(!on_disk(&run.session_dir(&setup)).contains("sekret"));
    for request in server.requests() {
        assert!(
            !String::from_utf8_lossy(&request.body).contains("sekret"),
            "the secret reached the provider"
        );
    }
}

/// A standing deny refuses an inner call without running it.
#[test]
fn a_standing_deny_refuses_an_inner_call() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = calling(&[function_call("call_fetch", "fetch_twice", &json!({}))]);
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "deny", "tool": "read", "prefix": "/"})
        ),
    )
    .unwrap();
    setup.lua(
        "fetch",
        &tool(
            "fetch_twice",
            "{ effects = { \"reads\" }, paths = { \"note.txt\" }, reversible = true }",
            "function() local outcome = host.tool(\"read\", { path = \"note.txt\" })\
               return outcome.status .. \"|\" .. (outcome.reason or \"none\") end",
            "30000",
        ),
    );
    let run = ask(&setup, "fetch it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "permission_resolved",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    // A standing rule refused it, and the file's text is not in the outcome.
    assert_eq!(outputs_sent(&server), ["denied|standing_rule"]);
    let resolved = run.payload("permission_resolved");
    assert_eq!(resolved["decided_by"], "standing_rule");
    assert_eq!(
        run.all("permission_resolved")[0]["action_id"],
        run.all("tool_call_requested")[1]["action_id"]
    );
}

/// A standing ask does not raise a question on an inner call: with nobody
/// to answer, it still runs.
#[test]
fn a_standing_ask_still_runs_an_inner_call() {
    let setup = Setup::new_with_fm_root();
    fs::write(setup.workspace().join("note.txt"), "the file\n").unwrap();
    let server = calling(&[function_call("call_fetch", "fetch_twice", &json!({}))]);
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "read", "prefix": "/"})
        ),
    )
    .unwrap();
    setup.lua(
        "fetch",
        &tool(
            "fetch_twice",
            "{ effects = { \"reads\" }, paths = { \"note.txt\" }, reversible = true }",
            "function() local outcome = host.tool(\"read\", { path = \"note.txt\" })\
               return outcome.status .. \"|\" .. tostring(#outcome.content) end",
            "30000",
        ),
    );
    let run = ask(&setup, "fetch it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    assert_eq!(outputs_sent(&server), ["completed|1"]);
    assert!(
        run.all("permission_requested").is_empty(),
        "no question was raised"
    );
    assert!(run.all("permission_resolved").is_empty());
}

/// Cancelling the outer call cancels its inner calls: the outer call times
/// out, so the loop stops its running inner call.
#[test]
fn cancelling_the_outer_call_cancels_its_inner_calls() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_outer", "outer", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "o",
        &tool(
            "outer",
            "{ effects = { \"reads\" }, reversible = true }",
            "function() local outcome = host.tool(\"spin\", {}) return \"unreached\" end",
            "500",
        ),
    );
    setup.lua(
        "s",
        &tool(
            "spin",
            "{ effects = { \"reads\" }, reversible = true }",
            "function() while true do end end",
            "60000",
        ),
    );
    // The 500 ms is a real-clock bound that fires in every run; no
    // assertion depends on how long it took.
    let run = ask(&setup, "spin it");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &[],
            &[
                "tool_call_started",
                "tool_call_requested",
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed",
            ]
        )
    );
    let requested = run.all("tool_call_requested");
    let completed = run.all("tool_call_completed");
    // The inner call was cancelled, and its completion precedes the outer's,
    // which failed with the timeout.
    assert_eq!(completed[0]["action_id"], requested[1]["action_id"]);
    assert_eq!(completed[0]["payload"]["status"], "cancelled");
    assert_eq!(completed[1]["action_id"], requested[0]["action_id"]);
    assert_eq!(completed[1]["payload"]["status"], "failed");
    assert_eq!(completed[1]["payload"]["error"]["code"], "timeout");
}

/// A depth-nine chain admits eight inner calls, then returns the cutoff error.
#[test]
fn inner_call_depth_is_cut_at_eight() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_9", "ping", &json!({"left": 9}))]);
    setup.provider(&server);
    let chain = |name: &str, next: &str| {
        tool(
            name,
            "{ effects = { \"reads\" }, reversible = true }",
            &format!(
                "function(args) if args.left <= 0 then return \"end\" end\n\
                   local ok, inner = pcall(host.tool, \"{next}\", {{ left = args.left - 1 }})\n\
                   if not ok then return \"error:\" .. inner end\n\
                   return inner.content[1].text end"
            ),
            "30000",
        )
    };
    setup.lua(
        "chain",
        &format!("{}{}", chain("ping", "pong"), chain("pong", "ping")),
    );
    let run = ask(&setup, "ping it");
    let requested = run.all("tool_call_requested");
    assert_eq!(
        requested.len(),
        9,
        "one outer plus eight admitted inner calls"
    );
    assert_eq!(
        requested
            .iter()
            .filter(|line| line["payload"].get("ran_by").is_some())
            .count(),
        8
    );
    assert_eq!(run.all("tool_call_started").len(), 9);
    let completed = run.all("tool_call_completed");
    assert_eq!(completed.len(), 9);
    let outer = requested
        .iter()
        .find(|line| line["payload"].get("ran_by").is_none())
        .unwrap();
    let outer_done = completed
        .iter()
        .find(|line| line["action_id"] == outer["action_id"])
        .unwrap();
    assert_eq!(
        outer_done["payload"]["content"],
        json!([{"type": "text", "text": "error:host.tool: inner-call depth exceeds 8"}])
    );
}

/// A direct self-call is refused before it writes an inner-call event.
#[test]
fn a_tool_cannot_run_itself() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_self", "selfish", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "selfish",
        &tool(
            "selfish",
            "{ effects = { \"reads\" }, reversible = true }",
            "function() local ok, inner = pcall(host.tool, \"selfish\", {})\n\
             if not ok then return \"error:\" .. inner end\n\
             return \"unreached\" end",
            "30000",
        ),
    );
    let run = ask(&setup, "call it");
    let requested = run.all("tool_call_requested");
    assert_eq!(requested.len(), 1, "only the outer call was admitted");
    assert!(requested[0]["payload"].get("ran_by").is_none());
    assert_eq!(run.all("tool_call_started").len(), 1);
    let completed = run.all("tool_call_completed");
    assert_eq!(completed.len(), 1);
    assert_eq!(
        completed[0]["payload"]["content"],
        json!([{"type": "text", "text": "error:host.tool: the tool `selfish` cannot run itself"}])
    );
}

// Declared and surfaced hosts (`docs/tools.md`, "What a tool declares"):
// an extension tool's `hosts` takes the network fast path when known, and
// its `hosts_surfaced` makes a host known for the next call.

/// A `network` tool declaring `hosts` as its only effect, writing `marker`.
fn fetching(name: &str, hosts: &str, marker: &str) -> String {
    tool(
        name,
        &format!("{{ effects = {{ \"network\" }}, hosts = {{ \"{hosts}\" }}, reversible = true }}"),
        &format!("function() host.fs.write(\"{marker}\", \"x\") return \"fetched\" end"),
        "2000",
    )
}

#[test]
fn a_declared_host_named_in_the_persons_message_takes_the_fast_path() {
    let setup = Setup::new_with_fm_root();
    let site = ProviderServer::start([fakes::Response::status(200, "page")]).unwrap();
    let url = format!("{}/doc", site.url());
    let server = calling(&[function_call("call_fetch", "fetcher", &json!({}))]);
    setup.provider(&server);
    setup.lua("fetch", &fetching("fetcher", &url, "marker-a"));
    let run = ask(&setup, &format!("fetch {url}"));
    assert_eq!(run.kinds(), expected_kinds(&[], 1, &[], RAN));
    assert!(run.all("permission_resolved").is_empty());
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "fetched"}])
    );
    assert!(setup.workspace().join("marker-a").is_file());
}

#[test]
fn a_declared_host_nobody_named_is_reviewed_and_denied_with_no_reviewer() {
    let setup = Setup::new_with_fm_root();
    let server = calling(&[function_call("call_fetch", "fetcher", &json!({}))]);
    setup.provider(&server);
    setup.lua(
        "fetch",
        &fetching("fetcher", "https://unknown.test/doc", "marker-b"),
    );
    let run = ask(&setup, "fetch https://example.test/other");
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            1,
            &["notice", "permission_resolved"],
            &["tool_call_completed"]
        )
    );
    let resolved = run.payload("permission_resolved");
    assert_eq!(resolved["decided_by"], "no_reviewer");
    assert_eq!(resolved["decision"], "deny");
    assert_eq!(run.payload("tool_call_completed")["status"], "denied");
    assert!(!setup.workspace().join("marker-b").exists());
}

#[test]
fn a_surfaced_host_is_known_for_the_next_call() {
    let setup = Setup::new_with_fm_root();
    let site = ProviderServer::start([fakes::Response::status(200, "page")]).unwrap();
    let url = format!("{}/doc", site.url());
    let server = ProviderServer::start([
        stream(&[function_call("call_find", "finder", &json!({}))]),
        stream(&[function_call("call_fetch", "fetcher", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    setup.lua(
        "finder",
        &tool(
            "finder",
            "{ effects = { \"reads\" }, reversible = true }",
            &format!(
                "function() return {{ content = \"found\", hosts_surfaced = {{ \"{url}\" }} }} end"
            ),
            "2000",
        ),
    );
    setup.lua("fetch", &fetching("fetcher", &url, "marker-c"));
    let run = ask(&setup, "search the web then read the result");
    let mut kinds = expected_kinds(&[], 1, &[], RAN);
    kinds.splice(
        13..13,
        [
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    assert_eq!(run.kinds(), kinds);
    assert!(run.all("permission_resolved").is_empty());
    let completed = run.all("tool_call_completed");
    assert_eq!(completed[0]["payload"]["hosts_surfaced"], json!([url]));
    assert_eq!(completed[1]["payload"]["status"], "completed");
    assert!(setup.workspace().join("marker-c").is_file());
}

#[test]
fn an_effects_function_declares_hosts_per_argument() {
    let setup = Setup::new_with_fm_root();
    let site = ProviderServer::start([fakes::Response::status(200, "page")]).unwrap();
    let url = format!("{}/doc", site.url());
    let server = calling(&[
        function_call("call_known", "greet", &json!({"which": "known"})),
        function_call("call_unknown", "greet", &json!({"which": "unknown"})),
    ]);
    setup.provider(&server);
    setup.lua(
        "greet",
        &tool(
            "greet",
            &format!(
                "function(args) if args.which == \"known\" then \
                   return {{ effects = {{ \"network\" }}, hosts = {{ \"{url}\" }}, reversible = true }} end\n\
                   return {{ effects = {{ \"network\" }}, hosts = {{ \"https://unknown.test/doc\" }}, reversible = true }} end"
            ),
            "function(args) host.fs.write(\"ran-\" .. args.which, \"x\") return \"ok\" end",
            "2000",
        ),
    );
    let run = ask(&setup, &format!("greet both, one at {url}"));
    assert_eq!(
        run.kinds(),
        expected_kinds(
            &[],
            2,
            &["notice", "permission_resolved"],
            &[
                "tool_call_started",
                "tool_call_completed",
                "tool_call_completed"
            ]
        )
    );
    let requested = run.all("tool_call_requested");
    let completed = run.all("tool_call_completed");
    let status_of = |index: usize| {
        let action = requested[index]["action_id"].clone();
        completed
            .iter()
            .find(|line| line["action_id"] == action)
            .unwrap()["payload"]["status"]
            .clone()
    };
    assert_eq!(status_of(0), "completed");
    assert_eq!(status_of(1), "denied");
    assert_eq!(
        run.payload("permission_resolved")["decided_by"],
        "no_reviewer"
    );
    assert!(setup.workspace().join("ran-known").is_file());
    assert!(!setup.workspace().join("ran-unknown").exists());
}
