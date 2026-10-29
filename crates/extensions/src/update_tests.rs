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

use super::{Target, release_triple, stage_update};
use crate::Error;
use crate::manage::{Request, plan};
use crate::release::{Release, install_release};
use contract::ErrorCode;

const RELEASE: &str = "0.2.0";
/// A listed triple the fixture serves: [`release_triple`] maps it to
/// itself, so every existing stage test asks for the served archive.
const TRIPLE: &str = "x86_64-unknown-linux-musl";
/// The nightly release the fixture serves above [`RELEASE`].
const NIGHTLY: &str = "0.2.1-nightly.20240101";

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

/// A fixture nightly release served over HTTP: the binary, docs and
/// first-party extensions archives for [`NIGHTLY`], and the nightly
/// list. Every archive route is answered twice, since the install and
/// the stage fetch every archive and the fixture server answers one
/// request per route.
// debt: serve and serve_nightly build the same three archives from
// (marker, docs, shorts); share one archives(marker, docs, shorts) helper
// after lane 1232's held update_tests work lands and serve is available
// to edit.
fn serve_nightly(marker: &[u8], docs: &[u8], shorts: &[(&str, &str)]) -> fakes::ProviderServer {
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
    let mut paths = Vec::new();
    let mut bodies = Vec::new();
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    // The install and the stage fetch every archive: two answers per
    // route, as a real server answers every request.
    for _ in 0..2 {
        for (name, bytes) in [
            (
                format!("/download/v{NIGHTLY}/{binary_file}"),
                binary.clone(),
            ),
            (
                format!("/download/v{NIGHTLY}/fiber-docs.tar.gz"),
                docs_archive.clone(),
            ),
            (
                format!("/download/v{NIGHTLY}/fiber-extensions.tar.gz"),
                extensions.clone(),
            ),
        ] {
            paths.push(name);
            bodies.push(ok(bytes.clone()));
            paths.push(format!("{}.sha256", paths.last().unwrap()));
            bodies.push(ok(sha(&bytes).into_bytes()));
        }
    }
    paths.push("/releases".into());
    bodies.push(ok("[]".as_bytes().to_vec()));
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
            None,
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

/// The scratch-to-staged rename crosses filesystems when the installed
/// binary lives on another mount, so the commit falls back to a copy:
/// the installed binary sits in a fresh directory on `/dev/shm`, and a
/// probe rename from the test root there first proves the filesystems
/// differ. Flipping the `CrossesDevices` comparison to `!=` makes the
/// commit return `Err` instead of copying, so this test kills that mutant.
#[cfg(target_os = "linux")]
#[test]
fn cross_device_commit_falls_back_to_copy() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
    setup.tag_release();
    assert!(
        Path::new("/dev/shm").is_dir(),
        "`/dev/shm` is absent: the cross-device commit test needs it"
    );
    let shm = fakes::TempDir::under(Path::new("/dev/shm"), "fiber-update");
    // The scratch directory lives under the system temporary directory,
    // so a probe rename from the test root to `/dev/shm` takes the same
    // cross-filesystem path the commit's rename takes.
    let probe_src = setup.root.path().join("probe");
    fs::write(&probe_src, b"probe").unwrap();
    let probe_dst = shm.path().join("probe");
    match fs::rename(&probe_src, &probe_dst) {
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(err) => panic!("the probe rename failed unexpectedly: {err}"),
        Ok(()) => panic!("the probe rename succeeded: `/dev/shm` is no separate filesystem"),
    }
    let installed = shm.path().join("fiber");
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
        Some(&installed),
    )
    .unwrap();
    let binary = staged.commit().unwrap();
    assert_eq!(fs::read(&binary).unwrap(), b"marker");
    assert_eq!(binary.parent(), Some(shm.path()));
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
    // The installed binary lives in its own directory, so the staged
    // binary sits beside it rather than in the home.
    let bindir = setup.root.path().join("bin");
    fs::create_dir(&bindir).unwrap();
    let installed = bindir.join("fiber");
    let staged = stage_update(
        &setup.home(),
        &setup.target(&server),
        &setup.origin(),
        clock.as_ref(),
        true,
        Some(&installed),
    )
    .unwrap();
    let names: Vec<String> = staged.plan().items().map(|i| i.name.clone()).collect();
    assert_eq!(names, ["github.com/acme/widget"]);
    let binary = staged.commit().unwrap();
    assert_eq!(fs::read(&binary).unwrap(), b"marker");
    assert_eq!(binary.parent(), Some(bindir.as_path()));
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

