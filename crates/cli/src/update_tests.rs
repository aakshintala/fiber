//! `resolve_channel` precedence (`docs/releasing.md`, "Updating"): the
//! flag beats `FIBER_CHANNEL`, and an empty or unset variable means
//! stable. An invalid non-empty value names itself.

#![allow(clippy::unwrap_used, reason = "a failing resolve is the test's")]

use super::*;

#[test]
fn resolve_channel_prefers_the_flag_over_the_environment() {
    assert_eq!(
        resolve_channel(Some(Channel::Stable), Some("nightly")),
        Ok(Channel::Stable)
    );
    assert_eq!(
        resolve_channel(Some(Channel::Nightly), Some("stable")),
        Ok(Channel::Nightly)
    );
    assert_eq!(
        resolve_channel(Some(Channel::Nightly), None),
        Ok(Channel::Nightly)
    );
}

#[test]
fn resolve_channel_reads_fiber_channel_without_a_flag() {
    assert_eq!(resolve_channel(None, None), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("")), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("stable")), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("nightly")), Ok(Channel::Nightly));
}

#[test]
fn resolve_channel_rejects_any_other_value_naming_it() {
    let error = resolve_channel(None, Some("beta")).unwrap_err();
    assert!(error.to_string().contains("`beta`"), "{error}");
}

#[test]
fn production_bases_point_at_the_release_roots() {
    let bases = ReleaseBases::production();
    assert_eq!(bases.web, "https://github.com/aakshintala/fiber");
    assert_eq!(bases.api, "https://api.github.com/repos/aakshintala/fiber");
}

// `update` runs against a fixture release server and a local fiber git
// stand-in (`docs/releasing.md`, "Updating"): the staged release replaces
// the running binary, moves installed first-party members and installs
// docs, while an already-newest binary keeps its bytes.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use fakes::ustar::{archive, gzip, header, sha256};

/// The triple `update` asks the fixture server for: a listed triple,
/// so [`super::run`]'s platform check maps it to itself and the stage
/// asks for the served archive.
const TRIPLE: &str = "x86_64-unknown-linux-musl";

/// The bytes the fixture binary archive holds: the staged `fiber` binary.
const MARKER: &[u8] = b"new-fiber-binary";

/// The bytes the running binary holds before an update: replaced on a
/// newer target, untouched on the already-newest path.
const OLD_BINARY: &[u8] = b"old-fiber-binary";

