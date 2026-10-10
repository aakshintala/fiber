//! `docs/releasing.md`, "Installing": the release step downloads the docs
//! and extensions archives, checks them, and puts them in place in Fiber
//! home. Every failure before the first rename leaves Fiber home as it was,
//! or absent when it was absent.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Setup, manifest};
use contract::clock::Clock;
use extensions::{Error, Origin, Provenance, Release, Request, list, plan};
use fakes::ustar::{archive, gzip, header, sha256 as sha};
use serde_json::{Value, json};

const COMMIT: &str = "4f2a9c1";

/// One member of a fixture archive.
enum Member {
    File(&'static str, Vec<u8>),
    Link(&'static str, &'static str),
    Raw(Box<[u8; 512]>, Vec<u8>),
}

fn file(name: &'static str, data: impl Into<Vec<u8>>) -> Member {
    Member::File(name, data.into())
}

fn tar(members: &[Member]) -> Vec<u8> {
    let blocks: Vec<([u8; 512], Vec<u8>)> = members
        .iter()
        .map(|m| match m {
            Member::File(name, data) => (
                header(name, b'0', data.len() as u64, 0o644, ""),
                data.clone(),
            ),
            Member::Link(name, target) => (header(name, b'2', 0, 0o777, target), Vec::new()),
            Member::Raw(block, data) => (**block, data.clone()),
        })
        .collect();
    let refs: Vec<([u8; 512], &[u8])> = blocks.iter().map(|(h, d)| (*h, d.as_slice())).collect();
    archive(&refs)
}

fn first_party(short: &str, kind: &str) -> Value {
    json!({
        "name": format!("github.com/aakshintala/fiber/{kind}/{short}"),
        "version": "0.0.0",
        "fiber": "0.0.0",
        "api": 1,
    })
}

fn provider() -> Value {
    json!({
        "name": "anthropic",
        "credential": { "env": "FIBER_TEST_UNSET_KEY" },
        "models": [{
            "id": "m",
            "protocol": "anthropic-messages",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 1000,
        }],
    })
}

/// The good release: `anthropic` with a provider, `memory` with a prompt.
fn extension_members(anthropic: &Value) -> Vec<Member> {
    vec![
        file("anthropic/extension.json", anthropic.to_string()),
        file("anthropic/providers/anthropic.json", provider().to_string()),
        file(
            "memory/extension.json",
            first_party("memory", "extensions").to_string(),
        ),
        file("memory/prompt.md", "remember"),
    ]
}

fn good_extensions() -> Vec<u8> {
    gzip(&tar(&extension_members(&first_party(
        "anthropic",
        "providers",
    ))))
}

fn good_docs() -> Vec<u8> {
    gzip(&tar(&[
        file("README.md", "the docs"),
        file("user/index.md", "for a person"),
    ]))
}

/// The four release files a test serves, each replaceable.
struct Files {
    docs: Vec<u8>,
    docs_sum: Vec<u8>,
    extensions: Vec<u8>,
    extensions_sum: Vec<u8>,
    missing: Option<&'static str>,
}

impl Files {
    fn new(docs: Vec<u8>, extensions: Vec<u8>) -> Self {
        Self {
            docs_sum: format!("{}  fiber-docs.tar.gz\n", sha(&docs)).into_bytes(),
            extensions_sum: sha(&extensions).into_bytes(),
            docs,
            extensions,
            missing: None,
        }
    }

    fn good() -> Self {
        Self::new(good_docs(), good_extensions())
    }

    fn with_extensions(members: &[Member]) -> Self {
        Self::new(good_docs(), gzip(&tar(members)))
    }

