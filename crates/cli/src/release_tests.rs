//! The release install step's checks and messages, against a fixture
//! release served from a fake server.

use std::fs;
use std::path::Path;

use contract::ErrorCode;
use fakes::ustar::{archive, gzip, header, sha256};
use serde_json::json;

use super::run;

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
) -> (Result<(), contract::shapes::Failure>, String) {
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
    assert_eq!(failure.code, ErrorCode::Usage);
    assert_eq!(
        failure.message,
        "This is Fiber 0.0.0; it cannot install the docs and extensions of 0.0.1."
    );
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
        assert_eq!(doors::exit_code(&failure), 1, "{commit:?}");
        assert_eq!(
            failure.message,
            "This build records no commit, so it cannot install a release's extensions."
        );
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
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert!(
        failure.message.contains("does not match its .sha256 file"),
        "{}",
        failure.message
    );
    assert!(err.is_empty());
    assert!(!home.exists());
}
