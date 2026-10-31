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
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a failed journey prints its name"
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

/// The contract edges below run through this child-side probe, so no test
/// mutates its own process environment: each parent spawns the test
/// binary filtered to this one test with a crafted environment, and
/// asserts on the child's exit and stderr. Without `FIBER_JOURNEY_EDGE`
/// it passes silently, so the normal suite run stays green.
#[test]
fn edge_child_probe() {
    match std::env::var("FIBER_JOURNEY_EDGE").as_deref() {
        Ok("bin") => {
            let _ = journey_bin();
        }
        Ok("record") => record_failure("edge-line"),
        _ => {}
    }
}

/// Runs [`edge_child_probe`] in a child process with `vars` applied over
/// the inherited environment, returning its output. The blocking wait
/// runs on a thread under a fresh test deadline, as every binary-level
/// wait does (`docs/testing.md`, "Waits and timeouts"): expiry kills
/// the child's group and reaps it before failing naming the probe, so
/// no hang outlives the deadline and no child is left behind.
fn run_edge_child(vars: &[(&str, Option<&str>)]) -> std::process::Output {
    let deadline = support::Deadline::start();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .arg("edge_child_probe")
        .arg("--exact")
        .arg("--nocapture");
    for (key, value) in vars {
        match value {
            Some(text) => {
                command.env(key, text);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    // Piped, as `Command::output` pipes them: the assertions below
    // read the child's stderr, and an inherited terminal would also
    // interleave the child's harness output with this test's own.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (child, watchdog) = support::spawn_watched(&mut command);
    let group = child.id();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match deadline.recv(&finished) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(deadline, group, &finished, "the edge child probe to exit"),
    };
    watchdog.stand_down(deadline.cleanup_phase());
    output
}

/// A fresh per-test temporary directory for the contract edge probes:
/// the system temporary directory is shared across test runs, so a
/// fixed name could meet another run's file (`docs/testing/running.md`,
/// "Running tests").
fn edge_temp_dir() -> fakes::TempDir {
    fakes::TempDir::new("fiber-journey-edge")
}

/// Without `FIBER_JOURNEY_BIN` the suite names the variable instead of
/// falling back to any build: the failure tells the caller what to set.
#[test]
fn journey_bin_without_its_variable_names_it() {
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("bin")),
        ("FIBER_JOURNEY_BIN", None),
    ]);
    assert!(
        !run_edge_child_output.status.success(),
        "an unset variable fails"
    );
    let stderr = String::from_utf8_lossy(&run_edge_child_output.stderr);
    assert!(
        stderr.contains("FIBER_JOURNEY_BIN is not set"),
        "names the variable: {stderr}"
    );
}

/// A missing binary path fails naming it: the file check, not the
/// executable bit, rejects it on every platform.
#[test]
fn journey_bin_at_a_missing_path_names_it() {
    let edge_dir = edge_temp_dir();
    let missing = edge_dir.path().join("fiber-journey-no-such-binary");
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("bin")),
        ("FIBER_JOURNEY_BIN", Some(missing.to_str().unwrap())),
    ]);
    assert!(
        !run_edge_child_output.status.success(),
        "a missing file fails"
    );
    let stderr = String::from_utf8_lossy(&run_edge_child_output.stderr);
    assert!(
        stderr.contains("FIBER_JOURNEY_BIN is not executable"),
        "names the variable: {stderr}"
    );
    assert!(
        stderr.contains("fiber-journey-no-such-binary"),
        "names the path: {stderr}"
    );
}

/// A present file without the executable bit fails naming it. Unix-only:
/// elsewhere executability is the file check the missing-path test pins.
#[cfg(unix)]
#[test]
fn journey_bin_at_a_non_executable_file_names_it() {
    let edge_dir = edge_temp_dir();
    let plain = edge_dir.path().join("fiber-journey-plain-file");
    std::fs::write(&plain, "not a binary").unwrap();
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("bin")),
        ("FIBER_JOURNEY_BIN", Some(plain.to_str().unwrap())),
    ]);
    assert!(
        !run_edge_child_output.status.success(),
        "a non-executable file fails"
    );
    let stderr = String::from_utf8_lossy(&run_edge_child_output.stderr);
    assert!(
        stderr.contains("FIBER_JOURNEY_BIN is not executable"),
        "names the variable: {stderr}"
    );
    assert!(
        stderr.contains("fiber-journey-plain-file"),
        "names the path: {stderr}"
    );
}

