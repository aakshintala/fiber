//! Binary-level test of the first-party bedrock package (`docs/testing.md`,
//! "Model calls"): the built `fiber` runs in its own process group with its
//! own `FIBER_HOME`, holding the package copied with its endpoint rewritten to
//! the fake server, and a Bedrock API key in `AWS_BEARER_TOKEN_BEDROCK`. The
//! package's `credential()` reads the key through `sh`, its `sign()` leaves
//! it as the bearer token, and the fake server answers with the probe's
//! recorded event stream (`research/bedrock-invoke-probe`).

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

/// The probe's recorded reply to "Say hello in five words.".
const PLAIN: &[u8] = include_bytes!("../../../research/bedrock-invoke-probe/raw/plain.eventstream");

/// The package's endpoint, which the copy points at the fake server.
const ENDPOINT: &str = "https://bedrock-runtime.{region}.amazonaws.com";

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

#[test]
fn a_bedrock_api_key_completes_a_turn_on_the_invoke_api() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response {
        status: 200,
        headers: vec![(
            "content-type".to_owned(),
            "application/vnd.amazon.eventstream".to_owned(),
        )],
        body: PLAIN.to_vec(),
        drop_connection: false,
        stall: false,
    }])
    .unwrap();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package("bedrock", &to, ENDPOINT, "http://{region}");
    let installed = run(&setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);
    // The region setting is the per-account host, read from `AWS_REGION`.
    let host = server.url().strip_prefix("http://").unwrap().to_owned();

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
