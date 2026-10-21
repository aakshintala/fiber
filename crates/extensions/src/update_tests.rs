//! Staging `fiber update` against a fixture release: the filter over the
//! installed set, the failures that rename nothing, and the staged
//! binary.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fakes::ustar::{archive, gzip, header, sha256 as sha};

use super::{Target, stage_update};
use crate::Error;
use crate::manage::{Request, plan};

const RELEASE: &str = "0.2.0";
const TRIPLE: &str = "test-triple";

/// A fixture release served over HTTP: a binary archive holding `marker`
/// as the top-level `fiber` file, the docs archive holding `docs`, the
/// first-party extensions archive holding `shorts` (each short with its
/// manifest), and the nightly list.
fn serve(
    marker: &[u8],
    docs: &[u8],
    shorts: &[(&str, &str)],
    nightlies: &str,
) -> fakes::ProviderServer {
    let binary = gzip(&archive(&[(
        header("fiber", b'0', marker.len() as u64, 0o755, ""),
        marker,
    )]));
    let docs_archive = gzip(&archive(&[(
        header("v", b'0', docs.len() as u64, 0o644, ""),
        docs,
    )]));
    let members: Vec<([u8; 512], &[u8])> = shorts
        .iter()
        .map(|(short, manifest)| {
            (
                header(
                    &format!("{short}/extension.json"),
                    b'0',
                    manifest.len() as u64,
                    0o644,
                    "",
                ),
                manifest.as_bytes(),
            )
        })
        .collect();
    let extensions = gzip(&archive(&members));
    let binary_file = format!("fiber-{TRIPLE}.tar.gz");
    let paths = [
        format!("/download/v{RELEASE}/{binary_file}"),
        format!("/download/v{RELEASE}/{binary_file}.sha256"),
        format!("/download/v{RELEASE}/fiber-docs.tar.gz"),
        format!("/download/v{RELEASE}/fiber-docs.tar.gz.sha256"),
        format!("/download/v{RELEASE}/fiber-extensions.tar.gz"),
        format!("/download/v{RELEASE}/fiber-extensions.tar.gz.sha256"),
        "/releases".into(),
    ];
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let bodies = [
        ok(binary.clone()),
        ok(sha(&binary).into_bytes()),
        ok(docs_archive.clone()),
        ok(sha(&docs_archive).into_bytes()),
        ok(extensions.clone()),
        ok(sha(&extensions).into_bytes()),
        ok(nightlies.as_bytes().to_vec()),
    ];
    fakes::ProviderServer::start_routed(
        paths.iter().map(String::as_str).zip(bodies),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

fn manifest(name: &str, version: &str) -> String {
    serde_json::json!({
        "name": name,
        "version": version,
        "fiber": "0.0.0",
        "api": 1,
    })
    .to_string()
}

fn commit_of(repo: &Path, tag: &str) -> String {
    fakes::git::git(repo, &["rev-list", "-n", "1", tag])
}

/// A git repository with `extension.json` holding `manifest` at the
/// repository-relative directory the extension `name` installs from,
/// committed and tagged `tag`.
fn member_repo(root: &Path, dir: &str, name: &str, manifest: &str, tag: &str) -> PathBuf {
    let repo = root.join(dir);
    let source = repo.join(subdir(name));
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("extension.json"), manifest).unwrap();
    fakes::git::git(&repo, &["init", "--quiet"]);
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "release"]);
    fakes::git::git(&repo, &["tag", tag]);
    repo
}

/// The repository-relative directory `name` installs from: the segments
/// after the repository's own three, so a first-party member lives
/// under `extensions/<short>` as it does in the fiber repository.
fn subdir(name: &str) -> PathBuf {
    name.split('/').skip(3).collect()
}