/// A release server for `install` and `stage`: archives for every version
/// in `archived`, the `/releases/latest` redirect to `redirect`, and the
/// nightly list `nightly` at `/releases`.
fn serve(
    archived: &[&str],
    redirect: &str,
    nightly: Option<serde_json::Value>,
) -> fakes::ProviderServer {
    let manifest = |version: &str| {
        serde_json::json!({
            "name": "github.com/aakshintala/fiber/providers/anthropic",
            "version": version,
            "fiber": version,
            "api": 1,
        })
        .to_string()
    };
    let file = |name: &str, body: &[u8]| header(name, b'0', body.len() as u64, 0o644, "");
    let mut routes: Vec<(String, fakes::Response)> = Vec::new();
    for version in archived {
        let binary = gzip(&archive(&[(file("fiber", MARKER), MARKER)]));
        let docs = gzip(&archive(&[(file("README.md", b"docs"), b"docs")]));
        let anthropic = manifest(version);
        let extensions = gzip(&archive(&[(
            file("anthropic/extension.json", anthropic.as_bytes()),
            anthropic.as_bytes(),
        )]));
        let ok = |body: Vec<u8>| fakes::Response::status(200, body);
        // One route per archive: every fetch runs once, against the
        // release the stage resolves.
        for (name, bytes) in [
            (format!("fiber-{TRIPLE}.tar.gz"), binary),
            ("fiber-docs.tar.gz".to_owned(), docs),
            ("fiber-extensions.tar.gz".to_owned(), extensions),
        ] {
            routes.push((
                format!("/releases/download/v{version}/{name}"),
                ok(bytes.clone()),
            ));
            routes.push((
                format!("/releases/download/v{version}/{name}.sha256"),
                ok(sha256(&bytes).into_bytes()),
            ));
        }
    }
    let mut latest = fakes::Response::status(302, "");
    latest
        .headers
        .push(("location".to_owned(), format!("/releases/tag/{redirect}")));
    routes.push(("/releases/latest".to_owned(), latest));
    routes.push((
        format!("/releases/tag/{redirect}"),
        fakes::Response::status(200, ""),
    ));
    if let Some(nightly) = nightly {
        routes.push((
            "/releases".to_owned(),
            fakes::Response::status(200, nightly.to_string()),
        ));
    }
    fakes::ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// The fixture world: a fiber git stand-in tagged with the staged version,
/// a home with `install` at `install`, and a running binary holding
/// [`OLD_BINARY`].
struct World {
    root: fakes::TempDir,
    server: fakes::ProviderServer,
    fiber: PathBuf,
    home: PathBuf,
    binary: PathBuf,
}

impl World {
    fn new(
        install: &str,
        stage: &str,
        redirect: &str,
        nightly: Option<serde_json::Value>,
        binary_bytes: &[u8],
    ) -> Self {
        let root = fakes::TempDir::new("fiber-cli-update");
        let fiber = root.path().join("fiber");
        fs::create_dir(&fiber).unwrap();
        fs::write(fiber.join("fiber.md"), "fiber").unwrap();
        // First-party members stage from the fiber repo's `providers/`
        // tree, replaced from the archives afterwards: the stand-in
        // carries the member the archives install.
        let manifest = serde_json::json!({
            "name": "github.com/aakshintala/fiber/providers/anthropic",
            "version": stage,
            "fiber": install,
            "api": 1,
        })
        .to_string();
        let provider = fiber.join("providers").join("anthropic");
        fs::create_dir_all(&provider).unwrap();
        fs::write(provider.join("extension.json"), &manifest).unwrap();
        fakes::git::git(&fiber, &["init", "--quiet"]);
        fakes::git::git(&fiber, &["add", "."]);
        fakes::git::git(&fiber, &["commit", "--quiet", "-m", "fiber"]);
        fakes::git::git(&fiber, &["tag", &format!("v{install}")]);
        if stage != install {
            fakes::git::git(&fiber, &["tag", &format!("v{stage}")]);
        }
        let server = serve(&[install, stage], redirect, nightly);
        let home = root.path().join("home");
        let base = format!("{}/releases", server.url());
        let commit = fakes::git::git(&fiber, &["rev-parse", "HEAD"]);
        extensions::install_release(
            &home,
            &extensions::Release {
                base,
                version: install.into(),
                commit: commit.trim().into(),
            },
            &fakes::clock::SystemClock,
        )
        .unwrap();
        let bindir = root.path().join("bin");
        fs::create_dir(&bindir).unwrap();
        let binary = bindir.join("fiber");
        fs::write(&binary, binary_bytes).unwrap();
        Self {
            root,
            server,
            fiber,
            home,
            binary,
        }
    }

    fn bases(&self) -> ReleaseBases {
        ReleaseBases {
            web: self.server.url(),
            api: self.server.url(),
        }
    }

    fn origin(&self) -> extensions::Origin {
        let fiber = self.fiber.clone();
        extensions::Origin::new("git", move |repo| {
            assert_eq!(repo, "github.com/aakshintala/fiber");
            format!("file://{}", fiber.display())
        })
    }

    /// Runs the update the way `update` does, with `restart` in place of
    /// the service restart: [`crate::hub_service::hub_restart`] in
    /// production, a scripted answer here.
    fn run(
        &self,
        running: &str,
        channel: Channel,
        terminal: bool,
        input: &[u8],
        restart: &dyn Fn(&std::path::Path) -> Result<bool, CliError>,
    ) -> (Result<i32, CliError>, String) {
        let mut err = Vec::new();
        let result = super::run(
            &self.home,
            running,
            TRIPLE,
            channel,
            &self.bases(),
            &fakes::clock::SystemClock,
            Io {
                terminal,
                input: &mut Cursor::new(input),
                err: &mut err,
            },
            &self.binary,
            &self.origin(),
            restart,
        );
        (result, String::from_utf8(err).unwrap())
    }

    fn no_restart(_: &std::path::Path) -> Result<bool, CliError> {
        Ok(false)
    }
}

/// The installed first-party member's version.
fn member_version(home: &std::path::Path) -> String {
    let text = fs::read_to_string(home.join("extensions/anthropic/extension.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn stable_full_update_replaces_the_binary_moves_members_and_installs_docs() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let (result, err) = world.run("0.1.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0);
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.2.0");
    assert!(world.home.join("docs").is_dir(), "{err}");
}

#[test]
fn stable_already_newest_keeps_the_binary_but_moves_members_and_docs() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, MARKER);
    fs::remove_dir_all(world.home.join("docs")).unwrap();
    let (result, err) = world.run("0.2.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.2.0");
    assert!(world.home.join("docs").is_dir(), "{err}");
    // The already-newest path stages the missing docs with the rest and
    // swaps them after approval: one docs fetch for the staged version.
    let docs = world
        .server
        .requests()
        .iter()
        .filter(|request| request.path.contains("v0.2.0/fiber-docs.tar.gz"))
        .count();
    assert_eq!(docs, 2, "{err}");
    assert!(err.contains("fiber: updated docs"), "{err}");
    assert_eq!(
        fs::read_dir(world.binary.parent().unwrap())
            .unwrap()
            .count(),
        1,
        "{err}"
    );
}

