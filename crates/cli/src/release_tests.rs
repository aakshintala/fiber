//! The release install step's checks and messages, against a fixture
//! release served from a fake server.

use std::fs;
use std::path::Path;

use contract::ErrorCode;
use contract::shapes::Failure;
use fakes::ustar::{archive, gzip, header, sha256};
use serde_json::json;

use super::{CliError, release_install, run};

/// A release of `anthropic` and `memory`, served at
/// `/download/v0.0.0/`. `extensions_sum` replaces the extensions archive's
/// checksum file when given.
fn serve(extensions_sum: Option<&str>) -> fakes::ProviderServer {
    let manifest = |short: &str, kind: &str| {
        json!({
            "name": format!("github.com/aakshintala/fiber/{kind}/{short}"),
            "version": "0.0.0",
            "fiber": "0.0.0",
            "api": 1,
        })
        .to_string()
    };
    let anthropic = manifest("anthropic", "providers");
    let memory = manifest("memory", "extensions");
    let file = |name: &str, data: &str| header(name, b'0', data.len() as u64, 0o644, "");
    let docs = gzip(&archive(&[(file("README.md", "docs"), b"docs")]));
    let extensions = gzip(&archive(&[
        (
            file("anthropic/extension.json", &anthropic),
            anthropic.as_bytes(),
        ),
        (file("memory/extension.json", &memory), memory.as_bytes()),
    ]));
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let extensions_sum = extensions_sum.map_or_else(|| sha256(&extensions), str::to_owned);
    fakes::ProviderServer::start_routed(
        [
            ("/download/v0.0.0/fiber-docs.tar.gz", ok(docs.clone())),
            (
                "/download/v0.0.0/fiber-docs.tar.gz.sha256",
                ok(sha256(&docs).into_bytes()),
            ),
            ("/download/v0.0.0/fiber-extensions.tar.gz", ok(extensions)),
            (
                "/download/v0.0.0/fiber-extensions.tar.gz.sha256",
                ok(extensions_sum.into_bytes()),
            ),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// Runs the step against `server` for a binary of version `0.0.0`.
fn step(
    home: &Path,
    server: &fakes::ProviderServer,
    version: &str,
    commit: Option<&str>,
) -> (Result<(), CliError>, String) {
    let mut err = Vec::new();
    let result = run(
        home,
        version,
        Some(&server.url()),
        "0.0.0",
        commit,
        &*fakes::clock::FakeClock::new(),
        &mut err,
    );
    (result, String::from_utf8(err).unwrap())
}

#[test]
fn another_version_is_refused_before_any_request() {
    let held = fakes::TempDir::new("fiber-release-cli");
    let home = held.path().join("home");
    let server = serve(None);
    let (result, _) = step(&home, &server, "0.0.1", Some("abc1234"));
    let failure = result.unwrap_err();
    let CliError::ReleaseVersionMismatch {
        fiber_version,
        version,
    } = &failure
    else {
        panic!("expected ReleaseVersionMismatch, got {failure:?}");
    };
    assert_eq!(fiber_version, "0.0.0");
    assert_eq!(version, "0.0.1");
    assert_eq!(failure.code(), ErrorCode::Usage);
    assert!(server.requests().is_empty());
    assert!(!home.exists());
}

#[test]
fn a_build_with_no_commit_is_refused_before_any_request() {
    for commit in [None, Some("")] {
        let held = fakes::TempDir::new("fiber-release-cli");
        let home = held.path().join("home");
        let server = serve(None);
        let (result, _) = step(&home, &server, "0.0.0", commit);
        let failure = result.unwrap_err();
        assert_eq!(doors::exit_code(&Failure::from(&failure)), 1, "{commit:?}");
        assert!(
            matches!(failure, CliError::ReleaseNoCommit),
            "expected ReleaseNoCommit, got {failure:?}"
        );
        assert_eq!(failure.code(), ErrorCode::ExtensionIncompatible);
        assert!(server.requests().is_empty(), "{commit:?}");
        assert!(!home.exists());
    }
}

#[test]
fn success_names_each_extension_then_the_docs() {
    let held = fakes::TempDir::new("fiber-release-cli");
    let home = held.path().join("home");
    let server = serve(None);
    let (result, err) = step(&home, &server, "0.0.0", Some("abc1234"));
    result.unwrap();
    assert_eq!(
        err,
        "fiber: installed memory\nfiber: installed anthropic\nfiber: installed docs\n"
    );
    assert_eq!(
        fs::read_to_string(home.join("docs/README.md")).unwrap(),
        "docs"
    );
}

#[test]
fn a_checksum_mismatch_is_io_failed_and_creates_no_home() {
    let held = fakes::TempDir::new("fiber-release-cli");
    let home = held.path().join("home");
    let server = serve(Some(&sha256(b"other")));
    let (result, err) = step(&home, &server, "0.0.0", Some("abc1234"));
    let failure = result.unwrap_err();
    assert_eq!(failure.code(), ErrorCode::IoFailed);
    assert!(
        matches!(failure, CliError::Extensions(_)),
        "expected Extensions, got {failure:?}"
    );
    assert!(
        failure
            .to_string()
            .contains("does not match its .sha256 file"),
        "{failure}"
    );
    assert!(err.is_empty());
    assert!(!home.exists());
}

#[test]
fn release_install_exits_2_for_another_version() {
    // Another version is refused before any request, and Fiber home is
    // only resolved, never created; a refusal is a usage error, exit 2.
    let clock = fakes::clock::FakeClock::new();
    let code = release_install(
        "9.9.9",
        Some("http://127.0.0.1:9"),
        "0.0.0",
        Some("abc"),
        &*clock,
    );
    assert_eq!(code, 2);
}

/// A debug install logs one `extension_release` line per download to the
/// pid-named CLI log: the two archives and their checksum files, each
/// with the server's host and path, status 200, and the served count.
#[test]
fn a_debug_install_logs_one_extension_release_line_per_download() {
    let held = fakes::TempDir::new("fiber-release-cli");
    let home = held.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("config.json"),
        serde_json::json!({"diagnostics": {"level": "debug"}}).to_string(),
    )
    .unwrap();
    let server = serve(None);
    let (result, _) = step(&home, &server, "0.0.0", Some("abc1234"));
    result.unwrap();
    let name = format!("cli-{}.log", std::process::id());
    let text = fs::read_to_string(home.join("logs").join(&name)).unwrap();
    let host = server.url().trim_start_matches("http://").to_owned();
    let requests: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|line: &serde_json::Value| line["code"] == "http_request")
        .filter(|line: &serde_json::Value| {
            line["data"].get("host").and_then(|host| host.as_str()) == Some(host.as_str())
        })
        .collect();
    // docs/state.md, "Diagnostic logs": one line per tag lookup, checksum and binary download.
    assert_eq!(
        requests.len(),
        4,
        "two archives and their checksums: {requests:?}"
    );
    let mut paths: Vec<&str> = requests
        .iter()
        .map(|line| line["data"]["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/download/v0.0.0/fiber-docs.tar.gz",
            "/download/v0.0.0/fiber-docs.tar.gz.sha256",
            "/download/v0.0.0/fiber-extensions.tar.gz",
            "/download/v0.0.0/fiber-extensions.tar.gz.sha256",
        ]
    );
    for request in &requests {
        assert_eq!(request["process"], "cli");
        assert!(
            request.get("session_id").is_none(),
            "no session on a one-shot command: {request}"
        );
        let data = &request["data"];
        assert_eq!(data["purpose"], "extension_release", "{data}");
        assert_eq!(data["status"], 200, "{data}");
        assert_eq!(data["attempt"], 1, "{data}");
        assert_eq!(data["request_bytes"], 0, "{data}");
        assert!(data.get("provider").is_none(), "{data}");
        assert!(data.get("extension").is_none(), "{data}");
        assert!(data.get("model").is_none(), "{data}");
    }
}

/// An info install writes no CLI log: the release downloads stay silent.
#[test]
fn an_info_install_writes_no_cli_log() {
    let held = fakes::TempDir::new("fiber-release-cli");
    let home = held.path().join("home");
    let server = serve(None);
    let (result, _) = step(&home, &server, "0.0.0", Some("abc1234"));
    result.unwrap();
    assert!(
        !home.join("logs").exists(),
        "no log without debug: {:?}",
        home.join("logs")
    );
}