/// The fixture home, the member repositories by extension name, and the
/// fiber repository the release tag is cut in.
struct Setup {
    root: fakes::TempDir,
    repos: BTreeMap<String, PathBuf>,
    fiber: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-update");
        let fiber = root.path().join("fiber-repo");
        fs::create_dir_all(&fiber).unwrap();
        fs::write(fiber.join("fiber.md"), "fiber").unwrap();
        fakes::git::git(&fiber, &["init", "--quiet"]);
        fakes::git::git(&fiber, &["add", "."]);
        fakes::git::git(&fiber, &["commit", "--quiet", "-m", "fiber"]);
        Self {
            root,
            repos: BTreeMap::new(),
            fiber,
        }
    }

    /// Commits a release marker in the fiber repository and tags it with
    /// the release, so the release tag's commit resolves.
    fn tag_release(&self) {
        fs::write(self.fiber.join("fiber.md"), "fiber release").unwrap();
        fakes::git::git(&self.fiber, &["add", "."]);
        fakes::git::git(&self.fiber, &["commit", "--quiet", "-m", "release"]);
        fakes::git::git(&self.fiber, &["tag", &format!("v{RELEASE}")]);
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn member(&mut self, name: &str, version: &str) {
        // A monorepo member lives in the fiber repository, as first-party
        // extensions do; anything else gets its own repository.
        if subdir(name).as_os_str().is_empty() {
            let dir = name.replace('/', "-");
            let repo = member_repo(
                self.root.path(),
                &dir,
                name,
                &manifest(name, version),
                &format!("v{version}"),
            );
            self.repos.insert(name.into(), repo);
        } else {
            let source = self.fiber.join(subdir(name));
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("extension.json"), manifest(name, version)).unwrap();
            fakes::git::git(&self.fiber, &["add", "."]);
            fakes::git::git(&self.fiber, &["commit", "--quiet", "-m", name]);
            fakes::git::git(&self.fiber, &["tag", &format!("v{version}")]);
            self.repos.insert(name.into(), self.fiber.clone());
        }
    }

    /// Commits `manifest` for `version` in `name`'s repository and tags
    /// it, so the update has a newer tag to move to.
    fn release(&self, name: &str, version: &str) {
        self.release_manifest(name, version, &manifest(name, version));
    }

    /// Commits `manifest` for `version` in `name`'s repository and tags
    /// it, so the update has a newer tag to move to.
    fn release_manifest(&self, name: &str, version: &str, manifest: &str) {
        let repo = &self.repos[name];
        let source = repo.join(subdir(name));
        fs::write(source.join("extension.json"), manifest).unwrap();
        fakes::git::git(repo, &["add", "."]);
        fakes::git::git(repo, &["commit", "--quiet", "-m", "release"]);
        fakes::git::git(repo, &["tag", &format!("v{version}")]);
    }

    fn origin(&self) -> crate::Origin {
        let repos = self.repos.clone();
        let fiber = self.fiber.clone();
        crate::Origin::new("git", move |repo| {
            if repo == "github.com/aakshintala/fiber" {
                return format!("file://{}", fiber.display());
            }
            match repos.get(repo) {
                Some(dir) => format!("file://{}", dir.display()),
                None => format!("file:///definitely-not-here-{}", repo.replace('/', "-")),
            }
        })
    }

    /// Installs `name` from its repository, as asked for.
    fn install(&self, name: &str) {
        let clock = fakes::clock::FakeClock::new();
        plan(
            &self.home(),
            &Request::Install(name.into()),
            RELEASE,
            &self.origin(),
            clock.as_ref(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn target(&self, server: &fakes::ProviderServer) -> Target {
        Target {
            base: server.url(),
            version: RELEASE.into(),
            triple: TRIPLE.into(),
        }
    }

    fn record(&self, dir: &str) -> serde_json::Value {
        let text = fs::read_to_string(self.home().join("extensions").join(dir).join(".fiber.json"))
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Stages the fixture update, expecting it to fail.
    fn stage_err(&self, server: &fakes::ProviderServer) -> Error {
        let clock = fakes::clock::FakeClock::new();
        match stage_update(
            &self.home(),
            &self.target(server),
            &self.origin(),
            clock.as_ref(),
            true,
        ) {
            Ok(_) => panic!("the staged update was expected to fail"),
            Err(err) => err,
        }
    }

    /// The installed extension files, the docs, and the commit journal:
    /// what a failure before the first rename must leave alone.
    fn installed_state(&self) -> (Vec<(PathBuf, Vec<u8>)>, bool, bool) {
        let mut files = Vec::new();
        let root = self.home().join("extensions");
        if root.exists() {
            let mut stack = vec![root];
            while let Some(dir) = stack.pop() {
                for entry in fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        files.push((path.clone(), fs::read(&path).unwrap()));
                    }
                }
            }
            files.sort();
        }
        let docs = self.home().join("docs").exists();
        let journal = self.home().join("extensions/.commit").exists();
        (files, docs, journal)
    }
}

#[test]
fn an_update_moves_every_member_and_leaves_a_removed_first_party_absent() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    // A newer tag the update moves to.
    setup.release("github.com/acme/widget", "1.1.0");
    setup.tag_release();
    // `memory` ships in the release but is not installed: removed stays
    // removed.
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap();
    let names: Vec<String> = staged.plan().items().map(|i| i.name.clone()).collect();
    assert_eq!(names, ["github.com/acme/widget"]);
    let binary = staged.commit().unwrap();
    assert_eq!(fs::read(&binary).unwrap(), b"marker");
    assert_eq!(binary.parent(), Some(setup.home().as_path()));
    assert_eq!(
        fs::read_to_string(setup.home().join("docs/v")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::read_to_string(
            setup
                .home()
                .join("extensions/github.com-acme-widget/extension.json")
        )
        .unwrap(),
        manifest("github.com/acme/widget", "1.1.0")
    );
    assert!(
        !setup.home().join("extensions/memory").exists(),
        "a removed first-party extension stays removed"
    );
}

/// `fresh` holds only members not yet installed: the installed widget
/// the update moves is not fresh, while the dependency its newer tag
/// newly needs joins the plan fresh.
#[test]
fn fresh_names_hold_only_members_not_yet_installed() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.member("github.com/acme/dep", "1.0.0");
    setup.install("github.com/acme/widget");
    // The newer widget newly depends on dep, which nothing installed.
    setup.release_manifest(
        "github.com/acme/widget",
        "1.1.0",
        &serde_json::json!({
            "name": "github.com/acme/widget",
            "version": "1.1.0",
            "fiber": "0.0.0",
            "api": 1,
            "depends": { "github.com/acme/dep": "1.0.0" },
        })
        .to_string(),
    );
    setup.tag_release();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap();
    let mut fresh: Vec<&str> = staged.plan().fresh_names().collect();
    fresh.sort();
    assert_eq!(fresh, ["github.com/acme/dep"]);
}

