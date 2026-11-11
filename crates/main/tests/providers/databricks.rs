//! Binary-level tests of the first-party databricks package
//! (`docs/testing.md`, "Model calls"): the built `fiber` runs in its own
//! process group with its own `FIBER_HOME`, holding the package copied with
//! its origin rewritten to the fake server. Discovery answers the two Unity
//! Catalog endpoints and the serving-endpoint list, then each turn runs on
//! its protocol's stream.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::process::Output;
use std::sync::{Arc, Mutex};

use fakes::{ProviderServer, Request, Response, fingerprint};
use serde_json::{Value, json};
use support::{
    HELLO_KINDS, HubProc, Run, SessionGuard, Setup, connect_hub, hello, is_status, recv_answer,
    run_to_exit, subscribe, until, until_after, write_json,
};

/// Parses `fiber ask` stdout into a run: one JSON value per line, without
/// `session_status` lines, which an observer thread writes.
fn run_from(output: Output) -> Run {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

impl Run {
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
    run_from(run_to_exit(
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
/// returning the fake host and port. The copy also carries the given
/// thinking table, which `copy_package` does not copy: it only copies
/// `extension.json` and `providers/`.
fn install(setup: &Setup, server: &ProviderServer, thinking: &str) -> String {
    let to = setup.root.path().join("pkg-databricks");
    support::package::copy_package("databricks", &to, "https://", "http://");
    std::fs::write(to.join("thinking.lua"), thinking).unwrap();
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

/// The serving endpoints backing each discovered service.
fn endpoints_body() -> Value {
    json!({"endpoints": [
        {"name": "claude-opus-5"},
        {"name": "gpt-6-sol"},
        {"name": "glm-5.3"},
    ]})
}

/// The discovery's three answers: one per listing the provider fetches.
fn discovery() -> [Response; 3] {
    [
        Response::status(200, services_body().to_string()),
        Response::status(200, catalog_body().to_string()),
        Response::status(200, endpoints_body().to_string()),
    ]
}

/// An Anthropic Messages stream answering `Hello.` in two fragments, on the
/// pattern of `tests/delegation/ask.rs`.
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
/// the pattern of `tests/delegation/ask.rs`.
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

/// A request's tags header, parsed.
fn tags_header(request: &Request) -> Value {
    let raw = request
        .header("databricks-ai-gateway-request-tags")
        .unwrap_or_else(|| panic!("no tags header: {request:?}"));
    serde_json::from_str(raw).unwrap()
}

/// The recorded request to `path`. The fake records every request and tests
/// assert on the path, the headers and the body (`docs/testing/fakes.md`,
/// "Model calls"); no doc promises discovery's order or count, so tests find
/// requests by path rather than pinning indices (`docs/testing.md`, "Event
/// streams").
fn find_request<'a>(requests: &'a [Request], path: &str) -> &'a Request {
    requests
        .iter()
        .find(|request| request.path.as_str() == path)
        .unwrap_or_else(|| panic!("no request to {path}: {requests:?}"))
}

/// Discovery's listing paths: the service list, the catalog, and the
/// serving-endpoint list.
const DISCOVERY_PATHS: [&str; 3] = [
    "/api/2.1/unity-catalog/model-services?parent=schemas%2Fsystem.ai&page_size=100",
    "/api/ai-gateway/v2/external-provider-models?page_size=1000",
    "/api/2.0/serving-endpoints",
];

/// Every discovery listing was fetched. Presence, not order or count: no doc
/// promises either (`docs/testing.md`, "Event streams").
fn assert_discovery_fetched(requests: &[Request]) {
    for path in DISCOVERY_PATHS {
        find_request(requests, path);
    }
}

/// A turn's requests: discovery's answers, then the model's. All three
/// discovery requests carry the source-only header; the model's carries
/// `model_tags`, parsed.
fn assert_tagged_turn(requests: &[Request], model_path: &str, model_tags: &Value) {
    let model = find_request(requests, model_path);
    for path in DISCOVERY_PATHS {
        let request = find_request(requests, path);
        assert_eq!(
            tags_header(request),
            json!({"source": "fiber"}),
            "{request:?}"
        );
    }
    assert_eq!(tags_header(model), *model_tags, "{model:?}");
    for request in requests {
        assert_fingerprint(request, "authorization", "Bearer dapi-test");
    }
}

/// A turn's requests with the default, source-only model header.
fn assert_turn(requests: &[Request], model_path: &str) {
    assert_tagged_turn(requests, model_path, &json!({"source": "fiber"}));
}

#[test]
fn claude_completes_a_turn_on_the_anthropic_route() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([anthropic_hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

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
    let model = find_request(&requests, "/ai-gateway/anthropic/v1/messages");
    assert!(model.header("x-api-key").is_none());
    let body: Value = serde_json::from_slice(&model.body).unwrap();
    assert!(body.get("reasoning_effort").is_none(), "{body}");
}

#[test]
fn responses_model_completes_a_turn_on_the_codex_route() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

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
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

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
    let setup = Setup::without_provider();
    let server = ProviderServer::start(discovery()).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

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
    assert_discovery_fetched(&requests);
}

#[test]
fn the_setting_wins_over_the_environment() {
    let setup = Setup::new();
    let server =
        ProviderServer::start(discovery().into_iter().chain([completions_hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );
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
    install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

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
         `DATABRICKS_HOST` is not set in the hub's environment (your login shell). \
         Run `fiber login databricks`, or set `workspace` in \
         `config/databricks.json` in Fiber home, or export it in your shell profile \
         and run `fiber hub refresh`."
    );
    assert!(server.requests().is_empty());
}

/// A full catalog page of realistic rows decodes under the manifest's
/// memory cap on the production path, which honors it: one service, one
/// page of 1,000 `chat` rows, only one of them matching.
#[test]
fn a_thousand_row_catalog_page_fits_the_memory_cap() {
    let setup = Setup::without_provider();
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
        Response::status(
            200,
            json!({"endpoints": [{"name": "bulk-model"}]}).to_string(),
        ),
    ])
    .unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    let run = run(&setup, &["models", "--json"], &credentials(&host));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let models: Vec<&str> = run
        .lines
        .iter()
        .map(|line| line["model"].as_str().unwrap())
        .collect();
    assert_eq!(models, ["databricks/databricks-bulk-model"]);
    assert_discovery_fetched(&server.requests());
}

/// A `:level` suffix on a discovered Claude model reaches the Anthropic
/// route as adaptive thinking (`docs/model-routing.md`, "Thinking").
#[test]
fn claude_thinking_suffix_sends_adaptive_thinking() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([anthropic_hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "databricks/databricks-claude-opus-5:high",
            "hi",
        ],
        &credentials(&host),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    let requests = server.requests();
    assert_turn(&requests, "/ai-gateway/anthropic/v1/messages");
    let model = find_request(&requests, "/ai-gateway/anthropic/v1/messages");
    let body: Value = serde_json::from_slice(&model.body).unwrap();
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], json!({"effort": "high"}));
}