/// A home the nightly release installed, with its nightly-versioned
/// records, stages an update to that nightly: the shared floor accepts
/// the nightly form, so the update succeeds and keeps the nightly.
#[test]
fn stage_update_to_a_nightly_release_stages_nightly_installed_records() {
    let mut setup = Setup::new();
    let memory = "github.com/aakshintala/fiber/extensions/memory";
    // The member's package lives in the fiber repository, committed
    // before the nightly tag, so the update's clone finds it.
    setup.member(memory, "0.2.1");
    fakes::git::git(&setup.fiber, &["tag", &format!("v{NIGHTLY}")]);
    let server = serve_nightly(b"marker", b"new", &[("memory", &manifest(memory, "0.2.1"))]);
    let clock = fakes::clock::FakeClock::new();
    install_release(
        &setup.home(),
        &Release {
            base: server.url(),
            version: NIGHTLY.into(),
            commit: commit_of(&setup.fiber, &format!("v{NIGHTLY}")),
        },
        clock.as_ref(),
    )
    .unwrap();
    let staged = stage_update(
        &setup.home(),
        &Target {
            base: server.url(),
            version: NIGHTLY.into(),
            triple: TRIPLE.into(),
        },
        &setup.origin(),
        clock.as_ref(),
        true,
        None,
    )
    .unwrap();
    let names: Vec<String> = staged.plan().items().map(|i| i.name.clone()).collect();
    assert_eq!(names, [memory]);
    let binary = staged.commit().unwrap();
    assert_eq!(std::fs::read(&binary).unwrap(), b"marker");
    let record = setup.record("memory");
    assert_eq!(record["version"], NIGHTLY);
    assert_eq!(record["requested"], true);
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
        None,
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
        None,
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
        None,
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
        None,
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
        None,
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
        None,
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

/// A Linux gnu build stages the musl archive: the fixture serves only
/// the musl archive, and the stage asks for it rather than the gnu
/// name the binary was built against.
#[test]
fn a_gnu_linux_build_stages_the_musl_archive() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
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
        &Target {
            base: server.url(),
            version: RELEASE.into(),
            triple: "x86_64-unknown-linux-gnu".into(),
        },
        &setup.origin(),
        clock.as_ref(),
        true,
        None,
    )
    .unwrap();
    let binary = staged.commit().unwrap();
    assert_eq!(fs::read(&binary).unwrap(), b"marker");
    // Only the musl archive is ever fetched: no request names the gnu
    // archive the binary was built against.
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert!(
        paths
            .iter()
            .any(|path| path.contains("fiber-x86_64-unknown-linux-musl.tar.gz")),
        "the stage fetches the musl archive: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("linux-gnu")),
        "no request names the gnu archive: {paths:?}"
    );
}

/// A platform with no published archive fails before any download
/// (`docs/releasing.md`, "Updating" step 2): the error names the
/// platform and every published target, and the fixture sees no
/// request of any kind.
#[test]
fn an_unlisted_platform_fails_before_any_download() {
    let mut setup = Setup::new();
    setup.member("github.com/acme/widget", "1.0.0");
    setup.install("github.com/acme/widget");
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
    let err = match stage_update(
        &setup.home(),
        &Target {
            base: server.url(),
            version: RELEASE.into(),
            triple: "x86_64-apple-darwin".into(),
        },
        &setup.origin(),
        clock.as_ref(),
        true,
        None,
    ) {
        Ok(_) => panic!("an unlisted platform stages no update"),
        Err(err) => err,
    };
    assert_eq!(err.code(), ErrorCode::Usage);
    let said = err.to_string();
    assert!(said.contains("x86_64-apple-darwin"), "{said}");
    for archive in [
        "fiber-aarch64-apple-darwin.tar.gz",
        "fiber-x86_64-unknown-linux-musl.tar.gz",
        "fiber-aarch64-unknown-linux-musl.tar.gz",
    ] {
        assert!(said.contains(archive), "{said}");
    }
    // `docs/releasing.md`, "Updating" step 2: an unlisted platform stops
    // before any download.
    assert!(
        server.requests().is_empty(),
        "no download runs: {:?}",
        server.requests()
    );
}

/// Both Linux gnu builds map to their own architecture's musl archive:
/// flipping an arch maps a build at the other's archive, which the
/// release never publishes.
#[test]
fn release_triple_maps_linux_gnu_builds_to_their_musl_archive() {
    assert_eq!(
        release_triple("x86_64-unknown-linux-gnu").unwrap(),
        "x86_64-unknown-linux-musl"
    );
    assert_eq!(
        release_triple("aarch64-unknown-linux-gnu").unwrap(),
        "aarch64-unknown-linux-musl"
    );
}

/// Listed triples stay themselves: both musl archives and the macOS
/// archive pass through untouched.
#[test]
fn release_triple_passes_listed_triples_through() {
    assert_eq!(
        release_triple("x86_64-unknown-linux-musl").unwrap(),
        "x86_64-unknown-linux-musl"
    );
    assert_eq!(
        release_triple("aarch64-unknown-linux-musl").unwrap(),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(
        release_triple("aarch64-apple-darwin").unwrap(),
        "aarch64-apple-darwin"
    );
}

/// Platforms with no published archive fail with the platform and every
/// published archive named: widening the match to pass one through
/// asks for an archive the release never publishes.
#[test]
fn release_triple_rejects_platforms_with_no_published_archive() {
    for unlisted in ["x86_64-apple-darwin", "x86_64-pc-windows-msvc"] {
        let err = release_triple(unlisted).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Usage, "{unlisted}");
        let said = err.to_string();
        assert!(said.contains(unlisted), "{said}");
        for archive in [
            "fiber-aarch64-apple-darwin.tar.gz",
            "fiber-x86_64-unknown-linux-musl.tar.gz",
            "fiber-aarch64-unknown-linux-musl.tar.gz",
        ] {
            assert!(said.contains(archive), "{said}");
        }
    }
}