    fn serve(&self) -> fakes::ProviderServer {
        let files = [
            ("fiber-docs.tar.gz", &self.docs),
            ("fiber-docs.tar.gz.sha256", &self.docs_sum),
            ("fiber-extensions.tar.gz", &self.extensions),
            ("fiber-extensions.tar.gz.sha256", &self.extensions_sum),
        ];
        let routes: Vec<(String, fakes::Response)> = files
            .into_iter()
            .filter(|(name, _)| Some(*name) != self.missing)
            .map(|(name, body)| {
                (
                    format!("/download/v0.0.0/{name}"),
                    fakes::Response::status(200, body.clone()),
                )
            })
            .collect();
        fakes::ProviderServer::start_routed(
            routes.iter().map(|(p, r)| (p.as_str(), r.clone())),
            fakes::Response::status(404, "not found"),
        )
        .unwrap()
    }
}

fn release(server: &fakes::ProviderServer) -> Release {
    Release {
        base: server.url(),
        version: "0.0.0".into(),
        commit: COMMIT.into(),
    }
}

fn install_release(
    home: &Path,
    files: &Files,
) -> (Result<Vec<String>, Error>, fakes::ProviderServer) {
    let server = files.serve();
    let result =
        extensions::install_release(home, &release(&server), &*fakes::clock::FakeClock::new());
    (result, server)
}

/// Every path under `dir`, with each file's bytes and each link's target;
/// `None` when `dir` does not exist.
fn snapshot(dir: &Path) -> Option<BTreeMap<PathBuf, String>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(root, &path, out);
            } else if meta.is_symlink() {
                out.insert(
                    rel,
                    format!("-> {}", fs::read_link(&path).unwrap().display()),
                );
            } else {
                out.insert(
                    rel,
                    String::from_utf8_lossy(&fs::read(&path).unwrap()).into(),
                );
            }
        }
    }
    if !dir.exists() {
        return None;
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    Some(out)
}

/// The installed release, plus an unrelated extension and old docs.
fn populated(setup: &Setup) -> PathBuf {
    let home = setup.home();
    install_release(&home, &Files::good()).0.unwrap();
    common::write(&home.join("docs/stale.md"), "old");
    let other = home.join("extensions/github.com-acme-x");
    common::write(&other.join("extension.json"), "{}");
    home
}

/// `files` fails with an error `check` accepts, with Fiber home present and
/// with it absent, and changes nothing in either. Returns the request
/// paths the server saw on the second run.
fn refused(files: &Files, check: impl Fn(&Error) -> bool) -> Vec<String> {
    let setup = Setup::new();
    let home = populated(&setup);
    let before = snapshot(&home);
    let (result, _server) = install_release(&home, files);
    let err = result.unwrap_err();
    assert!(check(&err), "{err:?}");
    assert_eq!(snapshot(&home), before, "{err}");

    let absent = setup.root().join("absent/home");
    let (result, server) = install_release(&absent, files);
    let err = result.unwrap_err();
    assert!(check(&err), "{err:?}");
    assert!(!setup.root().join("absent").exists(), "{err}");
    server.requests().into_iter().map(|r| r.path).collect()
}

fn bad_archive(archive: &'static str, why: &'static str) -> impl Fn(&Error) -> bool {
    move |err| {
        matches!(err, Error::BadArchive { archive: a, .. } if a == archive)
            && err.to_string().contains(why)
    }
}

#[test]
fn a_release_installs_its_docs_and_extensions() {
    let setup = Setup::new();
    let home = setup.home();
    let (result, _server) = install_release(&home, &Files::good());
    let names = result.unwrap();
    assert_eq!(
        names,
        [
            "github.com/aakshintala/fiber/extensions/memory",
            "github.com/aakshintala/fiber/providers/anthropic",
        ]
    );
    assert_eq!(
        fs::read_to_string(home.join("docs/README.md")).unwrap(),
        "the docs"
    );
    assert_eq!(
        fs::read_to_string(home.join("docs/user/index.md")).unwrap(),
        "for a person"
    );
    assert_eq!(
        fs::read_to_string(home.join("extensions/memory/prompt.md")).unwrap(),
        "remember"
    );
    for short in ["anthropic", "memory"] {
        let text =
            fs::read_to_string(home.join("extensions").join(short).join(".fiber.json")).unwrap();
        let record: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            record,
            json!({
                "name": config::full_name(short),
                "version": "0.0.0",
                "requested": true,
                "source": { "commit": COMMIT },
            })
        );
    }
    let listing = list(&home, &*fakes::clock::FakeClock::new()).unwrap();
    assert!(listing.damaged.is_empty());
    let installed: Vec<(String, Provenance)> = listing
        .installed
        .into_iter()
        .map(|i| (i.name, i.provenance))
        .collect();
    let git = Provenance::Git {
        commit: COMMIT.into(),
    };
    assert_eq!(
        installed,
        [
            (
                "github.com/aakshintala/fiber/extensions/memory".into(),
                git.clone()
            ),
            (
                "github.com/aakshintala/fiber/providers/anthropic".into(),
                git
            ),
        ]
    );
    let left: Vec<String> = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".docs."))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_second_run_reinstalls_a_removed_extension() {
    let setup = Setup::new();
    let home = setup.home();
    install_release(&home, &Files::good()).0.unwrap();
    fs::remove_dir_all(home.join("extensions/anthropic")).unwrap();
    install_release(&home, &Files::good()).0.unwrap();
    assert!(home.join("extensions/anthropic/.fiber.json").exists());
}