#[test]
fn an_archive_member_keeps_its_requested_and_floor() {
    let mut setup = Setup::new();
    setup.member("github.com/aakshintala/fiber/extensions/memory", "0.1.0");
    setup.install("github.com/aakshintala/fiber/extensions/memory");
    // An update-all run records the floor the archive member keeps.
    let clock = fakes::clock::FakeClock::new();
    plan(
        &setup.home(),
        &Request::UpdateAll("github.com/aakshintala/fiber/extensions/memory".into()),
        RELEASE,
        &setup.origin(),
        clock.as_ref(),
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(setup.record("memory")["floor"], "v0.1.0");
    setup.tag_release();

    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap();
    staged.commit().unwrap();
    let record = setup.record("memory");
    assert_eq!(record["version"], "0.2.0");
    assert_eq!(record["requested"], true);
    assert_eq!(record["floor"], "v0.1.0");
    assert_eq!(
        record["source"]["commit"],
        commit_of(&setup.fiber, &format!("v{RELEASE}")).as_str()
    );
}

#[test]
fn docs_are_installed_when_absent() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    assert!(!setup.home().join("docs").exists());
    setup.tag_release();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap()
    .commit()
    .unwrap();
    assert_eq!(
        fs::read_to_string(setup.home().join("docs/v")).unwrap(),
        "new"
    );
}

/// The update's pre-stage runs no install step: the step first runs at
/// the commit, in the installed directory.
#[test]
fn an_install_step_runs_at_the_commit_not_the_stage() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.release_manifest(
        "github.com/acme/widget",
        "1.1.0",
        &serde_json::json!({
            "name": "github.com/acme/widget",
            "version": "1.1.0",
            "fiber": "0.0.0",
            "api": 1,
            "install": ["sh", "-c", "touch step-ran"],
        })
        .to_string(),
    );
    setup.tag_release();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap();
    for item in staged.plan().items() {
        assert!(
            !item.staged().join("step-ran").exists(),
            "the stage runs no install step for {}",
            item.name
        );
    }
    staged.commit().unwrap();
    assert!(
        setup
            .home()
            .join("extensions/github.com-acme-widget/step-ran")
            .exists(),
        "the commit runs the install step"
    );
}

/// A staged update that stages no docs swaps none in: the commit moves
/// the extensions and leaves `docs/` alone.
#[test]
fn a_stageless_docs_stage_leaves_docs_alone() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    assert!(!setup.home().join("docs").exists());
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        false,
    )
    .unwrap()
    .commit()
    .unwrap();
    assert!(
        !setup.home().join("docs").exists(),
        "a stageless docs stage swaps no docs in"
    );
}

/// Dropping a staged update without committing removes the staged docs
/// and installs nothing: the fresh directory beside `docs/` is gone and
/// the installed tree is untouched.
#[test]
fn dropping_a_staged_update_removes_the_staged_docs_and_installs_nothing() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    let before = setup.installed_state();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let clock = fakes::clock::FakeClock::new();
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
    )
    .unwrap();
    // The docs sit staged beside `docs/`, not yet swapped in.
    let fresh: Vec<PathBuf> = fs::read_dir(setup.home())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".docs-"))
        })
        .collect();
    assert_eq!(fresh.len(), 1, "one staged docs directory: {fresh:?}");
    drop(staged);
    assert!(
        !fresh[0].exists(),
        "dropping the staged update removes the staged docs"
    );
    assert_eq!(
        setup.installed_state(),
        before,
        "dropping the staged update installs nothing"
    );
}

