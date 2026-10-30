//! The main journeys (#3667): screen tests that drive the release binary
//! the backstop built, through the existing screen harness, against the
//! fake provider. Each journey walks one path a person walks, from start
//! to finish, and asserts what the screen shows at every step
//! (`docs/testing.md`, "Journeys").
//!
//! The binary runs after merge (`docs/ci.md`, "The journey suite"), never
//! in the gate: `crates/main/Cargo.toml` declares it with `test = false`,
//! so only an explicit `--test main_journeys` builds, lints or runs it.
//! Every run names its binary in `FIBER_JOURNEY_BIN` and its failures file
//! in `FIBER_JOURNEY_FAILURES`; the caller creates the failures file empty
//! beforehand, and each failed journey appends one line naming itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::PathBuf;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::Setup;
use support::journey::shows;
use support::pty::Run;

/// The release binary under test: the path in `FIBER_JOURNEY_BIN`. Fails
/// naming the variable when unset or not executable, and never falls back
/// to a debug build: this is the file's only binary source.
fn journey_bin() -> PathBuf {
    let bin = std::env::var("FIBER_JOURNEY_BIN").unwrap_or_else(|_| {
        panic!("FIBER_JOURNEY_BIN is not set: the journey suite drives the release binary it names")
    });
    let path = PathBuf::from(&bin);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = path.is_file()
            && path
                .metadata()
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0);
        assert!(executable, "FIBER_JOURNEY_BIN is not executable: {bin}");
    }
    #[cfg(not(unix))]
    {
        assert!(path.is_file(), "FIBER_JOURNEY_BIN is not executable: {bin}");
    }
    path
}