/// A discovered model's default level applies with no suffix: one synthetic
/// row overlaid onto the copied package's thinking table chooses `medium`.
#[test]
fn discovered_default_level_sends_without_a_suffix() {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([anthropic_hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        "return {[\"claude-opus-5\"]={levels={\"low\",\"medium\",\"high\"},default=\"medium\"}}\n",
    );

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
    let model = find_request(&requests, "/ai-gateway/anthropic/v1/messages");
    let body: Value = serde_json::from_slice(&model.body).unwrap();
    assert_eq!(body["thinking"], json!({"type": "adaptive"}));
    assert_eq!(body["output_config"], json!({"effort": "medium"}));
}

/// One `fiber ask` turn on `model` with `-c providers.databricks.request_tags=<tags>`:
/// the run and the requests the fake server saw.
fn tagged_ask(model: &str, reply: Response, tags: &str) -> (Run, Vec<Request>) {
    let setup = Setup::new();
    let server = ProviderServer::start(discovery().into_iter().chain([reply])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );
    let flag = format!("providers.databricks.request_tags={tags}");
    let run = run(
        &setup,
        &["ask", "-c", &flag, "--model", model, "hi"],
        &credentials(&host),
    );
    (run, server.requests())
}

#[test]
fn request_tags_ask_accepts_native_objects() {
    let routes = [
        (
            "databricks/databricks-claude-opus-5",
            anthropic_hello(),
            "/ai-gateway/anthropic/v1/messages",
        ),
        (
            "databricks/databricks-gpt-6-sol",
            hello(),
            "/ai-gateway/codex/v1/responses",
        ),
        (
            "databricks/databricks-glm-5.3",
            completions_hello(),
            "/ai-gateway/mlflow/v1/chat/completions",
        ),
    ];
    for (model, reply, path) in routes {
        let (run, requests) = tagged_ask(model, reply, r#"{"run":"r1"}"#);
        assert_eq!(run.code, Some(0), "{model}: {}", run.stderr);
        assert_eq!(run.kinds(), HELLO_KINDS, "{model}");
        assert_tagged_turn(&requests, path, &json!({"source": "fiber", "run": "r1"}));
    }
    // A `source` in the value never replaces `fiber`.
    let (run, requests) = tagged_ask(
        "databricks/databricks-glm-5.3",
        completions_hello(),
        r#"{"run":"r1","source":"mine"}"#,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_tagged_turn(
        &requests,
        "/ai-gateway/mlflow/v1/chat/completions",
        &json!({"source": "fiber", "run": "r1"}),
    );
}

#[test]
fn request_tags_invalid_value_uses_default() {
    let (run, requests) = tagged_ask(
        "databricks/databricks-glm-5.3",
        completions_hello(),
        r#"{"run":"r1","bad":7}"#,
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let notices: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "notice")
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["payload"]["code"], "config_invalid");
    let message = notices[0]["payload"]["message"].as_str().unwrap();
    assert!(
        message.contains("providers.databricks.request_tags"),
        "{message}"
    );
    assert!(!message.contains("r1"), "{message}");
    let kinds: Vec<&str> = run
        .kinds()
        .into_iter()
        .filter(|kind| *kind != "notice")
        .collect();
    assert_eq!(kinds, HELLO_KINDS);
    assert_eq!(run.text_completed(), "Hello.");
    assert_turn(&requests, "/ai-gateway/mlflow/v1/chat/completions");
}

/// Starts a session through the hub with `overrides` on the glm model,
/// waits for its turn, closes it and waits for it to exit. Returns the
/// stream's `config_invalid` notices.
fn hub_turn(client: &support::Socket, workspace: &str, id: &str, overrides: &[&str]) -> Vec<Value> {
    let mut all = vec!["model=databricks/databricks-glm-5.3"];
    all.extend(overrides);
    client.send(
        &json!({
            "id": id,
            "command": "start",
            "args": {"workspace": workspace, "overrides": all,
                     "content": [{"type": "text", "text": "hi"}]},
        })
        .to_string(),
    );
    let started = recv_answer(client, id, "the start acknowledgement");
    assert_eq!(started["kind"], "command_accepted", "{started}");
    let session = started["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    let sub = subscribe(client, &session);
    let mut stream = until(client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    client.send(
        &json!({"id": format!("{id}_close"), "session_id": session, "command": "close"})
            .to_string(),
    );
    stream.extend(until_after(client, &sub, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    }));
    stream
        .into_iter()
        .filter(|line| line["kind"] == "notice" && line["payload"]["code"] == "config_invalid")
        .collect()
}

#[test]
fn request_tags_hub_overrides_are_session_local() {
    const MODEL_PATH: &str = "/ai-gateway/mlflow/v1/chat/completions";
    let setup = Setup::new();
    let server = ProviderServer::start_responding(|request| {
        if request
            .path
            .starts_with("/api/2.1/unity-catalog/model-services")
        {
            Response::status(200, services_body().to_string())
        } else if request
            .path
            .starts_with("/api/ai-gateway/v2/external-provider-models")
        {
            Response::status(200, catalog_body().to_string())
        } else if request.path.starts_with("/api/2.0/serving-endpoints") {
            Response::status(200, endpoints_body().to_string())
        } else {
            completions_hello()
        }
    })
    .unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );
    // The hub's environment is empty, so the host and the token come from
    // configuration.
    write_json(
        &setup.home().join("config/databricks.json"),
        &json!({"workspace": host}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"providers": {"databricks": {"credentials": {
            "default": {"command": ["echo", "dapi-test"]}
        }}}}),
    );

    let hub: Arc<Mutex<Option<HubProc>>> = Arc::new(Mutex::new(None));
    let (client, _) = connect_hub(&setup, &hub);
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);

    // The same literal ask takes, then a second tag set, then none.
    let first = r#"providers.databricks.request_tags={"run":"r1"}"#;
    let second = r#"providers.databricks.request_tags={"run":"r2","source":"mine"}"#;
    let mut invalid = Vec::new();
    invalid.extend(hub_turn(&client, &workspace, "c_one", &[first]));
    invalid.extend(hub_turn(&client, &workspace, "c_two", &[second]));
    invalid.extend(hub_turn(&client, &workspace, "c_none", &[]));
    assert!(invalid.is_empty(), "{invalid:?}");

    let requests = server.requests();
    let models: Vec<&Request> = requests.iter().filter(|r| r.path == MODEL_PATH).collect();
    let tags: Vec<Value> = models.iter().map(|r| tags_header(r)).collect();
    assert_eq!(
        tags,
        [
            json!({"source": "fiber", "run": "r1"}),
            json!({"source": "fiber", "run": "r2"}),
            json!({"source": "fiber"}),
        ],
        "{requests:?}"
    );
    // Discovery, wherever a session ran it, stays source-only.
    for request in requests.iter().filter(|r| r.path != MODEL_PATH) {
        assert_eq!(
            tags_header(request),
            json!({"source": "fiber"}),
            "{request:?}"
        );
    }

    guard.wait_gone();
    drop(client);
    hub.lock()
        .unwrap()
        .take()
        .expect("the hub ran")
        .kill_and_wait();
}