#[test]
fn a_tampered_binary_archive_errors_before_any_rename() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    let before = setup.installed_state();
    let binary = gzip(&archive(&[(
        header("fiber", b'0', 6, 0o755, ""),
        b"marker".as_slice(),
    )]));
    let binary_file = format!("fiber-{TRIPLE}.tar.gz");
    let server = fakes::ProviderServer::start_routed(
        [
            (
                format!("/download/v{RELEASE}/{binary_file}").as_str(),
                fakes::Response::status(200, b"tampered".to_vec()),
            ),
            (
                format!("/download/v{RELEASE}/{binary_file}.sha256").as_str(),
                fakes::Response::status(200, sha(&binary).into_bytes()),
            ),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let err = setup.stage_err(&server);
    let Error::ArchiveChecksum { archive, .. } = &err else {
        panic!("a tampered archive fails its checksum: {err}");
    };
    assert_eq!(archive, &binary_file);
    assert_eq!(setup.installed_state(), before);
}

#[test]
fn a_missing_release_tag_errors_before_any_rename() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    let before = setup.installed_state();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let err = setup.stage_err(&server);
    let Error::MissingTag { repo, tag } = &err else {
        panic!("a missing release tag names itself: {err}");
    };
    assert_eq!(repo, "github.com/aakshintala/fiber");
    assert_eq!(tag, &format!("v{RELEASE}"));
    assert_eq!(setup.installed_state(), before);
}

#[test]
fn an_unreachable_second_member_renames_nothing() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/aaa", "1.0.0");
    setup.member("github.com/acme/zzz", "1.0.0");
    setup.install("github.com/acme/aaa");
    setup.install("github.com/acme/zzz");
    setup.tag_release();
    // The second member's repository vanishes after the install: staging
    // it fails after the first member staged.
    fs::remove_dir_all(&setup.repos["github.com/acme/zzz"]).unwrap();
    let before = setup.installed_state();
    let server = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let err = setup.stage_err(&server);
    assert!(
        matches!(err, Error::NoRepository { .. }),
        "an unreachable member names its repository: {err}"
    );
    assert!(err.to_string().contains("github.com/acme/zzz"), "{err}");
    assert_eq!(setup.installed_state(), before);
}

#[test]
fn an_empty_extensions_archive_errors_before_any_rename() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    let before = setup.installed_state();
    let server = serve(b"marker", b"new", &[], "[]");
    let err = setup.stage_err(&server);
    let Error::BadArchive { archive, .. } = &err else {
        panic!("an empty extensions archive fails the layout: {err}");
    };
    assert_eq!(archive, "fiber-extensions.tar.gz");
    assert_eq!(setup.installed_state(), before);
}

#[test]
fn a_member_binary_mismatch_in_pre_stage_renames_nothing() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    let body = b"#!/bin/sh\necho hi\n";
    let server =
        fakes::ProviderServer::start([fakes::Response::status(200, body.to_vec())]).unwrap();
    let url = format!("{}/tool", server.url());
    // The newer tag carries a binary whose digest does not match: the
    // pre-stage prepare fails before the first rename.
    setup.release_manifest(
        "github.com/acme/widget",
        "1.1.0",
        &serde_json::json!({
            "name": "github.com/acme/widget",
            "version": "1.1.0",
            "fiber": "0.0.0",
            "api": 1,
            "binaries": { crate::prepare::platform(): { "url": url, "sha256": "0".repeat(64) } },
        })
        .to_string(),
    );
    setup.tag_release();
    let before = setup.installed_state();
    let release = serve(
        b"marker",
        b"new",
        &[(
            "memory",
            &manifest("github.com/aakshintala/fiber/extensions/memory", "0.2.0"),
        )],
        "[]",
    );
    let err = setup.stage_err(&release);
    let Error::BinaryChecksum { name, url: got } = &err else {
        panic!("a member binary mismatch fails as its checksum failed: {err}");
    };
    assert_eq!(name, "github.com/acme/widget");
    assert_eq!(got, &url);
    assert_eq!(setup.installed_state(), before);
}

#[test]
fn a_binary_archive_without_fiber_errors_before_any_rename() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    let before = setup.installed_state();
    let binary = gzip(&archive(&[(
        header("not-fiber", b'0', 6, 0o755, ""),
        b"marker".as_slice(),
    )]));
    let binary_file = format!("fiber-{TRIPLE}.tar.gz");
    let server = fakes::ProviderServer::start_routed(
        [
            (
                format!("/download/v{RELEASE}/{binary_file}").as_str(),
                fakes::Response::status(200, binary.clone()),
            ),
            (
                format!("/download/v{RELEASE}/{binary_file}.sha256").as_str(),
                fakes::Response::status(200, sha(&binary).into_bytes()),
            ),
        ],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let err = setup.stage_err(&server);
    let Error::BadArchive { archive, .. } = &err else {
        panic!("a binary archive without fiber fails the layout: {err}");
    };
    assert_eq!(archive, &binary_file);
    assert_eq!(setup.installed_state(), before);
}
