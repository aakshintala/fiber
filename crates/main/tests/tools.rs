//! Binary-level tests of the built-in tools `fiber ask` registers
//! (`docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider whose base
//! URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// The request's tool order: the loop keys tools by name, so this is name
/// order, and `main` pushes them in the same order.
const TOOL_NAMES: [&str; 4] = ["edit", "read", "shell", "write"];

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fa");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
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
        self.provider_on(server, "openai-responses");
    }

    /// [`Setup::provider`] on `protocol`.
    fn provider_on(&self, server: &ProviderServer, protocol: &str) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": protocol, "base_url": format!("{}/v1", server.url())}]
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

    /// Runs `fiber` in its own process group, waits for it under
    /// [`DEADLINE`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    /// A watchdog beside it kills that group if this process dies first.
    fn run(&self, args: &[&str]) -> Run {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
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
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                assert!(
                    !group_alive(group),
                    "`fiber` left a process in its group behind"
                );
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(
            !group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Run::from(output)
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. The watchdog's stdin is a pipe only this process holds: a newline
/// means the child is reaped, and EOF means this process died, so the
/// watchdog kills the group. The watchdog is started immediately after the
/// child; a kill in the gap between the two spawns can still orphan it.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    stdout: String,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            stdout,
            lines,
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

    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
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
            .join(self.session_id())
    }
}

/// `preamble_built` for `reason`, with the four built-in tools and nothing
/// replaced. A resume builds the same set again; it does not replace one.
fn assert_preamble(run: &Run, reason: &str) {
    let preamble = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(preamble["payload"]["reason"], reason);
    assert!(preamble["payload"].get("replaced").is_none());
    let sent: Vec<_> = preamble["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap(),
                tool["registered_by"].as_str().unwrap(),
            )
        })
        .collect();
    let expected: Vec<_> = TOOL_NAMES.iter().map(|name| (*name, "builtin")).collect();
    assert_eq!(sent, expected);
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

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

fn tool_names(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The event kinds of a turn whose first reply calls one tool and whose
/// second is [`hello`]. A reads-only workspace call is a fast path, so no
/// `permission_` line is written (`docs/permissions.md`, "Fast paths").
fn read_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
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
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(["assistant_message_delta"; 2]);
    kinds.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

#[test]
fn a_new_session_offers_the_four_builtin_tools_and_a_read_completes() {
    let setup = Setup::new();
    let note = "alpha line\n";
    fs::write(setup.workspace().join("note.txt"), note).unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "read the note"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], note);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let output = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["output"], note);
}