/// An already-newest run with its docs present stages no docs: the
/// binary keeps its bytes, the members still move, and no docs fetch
/// runs.
#[test]
fn stable_already_newest_with_present_docs_fetches_no_docs() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, MARKER);
    fs::create_dir_all(world.home.join("docs")).unwrap();
    fs::write(world.home.join("docs").join("v"), "old").unwrap();
    let (result, err) = world.run("0.2.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.2.0");
    assert_eq!(
        fs::read_to_string(world.home.join("docs").join("v")).unwrap(),
        "old",
        "present docs stay untouched"
    );
    // The v0.1.0 docs fetches belong to the install; the stage fetches
    // no docs for its own version.
    assert!(
        world
            .server
            .requests()
            .iter()
            .all(|request| !request.path.contains("v0.2.0/fiber-docs.tar.gz")),
        "no staged docs fetch runs: {:?}",
        world.server.requests()
    );
    assert!(!err.contains("fiber: updated docs"), "{err}");
}

/// A refused already-newest run with missing docs installs nothing: the
/// docs swap after approval, so the refusal leaves them missing.
#[test]
fn stable_already_newest_refusal_leaves_missing_docs_missing() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, MARKER);
    fs::remove_dir_all(world.home.join("docs")).unwrap();
    let (result, err) = world.run("0.2.0", Channel::Stable, true, b"n\n", &World::no_restart);
    assert_eq!(result.unwrap(), 1, "{err}");
    assert!(
        !world.home.join("docs").exists(),
        "a refused update stages nothing"
    );
    assert_eq!(member_version(&world.home), "0.1.0");
}

/// `update` names each damaged member's skip line and still updates the
/// rest: the damaged directory stays, recorded for `reinstall`.
#[test]
fn stable_update_names_damaged_members_and_updates_the_rest() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    fs::create_dir_all(world.home.join("extensions").join("broken")).unwrap();
    let (result, err) = world.run("0.1.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert!(
        err.contains("fiber: `broken` is damaged, so its dependency minimums are unknown"),
        "{err}"
    );
    assert_eq!(member_version(&world.home), "0.2.0");
    assert!(world.home.join("extensions").join("broken").is_dir());
}

#[test]
fn stable_older_than_running_takes_the_already_newest_path() {
    let world = World::new("0.1.0", "0.3.0", "v0.2.0", None, MARKER);
    let (result, err) = world.run("0.3.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.3.0");
}

#[test]
fn nightly_full_update_replaces_the_binary() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0-nightly.20240102", "prerelease": true, "draft": false},
    ]);
    let world = World::new(
        "0.1.0",
        "0.2.0-nightly.20240102",
        "v0.2.0",
        Some(nightly),
        OLD_BINARY,
    );
    let (result, err) = world.run("0.1.0", Channel::Nightly, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.2.0-nightly.20240102");
}

#[test]
fn nightly_without_a_match_names_the_channel() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0", "prerelease": false, "draft": false},
    ]);
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", Some(nightly), OLD_BINARY);
    let (result, _) = world.run("0.1.0", Channel::Nightly, false, b"", &World::no_restart);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("`nightly`"), "{err}");
    let CliError::Extensions(extensions::Error::NoNightly { url, .. }) = err else {
        panic!("a nightly without a match fails as no nightly");
    };
    assert!(url.contains(&world.server.url()), "{url}");
}