/// `docs/testing.md`, "Event streams": the event kinds as a set with counts.
const COUNT_DOC: &str = r#"`docs/testing.md`, "Event streams": - the event kinds as a set with counts, so a duplicated or missing event fails"#;

/// Every diagnostic line under the home's `logs/`, parsed: the witness for
/// an omission is the session's diagnostic log, read after the child exits
/// (`crates/main/tests/extension_host/extension_diagnostics.rs`).
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in std::fs::read_dir(&logs).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// A service with no serving endpoint is left out of discovery and named
/// once in the diagnostic log: three services, two backed endpoints, and
/// the turn runs on a backed model.
#[test]
fn service_without_an_endpoint_is_left_out_and_logged() {
    let setup = Setup::new();
    let server = ProviderServer::start(
        [
            Response::status(200, services_body().to_string()),
            Response::status(200, catalog_body().to_string()),
            Response::status(
                200,
                json!({"endpoints": [{"name": "claude-opus-5"}, {"name": "gpt-6-sol"}]})
                    .to_string(),
            ),
        ]
        .into_iter()
        .chain([hello()]),
    )
    .unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    let ask = run(
        &setup,
        &["ask", "--model", "databricks/databricks-gpt-6-sol", "hi"],
        &credentials(&host),
    );

    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    let stream: Vec<Value> = ask
        .lines
        .iter()
        .filter(|line| line["kind"] != "extension_log")
        .cloned()
        .collect();
    // The omission is a client-facing notice on the ask stream, the way
    // `model_invalid` is: one `notice` event naming the model and the
    // provider's reason (`docs/model-routing.md`, "Model discovery"), with
    // the remaining kinds equal to `HELLO_KINDS`.
    let notices: Vec<&Value> = stream
        .iter()
        .filter(|line| line["kind"] == "notice")
        .collect();
    // One notice per marked entry (`docs/model-routing.md`, "Model discovery").
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["payload"]["code"], "model_omitted");
    assert_eq!(
        notices[0]["payload"]["message"],
        "The model `databricks/databricks-glm-5.3` was left out of the model \
         list: no serving endpoint with a matching name."
    );
    assert_eq!(
        notices[0]["payload"]["extension"],
        "github.com/aakshintala/fiber/providers/databricks"
    );
    let rest: Vec<Value> = stream
        .into_iter()
        .filter(|line| line["kind"] != "notice")
        .collect();
    support::assert_kinds_multiset(&rest, &HELLO_KINDS, &[], None, COUNT_DOC);
    assert_eq!(ask.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/codex/v1/responses");
    // The same discovery behind `fiber models` prints the singleton
    // notice unchanged and lists only the backed models.
    let models = run(&setup, &["models", "--json"], &credentials(&host));
    assert_eq!(models.code, Some(0), "stderr: {}", models.stderr);
    assert!(
        models.stderr.contains(
            "The model `databricks/databricks-glm-5.3` was left out of the model \
             list: no serving endpoint with a matching name.\n"
        ),
        "stderr: {}",
        models.stderr
    );
    // The home also holds the harness's fake provider, so the comparison
    // scopes to the provider under test.
    let mut listed: Vec<&str> = models
        .lines
        .iter()
        .map(|line| line["model"].as_str().unwrap())
        .filter(|model| model.starts_with("databricks/"))
        .collect();
    listed.sort_unstable();
    // Discovery drains the marked entry (`docs/model-routing.md`, "Model discovery"):
    // two backed models listed.
    assert_eq!(
        listed,
        [
            "databricks/databricks-claude-opus-5",
            "databricks/databricks-gpt-6-sol",
        ],
        "{listed:?}"
    );
    let lines = log_lines(&setup);
    let omissions: Vec<&Value> = lines
        .iter()
        .filter(|line| line["code"] == "extension_log")
        .collect();
    // The script backs two of the three services, so `glm-5.3` is the one
    // deliberate omission, and each deliberate omission is named in the
    // diagnostic log (`docs/model-routing.md`, "Model discovery"): one line.
    assert_eq!(omissions.len(), 1, "{lines:?}");
    let message = omissions[0]["message"].as_str().unwrap();
    assert!(
        message.contains(
            "databricks models: databricks-glm-5.3 left out: \
             no serving endpoint with a matching name"
        ),
        "{message}"
    );
    assert!(!message.contains("dapi-test"), "{message}");
    assert!(!message.contains("authorization"), "{message}");
}

