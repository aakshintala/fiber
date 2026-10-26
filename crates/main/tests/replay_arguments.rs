//! Binary-level replay of non-object tool arguments (#3017): the built
//! `fiber` runs against the shared fake provider server, which streams a
//! tool call whose input text does not parse. The loop rejects the call
//! itself, the next request replays the call with `input: {}` on the
//! object-input protocols, and its failed result quotes the raw text, so
//! the turn completes and a resumed session replays the same bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{Setup, StdoutRun, spawn_watched};

/// The malformed input text both binary tests stream: an object that never
/// closes, so the decoder keeps the raw text.
const MALFORMED: &str = "{\"command\":\"ls\"]}";

impl Setup {
    /// Installs a provider `fake` with model `m` on `protocol` at the fake
    /// server, and makes `fake/m` the configured model.
    fn provider_on(&self, server: &ProviderServer, protocol: &str) {
        let source = self.root.path().join("src");
        support::write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        let model = json!({"id": "m", "protocol": protocol,
            "base_url": format!("{}/v1", server.url()), "context_window": 100000,
            "input": ["text", "image"]});
        support::write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [model]
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
        support::write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Runs `fiber` in its own process group, waits for it under the test's
    /// deadline, and asserts nothing it started is left in the group.
    fn run(&self, args: &[&str]) -> StdoutRun {
        self.run_with_env(args, &[])
    }

    /// As [`Setup::run`], with `env` added to the child's environment.
    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> StdoutRun {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = support::KillGroup(group);
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
        StdoutRun::from(output)
    }
}

impl StdoutRun {
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

/// An `anthropic-messages` tool call streaming the malformed text.
fn anthropic_malformed() -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_1", "name": "shell", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": MALFORMED}}),
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

/// The event kinds of a turn whose tool call streams malformed input and
/// fails before it starts: the argument delta arrives, but no
/// `tool_call_started` ever does.
fn malformed_kinds() -> Vec<&'static str> {
    let mut kinds = support::read_kinds();
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
    kinds.retain(|kind| *kind != "tool_call_started");
    kinds
}