/// An executable file passes: the test binary itself, which the suite
/// just executed.
#[test]
fn journey_bin_returns_an_executable_file() {
    let exe = std::env::current_exe().unwrap();
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("bin")),
        ("FIBER_JOURNEY_BIN", Some(exe.to_str().unwrap())),
    ]);
    assert!(
        run_edge_child_output.status.success(),
        "an executable file passes: {}",
        String::from_utf8_lossy(&run_edge_child_output.stderr)
    );
}

/// A failure appends after the entries already in the file, preserving
/// their order: the caller-created file is never truncated.
#[test]
fn record_failure_appends_after_existing_entries() {
    let edge_dir = edge_temp_dir();
    let file = edge_dir.path().join("fiber-journey-failures");
    std::fs::write(&file, "earlier-line\n").unwrap();
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("record")),
        ("FIBER_JOURNEY_FAILURES", Some(file.to_str().unwrap())),
    ]);
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(
        run_edge_child_output.status.success(),
        "the append passes: {}",
        String::from_utf8_lossy(&run_edge_child_output.stderr)
    );
    assert_eq!(body, "earlier-line\nedge-line\n");
}

/// When the append cannot open its file, the name is already on stderr
/// before the panic naming the path: the failure is never lost silently.
#[test]
fn record_failure_prints_before_the_append_panics() {
    let edge_dir = edge_temp_dir();
    let missing = edge_dir.path().join("fiber-journey-no-such-dir");
    let target = missing.join("failures");
    let run_edge_child_output = run_edge_child(&[
        ("FIBER_JOURNEY_EDGE", Some("record")),
        ("FIBER_JOURNEY_FAILURES", Some(target.to_str().unwrap())),
    ]);
    assert!(
        !run_edge_child_output.status.success(),
        "an unopenable file fails"
    );
    let stderr = String::from_utf8_lossy(&run_edge_child_output.stderr);
    let printed = stderr
        .find("Journey failure: edge-line")
        .expect("prints the name");
    let panicked = stderr.find("panicked").expect("then panics");
    assert!(printed < panicked, "the print precedes the panic: {stderr}");
    assert!(
        stderr.contains("fiber-journey-no-such-dir"),
        "then names the path: {stderr}"
    );
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

/// The newer nightly the stub feed also serves: a nightly-floored install
/// moves onto it.
const JOURNEY_NIGHTLY_NEXT: &str = "0.0.1-nightly.20240102";

/// The bytes the stub binary archive holds: the staged `fiber` binary.
const JOURNEY_MARKER: &[u8] = b"binary-from-the-journey-release";

/// Fiber's own repository as a local one, tagged with the nightlies,
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
        for nightly in [JOURNEY_NIGHTLY, JOURNEY_NIGHTLY_NEXT] {
            fakes::git::git(&repo, &["tag", &format!("v{nightly}")]);
        }
        Self {
            key: format!("url.file://{}/.insteadOf", base.display()),
        }
    }
}

/// The stub nightly feed: the nightly list naming the nightlies as
/// pre-releases, their tag pages, and each nightly's binary, docs and
/// extensions archives with checksums. It serves no stable tag or
/// release asset.
fn serve_nightly_feed() -> ProviderServer {
    use fakes::ustar::{archive, gzip, header, sha256};
    let binary_archive = format!(
        "fiber-{}.tar.gz",
        extensions::release_triple(env!("FIBER_TARGET")).unwrap()
    );
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let mut routes: Vec<(String, fakes::Response)> = Vec::new();
    // Every nightly run fetches its own archives, as a real server
    // answers every request.
    for nightly in [JOURNEY_NIGHTLY, JOURNEY_NIGHTLY_NEXT] {
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
            "version": nightly,
            "fiber": nightly,
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
        for (name, bytes) in [
            (binary_archive.clone(), binary),
            ("fiber-docs.tar.gz".to_owned(), docs),
            ("fiber-extensions.tar.gz".to_owned(), extensions),
        ] {
            routes.push((
                format!("/releases/download/v{nightly}/{name}"),
                ok(bytes.clone()),
            ));
            routes.push((
                format!("/releases/download/v{nightly}/{name}.sha256"),
                ok(sha256(&bytes).into_bytes()),
            ));
        }
        routes.push((
            format!("/releases/tag/v{nightly}"),
            fakes::Response::status(200, ""),
        ));
    }
    let list: Vec<Value> = [JOURNEY_NIGHTLY, JOURNEY_NIGHTLY_NEXT]
        .iter()
        .map(|nightly| {
            json!({ "tag_name": format!("v{nightly}"), "prerelease": true, "draft": false })
        })
        .collect();
    routes.push((
        "/releases".to_owned(),
        fakes::Response::status(200, Value::Array(list).to_string()),
    ));
    ProviderServer::start_routed(
        routes
            .iter()
            .map(|(path, response)| (path.as_str(), response.clone())),
        fakes::Response::status(404, "not found"),
    )
    .unwrap()
}