/// Two services without endpoints group into one `model_omitted` notice on
/// `fiber models` stderr: the grouped count names both omitted models with
/// their per-item extension attribution, and stdout lists only the backed
/// model. No order across omissions is promised, so the test asserts the
/// set, not which omission leads.
#[test]
fn two_services_without_endpoints_group_into_one_notice() {
    // No harness fake provider, so the list holds only what discovery
    // found, like `models_lists_exactly_the_three_discovered_references`.
    let setup = Setup::without_provider();
    let server = ProviderServer::start([
        Response::status(200, services_body().to_string()),
        Response::status(200, catalog_body().to_string()),
        Response::status(
            200,
            json!({"endpoints": [{"name": "gpt-6-sol"}]}).to_string(),
        ),
    ])
    .unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    let models = run(&setup, &["models", "--json"], &credentials(&host));
    assert_eq!(models.code, Some(0), "stderr: {}", models.stderr);
    // A pass that finds several problems of one code raises one notice whose
    // message gives the count (`docs/errors.md`, "Registry"): two omissions.
    assert!(
        models.stderr.contains("2 `model_omitted` notices:"),
        "stderr: {}",
        models.stderr
    );
    for omitted in [
        "databricks/databricks-claude-opus-5",
        "databricks/databricks-glm-5.3",
    ] {
        assert!(
            models.stderr.contains(&format!(
                "- The model `{omitted}` was left out of the model list: \
                 no serving endpoint with a matching name. \
                 (extension `github.com/aakshintala/fiber/providers/databricks`)"
            )),
            "stderr: {}",
            models.stderr
        );
    }
    // The grouped notice lists each item (`docs/errors.md`, "Registry"): two items.
    assert_eq!(
        models
            .stderr
            .lines()
            .filter(|line| line.starts_with("- "))
            .count(),
        2,
        "stderr: {}",
        models.stderr
    );
    let listed: Vec<&str> = models
        .lines
        .iter()
        .map(|line| line["model"].as_str().unwrap())
        .collect();
    // Discovery drains the marked entries (`docs/model-routing.md`, "Model discovery"):
    // one backed model listed.
    assert_eq!(listed, ["databricks/databricks-gpt-6-sol"], "{listed:?}");
}

