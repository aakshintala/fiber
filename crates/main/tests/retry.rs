//! `fiber ask` retries a failed model call, then fails it once the retries run
//! out (`docs/model-routing.md`, "When a model call fails"): the built
//! binary against the fake provider server, with zero backoffs so nothing
//! sleeps (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// A temporary root holding Fiber home and the workspace, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fa-retry");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    /// Installs a provider `fake` with model `m` speaking `protocol` at the
    /// fake server, and configures `fake/m` with zero backoffs and
    /// `attempts` retries.
    fn provider(&self, server: &ProviderServer, protocol: &str, attempts: u64) {
        let source = self.root.path().join("src");
        write_file(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write_file(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": protocol,
                    "base_url": format!("{}/v1", server.url())}]
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
        write_file(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "retry": {
                "attempts": attempts,
                "initial_delay_ms": 0,
                "max_delay_ms": 0,
            }}),
        );
    }

    /// Runs `fiber ask hi` in its own process group, waits for it under
    /// [`DEADLINE`], and asserts that nothing it started is left in the
    /// group (`docs/testing.md`, "Running tests").
    fn ask(&self) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["ask", "hi"])
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        drop(child.stdin.take());
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                panic!("waited {DEADLINE:?} for `fiber ask` to exit (reaped: {reaped})");
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

fn write_file(file: &std::path::Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    fakes::kill_group(group, "0").unwrap()
}

/// Spawns `command` in a new process group beside a watchdog that kills the
/// group if this process dies first.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// One finished run: its exit code and stdout's lines, parsed.
struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn last(&self) -> &Value {
        self.lines.last().expect("stdout has a line")
    }

    /// The `code` of every `retry_scheduled`, in order.
    fn retried(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|l| l["kind"] == "retry_scheduled")
            .map(|l| l["payload"]["code"].as_str().unwrap())
            .collect()
    }
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn responses_hello() -> Response {
    responses_stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        }}),
    ])
}