/// Installs the anthropic extension as a nightly install wrote it: the
/// installed record carries the nightly version. #3617's update failed
/// on such a home, where the shared resolver fed the nightly have-value
/// through the strict version parser; a fresh home never trips it.
fn install_nightly_extension(setup: &Setup) {
    let source = setup.root.path().join("nightlysrc");
    support::write_json(
        &source.join("extension.json"),
        &json!({
            "name": "github.com/aakshintala/fiber/providers/anthropic",
            "version": JOURNEY_NIGHTLY,
            "fiber": "0.0.0",
            "api": 1,
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

/// `fiber update --channel nightly` against the stub feed: the
/// person-visible approval prompt and update announcement on the release
/// binary's terminal. Evidence for #3617, whose parent rejects the
/// nightly install record before the approval prompt is ever flushed:
/// this journey fails at that approval-prompt wait.
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
            install_nightly_extension(&setup);
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

/// Installs the first-party extension at the nightly the feed serves, as
/// a nightly install wrote it: a repository-backed install from
/// JourneyGithub's tagged origin, with the record carrying the nightly
/// version and floor. `Request::Install` resolves the newest stable tag,
/// so the helper tags a stable one first; the record is then set to the
/// nightly, mirroring the binary fixture's nightly-floored home. Path
/// provenance would bypass the resolver, so the install goes through git.
fn install_first_party_nightly_extension(setup: &Setup) {
    const NAME: &str = "github.com/aakshintala/fiber/providers/anthropic";
    let repo = setup.root.path().join("gh/aakshintala/fiber");
    // The stable tag the install resolves; only the nightly floor moves
    // the extension afterwards.
    fakes::git::git(&repo, &["tag", "v0.0.0"]);
    // A file-backed origin: the install runs in-process, where the
    // child's `url.insteadOf` rewrite does not apply.
    let origin = extensions::Origin::new("git", {
        let dir = repo.clone();
        move |_| format!("file://{}", dir.display())
    });
    extensions::plan(
        &setup.home(),
        &extensions::Request::Install(NAME.into()),
        "0.0.0",
        &origin,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    // The nightly install's record: the version from the record, the
    // nightly floor, and the release tag's commit as the source.
    let commit = fakes::git::git(
        &repo,
        &["rev-list", "-n", "1", &format!("v{JOURNEY_NIGHTLY}")],
    );
    let path = setup.home().join("extensions/anthropic/.fiber.json");
    let mut record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record["version"] = Value::String(JOURNEY_NIGHTLY.into());
    record["floor"] = Value::String(JOURNEY_NIGHTLY.into());
    record["source"] = json!({ "commit": commit });
    std::fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
}

/// `fiber update --channel nightly` with a nightly-floored first-party
/// extension installed: the person-visible approval prompt and update
/// announcement on the release binary's terminal, and the extension
/// record moved to the newest nightly. Evidence for #3746, whose base
/// leaves the record damaged-skipped at its nightly floor and never
/// reaches the resolver: this journey fails before the record moves.
#[test]
fn update_nightly_floored_extension_to_newest_nightly() {
    run_journey(
        "update_nightly_floored_extension_to_newest_nightly",
        // As in the setup journey: the body borrows the fake server, so
        // the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = serve_nightly_feed();
            let github = JourneyGithub::new(&setup);
            install_first_party_nightly_extension(&setup);
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
            let record: Value = serde_json::from_str(
                &std::fs::read_to_string(setup.home().join("extensions/anthropic/.fiber.json"))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(record["version"], JOURNEY_NIGHTLY_NEXT, "{record}");
            assert_eq!(record["floor"], JOURNEY_NIGHTLY, "{record}");
        }),
    );
}

/// Says not now to the unseeded home's first-run import offer: every TUI
/// journey starts from a plain home, since the suite never seeds import
/// receipts.
fn dismiss_first_run_offer(run: &mut Run) {
    run.wait_screen("the first-run import offer", |grid| {
        shows(grid, "Import settings")
    });
    run.write(b"\x1b[27u");
    run.wait_screen("the dismissed offer", |grid| {
        !shows(grid, "Import settings")
    });
}

/// Quits a TUI journey's terminal and waits for it, as the quota screen
/// tests do: the person-visible exit.
fn quit_journey(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Installs the Lua extension `test-ext` with a `test` provider whose
/// models, credential and quota functions each make one real HTTP
/// request: the quota-journey half of `quota_fetch_session.rs`'s
/// installer, file-local to this binary.
fn install_quota_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("luasrc");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "test-ext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"test": [format!("{server_url}/v1")]}}),
    );
    std::fs::create_dir_all(source.join("providers")).unwrap();
    std::fs::write(
        source.join("providers/test.lua"),
        format!(
            "fiber.provider(\"test\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ url = \"{server_url}/lua-models\" }})\n\
                 return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{server_url}/v1\", context_window = 100000 }}}}\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 local r = host.http({{ url = \"{server_url}/lua-quota\" }})\n\
                 return {{ windows = {{ {{ name = \"w\", used_percent = tonumber(r.body) or 1, resets_at = nil }} }} }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// A server answering every Lua purpose plus native model calls: the
/// quota fetch reports 90% used.
fn start_quota_server() -> ProviderServer {
    ProviderServer::start_responding(|request| match request.path.as_str() {
        "/lua-quota" => fakes::Response::status(200, "90"),
        "/lua-models" | "/lua-token" => fakes::Response::status(200, "{}"),
        "/v1/responses" => support::hello(),
        _ => fakes::Response::status(500, "unrouted"),
    })
    .unwrap()
}

/// A quota notice in the middle of a conversation, then the next prompt
/// drawn as the person's: two turns on the fake Lua provider, the quota
/// fetch landing between them. Evidence for #3581, whose parent drew the
/// second prompt's rows wrong after the notice.
#[test]
fn quota_notice_then_next_prompt_drawn_as_person() {
    run_journey(
        "quota_notice_then_next_prompt_drawn_as_person",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = start_quota_server();
            install_quota_provider(&setup, &server.url());
            std::fs::create_dir_all(setup.home().join("credentials/test")).unwrap();
            std::fs::write(setup.home().join("credentials/test/work"), "test-key").unwrap();
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "test/m1", "diagnostics": {"level": "debug"}, "hub": {"idle_exit_ms": 200}}),
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(&setup, 160, 48, &bin, &[], &support::TRUECOLOUR);
            run.ready();
            dismiss_first_run_offer(&mut run);
            let from = run.output().len();
            run.write(b"first\r");
            run.wait_screen("the first answered turn", |grid| {
                grid.contents.contains("Hello.")
            });
            // The drawn value proves the fetch landed: the fetch-less
            // snapshot behind focus-in never re-asks when a flight lands
            // after it, so one focus-in can miss the first turn's fetch
            // still in flight. Re-ask on every pass until the drawn
            // value proves `finish_quota` stored it
            // (`quota_fetch_session.rs`,
            // `slash_quota_opens_the_view_and_shows_the_fetched_values`;
            // `docs/testing.md`, "Waits and timeouts"): each focus-in
            // sends a fetch-less query that starts no new fetch, so
            // re-asking starts no new `/lua-quota` request. The poll
            // reads the raw output, never the grid, so the file's zero
            // bare screen reads stand; the grid wait below is the
            // assertion.
            let quota_from = run.output().len();
            loop {
                run.write(b"\x1b[I");
                if String::from_utf8_lossy(&run.output()[quota_from..]).contains("10%") {
                    break;
                }
                assert!(
                    !setup.deadline.left().is_zero(),
                    "waited until the deadline for the fetched quota share to draw after focus-in"
                );
                std::thread::yield_now();
            }
            run.wait_screen("the fetched quota share", |grid| {
                grid.contents.contains("10%")
            });
            // The reply draws while the first turn is still running, so
            // the second prompt waits for its completed card: sent
            // earlier, it would steer the first turn instead of starting
            // the second.
            run.turn_finished(from);
            let from = run.output().len();
            run.write(b"second\r");
            let shown = run.wait_screen("the second turn's quota notice", |grid| {
                grid.contents.contains("Fiber")
                    && grid.contents.contains("used 90%")
                    && grid.contents.contains("second")
            });
            // Exactly one quota entry: the count the notice gives, never
            // one row per item and never the full verbatim list.
            assert_eq!(
                shown
                    .rows
                    .iter()
                    .filter(|row| row.contains("used 90%"))
                    .count(),
                1,
                "one quota entry: {:?}",
                shown.rows
            );
            // The first-item identity: the provider credential and window
            // the quota line names.
            assert!(
                shown.contents.contains("w quota"),
                "the quota window: {}",
                shown.contents
            );
            // Only the person's prompt floats above its card: one `you`
            // row per turn (`docs/tui.md`, "Turns").
            assert_eq!(
                shown
                    .contents
                    .lines()
                    .filter(|line| line.trim_start().starts_with("you "))
                    .count(),
                2,
                "one you row per turn: {}",
                shown.contents
            );
            run.turn_finished(from);
            // The second answer drew beside the first: two turns, two
            // answers on the grid.
            let done = run.wait_screen("the second answer", |grid| {
                grid.contents.matches("Hello.").count() >= 2
            });
            assert_eq!(
                done.contents.matches("Hello.").count(),
                2,
                "both answers drawn: {}",
                done.contents
            );
            quit_journey(run);
        }),
    );
}

/// Installs a provider `fake` with five models in distinct states: `m-ok`
/// is valid and configured, `m-nocontext` and `m-nocontext2` declare no
/// `context_window` (`model_invalid`, first message naming `m-nocontext`),
/// and `m-host` and `m-host2` name the `workspace` placeholder whose
/// setting value is not a host (`model_unconfigured`, first message naming
/// `m-host`). Each pair groups into one notice row with its count, which
/// is what the journey asserts (`docs/errors.md`, "Notices").
fn install_picker_states_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("src");
    // The manifest lists every base-URL pattern the models use,
    // including the placeholder one (`docs/model-routing.md`, "A
    // per-account host").
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{server_url}/v1"), "https://{workspace}/v1"]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {}},
            "models": [
                {"id": "m-ok", "protocol": "openai-responses", "base_url": format!("{server_url}/v1"), "context_window": 100000},
                {"id": "m-nocontext", "protocol": "openai-responses", "base_url": format!("{server_url}/v1")},
                {"id": "m-nocontext2", "protocol": "openai-responses", "base_url": format!("{server_url}/v1")},
                {"id": "m-host", "protocol": "openai-responses", "base_url": "https://{workspace}/v1", "context_window": 100000},
                {"id": "m-host2", "protocol": "openai-responses", "base_url": "https://{workspace}/v1", "context_window": 100000},
            ],
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
    // The setting value is not a host, so the fill leaves the model out
    // per model rather than grouping it.
    support::write_json(
        &setup.home().join("config/fake.json"),
        &json!({"workspace": "x/y"}),
    );
}

/// Installs the Lua extension `extension` with provider `provider` whose
/// models function always fails: the catalogue refresh names the
/// extension with `extension_failed`. The journey installs two, so the
/// pair groups into one row with its count.
fn install_failing_lua_provider(setup: &Setup, server_url: &str, extension: &str, provider: &str) {
    let source = setup.root.path().join(format!("{extension}src"));
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": extension, "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {provider: [format!("{server_url}/v1")]}}),
    );
    std::fs::create_dir_all(source.join("providers")).unwrap();
    std::fs::write(
        source.join(format!("providers/{provider}.lua")),
        format!(
            "fiber.provider(\"{provider}\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 error(\"{extension} models are broken\")\n\
               end }},\n\
               credential = {{ timeout = 60000, run = function()\n\
                 local r = host.http({{ method = \"POST\", url = \"{server_url}/lua-token\" }})\n\
                 return {{ token = \"tok\", expires_at = 1893456000 }}\n\
               end }},\n\
               quota = {{ timeout = 60000, run = function(q)\n\
                 return {{ windows = {{}} }}\n\
               end }},\n\
               sign = {{ timeout = 60000, run = function(request) return {{}} end }},\n\
             }})\n"
        ),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Every model-picker notice code shown as itself: the picker band carries
/// each bad state's own code beside the model it names. Evidence for
/// #3580, whose parent showed every picker notice as `model_invalid`.
/// The journey covers `model_invalid` itself with two models declaring no
/// `context_window`, grouped into one row with its count.
#[test]
fn each_model_picker_notice_code_shown_as_itself() {
    run_journey(
        "each_model_picker_notice_code_shown_as_itself",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = ProviderServer::start_responding(|request| match request.path.as_str() {
                "/lua-models" | "/lua-token" | "/lua-quota" => fakes::Response::status(200, "{}"),
                _ => fakes::Response::status(500, "unrouted"),
            })
            .unwrap();
            install_picker_states_provider(&setup, &server.url());
            install_failing_lua_provider(&setup, &server.url(), "badext", "bad");
            install_failing_lua_provider(&setup, &server.url(), "zbadext", "zbad");
            for provider in ["bad", "zbad"] {
                std::fs::create_dir_all(setup.home().join(format!("credentials/{provider}")))
                    .unwrap();
                std::fs::write(
                    setup.home().join(format!("credentials/{provider}/work")),
                    "test-key",
                )
                .unwrap();
            }
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "fake/m-ok", "hub": {"idle_exit_ms": 200}}),
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &[],
                &[("FIBER_TEST_FAKE_KEY", "sk-test")],
            );
            run.ready();
            dismiss_first_run_offer(&mut run);
            // Each wait names its code and its first model, which survive
            // the 120-column truncation that cuts the message's tail. Each
            // pair groups into one row: the bulk lead counts both, and the
            // first item names the first model (`docs/errors.md`,
            // "Notices").
            let screen = run.wait_screen("the model_unconfigured notice", |grid| {
                shows(grid, "model_unconfigured") && shows(grid, "fake/m-host")
            });
            let flagged: Vec<&String> = screen
                .rows
                .iter()
                .filter(|row| row.contains("model_unconfigured"))
                .collect();
            assert_eq!(
                flagged.len(),
                1,
                "one model_unconfigured row: {:?}",
                screen.rows
            );
            assert!(
                flagged[0].contains("2 models are unconfigured"),
                "the pair's count: {:?}",
                screen.rows
            );
            assert!(
                flagged[0].contains("fake/m-host"),
                "the first model: {:?}",
                screen.rows
            );
            assert!(
                screen
                    .rows
                    .iter()
                    .all(|row| !(row.contains("model_invalid") && row.contains("fake/m-host"))),
                "no model_invalid row carries it: {:?}",
                screen.rows
            );
            let screen = run.wait_screen("the model_invalid notice", |grid| {
                shows(grid, "model_invalid") && shows(grid, "fake/m-nocontext")
            });
            let flagged: Vec<&String> = screen
                .rows
                .iter()
                .filter(|row| row.contains("model_invalid"))
                .collect();
            assert_eq!(flagged.len(), 1, "one model_invalid row: {:?}", screen.rows);
            assert!(
                flagged[0].contains("2 `model_invalid` notices"),
                "the pair's count: {:?}",
                screen.rows
            );
            assert!(
                flagged[0].contains("fake/m-nocontext"),
                "the first model: {:?}",
                screen.rows
            );
            assert!(
                screen
                    .rows
                    .iter()
                    .all(|row| !(row.contains("model_unconfigured")
                        && row.contains("fake/m-nocontext"))),
                "no model_unconfigured row carries it: {:?}",
                screen.rows
            );
            // Ctrl+L opens the picker: each open asks `Stale`, so the open
            // runs the Lua refresh, which fails and names the extension.
            run.write(b"\x0c");
            run.wait_screen("the open picker", |grid| shows(grid, "Type to search"));
            let screen = run.wait_screen("the extension_failed notice", |grid| {
                shows(grid, "extension_failed") && shows(grid, "bad")
            });
            let flagged: Vec<&String> = screen
                .rows
                .iter()
                .filter(|row| row.contains("extension_failed"))
                .collect();
            assert_eq!(
                flagged.len(),
                1,
                "one extension_failed row: {:?}",
                screen.rows
            );
            assert!(
                flagged[0].contains("2 `extension_failed` notices"),
                "the pair's count: {:?}",
                screen.rows
            );
            assert!(
                flagged[0].contains("badext"),
                "the first extension: {:?}",
                screen.rows
            );
            assert!(
                screen
                    .rows
                    .iter()
                    .all(|row| !(row.contains("extension_failed") && row.contains("zbadext"))),
                "the second extension is not first: {:?}",
                screen.rows
            );
            // No other code reaches the band: the catalogue read carries
            // only these three bad states, and every other registry code
            // belongs to another pass (login, extension load, session).
            assert!(
                screen
                    .rows
                    .iter()
                    .filter(|row| row.contains("⚠ "))
                    .all(|row| row.contains("model_unconfigured")
                        || row.contains("model_invalid")
                        || row.contains("extension_failed")),
                "only the three bad states show: {:?}",
                screen.rows
            );
            // Esc closes the picker with no query typed.
            run.write(b"\x1b[27u");
            run.wait_screen("home again", |grid| !shows(grid, "Type to search"));
            quit_journey(run);
        }),
    );
}

