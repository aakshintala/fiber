//! Binary-level test of the first-party vertex package (`docs/testing.md`,
//! "Model calls"): the built `fiber` runs in its own process group with its
//! own `FIBER_HOME`, holding the package copied with its endpoint rewritten to
//! the fake server, a stand-in `gcloud` first on `PATH`, and the project in
//! `GOOGLE_CLOUD_PROJECT`. The package's `credential()` takes the token
//! `gcloud config config-helper` prints, Fiber sends it as the bearer token,
//! and the fake server answers with the probe's recorded Vertex streams
//! (`research/vertex-probe`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::Path;
use std::process::Output;

use fakes::{ProviderServer, Response, fingerprint};
use support::{Run, Setup, is_status, run_to_exit};

/// The probe's recorded call to `get_weather`, a tool this session lacks.
const CALL: &[u8] = include_bytes!("../../../research/vertex-probe/raw/gemini-tool.sse");

/// The probe's recorded "PINEAPPLE", the instruction's own word.
const ANSWER: &[u8] = include_bytes!("../../../research/vertex-probe/raw/gemini-sysrole-none.sse");

/// The body of the probe's recorded 401 for a token Vertex does not take
/// (`research/vertex-probe/raw/gemini-bad-token.json`), its `details`
/// left out.
const GEMINI_BAD_TOKEN: &str = r#"{"error":{"code":401,"message":"Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential. See https://developers.google.com/identity/sign-in/web/devconsole-project.","status":"UNAUTHENTICATED"}}"#;

/// The package's endpoint origin, which the copy points at the fake server.
const ORIGIN: &str = "https://aiplatform.googleapis.com";

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

/// The fake server answering with `responses`, the package installed
/// against it, and the environment for `fiber ask`: the checked-in stand-in
/// `gcloud` (`tests/fixtures/gcloud`) first on `PATH`, noting each run in the
/// returned file, and the project in `GOOGLE_CLOUD_PROJECT`.
fn vertex<const N: usize>(
    setup: &Setup,
    responses: [Response; N],
) -> (
    ProviderServer,
    Vec<(&'static str, String)>,
    std::path::PathBuf,
) {
    let server = ProviderServer::start(responses).unwrap();
    let to = setup.root.path().join("pkg-vertex");
    support::package::copy_package("vertex", &to, ORIGIN, &server.url());
    let installed = run(setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);

    let ran = setup.root.path().join("gcloud-ran");
    let mut paths = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gcloud")];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap().into_string().unwrap();
    let env = vec![
        ("PATH", path),
        ("FIBER_TEST_GCLOUD_RAN", ran.to_str().unwrap().to_owned()),
        ("GOOGLE_CLOUD_PROJECT", "example-project".to_owned()),
    ];
    (server, env, ran)
}

fn borrowed<'a>(env: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    env.iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

#[test]
fn a_gcloud_token_signs_every_call_of_a_turn_and_is_fetched_once() {
    let setup = Setup::new();
    let (server, env, ran) = vertex(&setup, [Response::stream(CALL), Response::stream(ANSWER)]);

    let run = run(
        &setup,
        &["ask", "--model", "vertex/gemini-3.1-flash-lite", "hi"],
        &borrowed(&env),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let kinds = run.kinds();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "extension_exec")
            .count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(run.first("extension_exec")["payload"]["program"], "gcloud");
    assert_eq!(
        run.first("extension_exec")["payload"]["args"],
        serde_json::json!([
            "config",
            "config-helper",
            "--format=json(credential.access_token,credential.token_expiry)"
        ])
    );
    assert_eq!(
        run.first("tool_call_requested")["payload"]["name"],
        "get_weather"
    );
    assert_eq!(kinds.last(), Some(&"fiber_exited"));
    let texts: Vec<&str> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "text_completed")
        .map(|line| line["payload"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["PINEAPPLE"]);
    assert!(
        run.lines
            .iter()
            .all(|line| !line.to_string().contains("vertex-test-token")),
        "the token reached the session log"
    );

    // Two calls, one `gcloud` run: the token is cached until shortly
    // before it expires.
    assert_eq!(std::fs::read_to_string(&ran).unwrap().lines().count(), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    for sent in &requests {
        assert_eq!(
            sent.path,
            "/v1/projects/example-project/locations/global/publishers/google/models/gemini-3.1-flash-lite:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            sent.header("authorization"),
            Some(fingerprint("Bearer vertex-test-token").as_str())
        );
        assert_eq!(sent.header("x-goog-api-key"), None);
    }
}

#[test]
fn a_rejected_token_is_fiber_s_sentence_without_fiber_login() {
    let setup = Setup::new();
    let (server, env, _) = vertex(
        &setup,
        [Response::status(401, GEMINI_BAD_TOKEN).header("content-type", "application/json")],
    );

    let run = run(
        &setup,
        &["ask", "--model", "vertex/gemini-3.1-flash-lite", "hi"],
        &borrowed(&env),
    );

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
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
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    // `fiber login vertex` stores a key `credential()` never reads, so the
    // sentence names no login (`docs/errors.md`, "The shape").
    let error = &run.first("fiber_exited")["payload"]["error"];
    assert_eq!(error["code"], "authentication_failed");
    assert_eq!(
        error["message"],
        "vertex rejected the credential (HTTP 401)."
    );
    let said = error["provider"]["message"].as_str().unwrap();
    assert!(
        said.starts_with("Request had invalid authentication credentials."),
        "{said}"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_session_without_a_project_names_the_setting() {
    let setup = Setup::new();
    let to = setup.root.path().join("pkg-vertex");
    support::package::copy_package("vertex", &to, ORIGIN, "http://127.0.0.1:9");
    let installed = run(&setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);

    let run = run(
        &setup,
        &["ask", "--model", "vertex/gemini-3.1-flash-lite", "hi"],
        &[],
    );

    assert_ne!(run.code, Some(0));
    let text = format!("{:?}{}", run.lines, run.stderr);
    assert!(text.contains("model_unconfigured"), "{text}");
    assert!(
        text.contains("needs the setting `project` for its base URL, which has no value, and `GOOGLE_CLOUD_PROJECT` is not set in the hub's environment (your login shell). Run `fiber login"),
        "{text}"
    );
}