#[test]
fn nightly_list_failure_names_the_url() {
    let root = fakes::TempDir::new("fiber-cli-update-nightly-500");
    let server = fakes::ProviderServer::start([fakes::Response::status(500, "nope")]).unwrap();
    let home = root.path().join("home");
    let binary = root.path().join("fiber");
    fs::write(&binary, OLD_BINARY).unwrap();
    let mut err = Vec::new();
    let result = super::run(
        &home,
        "0.1.0",
        TRIPLE,
        Channel::Nightly,
        &ReleaseBases {
            web: server.url(),
            api: server.url(),
        },
        &fakes::clock::SystemClock,
        Io {
            terminal: false,
            input: &mut Cursor::new(b"".as_slice()),
            err: &mut err,
        },
        &binary,
        &extensions::Origin::github(),
        &World::no_restart,
    );
    let CliError::Extensions(extensions::Error::Download { name, why }) = result.unwrap_err()
    else {
        panic!("a failed nightly list fails as its download failed");
    };
    assert_eq!(name, "nightly");
    assert!(
        why.contains(&server.url()),
        "the failure names the URL: {why}"
    );
}

#[test]
fn nightly_skips_non_matching_entries_and_takes_the_maximum_unsorted() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0-nightly.20240101", "prerelease": true, "draft": false},
        {"tag_name": "not-a-version", "prerelease": true, "draft": false},
        {"tag_name": "v0.9.9", "prerelease": false, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240101", "prerelease": false, "draft": false},
        {"tag_name": "v0.3.0-nightly.20240101", "prerelease": true, "draft": true},
        {"tag_name": "v0.3.0-nightly.20240201", "prerelease": true, "draft": false},
    ]);
    let world = World::new(
        "0.1.0",
        "0.3.0-nightly.20240201",
        "v0.2.0",
        Some(nightly),
        OLD_BINARY,
    );
    let (result, err) = world.run("0.1.0", Channel::Nightly, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn nightly_below_its_own_stable_is_already_newest() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0-nightly.20240101", "prerelease": true, "draft": false},
    ]);
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", Some(nightly), MARKER);
    let (result, err) = world.run("0.2.0", Channel::Nightly, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn nightly_later_date_on_the_same_base_updates() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0-nightly.20240201", "prerelease": true, "draft": false},
    ]);
    let world = World::new(
        "0.1.0",
        "0.2.0-nightly.20240201",
        "v0.2.0",
        Some(nightly),
        OLD_BINARY,
    );
    let (result, err) = world.run(
        "0.2.0-nightly.20240101",
        Channel::Nightly,
        false,
        b"",
        &World::no_restart,
    );
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn nightly_equal_to_running_is_already_newest() {
    let nightly = serde_json::json!([
        {"tag_name": "v0.2.0-nightly.20240101", "prerelease": true, "draft": false},
    ]);
    let world = World::new(
        "0.1.0",
        "0.2.0-nightly.20240101",
        "v0.2.0",
        Some(nightly),
        MARKER,
    );
    let (result, err) = world.run(
        "0.2.0-nightly.20240101",
        Channel::Nightly,
        false,
        b"",
        &World::no_restart,
    );
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn malformed_stable_tag_is_a_bad_version_before_any_rename() {
    let world = World::new("0.1.0", "0.2.0", "not-a-version", None, OLD_BINARY);
    let before = fs::read(&world.binary).unwrap();
    let (result, _) = world.run("0.1.0", Channel::Stable, false, b"", &World::no_restart);
    let CliError::Extensions(extensions::Error::BadVersion { .. }) = result.unwrap_err() else {
        panic!("a malformed redirect tag fails as a bad version");
    };
    assert_eq!(fs::read(&world.binary).unwrap(), before);
    assert_eq!(member_version(&world.home), "0.1.0");
}

#[test]
fn unwritable_binary_directory_names_it_before_any_rename() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let blocked = world.root.path().join("blocked");
    fs::write(&blocked, "not a directory").unwrap();
    let binary = blocked.join("fiber");
    let mut err = Vec::new();
    let result = super::run(
        &world.home,
        "0.1.0",
        TRIPLE,
        Channel::Stable,
        &world.bases(),
        &fakes::clock::SystemClock,
        Io {
            terminal: false,
            input: &mut Cursor::new(b"".as_slice()),
            err: &mut err,
        },
        &binary,
        &world.origin(),
        &World::no_restart,
    );
    let CliError::UpdateBinaryDirectory { dir, .. } = result.unwrap_err() else {
        panic!("an unwritable binary directory fails naming it");
    };
    assert_eq!(dir, blocked);
    assert_eq!(member_version(&world.home), "0.1.0");
}