#[test]
fn a_resumed_session_offers_the_same_tools_in_the_same_order() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.run(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    let second = setup.run(&["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let first_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first_body["tools"], second_body["tools"]);
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
    let turn = [
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
    // A resume writes no `session_started` and no `opening_message`: the log
    // already holds both.
    assert_eq!(
        first.kinds(),
        [
            &[
                "session_started",
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
                "turn_started",
            ][..],
            &turn,
        ]
        .concat()
    );
    assert_eq!(
        second.kinds(),
        [
            &[
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "turn_started"
            ][..],
            &turn
        ]
        .concat()
    );
    assert_preamble(&first, "start");
    assert_preamble(&second, "resume");
}

/// The credential file's text. It is not a path fragment and not a word the
/// denial sentence uses, so a hit in an output can only be the file's bytes.
const MARKER: &str = "xylophone-quorum-91";

/// The event kinds of a turn whose first reply makes `calls` denials and
/// whose second is [`hello`]. Every denial is decided before any completion.
fn denied_kinds(calls: usize) -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(std::iter::repeat_n("tool_call_requested", calls));
    kinds.extend(["usage_recorded", "assistant_message_completed"]);
    kinds.extend(std::iter::repeat_n("permission_resolved", calls));
    kinds.extend(std::iter::repeat_n("tool_call_completed", calls));
    kinds.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

fn holds_marker(bytes: &[u8], marker: &str) -> bool {
    let marker = marker.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

fn assert_no_marker(label: &str, bytes: &[u8]) {
    assert!(
        !holds_marker(bytes, MARKER),
        "{label} holds the credential file's text"
    );
}

/// Every file under `dir`, symlink targets included. `events.jsonl` is one
/// of them; `artifacts/` is entered even when a call wrote nothing there.
fn assert_session_has_no_marker(dir: &Path) {
    assert!(dir.join("events.jsonl").is_file());
    assert!(dir.join("artifacts").is_dir());
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            continue;
        }
        assert_no_marker(&path.display().to_string(), &fs::read(&path).unwrap());
    }
}

#[test]
fn a_credential_path_is_denied_for_a_read_and_a_shell_cat_under_every_spelling() {
    let setup = Setup::new();
    let secret = setup.home().join("credentials").join("secret.txt");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, format!("{MARKER}\n")).unwrap();
    let real = fs::canonicalize(&secret).unwrap();
    let absolute = real.to_str().unwrap().to_owned();
    std::os::unix::fs::symlink(&real, setup.workspace().join("via")).unwrap();
    // Through directories that exist: `..` over a missing directory would
    // end `tool_error` and prove nothing about the deny.
    let dotted = "../h/credentials/secret.txt";
    assert_eq!(
        fs::canonicalize(setup.workspace().join("via")).unwrap(),
        real
    );
    assert_eq!(
        fs::canonicalize(setup.workspace().join(dotted)).unwrap(),
        real
    );

    let spellings = [absolute.as_str(), "via", dotted];
    for spelling in spellings {
        assert!(!spelling.contains(MARKER), "{spelling}");
        assert!(!spelling.contains(char::is_whitespace), "{spelling}");
    }
    let mut events = Vec::new();
    for (index, path) in spellings.iter().enumerate() {
        events.push(function_call(
            &format!("read_{index}"),
            "read",
            &json!({"path": path}),
        ));
    }
    for (index, path) in spellings.iter().enumerate() {
        events.push(function_call(
            &format!("shell_{index}"),
            "shell",
            &json!({"command": format!("cat {path}")}),
        ));
    }
    let script = serde_json::to_string(&events).unwrap();
    assert!(
        !script.contains(MARKER),
        "the scripted arguments contain the credential file's text"
    );
    assert!(holds_marker(&fs::read(&secret).unwrap(), MARKER));

    let server = ProviderServer::start([stream(&events), hello()]).unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "show the secret"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), denied_kinds(spellings.len() * 2));
    let requested: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_requested")
        .collect();
    let resolved: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    let completed: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(requested.len(), spellings.len() * 2);
    for (request, (decision, done)) in requested.iter().zip(resolved.iter().zip(&completed)) {
        let action = &request["action_id"];
        assert_eq!(&decision["action_id"], action);
        assert_eq!(&done["action_id"], action);
        assert_eq!(decision["payload"]["decided_by"], "credential_deny");
        assert_eq!(done["payload"]["status"], "denied");
        assert_eq!(done["payload"]["reason"], "credentials");
        assert!(
            !run.lines
                .iter()
                .any(|line| line["kind"] == "tool_call_started" && &line["action_id"] == action)
        );
    }
    assert_no_marker("stdout", run.stdout.as_bytes());
    assert_no_marker("stderr", run.stderr.as_bytes());
    for (index, request) in server.requests().iter().enumerate() {
        assert_no_marker(&format!("request {index}"), &request.body);
    }
    assert_session_has_no_marker(&run.session_dir(&setup));
}

/// A 1x1 PNG, 69 bytes: within every cap, so the image child stores it byte
/// for byte.
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// [`PIXEL`] as base64.
const PIXEL_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// An `anthropic-messages` stream of `events`, then its end.
fn anthropic(events: &[Value]) -> Response {
    let mut all = vec![json!({"type": "message_start", "message": {"id": "msg_1"}})];
    all.extend(events.iter().cloned());
    all.push(
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
        "usage": {"input_tokens": 10, "output_tokens": 3}}),
    );
    all.push(json!({"type": "message_stop"}));
    Response::stream(
        all.iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

/// An `anthropic-messages` reply that calls `read` on `path`.
fn anthropic_read(path: &str) -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": json!({"path": path}).to_string()}}),
        json!({"type": "content_block_stop", "index": 0}),
    ])
}

/// An `anthropic-messages` reply of `Hello.`.
fn anthropic_hello() -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello."}}),
        json!({"type": "content_block_stop", "index": 0}),
    ])
}

/// The `tool_result` block of a request body's messages.
fn tool_result(body: &[u8]) -> Value {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_result")
        .unwrap()
}

/// [`read_kinds`] on the Anthropic stream: the call's arguments arrive as a
/// delta, and the second reply is one text delta.
fn anthropic_read_kinds() -> Vec<&'static str> {
    let mut kinds = read_kinds();
    kinds.retain(|kind| *kind != "assistant_message_delta");
    kinds.insert(
        kinds
            .iter()
            .position(|kind| *kind == "tool_call_requested")
            .unwrap(),
        "tool_call_arguments_delta",
    );
    let text = kinds
        .iter()
        .position(|kind| *kind == "text_completed")
        .unwrap();
    kinds.insert(text, "assistant_message_delta");
    kinds
}

