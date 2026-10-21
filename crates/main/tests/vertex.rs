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

use std::os::unix::fs::PermissionsExt as _;
use std::process::Output;

use fakes::{ProviderServer, Response, fingerprint};
use support::{Run, Setup, is_status, run_to_exit};

/// The probe's recorded call to `get_weather`, a tool this session lacks.
const CALL: &[u8] = include_bytes!("../../../research/vertex-probe/raw/gemini-tool.sse");

/// The probe's recorded "PINEAPPLE", the instruction's own word.
const ANSWER: &[u8] = include_bytes!("../../../research/vertex-probe/raw/gemini-sysrole-none.sse");

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

#[test]
fn a_gcloud_token_signs_every_call_of_a_turn_and_is_fetched_once() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::stream(CALL), Response::stream(ANSWER)]).unwrap();
    let to = setup.root.path().join("pkg-vertex");
    support::package::copy_package("vertex", &to, ORIGIN, &server.url());
    let installed = run(&setup, &["extension", "install", to.to_str().unwrap()], &[]);
    assert_eq!(installed.code, Some(0), "stderr: {}", installed.stderr);

    // A stand-in `gcloud` that notes each run and prints a token that
    // expires long after the test.
    let bin = setup.root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ran = setup.root.path().join("gcloud-ran");
    let gcloud = bin.join("gcloud");
    std::fs::write(
        &gcloud,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\nprintf '%s' '{{\"credential\": {{\"access_token\": \"vertex-test-token\", \"token_expiry\": \"2099-01-01T00:00:00Z\"}}}}'\n",
            ran.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gcloud, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run = run(
        &setup,
        &["ask", "--model", "vertex/gemini-3.1-flash-lite", "hi"],
        &[("PATH", &path), ("GOOGLE_CLOUD_PROJECT", "example-project")],
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