#[test]
fn restart_failure_still_exits_zero_with_a_warning() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let refused = |_: &std::path::Path| {
        Err(CliError::Forwarded(crate::failed(
            contract::ErrorCode::IoFailed,
            "the manager refused",
        )))
    };
    let (result, err) = world.run("0.1.0", Channel::Stable, false, b"", &refused);
    assert_eq!(result.unwrap(), 0);
    assert!(err.contains("warning"), "{err}");
    assert!(err.contains("the manager refused"), "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn terminal_approval_is_asked_once_over_the_combined_summaries() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let (result, err) = world.run("0.1.0", Channel::Stable, true, b"y\n", &World::no_restart);
    assert_eq!(result.unwrap(), 0, "{err}");
    assert_eq!(err.matches("Go ahead?").count(), 1, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn non_terminal_proceeds_without_asking() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let (result, _) = world.run("0.1.0", Channel::Stable, false, b"", &World::no_restart);
    assert_eq!(result.unwrap(), 0);
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
}

#[test]
fn terminal_refusal_installs_nothing() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let (result, err) = world.run("0.1.0", Channel::Stable, true, b"n\n", &World::no_restart);
    assert_eq!(result.unwrap(), 1, "{err}");
    assert!(err.contains("nothing was installed"), "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), OLD_BINARY);
    assert_eq!(member_version(&world.home), "0.1.0");
}

/// `update` on the already-newest path returns zero through the public
/// entry point: the binary keeps its bytes, the members move, the docs
/// stay, and no rename touches the running binary. This kills
/// `update -> i32 with 1`.
#[test]
fn update_returns_zero_without_touching_the_binary_when_already_newest() {
    let world = World::new("0.2.0", "0.2.0", "v0.2.0", None, MARKER);
    let mut err = Vec::new();
    let code = super::update(
        &world.home,
        "0.2.0",
        TRIPLE,
        Channel::Stable,
        &world.bases(),
        &fakes::clock::SystemClock,
        Io {
            terminal: false,
            input: &mut Cursor::new(b"".as_slice()),
            err: &mut err,
        },
        &world.origin(),
    );
    let err = String::from_utf8(err).unwrap();
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read(&world.binary).unwrap(), MARKER);
    assert_eq!(member_version(&world.home), "0.2.0");
}

/// `update` reports its run's exit code: a failed tag fetch exits one.
/// The failure answers before any git runs, so the fixture origin never
/// leaves the loopback server.
#[test]
fn update_maps_a_failed_tag_fetch_to_exit_one() {
    let root = fakes::TempDir::new("fiber-cli-update-exit");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let server = fakes::ProviderServer::start_routed(
        [("/releases", fakes::Response::status(500, "boom"))],
        fakes::Response::status(404, "not found"),
    )
    .unwrap();
    let bases = ReleaseBases {
        web: server.url(),
        api: server.url(),
    };
    let mut err = Vec::new();
    let code = super::update(
        &home,
        "0.1.0",
        TRIPLE,
        Channel::Stable,
        &bases,
        &fakes::clock::SystemClock,
        Io {
            terminal: false,
            input: &mut Cursor::new(b"".as_slice()),
            err: &mut err,
        },
        &extensions::Origin::github(),
    );
    assert_eq!(code, 1);
}

/// A platform with no published archive fails before any download: the
/// run reports the platform and the published targets, and the fixture
/// sees no request of any kind, including the release-tag fetch.
#[test]
fn unlisted_platform_fails_before_any_download() {
    let world = World::new("0.1.0", "0.2.0", "v0.2.0", None, OLD_BINARY);
    let before = world.server.requests().len();
    let mut err = Vec::new();
    let result = super::run(
        &world.home,
        "0.1.0",
        "x86_64-apple-darwin",
        Channel::Stable,
        &world.bases(),
        &fakes::clock::SystemClock,
        Io {
            terminal: false,
            input: &mut Cursor::new(b"".as_slice()),
            err: &mut err,
        },
        &world.binary,
        &world.origin(),
        &World::no_restart,
    );
    let failed = result.unwrap_err();
    assert_eq!(failed.code(), contract::ErrorCode::Usage);
    let said = failed.to_string();
    assert!(said.contains("x86_64-apple-darwin"), "{said}");
    for archive in [
        "fiber-aarch64-apple-darwin.tar.gz",
        "fiber-x86_64-unknown-linux-musl.tar.gz",
        "fiber-aarch64-unknown-linux-musl.tar.gz",
    ] {
        assert!(said.contains(archive), "{said}");
    }
    assert_eq!(
        world.server.requests().len(),
        before,
        "no request runs, including the tag fetch: {:?}",
        world.server.requests()
    );
}