#[test]
fn existing_docs_and_installs_are_replaced_and_others_kept() {
    let setup = Setup::new();
    let home = populated(&setup);
    common::write(&home.join("extensions/memory/stale.md"), "old");
    install_release(&home, &Files::good()).0.unwrap();
    assert!(!home.join("docs/stale.md").exists());
    assert!(home.join("docs/README.md").exists());
    assert!(!home.join("extensions/memory/stale.md").exists());
    assert_eq!(
        fs::read_to_string(home.join("extensions/github.com-acme-x/extension.json")).unwrap(),
        "{}"
    );
}

#[test]
fn a_digest_mismatch_on_either_archive_changes_nothing() {
    let mut docs = Files::good();
    docs.docs_sum = sha(b"other").into_bytes();
    refused(
        &docs,
        |e| matches!(e, Error::ArchiveChecksum { archive, .. } if archive == "fiber-docs.tar.gz"),
    );
    let mut extensions = Files::good();
    extensions.extensions_sum = format!("{}\n", sha(b"other").to_uppercase()).into_bytes();
    refused(&extensions, |e| {
        matches!(e, Error::ArchiveChecksum { archive, .. } if archive == "fiber-extensions.tar.gz")
            && e.to_string().ends_with("does not match its .sha256 file.")
    });
}

#[test]
fn a_missing_release_file_changes_nothing() {
    for missing in [
        "fiber-docs.tar.gz",
        "fiber-docs.tar.gz.sha256",
        "fiber-extensions.tar.gz",
        "fiber-extensions.tar.gz.sha256",
    ] {
        let mut files = Files::good();
        files.missing = Some(missing);
        refused(&files, |e| {
            matches!(e, Error::Download { name, .. } if name == missing)
                && e.to_string()
                    .starts_with(&format!("`{missing}`: a download failed: "))
        });
    }
}

#[test]
fn a_checksum_file_without_a_digest_changes_nothing() {
    let mut files = Files::good();
    files.docs_sum = b"no digest here\n".to_vec();
    refused(
        &files,
        bad_archive("fiber-docs.tar.gz.sha256", "64 hex digits"),
    );
}

#[test]
fn a_hostile_archive_changes_nothing() {
    let manifest = first_party("memory", "extensions").to_string();
    let base = || file("memory/extension.json", manifest.clone());
    let cases: Vec<(Vec<Member>, &str)> = vec![
        (vec![base(), file("../escape", "x")], "has a `..` component"),
        (vec![base(), file("/abs", "x")], "is absolute"),
        (
            vec![
                base(),
                Member::Link("memory/link", "sub"),
                file("memory/link/x", "x"),
            ],
            "which is not a directory",
        ),
        (
            vec![
                base(),
                Member::Link("memory/a", ".."),
                Member::Link("memory/b", "a/../outside"),
            ],
            "has a link target with `..`",
        ),
        (
            vec![base(), Member::Link("memory/etc", "/etc")],
            "has an absolute link target",
        ),
        (
            vec![
                base(),
                Member::Raw(
                    Box::new(header(
                        "memory/hard",
                        b'1',
                        0,
                        0o644,
                        "memory/extension.json",
                    )),
                    Vec::new(),
                ),
            ],
            "has type `1`",
        ),
        (
            vec![
                base(),
                Member::Raw(
                    Box::new(header("memory/fifo", b'6', 0, 0o644, "")),
                    Vec::new(),
                ),
            ],
            "has type `6`",
        ),
    ];
    for (members, why) in cases {
        refused(
            &Files::with_extensions(&members),
            bad_archive("fiber-extensions.tar.gz", why),
        );
    }
}

#[test]
fn a_manifest_with_binaries_or_an_install_step_changes_nothing() {
    let mut binaries = first_party("anthropic", "providers");
    binaries["binaries"] = json!({
        "darwin-arm64": { "url": "http://127.0.0.1:1/tool", "sha256": "00" },
    });
    let mut step = first_party("anthropic", "providers");
    step["install"] = json!(["sh", "-c", "exit 0"]);
    for manifest in [binaries, step] {
        let paths = refused(
            &Files::with_extensions(&extension_members(&manifest)),
            bad_archive(
                "fiber-extensions.tar.gz",
                "declares binaries or an install step",
            ),
        );
        assert_eq!(paths.len(), 4, "{paths:?}");
        assert!(
            paths
                .iter()
                .all(|p| p.starts_with("/download/v0.0.0/fiber-")),
            "{paths:?}"
        );
    }
}

