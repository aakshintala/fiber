//! The release step's parts: the checksum file, the URL, the docs swap with
//! renames that fail on purpose, a commit that cannot be put back, and the
//! scratch directory.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;

use fakes::ustar::{archive, gzip, header, sha256 as sha};

use super::{Release, expected_digest, install_release_with, scratch, swap_docs, url};
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
            name.to_string_lossy().starts_with(".docs.")
        })
        .count();
    assert_eq!(docs_left, 0);

    let listed = crate::list(&home, &*clock).unwrap();
    assert_eq!(listed.installed.len(), 2);
    assert!(!home.join("extensions/.commit").exists());
    assert_eq!(commit_of(&home, "anthropic"), "aaa");
    assert_eq!(commit_of(&home, "memory"), "aaa");
}

#[test]
fn two_scratch_directories_are_never_shared() {
    let held = fakes::TempDir::new("fiber-scratch");
    let one = scratch(held.path()).unwrap();
    let two = scratch(held.path()).unwrap();
    assert_ne!(one.path, two.path);
    assert!(one.path.is_dir() && two.path.is_dir());
    let (first, second) = (one.path.clone(), two.path.clone());
    drop(one);
    assert!(!first.exists());
    assert!(second.is_dir());
}

#[test]
fn a_taken_scratch_name_is_skipped_not_reused() {
    let held = fakes::TempDir::new("fiber-scratch-taken");
    let pid = std::process::id();
    let next = super::NEXT.load(std::sync::atomic::Ordering::Relaxed);
    let taken = held.path().join(format!("fiber-release-{pid}-{next}"));
    fs::create_dir(&taken).unwrap();
    fs::write(taken.join("mine"), "x").unwrap();
    let got = scratch(held.path()).unwrap();
    assert_ne!(got.path, taken);
    assert_eq!(fs::read_dir(&got.path).unwrap().count(), 0);
    assert!(taken.join("mine").exists());
}

#[test]
fn a_scratch_directory_that_cannot_be_made_is_that_error_not_a_retry() {
    let held = fakes::TempDir::new("fiber-scratch-missing");
    let missing = held.path().join("missing");
    let before = super::NEXT.load(std::sync::atomic::Ordering::Relaxed);
    let Err(err) = scratch(&missing) else {
        panic!("a scratch directory was made under a missing directory");
    };
    let Error::Io { path, source } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(path.parent(), Some(missing.as_path()), "{err}");
    assert_eq!(source.kind(), io::ErrorKind::NotFound, "{err}");
    // Other tests may take names meanwhile, but never all the tries.
    let after = super::NEXT.load(std::sync::atomic::Ordering::Relaxed);
    assert!(after - before < super::SCRATCH_TRIES, "{before}..{after}");
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
