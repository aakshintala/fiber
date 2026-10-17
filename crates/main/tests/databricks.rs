//! Binary-level tests of the first-party databricks package
//! (`docs/testing.md`, "Model calls"): the built `fiber` runs in its own
//! process group with its own `FIBER_HOME`, holding the package copied with
//! its origin rewritten to the fake server. Discovery answers the two Unity
//! Catalog endpoints, then each turn runs on its protocol's stream.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::process::Output;

use fakes::{ProviderServer, Request, Response, fingerprint};
use serde_json::{Value, json};
use support::{HELLO_KINDS, Setup, hello, is_status, run_to_exit, write_json};

/// One finished `fiber` run with its stdout parsed as one JSON value per
/// line, on the pattern of `tests/ask.rs`.
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
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        Self {
            code: output.status.code(),
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

    fn text_completed(&self) -> &str {
        self.lines
            .iter()
            .find(|line| line["kind"] == "text_completed")
            .unwrap()["payload"]["text"]
            .as_str()
            .unwrap()
    }
}

/// Runs `fiber` with `args` and the extra environment `env` to its exit
/// under the test's deadline.
#[track_caller]
fn run(setup: &Setup, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut command = setup.fiber(args);
    command.envs(env.iter().copied());
    Run::from(run_to_exit(
        setup.deadline,
        &format!("fiber {}", args.join(" ")),
        command,
    ))
}

/// The recorded credential header equals the fingerprint of `value`, the
/// bytes the package sends (`docs/testing.md`, "Model calls").
fn assert_fingerprint(request: &Request, header: &str, value: &str) {
    assert_eq!(
        request.header(header),
        Some(fingerprint(value).as_str()),
        "{header}"
    );
}

/// Copies the `databricks` package with its origin rewritten to the fake
/// server and installs it with `fiber extension install` on closed stdin,
/// returning the fake host and port.
fn install(setup: &Setup, server: &ProviderServer) -> String {
    let to = setup.root.path().join("pkg-databricks");
    support::package::copy_package("databricks", &to, "https://", "http://");
    let installed = run(setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);
    assert!(
        !installed.stderr.contains("Go ahead?"),
        "{}",
        installed.stderr
    );
    server.url().strip_prefix("http://").unwrap().to_owned()
}

/// The model services the fake discovery serves: one Claude service on the
/// chat API, one Responses-capable service, and one chat-only service.
fn services_body() -> Value {
    json!({"model_services": [
        {"name": "model-services/system.ai.claude-opus-5",
         "supported_api_types": ["mlflow/v1/chat/completions"]},
        {"name": "model-services/system.ai.gpt-6-sol",
         "supported_api_types": ["openai/v1/responses", "mlflow/v1/chat/completions"]},
        {"name": "model-services/system.ai.glm-5.3",
         "supported_api_types": ["mlflow/v1/chat/completions"]},
    ]})
}

/// The catalog rows matching each discovered service.
fn catalog_body() -> Value {
    json!({"models": [
        {"model_name": "claude-opus-5", "mode": "chat", "provider_type": "ANTHROPIC",
         "context_window": {"max_input": 200000, "max_output": 64000},
         "capabilities": {"vision": true},
         "base_pricing": {"input_cost_per_million_tokens": 5,
                           "output_cost_per_million_tokens": 25,
                           "cache_read_input_cost_per_million_tokens": 0.5,
                           "cache_creation_input_cost_per_million_tokens": 6.25}},
        {"model_name": "gpt-6-sol", "mode": "chat", "provider_type": "OPENAI",
         "context_window": {"max_input": 400000, "max_output": 160000},
         "capabilities": {"vision": false},
         "base_pricing": {"input_per_million_tokens": 2,
                           "output_per_million_tokens": 8}},
        {"model_name": "glm-5.3", "mode": "chat", "provider_type": "MICROSOFT_FOUNDRY",
         "context_window": {"max_input": 200000, "max_tokens": 32000},
         "capabilities": {},
         "base_pricing": {"input_cost_per_million_tokens": 0.6,
                           "output_cost_per_million_tokens": 2.2,
                           "cache_read_input_cost_per_million_tokens": 0.06}},
    ]})
}

