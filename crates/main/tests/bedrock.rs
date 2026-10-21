//! Binary-level test of the first-party bedrock package (`docs/testing.md`,
//! "Model calls"): the built `fiber` runs in its own process group with its
//! own `FIBER_HOME`, holding the package copied with its endpoint rewritten to
//! the fake server, and a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`. The
//! package's `credential()` reads the key through `sh`, its `sign()` leaves
//! it as the bearer token, and the fake server answers with a probe's
//! recorded event stream (`research/bedrock-invoke-probe`,
//! `research/bedrock-converse-probe`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::process::Output;

use fakes::{ProviderServer, Response, fingerprint};
use serde_json::Value;
use support::{Run, Setup, is_status, run_to_exit};

/// The Invoke probe's recorded reply to "Say hello in five words.".
const PLAIN: &[u8] = include_bytes!("../../../research/bedrock-invoke-probe/raw/plain.eventstream");

/// The Converse probe's recorded reply from Ministral 3 8B to the same.
const CONVERSE_PLAIN: &[u8] =
    include_bytes!("../../../research/bedrock-converse-probe/raw/plain.eventstream");

/// The package's endpoint, which the copy points at the fake server.
const ENDPOINT: &str = "https://bedrock-runtime.{region}.amazonaws.com";

/// The Invoke probe's recorded 403 for a malformed API key
/// (`research/bedrock-invoke-probe/raw/bad-bearer.json`): its body and the
/// exception its `x-amzn-ErrorType` names.
const BAD_BEARER: &str = r#"{"Message":"Invalid API Key format: Delimiter ':' not found"}"#;
const ACCESS_DENIED: &str =
    "AccessDeniedException:http://internal.amazon.com/coral/com.amazon.coral.service/";

/// Parses `fiber ask` stdout into a run: one JSON value per line, without
/// `session_status` lines, which an observer thread writes.
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

/// A synthetic request id: a fixed fake UUID, never one AWS issued.
const REQUEST_ID: &str = "11111111-2222-4333-8444-555555555555";

/// The event kinds of one Converse turn that answers in text.
const CONVERSE_TURN: [&str; 15] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "extension_exec",
    "preamble_built",
    "opening_message",
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

