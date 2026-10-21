//! The release step's parts: the checksum file, the URL, the docs swap with
//! renames that fail on purpose, a commit that cannot be put back, and the
//! scratch directory.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;

use fakes::ustar::{archive, gzip, header, sha256 as sha};

use super::{Release, expected_digest, install_release_with, swap_docs, url};
use crate::Error;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn a_checksum_file_holds_its_digest_first() {
    let upper = DIGEST.to_ascii_uppercase();
    let cases: [(String, Option<&str>); 11] = [
        (DIGEST.into(), Some(DIGEST)),
        (format!("{DIGEST}  fiber-docs.tar.gz\n"), Some(DIGEST)),
        (upper, Some(DIGEST)),
        (format!("{DIGEST}\r\n"), Some(DIGEST)),
        (format!("  {DIGEST}\n"), Some(DIGEST)),
        (String::new(), None),
        ("   ".into(), None),
        (DIGEST[..63].into(), None),
        (format!("{DIGEST}0"), None),
        (format!("g{}", &DIGEST[1..]), None),
        (format!("{}  x", &DIGEST[..63]), None),
    ];
    for (text, want) in cases {
        assert_eq!(expected_digest(&text).as_deref(), want, "{text:?}");
    }
}

#[test]
fn a_release_file_is_under_its_version() {
    for base in ["http://x/releases", "http://x/releases/"] {
        assert_eq!(
            url(base, "0.3.0", "fiber-docs.tar.gz"),
            "http://x/releases/download/v0.3.0/fiber-docs.tar.gz"
        );
    }
}

/// A Fiber home whose `docs/` holds `old`, and staged docs holding `new`.
struct Docs {
    held: fakes::TempDir,
}

impl Docs {
    fn new(with_old: bool) -> Self {
        let held = fakes::TempDir::new("fiber-swap-docs");
        fs::create_dir(held.path().join(".docs.new")).unwrap();
        fs::write(held.path().join(".docs.new/v"), "new").unwrap();
        if with_old {
            fs::create_dir(held.path().join("docs")).unwrap();
            fs::write(held.path().join("docs/v"), "old").unwrap();
        }
        Self { held }
    }

    fn at(&self, name: &str) -> std::path::PathBuf {
        self.held.path().join(name)
    }