/// Installs a provider `fake` with sixty valid models `m-00` to `m-59`,
/// so the model picker overflows any journey terminal.
fn install_many_models_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{server_url}/v1")]}}),
    );
    let models: Vec<Value> = (0..60)
        .map(|n| {
            json!({"id": format!("m-{n:02}"), "protocol": "openai-responses", "base_url": format!("{server_url}/v1"), "context_window": 100000})
        })
        .collect();
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": models,
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

/// The first row containing `needle`, as a `u16` screen row.
fn find_grid_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .and_then(|y| u16::try_from(y).ok())
}

/// One wheel step down at `coord`: SGR button 65.
fn wheel_down(coord: (u16, u16)) -> Vec<u8> {
    format!("\x1b[<65;{};{}M", coord.0, coord.1).into_bytes()
}

/// An `openai-responses` stream with one `read` call of `path`: one round
/// of the wheel journey's scripted turn.
fn wheel_read_call(n: usize, path: &str) -> fakes::Response {
    fakes::wire::stream(&[
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call", "id": format!("fc-{n}"), "call_id": format!("call-{n}"),
            "name": "read", "arguments": format!("{{\"path\":\"{path}\"}}")}}),
        json!({"type": "response.completed", "response": {
            "id": format!("resp-{n}"), "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 3}}}),
    ])
}