fn completed_line(run: &Run) -> &Value {
    run.lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap()
}

#[test]
fn an_image_is_stored_logged_by_path_and_sent_inside_the_tool_result_on_every_request() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start([
        anthropic_read("pic.png"),
        anthropic_hello(),
        anthropic_hello(),
    ])
    .unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "look at the picture"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), anthropic_read_kinds());
    let completed = completed_line(&run);
    assert_eq!(completed["payload"]["status"], "completed");
    let content = completed["payload"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["text"], "Image: 1x1 image/png.\n");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mime_type"], "image/png");
    assert_eq!(
        (content[1]["width"].clone(), content[1]["height"].clone()),
        (json!(1), json!(1))
    );
    let path = content[1]["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    let session = run.session_dir(&setup);
    // The artifact is the processed file: here, the input byte for byte.
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    // The log names the file and never holds its bytes.
    let log = fs::read(session.join("events.jsonl")).unwrap();
    assert!(
        !holds_marker(&log, PIXEL_BASE64),
        "the log holds the image's base64"
    );
    assert!(
        !log.windows(PIXEL.len()).any(|window| window == PIXEL),
        "the log holds the image's bytes"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_result(&requests[1].body)["content"],
        json!([
            {"type": "text", "text": "Image: 1x1 image/png.\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": PIXEL_BASE64}},
        ])
    );

    let id = run.session_id().to_owned();
    let resumed = setup.run(&["ask", "--resume", &id, "again"]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        tool_result(&requests[2].body),
        tool_result(&requests[1].body),
        "a resume sends the same bytes"
    );
}

/// A PNG that holds a signature, an IHDR of `width` x `height` and the start
/// of an IDAT, and no pixels.
fn header_only_png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    }
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x00]));
    out
}

#[test]
fn an_image_over_50_megapixels_fails_unsupported_file_with_the_pixel_count() {
    let setup = Setup::new();
    fs::write(
        setup.workspace().join("big.png"),
        header_only_png(8000, 7000),
    )
    .unwrap();
    let server = ProviderServer::start([anthropic_read("big.png"), anthropic_hello()]).unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "look at the big picture"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), anthropic_read_kinds());
    let completed = completed_line(&run);
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "unsupported_file");
    let message = completed["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("8000x7000 is 56000000 pixels; the limit is 50000000"),
        "{message}"
    );
    let images = fs::read_dir(run.session_dir(&setup).join("artifacts"))
        .unwrap()
        .count();
    assert_eq!(images, 0, "a refused image stores nothing");
    let result = tool_result(&server.requests()[1].body);
    assert_eq!(result["is_error"], true);
    assert!(result["content"].is_string());
}

/// Configuration with a handoff trigger of five tokens: any reply's prompt
/// passes it.
fn tiny_trigger(setup: &Setup) {
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "handoff": {"tokens": 5}}),
    );
}

/// A turn whose first reply reads a file, then hands off on the five-token
/// trigger and answers from the new context, to the end of the process.
const HANDOFF_TURN: &[&str] = &[
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "handoff_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
    "opening_message",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The lines of `run` of `kind`.
fn of_kind<'a>(run: &'a Run, kind: &str) -> Vec<&'a Value> {
    run.lines
        .iter()
        .filter(|line| line["kind"] == kind)
        .collect()
}

#[test]
fn handoff_configuration_reaches_a_new_session() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    tiny_trigger(&setup);

    let run = setup.run(&["ask", "read the note"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            &[
                "session_started",
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
            ][..],
            HANDOFF_TURN,
        ]
        .concat()
    );
    assert_eq!(
        of_kind(&run, "preamble_built")[0]["payload"]["trigger_at"],
        5
    );
    assert_eq!(of_kind(&run, "handoff_started").len(), 1);
    assert_eq!(
        of_kind(&run, "handoff_completed")[0]["payload"]["outcome"],
        "completed"
    );
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn handoff_configuration_reaches_a_resumed_session() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    tiny_trigger(&setup);
    let first = setup.run(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // One reply, and no second step: nothing handed off yet.
    assert_eq!(
        first.kinds(),
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
    let id = first.session_id().to_owned();

    let second = setup.run(&["ask", "--resume", &id, "read the note"]);

    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(
        second.kinds(),
        [
            &["fiber_started", "extensions_loaded", "preamble_built"][..],
            HANDOFF_TURN,
        ]
        .concat()
    );
    assert_eq!(
        of_kind(&second, "preamble_built")[0]["payload"]["trigger_at"],
        5
    );
    assert_eq!(of_kind(&second, "handoff_started").len(), 1);
    assert_eq!(server.requests().len(), 4);
}