/// Records one failed journey: prints its name to stderr first, then
/// appends it to the caller-created `FIBER_JOURNEY_FAILURES` file. On an
/// open or write failure panics naming the path, after the stderr print,
/// so the name is never lost silently.
fn record_failure(name: &str) {
    eprintln!("Journey failure: {name}");
    let path = std::env::var("FIBER_JOURNEY_FAILURES").unwrap_or_else(|_| {
        panic!(
            "FIBER_JOURNEY_FAILURES is not set: the caller creates the empty failures file before the run"
        )
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap_or_else(|error| {
            panic!("Journey failure: {name}: cannot append to FIBER_JOURNEY_FAILURES at {path}: {error}")
        });
    std::io::Write::write_all(&mut file, format!("{name}\n").as_bytes()).unwrap_or_else(|error| {
        panic!("Journey failure: {name}: cannot write to FIBER_JOURNEY_FAILURES at {path}: {error}")
    });
}

/// Runs one journey, recording its failure under its own test name: runs
/// `body` under `catch_unwind`, and on failure appends `name` to the
/// failures file, then resumes the unwind so nextest still reports the
/// failure itself. Never truncates the shared file.
fn run_journey(name: &str, body: impl FnOnce() + std::panic::UnwindSafe) {
    if let Err(payload) = std::panic::catch_unwind(body) {
        record_failure(name);
        std::panic::resume_unwind(payload);
    }
}

/// Installs only the fake provider extension, as the installed-provider
/// stand-in of a home right after install (`docs/releasing.md`, "Install
/// steps"): the extension-install portion of `Setup::provider`, without
/// its `config.json` write, so setup itself picks the provider, stores
/// the key and writes the model.
fn install_fake_provider_extension(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Setup from an installed home with a provider picked, then a first
/// turn: the person-visible grids of `fiber setup` answering the provider
/// pick, the key and the model, then one `fiber ask` turn answered by the
/// fake provider. Evidence for #3092, whose parent answers the pick and
/// then blocks before the nested login flushes its credential prompt:
/// this journey fails at that later credential-prompt wait.
#[test]
fn setup_from_installed_home_with_provider_pick_then_first_turn() {
    run_journey(
        "setup_from_installed_home_with_provider_pick_then_first_turn",
        // The body borrows the fake server, whose shared state is a
        // mutex: the standard wrapper for `catch_unwind` captures. No
        // server state is touched after a caught panic; the wrapper only
        // records the failure and unwinds.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = ProviderServer::start([support::hello_inline_completed()]).unwrap();
            install_fake_provider_extension(&setup, &server);
            assert!(
                !setup.home().join("config.json").exists(),
                "setup writes the config"
            );
            assert!(
                !setup.home().join("credentials").exists(),
                "setup stores the key"
            );
            assert!(
                !setup.home().join("projects").exists(),
                "no session state before setup"
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &["setup", "--no-hub", "--no-completions"],
                &[],
            );
            let at = run.wait_bytes(
                0,
                b"Sign in to providers by number or name, separated by commas (blank skips): ",
                "the provider-selection prompt",
            );
            run.wait_screen("the provider question", |grid| {
                shows(grid, "Providers:") && shows(grid, "Sign in to providers")
            });
            run.write(b"1\n");
            let at = run.wait_bytes(at, b"Key for fake: ", "the fake credential prompt");
            run.wait_screen("the fake credential prompt", |grid| {
                shows(grid, "Key for fake:")
            });
            run.write(b"sk-journey\n");
            let at = run.wait_bytes(at, b"Reviewer, by number or name: ", "the reviewer prompt");
            run.wait_screen("the reviewer question", |grid| {
                shows(grid, "Reviewer for fake:") && shows(grid, "Reviewer, by number")
            });
            run.write(b"1\n");
            run.wait_bytes(
                at,
                b"Pick the default model by number or name (blank keeps current): ",
                "the default-model prompt",
            );
            run.wait_screen("the default-model question", |grid| {
                shows(grid, "Default model:") && shows(grid, "fake/m")
            });
            run.write(b"1\n");
            let exited = run.wait();
            assert!(exited.status.success(), "setup exits 0");
            assert!(
                server.requests().is_empty(),
                "setup makes no provider calls"
            );
            let config: Value = serde_json::from_str(
                &std::fs::read_to_string(setup.home().join("config.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(config["model"], json!("fake/m"));
            assert_eq!(
                std::fs::read_to_string(setup.home().join("credentials/fake/default")).unwrap(),
                "sk-journey"
            );
            let mut ask = Run::spawn_journey(&setup, 120, 32, &bin, &["ask", "hi"], &[]);
            ask.wait_screen("the first answer", |grid| shows(grid, "Hello."));
            let exited = ask.wait();
            assert!(exited.status.success(), "the first turn exits 0");
        }),
    );
}

/// The nightly the journey's stub feed serves, above the release
/// binary's `0.0.0`.
const JOURNEY_NIGHTLY: &str = "0.0.1-nightly.20240101";

/// The bytes the stub binary archive holds: the staged `fiber` binary.
const JOURNEY_MARKER: &[u8] = b"binary-from-the-journey-release";

/// Fiber's own repository as a local one, tagged only with the nightly,
/// and the git config key that makes `https://github.com/` mean the
/// directory holding it: the `update.rs` `Github` pattern, file-local.
/// No stable tag exists, so only the nightly path can satisfy the run.
struct JourneyGithub {
    key: String,
}

impl JourneyGithub {
    fn new(setup: &Setup) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        std::fs::create_dir_all(repo.join("providers/anthropic")).unwrap();
        std::fs::write(repo.join("fiber.md"), "fiber").unwrap();
        std::fs::write(
            repo.join("providers/anthropic/extension.json"),
            json!({
                "name": "github.com/aakshintala/fiber/providers/anthropic",
                "version": JOURNEY_NIGHTLY,
                "fiber": "0.0.0",
                "api": 1,
            })
            .to_string(),
        )
        .unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "fiber"]);
        fakes::git::git(&repo, &["tag", &format!("v{JOURNEY_NIGHTLY}")]);
        Self {
            key: format!("url.file://{}/.insteadOf", base.display()),
        }
    }
}

/// The stub nightly feed: the nightly list naming only the nightly, its
/// tag page, and the nightly's binary, docs and extensions archives with
/// checksums. It serves no stable tag or release asset.
fn serve_nightly_feed() -> ProviderServer {
    use fakes::ustar::{archive, gzip, header, sha256};
    let binary_archive = format!(
        "fiber-{}.tar.gz",
        extensions::release_triple(env!("FIBER_TARGET")).unwrap()
    );
    let binary = gzip(&archive(&[(
        header("fiber", b'0', JOURNEY_MARKER.len() as u64, 0o644, ""),
        JOURNEY_MARKER,
    )]));
    let docs = gzip(&archive(&[(
        header("README.md", b'0', 4, 0o644, ""),
        b"docs".as_slice(),
    )]));
    let manifest = json!({
        "name": "github.com/aakshintala/fiber/providers/anthropic",
        "version": JOURNEY_NIGHTLY,
        "fiber": JOURNEY_NIGHTLY,
        "api": 1,
    })
    .to_string();
    let extensions = gzip(&archive(&[(
        header(
            "anthropic/extension.json",
            b'0',
            manifest.len() as u64,
            0o644,
            "",
        ),
        manifest.as_bytes(),
    )]));
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let mut routes: Vec<(String, fakes::Response)> = Vec::new();
    for (name, bytes) in [
        (binary_archive, binary),
        ("fiber-docs.tar.gz".to_owned(), docs),
        ("fiber-extensions.tar.gz".to_owned(), extensions),
    ] {
        routes.push((
            format!("/releases/download/v{JOURNEY_NIGHTLY}/{name}"),
            ok(bytes.clone()),
        ));
        routes.push((
            format!("/releases/download/v{JOURNEY_NIGHTLY}/{name}.sha256"),
            ok(sha256(&bytes).into_bytes()),
        ));
    }
    routes.push((
        format!("/releases/tag/v{JOURNEY_NIGHTLY}"),
        fakes::Response::status(200, ""),
    ));
    routes.push((
        "/releases".to_owned(),
        fakes::Response::status(
            200,
            json!([{ "tag_name": format!("v{JOURNEY_NIGHTLY}"), "prerelease": true, "draft": false }])
                .to_string(),
        ),
    ));
    ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// `fiber update --channel nightly` against the stub feed: the
/// person-visible approval prompt and update announcement on the release
/// binary's terminal. Evidence for #3617, whose parent rejects the
/// nightly before the approval prompt is ever flushed: this journey
/// fails at that approval-prompt wait.
#[test]
fn update_to_nightly_from_local_stub_feed() {
    run_journey(
        "update_to_nightly_from_local_stub_feed",
        // As in the setup journey: the body borrows the fake server, so
        // the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = serve_nightly_feed();
            let github = JourneyGithub::new(&setup);
            // A copy of the release binary the update renames over, so
            // the shared binary stays untouched.
            let copy = setup.root.path().join("fiber-copy");
            std::fs::copy(journey_bin(), &copy).unwrap();
            let base_url = server.url();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &copy,
                &[
                    "update",
                    "--channel",
                    "nightly",
                    "--base-url",
                    base_url.as_str(),
                ],
                &[
                    ("GIT_CONFIG_COUNT", "1"),
                    ("GIT_CONFIG_KEY_0", github.key.as_str()),
                    ("GIT_CONFIG_VALUE_0", "https://github.com/"),
                ],
            );
            let at = run.wait_bytes(
                0,
                b"Go ahead? [y/N/s to show the full source] ",
                "the update approval prompt",
            );
            run.wait_screen("the update approval", |grid| shows(grid, "Go ahead?"));
            run.write(b"y\n");
            run.wait_bytes(at, b"fiber: updated ", "the nightly update announcement");
            run.wait_screen("the nightly update announcement", |grid| {
                shows(grid, "fiber: updated")
            });
            let exited = run.wait();
            assert!(exited.status.success(), "nightly update exits 0");
            assert_eq!(std::fs::read(&copy).unwrap(), JOURNEY_MARKER);
        }),
    );
}