/// The wheel over every swapped view scrolls that view: the model
/// picker, config, keys and session views each move their own rows
/// under the wheel, never the hidden conversation. Evidence for #3578,
/// whose parent let the wheel reach the wrong surface.
#[test]
fn wheel_over_each_swapped_view_scrolls_that_view() {
    run_journey(
        "wheel_over_each_swapped_view_scrolls_that_view",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            // Five read rounds of distinct sizes, then the answer: the
            // turn's tool results fill the context view's largest-rows,
            // and every round's usage fills its sizes.
            let mut script: Vec<fakes::Response> = (1..=5)
                .map(|n| wheel_read_call(n, &format!("c{n}.txt")))
                .collect();
            script.push(support::hello_inline_completed());
            let server = ProviderServer::start(script).unwrap();
            install_many_models_provider(&setup, &server.url());
            for (n, size) in [100usize, 200, 300, 400, 500].iter().enumerate() {
                let letter = (b'a' + u8::try_from(n).unwrap()) as char;
                std::fs::write(
                    setup.workspace().join(format!("c{}.txt", n + 1)),
                    format!("{letter}\n").repeat(*size),
                )
                .unwrap();
            }
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "fake/m-00", "hub": {"idle_exit_ms": 1000},
                    "tui": {"rail": {"shown": false}}}),
            );
            std::fs::write(
                setup.home().join("rules"),
                format!(
                    "{}\n{}\n{}\n",
                    json!({"decision": "allow", "tool": "shell", "prefix": ""}),
                    json!({"decision": "allow", "tool": "edit", "prefix": ""}),
                    json!({"decision": "allow", "tool": "write", "prefix": ""}),
                ),
            )
            .unwrap();
            let bin = journey_bin();
            // Fourteen rows: the context breakdown still overflows its
            // body viewport by one full three-row wheel step, while the
            // picker stays in its full layout. At thirteen rows the picker
            // compacts, and the wheel coordinate lands outside its list.
            let mut run = Run::spawn_journey(
                &setup,
                120,
                14,
                &bin,
                &[],
                &[("FIBER_TEST_FAKE_KEY", "sk-test")],
            );
            run.ready();
            dismiss_first_run_offer(&mut run);
            let from = run.output().len();
            run.write(b"wheelmarker\r");
            run.wait_screen("the answered turn", |grid| {
                grid.rows.iter().any(|row| row.contains("Hello."))
            });
            run.turn_finished(from);
            // The tall turn scrolled the prompt off the top: the marker
            // is the transcript's bottom-anchored answer row. The turn is
            // over and no input falls between, so the frame the wait below
            // matches is settled; the predicate captures the row itself,
            // and no bare screen read follows any wait.
            let mut mark: Option<(u16, String)> = None;
            run.wait_screen("the settled answer row", |grid| {
                find_grid_row(&grid.rows, "Hello.")
                    .map(|row| {
                        mark = Some((row, grid.rows[usize::from(row)].clone()));
                    })
                    .is_some()
            });
            let (mark_row, mark_text) = mark.expect("the marker row");
            // One wheel step down inside every view, at the same fixed
            // coordinate in the conversation area, clear of the rail and
            // the panel.
            let at = (21, 9);
            // The model picker: each model takes two display rows, so one
            // three-row step moves the list top from the section row to
            // the second model.
            run.write(b"/model\r");
            let pre = run.wait_screen("the open picker", |grid| {
                shows(grid, "Type to search") && shows(grid, "m-00") && !shows(grid, "refreshing")
            });
            let first = find_grid_row(&pre.rows, "m-00 ●").expect("the first model row");
            assert!(
                pre.rows[usize::from(first) - 1].contains("fake"),
                "the section row heads the list: {:?}",
                pre.rows
            );
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled picker", |grid| {
                grid.rows[usize::from(first) - 1].contains("m-01")
            });
            assert!(
                post.rows.iter().all(|row| !row.contains("m-00 ●")),
                "the first model scrolled out: {:?}",
                post.rows
            );
            run.write(b"\x1b[27u");
            let grid = run.wait_screen("the closed picker", |grid| {
                grid.rows.iter().all(|row| !row.contains("Type to search"))
            });
            assert_eq!(
                grid.rows[usize::from(mark_row)],
                mark_text,
                "the picker wheel left the conversation's scroll"
            );
            // The settings table: one row per key under its header.
            run.write(b"/settings\r");
            let pre = run.wait_screen("the open settings", |grid| {
                shows(grid, "Settings") && shows(grid, "budget.usd")
            });
            let first = find_grid_row(&pre.rows, "budget.usd").expect("the first key row");
            assert!(
                pre.rows[usize::from(first) - 1].contains("key"),
                "the header heads the table: {:?}",
                pre.rows
            );
            let third = pre.rows[usize::from(first) + 2]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled settings", |grid| {
                grid.rows[usize::from(first) - 1].contains(&third)
            });
            assert!(
                !post.rows[usize::from(first) - 1].contains("budget.usd"),
                "the first key scrolled out: {:?}",
                post.rows
            );
            run.write(b"\x1b[27u");
            let grid = run.wait_screen("the closed settings", |grid| {
                grid.rows.iter().all(|row| !row.contains("Settings"))
            });
            assert_eq!(
                grid.rows[usize::from(mark_row)],
                mark_text,
                "the settings wheel left the conversation's scroll"
            );
            // The keys screen: one row per binding.
            run.write(b"/keys\r");
            let pre = run.wait_screen("the open keys", |grid| {
                shows(grid, "Keys") && shows(grid, "Send a prompt")
            });
            let first = find_grid_row(&pre.rows, "Send a prompt").expect("the first binding row");
            let third = pre.rows[usize::from(first) + 3]
                .split_whitespace()
                .find(|word| word.contains('_'))
                .unwrap_or_default()
                .to_owned();
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled keys", |grid| {
                grid.rows[usize::from(first)].contains(&third)
            });
            assert!(
                !post.rows[usize::from(first)].contains("Send a prompt"),
                "the first binding scrolled out: {:?}",
                post.rows
            );
            run.write(b"\x1b[27u");
            let grid = run.wait_screen("the closed keys", |grid| {
                grid.rows.iter().all(|row| !row.contains("/keys"))
            });
            assert_eq!(
                grid.rows[usize::from(mark_row)],
                mark_text,
                "the keys wheel left the conversation's scroll"
            );
            // The context breakdown: the sizes heading, the bar, the
            // categories, then the largest tool results. One step moves
            // three rows, from the heading to the first category.
            run.write(b"/context\r");
            let pre = run.wait_screen("the open context", |grid| {
                shows(grid, "Context")
                    && shows(grid, "system prompt")
                    && shows(grid, "Largest tool results")
            });
            let first = find_grid_row(&pre.rows, "tokens ·").expect("the sizes row");
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled context", |grid| {
                grid.rows[usize::from(first)].contains("system prompt")
            });
            assert!(
                !post.rows[usize::from(first)].contains("tokens ·"),
                "the sizes row scrolled out: {:?}",
                post.rows
            );
            run.write(b"\x1b[27u");
            let grid = run.wait_screen("the closed context", |grid| {
                grid.rows.iter().all(|row| !row.contains("/context"))
            });
            assert_eq!(
                grid.rows[usize::from(mark_row)],
                mark_text,
                "the context wheel left the conversation's scroll"
            );
            quit_journey(run);
        }),
    );
}