/// The event kinds of a resumed turn answering in text: the reply streams
/// `Hello.` as one delta.
const RESUME_HELLO_KINDS: [&str; 12] = [
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The durable lines one run wrote: every line carrying a sequence number
/// for this session, which is what the session log holds.
fn durable_lines(run: &StdoutRun) -> Vec<Value> {
    run.lines
        .iter()
        .filter(|line| line.get("seq").is_some() && line["session_id"] == run.session_id())
        .cloned()
        .collect()
}

/// Stdout filtered to this session's durable lines is the log, byte for
/// byte, setting aside the ephemeral status lines the shared support
/// drops when it reads stdout.
fn assert_stdout_matches_log(run: &StdoutRun, setup: &Setup) {
    let log = fs::read_to_string(run.session_dir(setup).join("events.jsonl")).unwrap();
    let logged: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(durable_lines(run), logged);
}

/// The resumed log holds the first run's lines then the resume's, with one
/// `session_started` and contiguous sequence numbers.
fn assert_resume_matches_log(first: &StdoutRun, resumed: &StdoutRun, setup: &Setup) {
    let log = fs::read_to_string(first.session_dir(setup).join("events.jsonl")).unwrap();
    let logged: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut want = durable_lines(first);
    want.extend(durable_lines(resumed));
    assert_eq!(want, logged);
    support::assert_one_continued_log(&logged, 2);
}

/// The `tool_use` block of a Messages request body.
fn messages_tool_use(body: &Value) -> Value {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_use")
        .unwrap_or_else(|| panic!("no tool_use in {body}"))
        .clone()
}

/// The `tool_result` block of a Messages request body.
fn messages_tool_result(body: &Value) -> Value {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_result")
        .unwrap_or_else(|| panic!("no tool_result in {body}"))
        .clone()
}

#[test]
fn malformed_tool_arguments_replay_as_an_empty_object_with_the_raw_text_quoted() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([anthropic_malformed(), anthropic_hello(), anthropic_hello()])
            .unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "call it"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), malformed_kinds());
    // The log records the arguments as the model sent them.
    let requested = run.first("tool_call_requested");
    assert_eq!(requested["payload"]["arguments"], json!(MALFORMED));
    // The call fails before it starts, quoting the raw text.
    let done = run.first("tool_call_completed");
    assert_eq!(done["payload"]["status"], "failed");
    assert_eq!(done["payload"]["error"]["code"], "invalid_arguments");
    assert!(
        done["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not a JSON object")
    );
    assert!(
        done["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(MALFORMED)
    );
    assert_eq!(run.first("text_completed")["payload"]["text"], "Hello.");
    assert!(run.stdout.contains("Hello."));
    assert!(!run.stdout.contains("tool_call_started"));

    // The second request replays the call with an empty object, paired
    // with its failed result quoting the raw text.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(messages_tool_use(&body)["input"], json!({}));
    let result = messages_tool_result(&body);
    assert_eq!(result["is_error"], true);
    assert!(result["content"].as_str().unwrap().contains(MALFORMED));
    assert_stdout_matches_log(&run, &setup);

    // A resume replays the same bytes without rewriting the log.
    let id = run.session_id().to_owned();
    let resumed = setup.run(&["ask", "--resume", &id, "again"]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    assert_eq!(resumed.kinds(), RESUME_HELLO_KINDS);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(messages_tool_use(&body)["input"], json!({}));
    assert!(
        messages_tool_result(&body)["content"]
            .as_str()
            .unwrap()
            .contains(MALFORMED)
    );
    assert_resume_matches_log(&run, &resumed, &setup);
}

/// The Converse probe's recorded plain-text reply, served to end the turn
/// and the resume.
const CONVERSE_PLAIN: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/plain.eventstream");

/// A synthetic request id: a fixed fake UUID, never one AWS issued.
const CONVERSE_REQUEST_ID: &str = "11111111-2222-4333-8444-555555555555";

/// CRC-32/ISO-HDLC of `bytes`, framing the Converse messages below the way
/// AWS frames them.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One Converse event-stream message carrying `payload` as `event_type`.
fn converse_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    for (name, value) in [
        (":event-type", event_type),
        (":content-type", "application/json"),
        (":message-type", "event"),
    ] {
        headers.push(u8::try_from(name.len()).unwrap());
        headers.extend_from_slice(name.as_bytes());
        headers.push(7u8);
        headers.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let total = u32::try_from(12 + headers.len() + payload.len() + 4).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&u32::try_from(headers.len()).unwrap().to_be_bytes());
    out.extend_from_slice(&crc32(&out).to_be_bytes());
    out.extend_from_slice(&headers);
    out.extend_from_slice(payload);
    let message_crc = crc32(&out);
    out.extend_from_slice(&message_crc.to_be_bytes());
    out
}

/// One Converse event of `kind` carrying `payload`.
fn converse_event(kind: &str, payload: &Value) -> Vec<u8> {
    converse_frame(kind, payload.to_string().as_bytes())
}

/// A Converse response carrying `body` as Bedrock frames it, naming the
/// request in `x-amzn-RequestId` as AWS does.
fn converse_response(body: Vec<u8>) -> Response {
    Response {
        status: 200,
        headers: vec![
            (
                "content-type".to_owned(),
                "application/vnd.amazon.eventstream".to_owned(),
            ),
            (
                "x-amzn-RequestId".to_owned(),
                CONVERSE_REQUEST_ID.to_owned(),
            ),
        ],
        body,
        drop_connection: false,
        stall: false,
    }
}

/// A Converse tool call streaming the malformed text, stopping for tool
/// use, then its metadata.
fn converse_malformed() -> Response {
    let body = [
        converse_event("messageStart", &json!({"role": "assistant"})),
        converse_event(
            "contentBlockStart",
            &json!({"contentBlockIndex": 0, "start": {"toolUse": {"toolUseId": "tooluse_probe0001", "name": "shell"}}}),
        ),
        converse_event(
            "contentBlockDelta",
            &json!({"contentBlockIndex": 0, "delta": {"toolUse": {"input": MALFORMED}}}),
        ),
        converse_event("contentBlockStop", &json!({"contentBlockIndex": 0})),
        converse_event("messageStop", &json!({"stopReason": "tool_use"})),
        converse_event("metadata", &json!({"usage": {"inputTokens": 3, "outputTokens": 1}})),
    ]
    .concat();
    converse_response(body)
}