/// The event kinds of one turn whose only model call fails.
const FAILED_TURN: [&str; 13] = [
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "extension_exec",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The fake server answering once with `body` as Bedrock frames it, naming
/// the request in `x-amzn-RequestId` when `request_id` is set, and the
/// installed package's region: the server's host, read from `AWS_REGION`.
fn bedrock(setup: &Setup, body: &[u8], request_id: Option<&str>) -> (ProviderServer, String) {
    let mut headers = vec![(
        "content-type".to_owned(),
        "application/vnd.amazon.eventstream".to_owned(),
    )];
    if let Some(id) = request_id {
        headers.push(("x-amzn-RequestId".to_owned(), id.to_owned()));
    }
    bedrock_answering(
        setup,
        Response {
            status: 200,
            headers,
            body: body.to_vec(),
            drop_connection: false,
            stall: false,
        },
    )
}

/// The fake server answering once with `response`, and the installed
/// package's region: the server's host, read from `AWS_REGION`.
fn bedrock_answering(setup: &Setup, response: Response) -> (ProviderServer, String) {
    let server = ProviderServer::start([response]).unwrap();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package("bedrock", &to, ENDPOINT, "http://{region}");
    let installed = run(setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);
    // The region setting is the per-account host, read from `AWS_REGION`.
    let host = server.url().strip_prefix("http://").unwrap().to_owned();
    (server, host)
}

#[test]
fn a_bedrock_api_key_completes_a_turn_on_the_invoke_api() {
    let setup = Setup::new();
    let (server, host) = bedrock(&setup, PLAIN, None);

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "bedrock/global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "hi",
        ],
        &[
            ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
            ("AWS_REGION", &host),
        ],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "extension_exec",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    // `credential()` reading the environment is logged, but never the key.
    assert_eq!(run.first("extension_exec")["payload"]["program"], "/bin/sh");
    assert!(
        run.lines
            .iter()
            .all(|line| !line.to_string().contains("bedrock-test-key")),
        "the key reached the session log"
    );
    assert_eq!(
        run.first("text_completed")["payload"]["text"],
        "Hello, how are you today?"
    );
    let usage = &run.first("usage_recorded")["payload"];
    assert_eq!(
        usage["generation_id"],
        "msg_bdrk_wo53fgumh24epnbvvdongfrupvcopsptfh6v6pmfag7knuem3k3a"
    );
    assert_eq!(usage["tokens"]["input"], 13);
    assert_eq!(usage["tokens"]["output"], 10);

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let sent = &requests[0];
    assert_eq!(
        sent.path,
        "/model/global.anthropic.claude-haiku-4-5-20251001-v1%3A0/invoke-with-response-stream"
    );
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer bedrock-test-key").as_str())
    );
    assert_eq!(
        sent.header("accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(sent.header("x-amz-date"), None);
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
    assert!(body.get("model").is_none(), "{body}");
    assert!(body.get("stream").is_none(), "{body}");
}

#[test]
fn a_bedrock_api_key_completes_a_turn_on_the_converse_api() {
    let setup = Setup::new();
    let (server, host) = bedrock(&setup, CONVERSE_PLAIN, Some(REQUEST_ID));

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "bedrock/mistral.ministral-3-8b-instruct",
            "hi",
        ],
        &[
            ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
            ("AWS_REGION", &host),
        ],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), CONVERSE_TURN);
    // `credential()` reading the environment is logged, but never the key.
    assert_eq!(run.first("extension_exec")["payload"]["program"], "/bin/sh");
    assert!(
        run.lines
            .iter()
            .all(|line| !line.to_string().contains("bedrock-test-key")),
        "the key reached the session log"
    );
    assert_eq!(
        run.first("text_completed")["payload"]["text"],
        "Hello! How\u{2019}s it goin\u{2019} now?"
    );
    // AWS's request id names the generation, so the model's declared prices
    // apply: 15 input and 11 output tokens at $0.15 per million each.
    let usage = &run.first("usage_recorded")["payload"];
    assert_eq!(usage["generation_id"], REQUEST_ID);
    assert_eq!(usage["tokens"]["input"], 15);
    assert_eq!(usage["tokens"]["output"], 11);
    let cost = usage["cost"].as_f64().unwrap();
    assert!((cost - 26.0 * 0.15 / 1e6).abs() < 1e-12, "{cost}");

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let sent = &requests[0];
    assert_eq!(
        sent.path,
        "/model/mistral.ministral-3-8b-instruct/converse-stream"
    );
    assert_eq!(
        sent.header("authorization"),
        Some(fingerprint("Bearer bedrock-test-key").as_str())
    );
    assert_eq!(
        sent.header("accept"),
        Some("application/vnd.amazon.eventstream")
    );
    assert_eq!(sent.header("x-amz-date"), None);
    let body: Value = serde_json::from_slice(&sent.body).unwrap();
    // The package's data for the model: its output limit, no cache marker,
    // and Fiber's tools as `toolSpec`s with no `strict`.
    assert_eq!(
        body["inferenceConfig"],
        serde_json::json!({"maxTokens": 4096})
    );
    assert!(!body.to_string().contains("cachePoint"), "{body}");
    let tools = body["toolConfig"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty(), "{body}");
    assert!(
        tools
            .iter()
            .all(|tool| tool["toolSpec"]["name"].is_string()
                && tool["toolSpec"].get("strict").is_none()),
        "{body}"
    );
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn a_converse_reply_without_a_request_id_is_minted_its_id_and_left_unpriced() {
    let setup = Setup::new();
    let (_server, host) = bedrock(&setup, CONVERSE_PLAIN, None);

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "bedrock/mistral.ministral-3-8b-instruct",
            "hi",
        ],
        &[
            ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
            ("AWS_REGION", &host),
        ],
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), CONVERSE_TURN);
    let usage = &run.first("usage_recorded")["payload"];
    let id = usage["generation_id"].as_str().unwrap();
    assert!(id.starts_with("fiber-"), "{id}");
    assert_eq!(usage["tokens"]["input"], 15);
    assert_eq!(usage["cost"], Value::Null);
}

#[test]
fn an_access_denied_reply_is_fiber_s_sentence_without_fiber_login() {
    let setup = Setup::new();
    let (server, host) = bedrock_answering(
        &setup,
        Response::status(403, BAD_BEARER).header("x-amzn-ErrorType", ACCESS_DENIED),
    );

    let run = run(
        &setup,
        &[
            "ask",
            "--model",
            "bedrock/global.anthropic.claude-haiku-4-5-20251001-v1:0",
            "hi",
        ],
        &[
            ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
            ("AWS_REGION", &host),
        ],
    );

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), FAILED_TURN);
    // `fiber login bedrock` stores a key `credential()` never reads, so the
    // sentence names no login (`docs/errors.md`, "The shape").
    let error = &run.first("fiber_exited")["payload"]["error"];
    assert_eq!(error["code"], "authentication_failed");
    assert_eq!(
        error["message"],
        "bedrock rejected the credential (HTTP 403)."
    );
    assert_eq!(
        error["provider"]["message"],
        "Invalid API Key format: Delimiter ':' not found"
    );
    assert_eq!(server.requests().len(), 1);
}