    /// Swaps with a rename that fails on each call number in `fail`.
    fn swap(&self, fail: &[usize]) -> Result<(), Error> {
        let calls = Cell::new(0);
        swap_docs(
            &self.at(".docs.new"),
            &self.at("docs"),
            &self.at(".docs.old"),
            &|from: &Path, to: &Path| {
                calls.set(calls.get() + 1);
                if fail.contains(&calls.get()) {
                    Err(io::Error::other("injected"))
                } else {
                    fs::rename(from, to)
                }
            },
        )
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.held.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[test]
fn the_docs_swap_puts_the_new_docs_in_place() {
    for with_old in [true, false] {
        let docs = Docs::new(with_old);
        docs.swap(&[]).unwrap();
        assert_eq!(fs::read_to_string(docs.at("docs/v")).unwrap(), "new");
        assert_eq!(docs.names(), ["docs"], "with old docs: {with_old}");
    }
}

#[test]
fn a_failed_docs_swap_puts_the_old_docs_back() {
    // Moves: docs aside, new docs in place (fails), old docs back.
    let docs = Docs::new(true);
    let err = docs.swap(&[2]).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
    assert_eq!(fs::read_to_string(docs.at("docs/v")).unwrap(), "old");
    assert_eq!(docs.names(), [".docs.new", "docs"]);
}

#[test]
fn old_docs_that_cannot_be_put_back_stay_where_the_error_says() {
    let docs = Docs::new(true);
    let err = docs.swap(&[2, 3]).unwrap_err();
    let Error::Rollback { stuck, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(stuck, &[docs.at(".docs.old")]);
    assert!(err.to_string().contains(".docs.old"), "{err}");
    assert_eq!(fs::read_to_string(docs.at(".docs.old/v")).unwrap(), "old");
}

#[test]
fn a_failed_first_install_of_docs_is_the_rename_error() {
    let docs = Docs::new(false);
    let err = docs.swap(&[1, 2]).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err:?}");
    assert_eq!(docs.names(), [".docs.new"]);
}

#[test]
fn a_failed_move_aside_leaves_the_docs() {
    let docs = Docs::new(true);
    docs.swap(&[1]).unwrap_err();
    assert_eq!(fs::read_to_string(docs.at("docs/v")).unwrap(), "old");
}

#[test]
fn docs_that_cannot_be_looked_at_are_an_error_before_any_rename() {
    // `file/docs` cannot be looked at, since `file` is not a directory: the
    // error is that, not a first install's rename.
    let docs = Docs::new(false);
    fs::write(docs.at("file"), "x").unwrap();
    let calls = Cell::new(0);
    let err = swap_docs(
        &docs.at(".docs.new"),
        &docs.at("file/docs"),
        &docs.at(".docs.old"),
        &|_: &Path, _: &Path| {
            calls.set(calls.get() + 1);
            Ok(())
        },
    )
    .unwrap_err();
    let Error::Io { path, source } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(path, &docs.at("file/docs"));
    assert_eq!(source.kind(), io::ErrorKind::NotADirectory, "{err}");
    assert_eq!(calls.get(), 0);
}

/// A release with `docs/v` holding `docs`, and `anthropic` and `memory`.
fn serve(docs: &str) -> fakes::ProviderServer {
    let manifest = |short: &str, kind: &str| {
        serde_json::json!({
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
    let docs_gz = gzip(&archive(&[(file("v", docs), docs.as_bytes())]));
    let extensions_gz = gzip(&archive(&[
        (
            file("anthropic/extension.json", &anthropic),
            anthropic.as_bytes(),
        ),
        (file("memory/extension.json", &memory), memory.as_bytes()),
    ]));
    let route = |file: &str| format!("/download/v0.0.0/{file}");
    let ok = |body: &[u8]| fakes::Response::status(200, body.to_vec());
    let paths = [
        route("fiber-docs.tar.gz"),
        route("fiber-docs.tar.gz.sha256"),
        route("fiber-extensions.tar.gz"),
        route("fiber-extensions.tar.gz.sha256"),
    ];
    let bodies = [
        ok(&docs_gz),
        ok(sha(&docs_gz).as_bytes()),
        ok(&extensions_gz),
        ok(sha(&extensions_gz).as_bytes()),
    ];
    fakes::ProviderServer::start_routed(
        paths.iter().map(String::as_str).zip(bodies),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

fn release(server: &fakes::ProviderServer, commit: &str) -> Release {
    Release {
        base: server.url(),
        version: "0.0.0".into(),
        commit: commit.into(),
    }
}

fn commit_of(home: &Path, short: &str) -> String {
    let text = fs::read_to_string(home.join("extensions").join(short).join(".fiber.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["source"]["commit"].as_str().unwrap().to_owned()
}

#[test]
fn a_commit_that_cannot_be_put_back_is_finished_by_the_next_lock() {
    let held = fakes::TempDir::new("fiber-release-rollback");
    let home = held.path().join("home");
    let clock = fakes::clock::FakeClock::new();
    let rename = |from: &Path, to: &Path| fs::rename(from, to);
    install_release_with(&home, &release(&serve("one"), "aaa"), &*clock, &rename).unwrap();

    // Moves: anthropic aside, anthropic in place, memory aside, memory in
    // place (fails), then every move back fails too.
    let calls = Cell::new(0);
    let failing = |from: &Path, to: &Path| {
        calls.set(calls.get() + 1);
        if calls.get() >= 4 {
            Err(io::Error::other("injected"))
        } else {
            fs::rename(from, to)
        }
    };
    let err =
        install_release_with(&home, &release(&serve("two"), "bbb"), &*clock, &failing).unwrap_err();
    assert!(matches!(err, Error::Rollback { .. }), "{err:?}");
    assert!(home.join("extensions/.commit").exists());
    assert_eq!(fs::read_to_string(home.join("docs/v")).unwrap(), "one");
    let docs_left = fs::read_dir(&home)
        .unwrap()
        .filter(|e| {
            let name = e.as_ref().unwrap().file_name();
            name.to_string_lossy().starts_with(".docs-")
        })
        .count();
    assert_eq!(docs_left, 0);

    let listed = crate::list(&home, &*clock).unwrap();
    assert_eq!(listed.installed.len(), 2);
    assert!(!home.join("extensions/.commit").exists());
    assert_eq!(commit_of(&home, "anthropic"), "aaa");
    assert_eq!(commit_of(&home, "memory"), "aaa");
}

/// This process's scratch directories left in the system temporary
/// directory.
fn scratch_left() -> Vec<String> {
    let prefix = format!("fiber-release-{}-", std::process::id());
    fs::read_dir(std::env::temp_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix))
        .collect()
}

#[test]
fn the_scratch_directory_is_removed_after_success_and_after_a_refusal() {
    let held = fakes::TempDir::new("fiber-release-scratch");
    let home = held.path().join("home");
    let clock = fakes::clock::FakeClock::new();
    let rename = |from: &Path, to: &Path| fs::rename(from, to);
    install_release_with(&home, &release(&serve("one"), "aaa"), &*clock, &rename).unwrap();
    assert_eq!(scratch_left(), Vec::<String>::new());

    let bad = gzip(&archive(&[(header("acme/x", b'0', 0, 0o644, ""), b"")]));
    let docs = gzip(&archive(&[(header("v", b'0', 0, 0o644, ""), b"")]));
    let ok = |body: &[u8]| fakes::Response::status(200, body.to_vec());
    let server = fakes::ProviderServer::start_routed(
        [
            ("/download/v0.0.0/fiber-docs.tar.gz", ok(&docs)),
            (
                "/download/v0.0.0/fiber-docs.tar.gz.sha256",
                ok(sha(&docs).as_bytes()),
            ),
            ("/download/v0.0.0/fiber-extensions.tar.gz", ok(&bad)),
            (
                "/download/v0.0.0/fiber-extensions.tar.gz.sha256",
                ok(sha(&bad).as_bytes()),
            ),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let err = install_release_with(&home, &release(&server, "aaa"), &*clock, &rename).unwrap_err();
    assert!(matches!(err, Error::BadArchive { .. }), "{err:?}");
    assert_eq!(scratch_left(), Vec::<String>::new());
}

/// A release `fiber-update-fixture` serving a binary archive holding
/// `marker`, the docs and extensions archives, and a release list holding
/// `nightlies`.
fn update_fixture(marker: &[u8], nightlies: serde_json::Value) -> fakes::ProviderServer {
    let binary = gzip(&archive(&[(
        header("fiber", b'0', marker.len() as u64, 0o755, ""),
        marker,
    )]));
    let docs = gzip(&archive(&[(header("v", b'0', 3, 0o644, ""), b"new")]));
    let extensions = gzip(&archive(&[]));
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    fakes::ProviderServer::start_routed(
        [
            (
                "/download/v0.2.0/fiber-test-triple.tar.gz",
                ok(binary.clone()),
            ),
            (
                "/download/v0.2.0/fiber-test-triple.tar.gz.sha256",
                ok(sha(&binary).into_bytes()),
            ),
            ("/download/v0.2.0/fiber-docs.tar.gz", ok(docs.clone())),
            (
                "/download/v0.2.0/fiber-docs.tar.gz.sha256",
                ok(sha(&docs).into_bytes()),
            ),
            (
                "/download/v0.2.0/fiber-extensions.tar.gz",
                ok(extensions.clone()),
            ),
            (
                "/download/v0.2.0/fiber-extensions.tar.gz.sha256",
                ok(sha(&extensions).into_bytes()),
            ),
            ("/releases", ok(nightlies.to_string().into_bytes())),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

fn fixture_nightly_list() -> serde_json::Value {
    serde_json::json!([
        {"tag_name": "v0.2.0", "prerelease": false, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240102", "prerelease": true, "draft": false},
        {"tag_name": "v0.2.9-nightly.20241231", "prerelease": true, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240101", "prerelease": true, "draft": false},
    ])
}

fn fixture_release(server: &fakes::ProviderServer, commit: &str) -> Release {
    Release {
        base: server.url(),
        version: "0.2.0".into(),
        commit: commit.into(),
    }
}

#[test]
fn fetch_verified_returns_the_checked_bytes() {
    let server = update_fixture(b"marker", fixture_nightly_list());
    let release = fixture_release(&server, "abc1234");
    let bytes = super::fetch_verified(&release, "fiber-test-triple.tar.gz").unwrap();
    let archived = gzip(&archive(&[(
        header("fiber", b'0', 6, 0o755, ""),
        b"marker".as_slice(),
    )]));
    assert_eq!(bytes, archived);
}

#[test]
fn fetch_verified_refuses_a_tampered_archive() {
    let binary = gzip(&archive(&[(
        header("fiber", b'0', 6, 0o755, ""),
        b"marker",
    )]));
    let server = fakes::ProviderServer::start_routed(
        [
            (
                "/download/v0.2.0/fiber-test-triple.tar.gz",
                fakes::Response::status(200, b"tampered".to_vec()),
            ),
            (
                "/download/v0.2.0/fiber-test-triple.tar.gz.sha256",
                fakes::Response::status(200, sha(&binary).into_bytes()),
            ),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let release = fixture_release(&server, "abc1234");
    let err = super::fetch_verified(&release, "fiber-test-triple.tar.gz").unwrap_err();
    let Error::ArchiveChecksum { archive, .. } = &err else {
        panic!("a tampered archive fails its checksum: {err}");
    };
    assert_eq!(archive, "fiber-test-triple.tar.gz");
}

#[test]
fn fetch_verified_names_a_missing_checksum_file() {
    let server = fakes::ProviderServer::start_routed(
        [(
            "/download/v0.2.0/fiber-test-triple.tar.gz",
            fakes::Response::status(200, b"bytes".to_vec()),
        )],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let release = fixture_release(&server, "abc1234");
    let err = super::fetch_verified(&release, "fiber-test-triple.tar.gz").unwrap_err();
    let Error::Download { name, .. } = &err else {
        panic!("a missing checksum fails the download: {err}");
    };
    assert_eq!(name, "fiber-test-triple.tar.gz.sha256");
}

#[test]
fn unpack_release_archive_writes_the_members_under_the_directory() {
    let marker = b"marker";
    let archive = gzip(&archive(&[(
        header("fiber", b'0', marker.len() as u64, 0o755, ""),
        marker.as_slice(),
    )]));
    let held = fakes::TempDir::new("fiber-unpack-release");
    let into = held.path().join("binary");
    std::fs::create_dir(&into).unwrap();
    super::unpack_release_archive(&archive, &into, "fiber-test-triple.tar.gz").unwrap();
    assert_eq!(std::fs::read(into.join("fiber")).unwrap(), marker);
}

#[test]
fn unpack_release_archive_names_a_refused_archive() {
    let held = fakes::TempDir::new("fiber-unpack-release-bad");
    let into = held.path().join("binary");
    std::fs::create_dir(&into).unwrap();
    let err =
        super::unpack_release_archive(b"not gzip", &into, "fiber-test-triple.tar.gz").unwrap_err();
    let Error::BadArchive { archive, .. } = &err else {
        panic!("a refused archive names itself: {err}");
    };
    assert_eq!(archive, "fiber-test-triple.tar.gz");
}

#[test]
fn staged_docs_swap_into_an_absent_or_present_docs_directory() {
    for with_old in [true, false] {
        let held = fakes::TempDir::new("fiber-stage-docs");
        let home = held.path().join("home");
        std::fs::create_dir(&home).unwrap();
        if with_old {
            std::fs::create_dir(home.join("docs")).unwrap();
            std::fs::write(home.join("docs/v"), "old").unwrap();
        }
        let docs = gzip(&archive(&[(header("v", b'0', 3, 0o644, ""), b"new")]));
        let scratch = super::Scratch::create(held.path(), "fiber-stage-docs", 0o700).unwrap();
        let fresh = super::stage_docs_from(&home, &docs, scratch.path()).unwrap();
        assert!(fresh.starts_with(&home));
        super::swap_docs_in(&home, &fresh).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("docs/v")).unwrap(), "new");
    }
}

#[test]
fn staged_docs_refuse_an_empty_archive_before_any_rename() {
    let held = fakes::TempDir::new("fiber-stage-docs-empty");
    let scratch_held = fakes::TempDir::new("fiber-stage-docs-empty-scratch");
    let scratch =
        super::Scratch::create(scratch_held.path(), "fiber-stage-docs-empty", 0o700).unwrap();
    let err =
        super::stage_docs_from(held.path(), &gzip(&archive(&[])), scratch.path()).unwrap_err();
    let Error::BadArchive { archive, why } = &err else {
        panic!("empty docs fail the staging: {err}");
    };
    assert_eq!(archive, "fiber-docs.tar.gz");
    assert!(why.contains("no entries"), "{err}");
    assert!(held.path().read_dir().unwrap().next().is_none());
}

#[test]
fn install_docs_if_missing_leaves_present_docs_alone() {
    let held = fakes::TempDir::new("fiber-docs-if-missing");
    let home = held.path().join("home");
    std::fs::create_dir_all(home.join("docs")).unwrap();
    std::fs::write(home.join("docs/v"), "old").unwrap();
    let server = update_fixture(b"marker", fixture_nightly_list());
    super::install_docs_if_missing(&home, &fixture_release(&server, "abc1234")).unwrap();
    assert_eq!(std::fs::read_to_string(home.join("docs/v")).unwrap(), "old");
    assert!(server.requests().is_empty());
}

#[test]
fn install_docs_if_missing_installs_absent_docs() {
    let held = fakes::TempDir::new("fiber-docs-if-absent");
    let home = held.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let server = update_fixture(b"marker", fixture_nightly_list());
    super::install_docs_if_missing(&home, &fixture_release(&server, "abc1234")).unwrap();
    assert_eq!(std::fs::read_to_string(home.join("docs/v")).unwrap(), "new");
}

#[test]
fn fetch_nightly_tag_picks_the_newest_base_then_date() {
    let server = update_fixture(b"marker", fixture_nightly_list());
    assert_eq!(
        super::fetch_nightly_tag(&server.url()).unwrap(),
        "v0.3.0-nightly.20240102"
    );
}

/// Two entries equal in (base, date) keep the deterministic winner: the
/// strictly-greater comparison holds the first the list names, in either
/// order. The `v` prefix is not part of the key, so `v0.3.0-nightly.D`
/// and `0.3.0-nightly.D` tie.
#[test]
fn fetch_nightly_tag_keeps_the_first_of_equal_base_and_date() {
    let server = update_fixture(
        b"marker",
        serde_json::json!([
            {"tag_name": "v0.3.0-nightly.20240101", "prerelease": true, "draft": false},
            {"tag_name": "0.3.0-nightly.20240101", "prerelease": true, "draft": false},
        ]),
    );
    assert_eq!(
        super::fetch_nightly_tag(&server.url()).unwrap(),
        "v0.3.0-nightly.20240101"
    );
    let server = update_fixture(
        b"marker",
        serde_json::json!([
            {"tag_name": "0.3.0-nightly.20240101", "prerelease": true, "draft": false},
            {"tag_name": "v0.3.0-nightly.20240101", "prerelease": true, "draft": false},
        ]),
    );
    assert_eq!(
        super::fetch_nightly_tag(&server.url()).unwrap(),
        "0.3.0-nightly.20240101"
    );
}

#[test]
fn fetch_nightly_tag_skips_what_is_not_a_matching_release() {
    let list = serde_json::json!([
        {"tag_name": "v0.3.0", "prerelease": false, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240101", "prerelease": false, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240101", "prerelease": true, "draft": true},
        {"tag_name": "v0.3.0-nightly.2024010", "prerelease": true, "draft": false},
        {"tag_name": 7, "prerelease": true, "draft": false},
        {"prerelease": true, "draft": false},
        "v0.3.0-nightly.20240101",
    ]);
    let server = update_fixture(b"marker", list);
    let err = super::fetch_nightly_tag(&server.url()).unwrap_err();
    let Error::NoNightly { url, .. } = &err else {
        panic!("no matching nightly names the channel: {err}");
    };
    assert!(err.to_string().contains("`nightly`"), "{err}");
    assert_eq!(
        url.as_str(),
        format!("{}/releases?per_page=100", server.url())
    );
}

#[test]
fn fetch_nightly_tag_names_a_failed_list() {
    let server = fakes::ProviderServer::start_routed(
        [("/releases", fakes::Response::status(500, "boom"))],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let err = super::fetch_nightly_tag(&server.url()).unwrap_err();
    let Error::Download { name, why } = &err else {
        panic!("a failed list names its URL: {err}");
    };
    assert!(
        why.contains(&format!("{}/releases?per_page=100", server.url())),
        "{err}"
    );
    assert_eq!(name, "nightly");
}

#[test]
fn latest_stable_tag_reads_the_redirect_tag() {
    let mut redirect = fakes::Response::status(302, "");
    redirect
        .headers
        .push(("location".into(), "/releases/tag/v0.2.0".into()));
    let server = fakes::ProviderServer::start_routed(
        [
            ("/releases/latest", redirect),
            ("/releases/tag/v0.2.0", fakes::Response::status(200, "")),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    assert_eq!(super::latest_stable_tag(&server.url()).unwrap(), "v0.2.0");
}

#[test]
fn latest_stable_tag_names_a_failed_redirect() {
    let server = fakes::ProviderServer::start([fakes::Response::status(500, "nope")]).unwrap();
    let err = super::latest_stable_tag(&server.url()).unwrap_err();
    let Error::Download { name, why } = err else {
        panic!("a failed redirect fails as its download failed: {err}");
    };
    assert_eq!(name, "stable");
    assert!(
        why.contains(&server.url()),
        "the failure names the URL: {why}"
    );
}