fn responses_stream(events: &[Value]) -> Response {
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// The same stream cut before its terminal event: no `response.completed`.
fn responses_cut() -> Response {
    responses_stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `anthropic-messages` stream answering `Hello.`.
fn anthropic_hello() -> Response {
    anthropic_stream(&[
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ])
}

fn anthropic_stream(events: &[Value]) -> Response {
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    Response::stream(body)
}

/// The same stream cut before its terminal event: no `message_stop`.
fn anthropic_cut() -> Response {
    anthropic_stream(&[
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello."}}),
    ])
}

/// An `openai-completions` stream answering `hi`.
fn completions_hello() -> Response {
    let chunks = [
        json!({"id": "gen-1", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]}),
        json!({"id": "gen-1", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        json!({"id": "gen-1", "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3}}),
    ];
    let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

/// The same stream cut before its terminal event: no `[DONE]`.
fn completions_cut() -> Response {
    let chunk = json!({"id": "gen-1", "object": "chat.completion.chunk",
        "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": "stop"}]});
    Response::stream(format!("data: {chunk}\n\n"))
}

/// A `google-generative-ai` stream answering `hi`.
fn gemini_hello() -> Response {
    gemini_stream(&[json!({"candidates": [{
        "content": {"role": "model", "parts": [{"text": "hi"}]},
        "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 3}})])
}

fn gemini_stream(chunks: &[Value]) -> Response {
    let body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    Response::stream(body)
}

/// The same stream cut before its terminal event: no `finishReason`.
fn gemini_cut() -> Response {
    gemini_stream(&[json!({"candidates": [{
        "content": {"role": "model", "parts": [{"text": "hi"}]}}]})])
}

/// A stub that drops its first connection without answering, then serves
/// `hello` to the next one: a dropped connection followed by a success.
/// Returns its base URL; the serving thread joins at the test's end.
fn drop_then_hello(hello: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let serve = thread::spawn(move || {
        for first in [true, false] {
            let (stream, _) = listener.accept().unwrap();
            if first {
                drop(stream);
                continue;
            }
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let header = line.trim_end_matches(['\r', '\n']);
                if header.is_empty() {
                    break;
                }
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let stream = reader.into_inner();
            write_response(stream, &hello);
        }
    });
    (url, serve)
}

fn write_response(mut stream: std::net::TcpStream, hello: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 Fake\r\ncontent-type: text/event-stream\r\ncontent-length: {}\
        \r\nconnection: close\r\n\r\n",
        hello.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(hello).unwrap();
    stream.flush().unwrap();
}

/// Installs `fake/m` speaking `protocol` at `server` with `attempts`
/// retries, runs `fiber ask hi` against `[failure, success]`, and asserts
/// the ask succeeds after exactly one retry.
fn succeeds_after_one_retry(
    protocol: &str,
    failure: Response,
    success: Response,
    code: &str,
    attempts: u64,
) {
    let setup = Setup::new();
    let server = ProviderServer::start([failure, success]).unwrap();
    setup.provider(&server, protocol, attempts);
    let run = setup.ask();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    assert_eq!(run.retried(), [code]);
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn a_429_with_retry_after_0_then_a_reply_succeeds() {
    succeeds_after_one_retry(
        "openai-responses",
        Response::status(429, "{}").header("retry-after", "0"),
        responses_hello(),
        "rate_limited",
        3,
    );
}

#[test]
fn server_errors_timeouts_and_conflicts_are_retried() {
    for status in [500, 503, 529, 408, 409] {
        succeeds_after_one_retry(
            "openai-responses",
            Response::status(status, "{}"),
            responses_hello(),
            "provider_unavailable",
            3,
        );
    }
}

#[test]
fn a_dropped_connection_is_retried() {
    let (url, serve) = drop_then_hello(responses_hello().body);
    let setup = Setup::new();
    // The stub is not a `ProviderServer`: install the provider at its URL.
    let source = setup.root.path().join("src");
    write_file(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    write_file(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses",
                "base_url": format!("{url}/v1")}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    write_file(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "retry": {
            "attempts": 3, "initial_delay_ms": 0, "max_delay_ms": 0}}),
    );
    let run = setup.ask();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.last()["payload"]["text"], "Hello.");
    assert_eq!(run.retried(), ["connection_failed"]);
    serve.join().unwrap();
}

#[test]
fn a_stream_cut_short_is_retried_on_every_protocol() {
    for (protocol, cut, hello) in [
        ("openai-responses", responses_cut(), responses_hello()),
        ("anthropic-messages", anthropic_cut(), anthropic_hello()),
        ("openai-completions", completions_cut(), completions_hello()),
        ("google-generative-ai", gemini_cut(), gemini_hello()),
    ] {
        let setup = Setup::new();
        let server = ProviderServer::start([Response::stream(cut.body), hello]).unwrap();
        setup.provider(&server, protocol, 3);
        let run = setup.ask();
        assert_eq!(run.code, Some(0), "{protocol}: {}", run.stderr);
        assert_eq!(run.retried(), ["stream_incomplete"], "{protocol}");
        assert_eq!(server.requests().len(), 2, "{protocol}");
    }
}

#[test]
fn x_should_retry_true_retries_a_400() {
    succeeds_after_one_retry(
        "openai-responses",
        Response::status(400, "{}").header("x-should-retry", "true"),
        responses_hello(),
        "invalid_request",
        3,
    );
}

/// A failure that is never retried fails the ask with one request only.
fn fails_at_once(failure: Response, code: &str) {
    let setup = Setup::new();
    let server = ProviderServer::start([failure]).unwrap();
    setup.provider(&server, "openai-responses", 3);
    let run = setup.ask();
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.last()["payload"]["error"]["code"], code);
    assert!(run.retried().is_empty());
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_400_a_401_and_false_on_a_503_fail_at_once() {
    fails_at_once(Response::status(400, "{}"), "invalid_request");
    fails_at_once(Response::status(401, "{}"), "authentication_failed");
    fails_at_once(
        Response::status(503, "{}").header("x-should-retry", "false"),
        "provider_unavailable",
    );
}

#[test]
fn two_503s_with_one_attempt_fail_the_ask() {
    let setup = Setup::new();
    let server =
        ProviderServer::start([Response::status(503, "{}"), Response::status(503, "{}")]).unwrap();
    setup.provider(&server, "openai-responses", 1);
    let run = setup.ask();
    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.last()["payload"]["error"]["code"],
        "provider_unavailable"
    );
    assert_eq!(run.retried(), ["provider_unavailable"]);
    assert_eq!(server.requests().len(), 2);
}