#[test]
fn an_empty_archive_changes_nothing() {
    let mut docs = Files::good();
    docs.docs = gzip(&archive(&[]));
    docs.docs_sum = sha(&docs.docs).into_bytes();
    refused(&docs, bad_archive("fiber-docs.tar.gz", "holds no entries"));
    let mut root_only = Files::good();
    root_only.docs = gzip(&archive(&[(header("./", b'5', 0, 0o755, ""), b"")]));
    root_only.docs_sum = sha(&root_only.docs).into_bytes();
    refused(
        &root_only,
        bad_archive("fiber-docs.tar.gz", "holds no entries"),
    );
    refused(
        &Files::new(good_docs(), gzip(&archive(&[]))),
        bad_archive("fiber-extensions.tar.gz", "holds no extension"),
    );
}

#[test]
fn an_archive_that_is_not_gzip_changes_nothing() {
    refused(
        &Files::new(good_docs(), b"plain bytes".to_vec()),
        bad_archive("fiber-extensions.tar.gz", "is not a valid gzip stream"),
    );
}

#[test]
fn a_layout_that_is_not_the_releases_changes_nothing() {
    let acme = json!({ "name": "acme", "version": "0.0.0", "fiber": "0.0.0", "api": 1 });
    let wrong = first_party("anthropic", "providers");
    let cases: Vec<(Vec<Member>, &str)> = vec![
        (
            vec![file("acme/extension.json", acme.to_string())],
            "`acme` is not a first-party extension's short name",
        ),
        (
            vec![file("README", "x")],
            "`README` at the top level is not a directory",
        ),
        (
            vec![
                file("anthropic/extension.json", wrong.to_string()),
                Member::Link("memory", "anthropic"),
            ],
            "`memory` at the top level is not a directory",
        ),
        (
            vec![file("memory/extension.json", wrong.to_string())],
            "`memory` holds a manifest that names `github.com/aakshintala/fiber/providers/anthropic`",
        ),
    ];
    for (members, why) in cases {
        refused(
            &Files::with_extensions(&members),
            bad_archive("fiber-extensions.tar.gz", why),
        );
    }
}

/// What a refused install's error must satisfy.
type Check = Box<dyn Fn(&Error) -> bool>;

#[test]
fn stagings_checks_run_before_fiber_home_exists() {
    let mut newer = first_party("anthropic", "providers");
    newer["fiber"] = json!("0.0.1");
    let mut api = first_party("anthropic", "providers");
    api["api"] = json!(2);
    let good = first_party("anthropic", "providers");
    let cases: Vec<(Vec<Member>, Check)> = vec![
        (
            extension_members(&newer),
            Box::new(|e| matches!(e, Error::NeedsNewerFiber { .. })),
        ),
        (
            extension_members(&api),
            Box::new(|e| matches!(e, Error::ApiVersion { .. })),
        ),
        (
            vec![
                file("anthropic/extension.json", good.to_string()),
                file("anthropic/providers/anthropic.json", "{"),
            ],
            Box::new(|e| matches!(e, Error::Config(_)) && e.to_string().contains("anthropic.json")),
        ),
        (
            vec![file("anthropic/prompt.md", "x")],
            Box::new(|e| matches!(e, Error::Config(_)) && e.to_string().contains("extension.json")),
        ),
        (
            vec![file("anthropic/extension.json", "{ not json")],
            Box::new(|e| matches!(e, Error::Config(_)) && e.to_string().contains("extension.json")),
        ),
    ];
    for (members, check) in cases {
        refused(&Files::with_extensions(&members), check);
    }
}

#[test]
fn a_held_lock_is_busy_and_adds_nothing() {
    let setup = Setup::new();
    let source = setup.source("acme", &manifest("github.com/acme/x"), &[]);
    let clock = fakes::clock::FakeClock::new();
    let held = plan(
        &setup.home(),
        &Request::Path(source),
        "0.1.0",
        &Origin::github(),
        &*clock,
    )
    .unwrap();
    let before = snapshot(&setup.home());
    let started = clock.now();
    let server = Files::good().serve();
    let err = extensions::install_release(&setup.home(), &release(&server), &*clock).unwrap_err();
    assert!(matches!(err, Error::Busy), "{err:?}");
    assert_eq!(
        clock.now().saturating_duration_since(started),
        Duration::from_millis(500)
    );
    assert_eq!(snapshot(&setup.home()), before);
    drop(held);
}