/// The discovery's two answers, in the order `models()` asks for them.
fn discovery() -> [Response; 2] {
    [
        Response::status(200, services_body().to_string()),
        Response::status(200, catalog_body().to_string()),
    ]
}

/// An Anthropic Messages stream answering `Hello.` in two fragments, on the
/// pattern of `tests/ask.rs`.
fn anthropic_hello() -> Response {
    let events = [
        json!({"type": "message_start", "message": {
            "id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}
        }}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "lo."}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// An `openai-completions` stream answering `Hello.` in two fragments, on
/// the pattern of `tests/ask.rs`.
fn completions_hello() -> Response {
    let chunks = [
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"content": "lo."}, "finish_reason": "stop"}]}),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {"prompt_tokens": 15,
            "completion_tokens": 9, "total_tokens": 24,
            "prompt_tokens_details": {"cached_tokens": 14}}}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

/// The credential and workspace environment a child reads the session's key
/// and host from.
fn credentials(host: &str) -> [(&str, &str); 2] {
    [("DATABRICKS_TOKEN", "dapi-test"), ("DATABRICKS_HOST", host)]
}

/// A turn's three requests: the two discovery answers, then the model's.
/// Every request carries the package's header.
fn assert_turn(requests: &[Request], model_path: &str) {
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert_eq!(
        requests[0].path,
        "/api/2.1/unity-catalog/model-services?parent=schemas%2Fsystem.ai&page_size=100"
    );
    assert_eq!(
        requests[1].path,
        "/api/ai-gateway/v2/external-provider-models?page_size=1000"
    );
    assert_eq!(requests[2].path, model_path);
    for request in requests {
        assert_eq!(
            request.header("databricks-ai-gateway-request-tags"),
            Some("{\"source\":\"fiber\"}"),
            "{request:?}"
        );
        assert_fingerprint(request, "authorization", "Bearer dapi-test");
    }
}

#[test]
fn claude_completes_a_turn_on_the_anthropic_route() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([anthropic_hello()])).unwrap();
    let host = install(&setup, &server);

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "databricks/databricks-claude-opus-5",
            "hi",
        ],
        &credentials(&host),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    let requests = server.requests();
    assert_turn(&requests, "/ai-gateway/anthropic/v1/messages");
    assert!(requests[2].header("x-api-key").is_none());
    let body: Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert!(body.get("reasoning_effort").is_none(), "{body}");
}

#[test]
fn responses_model_completes_a_turn_on_the_codex_route() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([hello()])).unwrap();
    let host = install(&setup, &server);

    let run = run(
        &setup,
        &["ask", "--model", "databricks/databricks-gpt-6-sol", "hi"],
        &credentials(&host),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/codex/v1/responses");
}

#[test]
fn completions_model_completes_a_turn_on_the_mlflow_route() {
    let setup = Setup::new();
    let server =
        ProviderServer::start(discovery().into_iter().chain([completions_hello()])).unwrap();
    let host = install(&setup, &server);

    let run = run(
        &setup,
        &["ask", "--model", "databricks/databricks-glm-5.3", "hi"],
        &credentials(&host),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/mlflow/v1/chat/completions");
}

#[test]
fn models_lists_exactly_the_three_discovered_references() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery()).unwrap();
    let host = install(&setup, &server);

    let run = run(&setup, &["models", "--json"], &credentials(&host));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let mut models: Vec<String> = run
        .lines
        .iter()
        .map(|line| line["model"].as_str().unwrap().to_owned())
        .collect();
    models.sort();
    assert_eq!(
        models,
        [
            "databricks/databricks-claude-opus-5",
            "databricks/databricks-glm-5.3",
            "databricks/databricks-gpt-6-sol",
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
}

#[test]
fn the_setting_wins_over_the_environment() {
    let setup = Setup::new();
    let server =
        ProviderServer::start(discovery().into_iter().chain([completions_hello()])).unwrap();
    let host = install(&setup, &server);
    // The person's setting names the fake server while the environment
    // names the refused address (`docs/testing/fakes.md`, "Fakes").
    write_json(
        &setup.home().join("config/databricks.json"),
        &json!({"workspace": host}),
    );

    let run = run(
        &setup,
        &["ask", "--model", "databricks/databricks-glm-5.3", "hi"],
        &[
            ("DATABRICKS_TOKEN", "dapi-test"),
            ("DATABRICKS_HOST", "127.0.0.1:1"),
        ],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/mlflow/v1/chat/completions");
}

/// The one line stdout holds when the process failed before any session:
/// kind `fiber_exited`, no `session_id`, the exit code and error, and the
/// message as one stderr sentence. Returns the message.
fn assert_pre_session(output: &Output, exit: i32, code: &str) -> String {
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout:?}");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["kind"], "fiber_exited");
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], exit);
    assert_eq!(line["payload"]["error"]["code"], code);
    let message = line["payload"]["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert_eq!(stderr, format!("fiber: {message}\n"), "{stderr:?}");
    message
}

