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
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc;
use std::thread;

use extension_harness::Setup;
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// One finished `fiber ask`: its exit code, its stdout lines parsed (less
/// `session_status`, which an observer thread writes), its raw stdout and
/// its stderr.
struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stdout: String,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|line| !line.contains(r#""kind":"session_status""#))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stdout,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect()
    }

    fn all(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }

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
fn ask(setup: &Setup, prompt: &str) -> Run {
    let mut command = setup.fiber(&["ask", prompt]);
    command.current_dir(setup.workspace());
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(setup.deadline.left()) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, "`fiber ask` to exit"),
    };
    assert!(
        !support::group_alive(setup.deadline, group),
        "`fiber` left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup());
    let run = Run::from(output);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    run
}

/// An `openai-responses` stream of `events`, then a completed reply.
fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// An `openai-responses` stream answering `Hello.`.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
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

/// The tool outputs the second request carries, in order.
fn outputs_sent(server: &ProviderServer) -> Vec<String> {
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    second["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .map(|item| item["output"].as_str().unwrap().to_owned())
        .collect()
}

/// Every file's bytes under `dir`, as text, joined.
fn on_disk(dir: &Path) -> String {
    let mut all = String::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.is_file() {
                all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    all
}

/// The complete event kinds of a run whose model makes one step of calls,
/// then answers: `loading` follows `extensions_loaded`, `decided` follows
/// the first step's `assistant_message_completed`, and `ran` follows that.
fn kinds(
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
    let timeout = if timeout.is_empty() {
        String::new()
    } else {
        format!(" timeout = {timeout},")
    };
    format!(
        "fiber.tool(\"{name}\", {{ description = \"The {name} tool.\", input_schema = {{ type = \"object\" }},{timeout} effects = {effects}, run = {run} }})\n"
    )
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

#[test]
fn an_extension_tool_is_declared_in_full_runs_and_its_result_passes_the_hooks() {
    let setup = Setup::new();
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
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
           run = function(call) return { content = call.content .. \"|t\" } end })\n",
    );
    let run = ask(&setup, "count the notes");
    assert_eq!(run.kinds(), kinds(&[], 1, &[], RAN));
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
    let setup = Setup::new();
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
        kinds(
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
fn resolved_action(run: &Run) -> Value {
    run.all("permission_resolved")[0]["action_id"].clone()
}

#[test]
fn an_executes_call_with_no_reviewer_is_denied_and_never_runs() {
    let setup = Setup::new();
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
        kinds(
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
    let setup = Setup::new();
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
    assert_eq!(run.kinds(), kinds(&[], 1, &[], RAN));
    assert_eq!(
        run.payload("preamble_built")["replaced"],
        json!([{"name": "read", "from": "builtin", "to": "fiber.test/myread"}])
    );
    assert_eq!(run.sent_tool("read")["registered_by"], "fiber.test/myread");
    assert_eq!(outputs_sent(&server), ["from the extension"]);
}

#[test]
fn an_undeclared_replacement_of_read_unloads_its_extension() {
    let setup = Setup::new();
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
    assert_eq!(run.kinds(), kinds(&["notice"], 1, &[], RAN));
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
    let setup = Setup::new();
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
    assert_eq!(run.kinds(), kinds(&["notice"], 1, &[], RAN));
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
    let setup = Setup::new();
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
    assert_eq!(run.kinds(), kinds(&["notice"], 1, &[], RAN));
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
    let setup = Setup::new();
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
    assert_eq!(run.kinds(), kinds(&[], 1, &[], RAN));
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "timeout");
    assert_eq!(
        completed["error"]["message"],
        "`fiber.test/slow`: `spin` passed its 200 ms timeout and was stopped."
    );
}