/// A warm model cache names the endpoint-less service once: the first ask
/// persists the marked omission, and a later ask turn carries exactly one
/// `model_omitted` notice event for it — the cache admission drains the
/// mark silently and only the Lua admission emits
/// (`docs/model-routing.md`, "Model discovery").
#[test]
fn a_warm_cache_names_the_omission_once_on_a_later_ask() {
    let setup = Setup::new();
    let server = ProviderServer::start(
        [
            Response::status(200, services_body().to_string()),
            Response::status(200, catalog_body().to_string()),
            Response::status(
                200,
                json!({"endpoints": [{"name": "claude-opus-5"}, {"name": "gpt-6-sol"}]})
                    .to_string(),
            ),
        ]
        .into_iter()
        .chain([hello(), hello()]),
    )
    .unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    // The first turn discovers and warms the model cache; the omission is
    // one notice on its stream.
    let first = run(
        &setup,
        &["ask", "--model", "databricks/databricks-gpt-6-sol", "hi"],
        &credentials(&host),
    );
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let first_notices: Vec<&Value> = first
        .lines
        .iter()
        .filter(|line| line["kind"] == "notice")
        .collect();
    // One notice per marked entry (`docs/model-routing.md`, "Model discovery").
    assert_eq!(first_notices.len(), 1, "{first_notices:?}");
    assert_eq!(first_notices[0]["payload"]["code"], "model_omitted");

    // A later turn on the warm cache carries exactly one `model_omitted`
    // notice event for the omitted service, with the same code, message
    // and extension as the cold turn.
    let ask = run(
        &setup,
        &["ask", "--model", "databricks/databricks-gpt-6-sol", "hi"],
        &credentials(&host),
    );
    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    let stream: Vec<Value> = ask
        .lines
        .iter()
        .filter(|line| line["kind"] != "extension_log")
        .cloned()
        .collect();
    let notices: Vec<&Value> = stream
        .iter()
        .filter(|line| line["kind"] == "notice")
        .collect();
    // One notice per marked entry (`docs/model-routing.md`, "Model discovery").
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["payload"]["code"], "model_omitted");
    assert_eq!(
        notices[0]["payload"]["message"],
        "The model `databricks/databricks-glm-5.3` was left out of the model \
         list: no serving endpoint with a matching name."
    );
    assert_eq!(
        notices[0]["payload"]["extension"],
        "github.com/aakshintala/fiber/providers/databricks"
    );
    let rest: Vec<Value> = stream
        .into_iter()
        .filter(|line| line["kind"] != "notice")
        .collect();
    support::assert_kinds_multiset(&rest, &HELLO_KINDS, &[], None, COUNT_DOC);
    assert_eq!(ask.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/codex/v1/responses");
}

/// Every service backed by an endpoint turns with no omission line in the
/// diagnostic log.
#[test]
fn all_services_backed_logs_no_omission() {
    let setup = Setup::new();
    let server =
        ProviderServer::start(discovery().into_iter().chain([completions_hello()])).unwrap();
    let host = install(
        &setup,
        &server,
        include_str!("../../../../providers/databricks/thinking.lua"),
    );

    let run = run(
        &setup,
        &["ask", "--model", "databricks/databricks-glm-5.3", "hi"],
        &credentials(&host),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    support::assert_kinds_multiset(&run.lines, &HELLO_KINDS, &[], None, COUNT_DOC);
    assert_eq!(run.text_completed(), "Hello.");
    assert_turn(&server.requests(), "/ai-gateway/mlflow/v1/chat/completions");
    let lines = log_lines(&setup);
    let omissions: Vec<&Value> = lines
        .iter()
        .filter(|line| {
            line["code"] == "extension_log"
                && line["message"].as_str().unwrap().contains("left out")
        })
        .collect();
    // Every service is backed, so there is no deliberate omission to name
    // (`docs/model-routing.md`, "Model discovery"): no line.
    assert!(omissions.is_empty(), "{lines:?}");
}