#[test]
fn unset_workspace_leaves_the_seed_model_out() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    install(&setup, &server);

    let mut command = setup.fiber(&[
        "ask",
        "--model",
        "databricks/databricks-claude-opus-5",
        "hi",
    ]);
    command.env("DATABRICKS_TOKEN", "dapi-test");
    let output = run_to_exit(setup.deadline, "fiber ask", command);

    let message = assert_pre_session(&output, 1, "model_unconfigured");
    assert_eq!(
        message,
        "The model `databricks/databricks-claude-opus-5` needs the setting \
         `workspace` for its base URL, which has no value, and \
         `DATABRICKS_HOST` has none either."
    );
    assert!(server.requests().is_empty());
}

/// A full catalog page of realistic rows decodes under the manifest's
/// memory cap on the production path, which honors it: one service, one
/// page of 1,000 `chat` rows, only one of them matching.
#[test]
fn a_thousand_row_catalog_page_fits_the_memory_cap() {
    let setup = Setup::new();
    let mut rows = vec![json!({
        "model_name": "bulk-model", "mode": "chat", "provider_type": "MICROSOFT_FOUNDRY",
        "context_window": {"max_input": 64000, "max_output": 8000},
        "capabilities": {"vision": true, "reasoning": true},
        "base_pricing": {"input_cost_per_million_tokens": 1,
                         "output_cost_per_million_tokens": 3,
                         "cache_read_input_cost_per_million_tokens": 0.1,
                         "cache_creation_input_cost_per_million_tokens": 0.2,
                         "long_context": [{"threshold_tokens": 200000,
                                           "input_cost_per_million_tokens": 2,
                                           "output_cost_per_million_tokens": 6}]},
    })];
    for i in 1..1000 {
        rows.push(json!({
            "model_name": format!("filler-{i:04}"), "mode": "chat",
            "provider_type": "MICROSOFT_FOUNDRY",
            "context_window": {"max_input": 32000, "max_output": 4000},
            "capabilities": {"vision": false, "reasoning": true},
            "base_pricing": {"input_cost_per_million_tokens": 0.5,
                             "output_cost_per_million_tokens": 1.5,
                             "cache_read_input_cost_per_million_tokens": 0.05,
                             "cache_write_per_million_tokens": 0.1,
                             "long_context": [{"threshold_tokens": 128000,
                                               "input_cost_per_million_tokens": 1,
                                               "output_cost_per_million_tokens": 3}]},
        }));
    }
    let server = ProviderServer::start([
        Response::status(
            200,
            json!({"model_services": [{"name": "model-services/system.ai.bulk-model",
                                       "supported_api_types":
                                           ["mlflow/v1/chat/completions"]}]})
            .to_string(),
        ),
        Response::status(200, json!({"models": rows}).to_string()),
    ])
    .unwrap();
    let host = install(&setup, &server);

    let run = run(&setup, &["models", "--json"], &credentials(&host));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let models: Vec<&str> = run
        .lines
        .iter()
        .map(|line| line["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["databricks/databricks-bulk-model"]);
    assert_eq!(server.requests().len(), 2);
}