/// The fake server answering Converse with `responses`, and the installed
/// bedrock package's region: the server's host, read from `AWS_REGION`.
fn bedrock_server(setup: &Setup, responses: Vec<Response>) -> (ProviderServer, String) {
    let server = ProviderServer::start(responses).unwrap();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package(
        "bedrock",
        &to,
        "https://bedrock-runtime.{region}.amazonaws.com",
        "http://{region}",
    );
    let installed = support::run_to_exit(
        setup.deadline,
        "fiber extension install",
        setup.fiber(&["extension", "install", to.to_str().unwrap()]),
    );
    assert!(installed.status.success(), "install: {installed:?}");
    let host = server.url().strip_prefix("http://").unwrap().to_owned();
    (server, host)
}

/// The Converse blocks of every message in `body`.
fn converse_blocks(body: &Value) -> Vec<Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .collect()
}

/// The `toolUse` block of a Converse request body.
fn converse_tool_use(body: &Value) -> Value {
    converse_blocks(body)
        .into_iter()
        .find(|block| block.get("toolUse").is_some())
        .unwrap_or_else(|| panic!("no toolUse in {body}"))
}

/// The `toolResult` block of a Converse request body.
fn converse_tool_result(body: &Value) -> Value {
    converse_blocks(body)
        .into_iter()
        .find(|block| block.get("toolResult").is_some())
        .unwrap_or_else(|| panic!("no toolResult in {body}"))
}

/// The event kinds of a Converse turn whose tool call streams malformed
/// input and fails before it starts.
const CONVERSE_MALFORMED_KINDS: [&str; 22] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "extension_exec",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_arguments_delta",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The event kinds of a resumed Converse turn answering in text.
const CONVERSE_RESUME_KINDS: [&str; 13] = [
    "fiber_started",
    "extensions_loaded",
    "extension_exec",
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn malformed_converse_tool_input_replays_as_an_empty_object_with_the_raw_text_quoted() {
    let setup = Setup::new();
    let (server, host) = bedrock_server(
        &setup,
        vec![
            converse_malformed(),
            converse_response(CONVERSE_PLAIN.to_vec()),
            converse_response(CONVERSE_PLAIN.to_vec()),
        ],
    );
    let env = [
        ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
        ("AWS_REGION", host.as_str()),
    ];

    let run = setup.run_with_env(
        &[
            "ask",
            "--model",
            "bedrock/mistral.ministral-3-8b-instruct",
            "call it",
        ],
        &env,
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), CONVERSE_MALFORMED_KINDS);
    // The log records the arguments as the model sent them.
    let requested = run.first("tool_call_requested");
    assert_eq!(requested["payload"]["arguments"], json!(MALFORMED));
    // The call fails before it starts, quoting the raw text.
    let done = run.first("tool_call_completed");
    assert_eq!(done["payload"]["status"], "failed");
    assert_eq!(done["payload"]["error"]["code"], "invalid_arguments");
    assert!(
        done["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not a JSON object")
    );
    assert!(
        done["payload"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(MALFORMED)
    );

    // The second request replays the call with an empty object, paired
    // with its failed result quoting the raw text.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(converse_tool_use(&body)["toolUse"]["input"], json!({}));
    let result = converse_tool_result(&body);
    assert_eq!(result["toolResult"]["status"], "error");
    assert!(
        result["toolResult"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(MALFORMED)
    );
    assert_stdout_matches_log(&run, &setup);

    // A resume replays the same bytes without rewriting the log.
    let id = run.session_id().to_owned();
    let resumed = setup.run_with_env(&["ask", "--resume", &id, "again"], &env);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    assert_eq!(resumed.kinds(), CONVERSE_RESUME_KINDS);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(converse_tool_use(&body)["toolUse"]["input"], json!({}));
    assert!(
        converse_tool_result(&body)["toolResult"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(MALFORMED)
    );
    assert_resume_matches_log(&run, &resumed, &setup);
}
