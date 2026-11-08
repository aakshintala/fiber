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

#[path = "main_journeys/delegate_steering.rs"]
mod delegate_steering;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fakes::{ProviderServer, Response, fingerprint};
use serde_json::{Value, json};
use support::Deadline;
use support::Setup;
use support::journey::shows;
use support::pty::{Grid, Run, focus_notice, reversed_cells};
use support::{TRUECOLOUR, write_json};

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

/// The nightly the journey's stub feed serves when the binary under test
/// reports no nightly of its own: a `0.0.0` dev build or a stable release
/// has no nightly date to count forward from, so the feed serves this
/// fixed pair, which is newer than either.
const JOURNEY_NIGHTLY_FALLBACK: &str = "0.0.1-nightly.20240101";

/// The newer nightly the stub feed also serves in the fallback case: a
/// nightly-floored install moves onto it.
const JOURNEY_NIGHTLY_NEXT_FALLBACK: &str = "0.0.1-nightly.20240102";

/// The two nightlies the stub feed serves for one run, derived from the
/// binary under test: on a nightly binary the floor is the running
/// nightly's date plus one day and the next plus two, so the feed stays
/// newer than the binary and the update reaches its approval prompt; on
/// any other binary the fallback pair stands in.
struct JourneyNightlies {
    floor: String,
    next: String,
}

/// The trimmed `--version` stdout line of the binary under test
/// (`fiber 0.0.1-nightly.20261031 (24a32fb)`). An execution failure
/// fails naming what it saw, so the failure tells whether the binary
/// or its output broke the read.
fn journey_version_line(bin: &Path) -> String {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "FIBER_JOURNEY_BIN --version failed for {}: {error}",
                bin.display()
            )
        });
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned()
}

/// The version token of the binary under test: the word after `fiber` in
/// its `--version` line. A line without one fails naming what it saw.
fn journey_version(bin: &Path) -> String {
    let line = journey_version_line(bin);
    line.split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("FIBER_JOURNEY_BIN --version printed no version: {line}"))
        .to_owned()
}

/// The nightly date `days` after the running nightly's `YYYYMMDD` date:
/// the date orders as a zero-padded number (`resolve::version_key`
/// compares two nightlies on one base by this string), so counting the
/// digits forward stays strictly newer. No path checks it is a real
/// calendar date (`resolve::nightly_parts` takes any 8 ASCII digits), so
/// month and year ends need no rollover.
fn shift_nightly_date(date: &str, days: u32) -> String {
    format!("{:08}", date.parse::<u32>().unwrap() + days)
}

/// The two nightlies the stub feed serves for this run: the running
/// binary's base with its date moved one day forward for the floor and
/// two for the next, keeping floor below next and both above the binary.
/// A binary whose version is not a nightly keeps the fallback pair.
fn journey_nightlies() -> JourneyNightlies {
    let bin = journey_bin();
    let version = journey_version(&bin);
    let (base, date) = match extensions::nightly_parts(&version) {
        Ok(parts) => parts,
        Err(_) => {
            return JourneyNightlies {
                floor: JOURNEY_NIGHTLY_FALLBACK.to_owned(),
                next: JOURNEY_NIGHTLY_NEXT_FALLBACK.to_owned(),
            };
        }
    };
    let floor = shift_nightly_date(&date, 1);
    let next = shift_nightly_date(&date, 2);
    JourneyNightlies {
        floor: format!("{}.{}.{}-nightly.{floor}", base[0], base[1], base[2]),
        next: format!("{}.{}.{}-nightly.{next}", base[0], base[1], base[2]),
    }
}

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
    fn new(setup: &Setup, nightlies: &JourneyNightlies) -> Self {
        let base = setup.root.path().join("gh");
        let repo = base.join("aakshintala/fiber");
        std::fs::create_dir_all(repo.join("providers/anthropic")).unwrap();
        std::fs::write(repo.join("fiber.md"), "fiber").unwrap();
        std::fs::write(
            repo.join("providers/anthropic/extension.json"),
            json!({
                "name": "github.com/aakshintala/fiber/providers/anthropic",
                "version": nightlies.floor.as_str(),
                "fiber": "0.0.0",
                "api": 1,
            })
            .to_string(),
        )
        .unwrap();
        fakes::git::git(&repo, &["init", "--quiet"]);
        fakes::git::git(&repo, &["add", "."]);
        fakes::git::git(&repo, &["commit", "--quiet", "-m", "fiber"]);
        for nightly in [&nightlies.floor, &nightlies.next] {
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
fn serve_nightly_feed(nightlies: &JourneyNightlies) -> ProviderServer {
    use fakes::ustar::{archive, gzip, header, sha256};
    let binary_archive = format!(
        "fiber-{}.tar.gz",
        extensions::release_triple(env!("FIBER_TARGET")).unwrap()
    );
    let ok = |body: Vec<u8>| fakes::Response::status(200, body);
    let mut routes: Vec<(String, fakes::Response)> = Vec::new();
    // Every nightly run fetches its own archives, as a real server
    // answers every request.
    for nightly in [&nightlies.floor, &nightlies.next] {
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
    let list: Vec<Value> = [&nightlies.floor, &nightlies.next]
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
fn install_nightly_extension(setup: &Setup, nightlies: &JourneyNightlies) {
    let source = setup.root.path().join("nightlysrc");
    support::write_json(
        &source.join("extension.json"),
        &json!({
            "name": "github.com/aakshintala/fiber/providers/anthropic",
            "version": nightlies.floor.as_str(),
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
            let nightlies = journey_nightlies();
            let server = serve_nightly_feed(&nightlies);
            let github = JourneyGithub::new(&setup, &nightlies);
            install_nightly_extension(&setup, &nightlies);
            // The pre-update binary is the journey suite's release binary,
            // built with git history, so its `--version` line is
            // `fiber <version> (<7 lowercase hex chars>)`: a bare version
            // means the binary was built without git history.
            let version_line = journey_version_line(&journey_bin());
            let version = journey_version(&journey_bin());
            let prefix = format!("fiber {version} (");
            let commit = version_line
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix(')'))
                .unwrap_or_else(|| {
                    panic!(
                        "pre-update --version line is not `fiber <version> (<7 lowercase hex chars>)`: saw {version_line:?}; a bare version means the binary was built without git history"
                    )
                });
            assert!(
                commit.len() == 7
                    && commit
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
                "pre-update --version commit is not 7 lowercase hex chars: saw {version_line:?}"
            );
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
fn install_first_party_nightly_extension(setup: &Setup, nightlies: &JourneyNightlies) {
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
        &["rev-list", "-n", "1", &format!("v{}", nightlies.floor)],
    );
    let path = setup.home().join("extensions/anthropic/.fiber.json");
    let mut record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record["version"] = Value::String(nightlies.floor.clone());
    record["floor"] = Value::String(nightlies.floor.clone());
    record["source"] = json!({ "commit": commit });
    std::fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
}

/// `fiber update --channel nightly` with a nightly-floored first-party
/// extension installed: the person-visible approval prompt and update
/// announcement on the release binary's terminal, and the extension
/// record moved to the newest nightly. The stub feed derives its two
/// nightlies from the binary under test, so the update path runs on
/// nightly binaries as well as dev ones; the record assertions name the
/// derived pair.
#[test]
fn update_nightly_floored_extension_to_newest_nightly() {
    run_journey(
        "update_nightly_floored_extension_to_newest_nightly",
        // As in the setup journey: the body borrows the fake server, so
        // the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let nightlies = journey_nightlies();
            let server = serve_nightly_feed(&nightlies);
            let github = JourneyGithub::new(&setup, &nightlies);
            install_first_party_nightly_extension(&setup, &nightlies);
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
            // docs/extensions.md:1797, 1805-1807.
            assert_eq!(record["version"], nightlies.next.as_str(), "{record}");
            // docs/extensions.md:1798-1799.
            assert_eq!(record["floor"], nightlies.floor.as_str(), "{record}");
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
/// quota fetch reports 90% used, signalling when the fetch's request
/// arrives so the journey waits for the fetch the first turn started.
fn start_quota_server() -> (ProviderServer, std::sync::mpsc::Receiver<()>) {
    let (quota_hit, quota_rx) = std::sync::mpsc::channel();
    let server = ProviderServer::start_responding(move |request| match request.path.as_str() {
        "/lua-quota" => {
            let _sent = quota_hit.send(());
            fakes::Response::status(200, "90")
        }
        "/lua-models" | "/lua-token" => fakes::Response::status(200, "{}"),
        "/v1/responses" => support::hello(),
        _ => fakes::Response::status(500, "unrouted"),
    })
    .unwrap();
    (server, quota_rx)
}

/// Person rows on `contents`: `you HH:MM` under the prompt bubble
/// (`docs/tui.md`, "Turns"). The rail and panel grips draw `⋮` on three
/// mid-height rows (`docs/tui.md`, "Layout"), and a prompt row there
/// still counts.
fn you_rows(contents: &str) -> usize {
    contents
        .lines()
        .filter(|line| line.trim_start_matches([' ', '⋮']).starts_with("you "))
        .count()
}

/// Wheel steps a scroll toward an off-screen row may take. One step
/// moves three rows (`docs/tui.md`, "Turns"), and the bound covers the
/// quota journey's 48-row screen.
const SCROLL_BOUND: usize = 16;

/// A cell in the conversation column of the quota journey's 160 by 48
/// screen. The rail is 24 columns and the panel 34, so column 76 is the
/// conversation's middle; row 16 sits above the input box. SGR
/// coordinates are 1-based.
const CONVERSATION_WHEEL: (u16, u16) = (76, 16);

/// Whether a stable conversation row moved. `down` is the on-screen
/// direction, which is where a wheel-up carries a row.
fn anchor_moved(prev: &[String], next: &[String], down: bool) -> bool {
    for needle in ["Hello.", "used 90%", "you "] {
        let before = prev.iter().position(|row| row.contains(needle));
        let after = next.iter().position(|row| row.contains(needle));
        if let (Some(before), Some(after)) = (before, after)
            && after != before
            && (after > before) == down
        {
            return true;
        }
    }
    false
}

/// Scrolls the conversation up until `pred` holds, at most `bound` wheel
/// steps, then waits on `pred`. The wait's deadline names `what` and
/// shows the last grid (`docs/testing/screens.md`, "Screens"). The count
/// is how many steps moved the rows, so the caller can wheel back to the
/// followed bottom.
fn scroll_up_then_wait(
    run: &mut Run,
    mut grid: Grid,
    bound: usize,
    what: &str,
    pred: impl Fn(&Grid) -> bool,
) -> (Grid, usize) {
    let mut steps = 0;
    for _ in 0..bound {
        if pred(&grid) {
            break;
        }
        let prev = grid.rows.clone();
        run.write(&wheel_up(CONVERSATION_WHEEL));
        grid = run.wait_screen("one wheel step toward the waited rows", |next| {
            pred(next) || anchor_moved(&prev, &next.rows, true)
        });
        if anchor_moved(&prev, &grid.rows, true) {
            steps += 1;
        }
        if pred(&grid) {
            break;
        }
    }
    (run.wait_screen(what, pred), steps)
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
            let (server, quota_rx) = start_quota_server();
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
            // The fake signals when the first turn's fetch reaches it, so
            // the waits below run after the fetch started: the signal fires
            // when the request arrives, before the response and before
            // `finish_quota` stores it, so one fetch-less re-query straight
            // after it could still miss (the fetch-less snapshot never
            // re-asks when a flight lands after it).
            setup.deadline.recv_or_fail(
                &quota_rx,
                "the fake receiving the first turn's quota request",
            );
            // The turn must end before typing: sent earlier, `/quota`
            // would steer the first turn instead of opening the view.
            run.turn_finished(from);
            // The view's `fetch: true` query either joins that flight or
            // starts its own, and answers only once a fetch ends
            // (`switch/quota.rs`, `answer_fetch` over `force_start_quota`
            // and `wait_quota`), so the drawn value proves `finish_quota`
            // stored it; the answer folds into the panel card too
            // (`tui/src/app/panel/quota.rs`, `panel_quota`). The view
            // draws the cached rows under `/quota · fetching` until that
            // answer lands (`tui/src/quota_view.rs`, `frame`), so the
            // wait also requires the fetching mark gone. `Quota` plus
            // `10%` alone could match the panel card, which the default
            // cards include (`crates/config/src/keys.rs`,
            // `tui.panel.cards`), before the view opens: the wait names
            // the view's own top row (`/quota` and `Esc returns`,
            // `swapped.rs`, `draw_top_row`), so only the open view's
            // fetched value passes. No wall-clock read or sleep of its own.
            run.write(b"/quota\r");
            run.wait_screen("the fetched quota value", |grid| {
                grid.contents.contains("/quota")
                    && grid.contents.contains("Esc returns")
                    && grid.contents.contains("10%")
                    && !grid.contents.contains("fetching")
            });
            run.write(b"\x1b[27u");
            run.wait_screen("the conversation after the quota view", |grid| {
                grid.contents.contains("Hello.")
            });
            // The cache is fresh from the view's fetch, so the one
            // focus-in below sends a fetch-less query that starts no new
            // `/lua-quota` request (`docs/testing.md`, "Waits and
            // timeouts"); the grid wait below is the assertion, failing
            // naming the wait when the share never draws. One 4-byte
            // focus-in cannot fill the PTY input buffer by construction.
            run.write(b"\x1b[I");
            run.wait_screen("the fetched quota share", |grid| {
                grid.contents.contains("10%")
            });
            // The second prompt waits for the first turn's completed card:
            // sent earlier, it would steer the first turn instead of
            // starting the second.
            let from = run.output().len();
            run.write(b"second\r");
            let shown = run.wait_screen("the second turn's quota notice", |grid| {
                grid.contents.contains("Fiber")
                    && grid.contents.matches("used 90%").count() == 1
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
            // One `you` row per turn, both on screen (`docs/tui.md`,
            // "Turns"). Scroll up to the bound when one has left the
            // top, then wait: the deadline shows the last grid
            // (`docs/testing/screens.md`, "Screens").
            let (with_rows, steps) = scroll_up_then_wait(
                &mut run,
                shown,
                SCROLL_BOUND,
                "the two you-rows are visible",
                |grid| you_rows(&grid.contents) == 2,
            );
            assert_eq!(
                you_rows(&with_rows.contents),
                2,
                "one you row per turn: {}",
                with_rows.contents
            );
            // A wheel up pauses following. The steps that moved the rows
            // wheel back down, which follows again at the bottom
            // (`docs/tui.md`, "Turns"), so the second answer draws.
            let mut grid = with_rows;
            for _ in 0..steps {
                let prev = grid.rows.clone();
                run.write(&wheel_down(CONVERSATION_WHEEL));
                grid = run.wait_screen("the conversation following again", |next| {
                    anchor_moved(&prev, &next.rows, false)
                });
            }
            run.turn_finished(from);
            // The second answer drew beside the first: two turns, two
            // answers on the grid.
            let done = run.wait_screen("the second answer", |grid| {
                grid.contents.matches("Hello.").count() == 2
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

/// One wheel step up at `coord`: SGR button 64.
fn wheel_up(coord: (u16, u16)) -> Vec<u8> {
    format!("\x1b[<64;{};{}M", coord.0, coord.1).into_bytes()
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
    const SWAPPED_HEADER_ROWS: u16 = 1;
    const OVERLAY_TOP_EDGE_AND_PAD_ROWS: u16 = 2;
    const PICKER_CONTROL_ROWS: u16 = 2;
    const PROVIDER_HEADING_ROWS: u16 = 1;
    const PICKER_MODEL_ROW: u16 = SWAPPED_HEADER_ROWS
        + OVERLAY_TOP_EDGE_AND_PAD_ROWS
        + PICKER_CONTROL_ROWS
        + PROVIDER_HEADING_ROWS;
    // SGR mouse coordinates are one-based, unlike the grid row above.
    const PICKER_WHEEL_ROW: u16 = PICKER_MODEL_ROW + 1;

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
            // At fourteen terminal rows, the picker cannot hold a full
            // wheel step. It must keep the session model under its provider
            // heading on open, then the focused model under its heading
            // after the wheel; the footer gives way before either row does.
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
            // `docs/tui.md`, "The model picker" keeps the session model and heading
            // visible on open; the focused model and heading stay visible
            // after scrolling. Place the wheel in the compact list using
            // fixture-derived row arithmetic; SGR coordinates add one.
            let at = (21, 9);
            let picker_at = (at.0, PICKER_WHEEL_ROW);
            let picker_frame = run.output().len();
            run.write(b"/model\r");
            run.wait_bytes(picker_frame, FRAME_END, "the open picker frame");
            let pre = run.wait_screen("the open picker", |grid| {
                shows(grid, "Type to search") && shows(grid, "m-00") && !shows(grid, "refreshing")
            });
            let first = find_grid_row(&pre.rows, "m-00 ●").expect("the first model row");
            assert!(
                pre.rows[usize::from(first) - 1].contains("fake"),
                "the section row heads the list: {:?}",
                pre.rows
            );
            run.write(&wheel_down(picker_at));
            let post = run.wait_screen("the wheeled picker", |grid| {
                grid.rows
                    .iter()
                    .any(|line| line.contains("› m-") && !line.contains("m-00"))
            });
            let focused = post
                .rows
                .iter()
                .position(|line| line.contains("› m-"))
                .expect("the focused model row");
            assert!(
                focused > 0 && post.rows[focused - 1].contains("fake"),
                "the section row heads the focused model: {:?}",
                post.rows
            );
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
            // The settings view: the tabbed rows. The wheel moves the
            // focused row, as for the picker, and leaves the conversation
            // where it was.
            run.write(b"/settings\r");
            let pre = run.wait_screen("the open settings", |grid| {
                shows(grid, "Settings") && shows(grid, "Appearance") && shows(grid, "Hover")
            });
            let focused = |grid: &Grid| {
                grid.rows
                    .iter()
                    .find_map(|row| row.trim_start().strip_prefix("› ").map(str::to_owned))
            };
            let first = focused(&pre).expect("a focused settings row");
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled settings", |grid| {
                focused(grid).is_some_and(|row| row != first)
            });
            assert!(
                shows(&post, "Appearance"),
                "the tab bar stays: {:?}",
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
            let mut probe: Option<(u16, usize, String)> = None;
            let pre = run.wait_screen("the open keys", |grid| {
                let Some(first) = find_grid_row(&grid.rows, "Send a prompt") else {
                    return false;
                };
                let Some((at_row, action)) =
                    (usize::from(first)..grid.rows.len()).find_map(|row| {
                        grid.rows[row]
                            .split_whitespace()
                            .find(|word| word.contains('_'))
                            .map(|word| (row, word.to_owned()))
                    })
                else {
                    return false;
                };
                let stays = at_row >= usize::from(first) + 3;
                if stays && grid.rows[at_row - 3].contains(&action) {
                    return false;
                }
                if !shows(grid, "Keys") {
                    return false;
                }
                probe = Some((first, at_row, action));
                true
            });
            // settled: "the open keys" checked the binding row, the action
            // word, and, when the step keeps it, that the landing row does
            // not already hold that word.
            let (first, at_row, action) = probe.expect("an action name on a binding row");
            // Rebind::wheel adds 3 to the display-line top, so an action
            // fewer than three rows below the first leaves the list.
            let post = if at_row < usize::from(first) + 3 {
                run.write(&wheel_down(at));
                run.wait_screen("the wheeled keys", |grid| {
                    grid.rows.iter().all(|row| !row.contains(&action))
                })
            } else {
                let lands = at_row - 3;
                // settled: "the open keys" checked this row does not already
                // hold the action word.
                assert!(
                    !pre.rows[lands].contains(&action),
                    "the wait would pass before the wheel moved: {:?}",
                    pre.rows
                );
                run.write(&wheel_down(at));
                run.wait_screen("the wheeled keys", |grid| {
                    grid.rows[lands].contains(&action)
                })
            };
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
            // categories, then the largest tool results. At fourteen rows,
            // the latter heading is below the initial viewport; one wheel
            // step moves it into view while moving the sizes row out.
            let context_frame = run.output().len();
            run.write(b"/context\r");
            run.wait_bytes(context_frame, FRAME_END, "the open context frame");
            let pre = run.wait_screen("the open context", |grid| {
                shows(grid, "Context")
                    && shows(grid, "system prompt")
                    && shows(grid, "tool results")
            });
            let first = find_grid_row(&pre.rows, "tokens ·").expect("the sizes row");
            run.write(&wheel_down(at));
            let post = run.wait_screen("the wheeled context", |grid| {
                grid.rows[usize::from(first)].contains("system prompt")
                    && shows(grid, "Largest tool results")
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

/// A solid-colour 120 by 80 PNG fixture.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0x50, 0x08, 0x02, 0x00, 0x00, 0x00, 0x5d, 0xf9, 0x26,
    0xde, 0x00, 0x00, 0x00, 0x82, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0xed, 0xd0, 0x31, 0x0d, 0x00,
    0x00, 0x08, 0x03, 0xb0, 0xbd, 0xb8, 0xc5, 0x27, 0xa6, 0x70, 0x41, 0x38, 0x9a, 0x54, 0x41, 0x53,
    0x3d, 0x1c, 0x88, 0x02, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0xb4, 0x05, 0xd1, 0xa2, 0x11, 0x2d, 0x5a,
    0xb4, 0x05, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68, 0x44, 0x8b,
    0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1, 0xa2, 0x45,
    0x23, 0x5a, 0x34, 0xa2, 0x45, 0x8b, 0x46, 0xb4, 0x68, 0x44, 0x8b, 0x16, 0x8d, 0x68, 0xd1, 0x88,
    0x16, 0x2d, 0x1a, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68, 0x44,
    0x8b, 0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1, 0xa2,
    0x45, 0x23, 0x5a, 0x34, 0xa2, 0x45, 0x8b, 0x46, 0xb4, 0x68, 0x44, 0x8b, 0x16, 0x8d, 0x68, 0xd1,
    0x88, 0x16, 0x2d, 0x1a, 0xd1, 0xa2, 0x11, 0x2d, 0x5a, 0x34, 0xa2, 0x45, 0x23, 0x5a, 0xb4, 0x68,
    0x44, 0x8b, 0x46, 0xb4, 0x68, 0xd1, 0x88, 0x16, 0x8d, 0x68, 0xd1, 0xa2, 0x11, 0x2d, 0x1a, 0xd1,
    0xa2, 0x45, 0x23, 0x5a, 0x34, 0xa2, 0xff, 0x58, 0xd9, 0xb8, 0x78, 0xc2, 0xab, 0x96, 0xaa, 0xe8,
    0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const FRAME_END: &[u8] = b"\x1b[?2026l";

/// The scripted turns: `reads` distinct PNG reads, grouped several to a
/// step so one turn pays one model round trip per group rather than one
/// per read, then closing text, then the same session's text-only turn.
/// The turn still runs one tool call per image, so the session logs one
/// completion per read and the ledger folds them into one group row.
fn script_reads(setup: &Setup, inline: bool, reads: usize, with_tail: bool) {
    for n in 0..reads {
        fs::write(setup.workspace().join(format!("pic-{n:03}.png")), PNG).unwrap();
    }
    // The number of reads sharing one scripted step: 255 reads become
    // 51 model steps, while the turn still runs one tool call per image.
    const READS_PER_STEP: usize = 5;
    let indexes: Vec<usize> = (0..reads).collect();
    let mut steps: Vec<Value> = indexes
        .chunks(READS_PER_STEP)
        .map(|chunk| {
            json!({"tool_calls": chunk.iter().map(|n| {
                json!({"name": "read", "arguments": {"path": format!("pic-{n:03}.png")}})
            }).collect::<Vec<_>>()})
        })
        .collect();
    if with_tail {
        let tail = (1..=30)
            .map(|n| format!("- T{n:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        steps.push(json!({"text": ["Seen.", tail]}));
    } else {
        steps.push(json!({"text": ["Seen."]}));
    }
    steps.push(json!({"text": ["Second."]}));
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"inline_images": inline}}),
    );
}

/// A fake `xdg-open`/`open` first on the child's PATH, recording its
/// arguments in `marker`: the viewer the caption click queues.
fn viewer_shim(setup: &Setup, marker: &Path) -> String {
    let bin = setup.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let body = format!("printf '%s\\n' \"$@\" >> '{}'", marker.to_str().unwrap());
    fakes::script(&bin, "xdg-open", &body);
    fakes::script(&bin, "open", &body);
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap()
    .into_string()
    .unwrap()
}

/// The started turn finished: approval allowed, the turn done. The
/// done signal is the turn's completed row, which the follow keeps in
/// view no matter how long the turn's tail grows.
fn started(run: &mut Run) {
    let mut from = run.ready();
    run.write(b"go\r");
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains("▣ completed")
        });
        if grid.contents.contains("▣ completed") {
            break;
        }
        let approval = run.wait_bytes(from, b"Allow once", "the approval prompt");
        from = run.wait_bytes(approval, FRAME_END, "the approval frame");
        run.write(b"\r");
    }
    run.turn_finished(from);
}

/// The session directories under the workspace, oldest first.
fn sessions(setup: &Setup) -> Vec<PathBuf> {
    let dir = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
    let mut ids: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("events.jsonl").is_file())
        .collect();
    ids.sort();
    ids
}

/// The logged path and size of every image part in `events`' tool
/// completions, in log order.
fn logged_images(text: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in support::complete_lines(text) {
        if line.get("kind").and_then(Value::as_str) != Some("tool_call_completed") {
            continue;
        }
        let content = line
            .get("payload")
            .and_then(|payload| payload.get("content"))
            .and_then(Value::as_array);
        for part in content.into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) == Some("image")
                && let (Some(path), Some(width), Some(height)) = (
                    part.get("path").and_then(Value::as_str),
                    part.get("width").and_then(Value::as_u64),
                    part.get("height").and_then(Value::as_u64),
                )
            {
                out.push((path.to_owned(), width, height));
            }
        }
    }
    out
}

/// The logged path and size of the image the read of `wanted` completed
/// in `text`, matching each completion to its request by `action_id`:
/// the completion carries the stored copy under a fresh stem, while the
/// request carries the `pic-NNN.png` source path. Calls sharing one
/// scripted step can complete out of order, so the past-range image is
/// found by its requested name rather than by completion position.
fn image_named(text: &str, wanted: &str) -> Option<(String, u64, u64)> {
    let mut sources: HashMap<String, String> = HashMap::new();
    for line in support::complete_lines(text) {
        if line.get("kind").and_then(Value::as_str) != Some("tool_call_requested") {
            continue;
        }
        let payload = line.get("payload");
        if payload
            .and_then(|payload| payload.get("name"))
            .and_then(Value::as_str)
            != Some("read")
        {
            continue;
        }
        let Some(action) = line.get("action_id").and_then(Value::as_str) else {
            continue;
        };
        let path = payload
            .and_then(|payload| payload.get("arguments"))
            .and_then(|arguments| arguments.get("path"))
            .and_then(Value::as_str)
            .unwrap_or("");
        sources.insert(
            action.to_owned(),
            path.rsplit('/').next().unwrap_or(path).to_owned(),
        );
    }
    for line in support::complete_lines(text) {
        if line.get("kind").and_then(Value::as_str) != Some("tool_call_completed") {
            continue;
        }
        let Some(action) = line.get("action_id").and_then(Value::as_str) else {
            continue;
        };
        if sources.get(action).map(String::as_str) != Some(wanted) {
            continue;
        }
        let content = line
            .get("payload")
            .and_then(|payload| payload.get("content"))
            .and_then(Value::as_array);
        for part in content.into_iter().flatten() {
            if part.get("type").and_then(Value::as_str) == Some("image")
                && let (Some(path), Some(width), Some(height)) = (
                    part.get("path").and_then(Value::as_str),
                    part.get("width").and_then(Value::as_u64),
                    part.get("height").and_then(Value::as_u64),
                )
            {
                return Some((path.to_owned(), width, height));
            }
        }
    }
    None
}

/// The logged path and size of the read of `wanted` in `events`' tool
/// completions, read under the setup's deadline: the hub logs each
/// read's image as its tool completes, so the wait holds until all
/// `reads` images landed and the wanted source's image is among them.
fn image_for_read(setup: &Setup, events: &Path, reads: usize, wanted: &str) -> (String, u64, u64) {
    let events = events.to_owned();
    let wanted_line = wanted.to_owned();
    // The reads' log lines land after their tool calls complete, and the
    // log exists (the session already ran): the watch registers before the
    // wait, and the reread after registration never loses a line, so no
    // polling loop can miss the lines either way.
    let watch = support::watch_log(&events, setup.deadline, "all reads to log their image");
    support::wait_for_watched_line(
        &watch,
        setup.deadline,
        "all reads to log their image",
        |text| logged_images(text).len() >= reads && image_named(text, &wanted_line).is_some(),
    );
    let text = fs::read_to_string(events).expect("the session log");
    image_named(&text, wanted).expect("the wanted logged image")
}

/// The caption the ledger draws for the image at `path`: its file name
/// and logged size.
fn caption_of(path: &str, width: u64, height: u64) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    format!("▣ {name} · {width}×{height}")
}

/// 255 distinct PNG reads past the placeholder range, keeping a unique
/// viewer target: the journey binary's run of the gate's 255-read
/// journey, against the release binary after merge.
#[test]
fn images_past_the_placeholder_range_keep_a_unique_viewer_target() {
    run_journey(
        "images_past_the_placeholder_range_keep_a_unique_viewer_target",
        // As in the earlier journeys: the body borrows no server, but the
        // standard `catch_unwind` wrapper still records the failure.
        std::panic::AssertUnwindSafe(|| {
            // 255 distinct PNG reads: the first 254 fill the kitty placeholder
            // colours, so the last image is the one past the range. It keeps its
            // own viewer target without entering the inline display path: a
            // click opens it, and the click itself transmits nothing
            // (`docs/tui.md`, "Images").
            const READS: usize = 255;
            let mut setup = Setup::new();
            // The 255-read run and its later screen and frame waits share one
            // bounded wall-clock budget with room for load.
            setup.deadline = Deadline::after(Duration::from_secs(120)); // ruling: #3504
            script_reads(&setup, true, READS, false);
            // A standing allow rule for `read`: 255 approval panels would bury
            // the turn, and the reads need no review.
            fs::write(
                setup.home().join("rules"),
                format!(
                    "{}\n",
                    json!({"decision": "allow", "tool": "read", "prefix": ""})
                ),
            )
            .unwrap();
            let marker = setup.workspace().join(".viewed");
            let shim = viewer_shim(&setup, &marker);
            let marker_value = marker.to_str().unwrap().to_owned();
            let bin = journey_bin();
            let mut env: Vec<(&str, &str)> = vec![
                ("PATH", shim.as_str()),
                ("FIBER_TEST_VIEW_MARKER", marker_value.as_str()),
                ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ];
            env.extend(TRUECOLOUR.iter().copied());
            let mut run = Run::spawn_journey(&setup, 100, 30, &bin, &[], &env);
            run.ready();
            dismiss_first_run_offer(&mut run);
            started(&mut run);
            let sessions = sessions(&setup);
            // The past-range image is `pic-254.png`: the first 254 reads fill
            // the kitty placeholder colours. It is selected by its requested
            // source path, never by completion position.
            let (path, width, height) = image_for_read(
                &setup,
                &sessions[0].join("events.jsonl"),
                READS,
                "pic-254.png",
            );
            let caption = caption_of(&path, width, height);

            // The turn's reads share one group row above the closing text; it
            // draws without scrolling. Opening it reveals one row per call
            // with its image's caption, the last carrying the past-range
            // image, so its caption is clickable without opening its call.
            // The turn already finished in `started`, so a grid showing the
            // group row together with the completed paint has nothing left to
            // render: the group row's position in that grid is final
            // (`docs/testing/screens.md`, "Screens").
            run.wait_screen("the settled reads group", |grid| {
                grid.contents.contains("completed")
                    && grid.rows.iter().any(|row| row.contains("Read 255 files"))
            });
            let from = run.output().len();
            run.click_where("the reads group row", |grid| grid.text_at("Read 255 files"));
            run.wait_bytes(from, FRAME_END, "the opened group frame");
            // The opened group settles the same way: the past-range caption
            // together with the completed paint proves every call row drew and
            // the caption row's position is final. The helper finds the
            // caption in the frame it clicks, never on a grid read earlier.
            let grid = run.wait_screen("the settled past-range caption", |grid| {
                grid.contents.contains("completed")
                    && grid.rows.iter().any(|row| row.contains(&caption))
            });
            // The caption click opens the viewer, never a display fetch: no
            // transmit may follow it for the past-range image.
            // The viewer open is the click's signal, so the test waits for
            // the marker next.
            run.click_where("the past-range caption", |grid| grid.text_at(&caption));

            // The click opens the viewer on the private copy, as for an
            // in-range image: the past-range target resolves.
            let marker_file = marker.clone();
            let session = sessions[0]
                .file_name()
                .expect("the session directory name")
                .to_string_lossy();
            let name = Path::new(&path)
                .file_name()
                .expect("the image file name")
                .to_string_lossy();
            let expected = setup
                .home()
                .join("cache/images")
                .join(format!("{session}-{name}"))
                .to_string_lossy()
                .into_owned();
            let expected_line = expected.clone();
            fakes::wait_until("the viewer to open", setup.deadline, move || {
                fs::read_to_string(&marker_file)
                    .is_ok_and(|opened| opened.lines().any(|line| line == expected_line))
            });
            assert_eq!(
                fs::read_to_string(&marker).unwrap().trim(),
                expected,
                "the viewer opened the private image copy"
            );
            // The past-range image draws no placeholder grid: the row above
            // its caption is the previous call's row, never a grid row.
            let at = grid
                .rows
                .iter()
                .position(|row| row.contains(&caption))
                .expect("the past-range caption row");
            assert!(at > 0, "the caption is not the first row");
            assert!(
                !grid.rows[at - 1].contains('\u{10EEEE}'),
                "no placeholder grid above the past-range caption"
            );

            run.write(b"\x03\x03\r");
            run.wait_screen("the primary screen with the resume line", |grid| {
                !grid.alternate_screen
                    && !grid.hide_cursor
                    && grid.contents.contains("fiber resume")
            });
            let output = run.wait();
            assert_eq!(output.status.code(), Some(0));
        }),
    );
}

/// One terminal's recorded or documented-default bytes per #4097 row,
/// each loaded from `fixtures/terminal_bytes/` at compile time. A new
/// terminal is a new entry, never a branch on the terminal name; a new
/// row is a new field on every entry.
struct TerminalBytes {
    /// The terminal the bytes were recorded or documented for.
    terminal: &'static str,
    /// Where the bytes come from; see `fixtures/terminal_bytes/PROVENANCE.md`.
    provenance: &'static str,
    /// The bytes that move to the start of the cursor's line.
    line_start: &'static [u8],
    /// The bytes that move to the end of the cursor's line.
    line_end: &'static [u8],
    /// The bytes that delete to the start of the line.
    delete_to_line_start: &'static [u8],
    /// The bytes that delete one word back, when the terminal has a
    /// default; `None` when it has none (Ghostty).
    delete_word: Option<&'static [u8]>,
}

/// Ghostty's #3751/#3754 bytes: Super+Left/Right arrive as Ctrl+A/Ctrl+E
/// and Super+Backspace as Ctrl+U. No `delete_word` default: Option+Backspace
/// is plain `0x7f` since 1.2 (PR #7320, discussion #9560) and `ESC 0x7f` is
/// a user keybind (1.2 removed the builtin, discussion #8677). The bug
/// reports state no Ghostty version.
const GHOSTTY_BYTES: TerminalBytes = TerminalBytes {
    terminal: "Ghostty",
    provenance: "bug reports #3751 and #3754; versions unknown (reports state none)",
    line_start: include_bytes!("fixtures/terminal_bytes/ghostty_line_start.bin"),
    line_end: include_bytes!("fixtures/terminal_bytes/ghostty_line_end.bin"),
    delete_to_line_start: include_bytes!(
        "fixtures/terminal_bytes/ghostty_delete_to_line_start.bin"
    ),
    delete_word: None,
};

/// iTerm2's factory default bytes: with no profile key mapping for
/// plain Ctrl+letter keys they pass through as C0 bytes, and the factory
/// option keys send no `ESC`-prefixed Alt bytes (`+Esc` is opt-in), so the
/// word key is Ctrl+W. Sources in
/// `fixtures/terminal_bytes/PROVENANCE.md`; the docs state no version.
const ITERM2_BYTES: TerminalBytes = TerminalBytes {
    terminal: "iTerm2",
    provenance: "factory key handling (no Ctrl+letter remaps; +Esc opt-in); C0 per man ascii(7); versions unknown",
    line_start: include_bytes!("fixtures/terminal_bytes/iterm2_line_start.bin"),
    line_end: include_bytes!("fixtures/terminal_bytes/iterm2_line_end.bin"),
    delete_to_line_start: include_bytes!("fixtures/terminal_bytes/iterm2_delete_to_line_start.bin"),
    delete_word: Some(include_bytes!(
        "fixtures/terminal_bytes/iterm2_delete_word.bin"
    )),
};

/// Terminal.app's default bytes: it declares `xterm-256color` and forwards
/// unmapped control keys as C0 bytes, with "Use Option as Meta key" off, so
/// the word key is Ctrl+W. Sources in
/// `fixtures/terminal_bytes/PROVENANCE.md`; Apple states no single version.
const TERMINAL_APP_BYTES: TerminalBytes = TerminalBytes {
    terminal: "Terminal.app",
    provenance: "xterm-256color declaration; Apple Control-U guide; C0 per man ascii(7); versions unknown",
    line_start: include_bytes!("fixtures/terminal_bytes/terminal_app_line_start.bin"),
    line_end: include_bytes!("fixtures/terminal_bytes/terminal_app_line_end.bin"),
    delete_to_line_start: include_bytes!(
        "fixtures/terminal_bytes/terminal_app_delete_to_line_start.bin"
    ),
    delete_word: Some(include_bytes!(
        "fixtures/terminal_bytes/terminal_app_delete_word.bin"
    )),
};

/// Linux terminals' default bytes (xterm, the reference X terminal): 7-bit
/// C0 controls for Ctrl+letter. Sources in
/// `fixtures/terminal_bytes/PROVENANCE.md`.
const LINUX_BYTES: TerminalBytes = TerminalBytes {
    terminal: "Linux",
    provenance: "readline C-a/C-e/C-u/C-w defaults; C0 per man ascii(7) (man-pages 6.19)",
    line_start: include_bytes!("fixtures/terminal_bytes/linux_line_start.bin"),
    line_end: include_bytes!("fixtures/terminal_bytes/linux_line_end.bin"),
    delete_to_line_start: include_bytes!("fixtures/terminal_bytes/linux_delete_to_line_start.bin"),
    delete_word: Some(include_bytes!(
        "fixtures/terminal_bytes/linux_delete_word.bin"
    )),
};

/// Every tabled terminal: one entry per terminal the ticket names. New
/// terminals join this table; no test branches on a terminal name.
const TERMINAL_TABLE: &[TerminalBytes] =
    &[GHOSTTY_BYTES, ITERM2_BYTES, TERMINAL_APP_BYTES, LINUX_BYTES];

/// The scripted `ask_user` step the byte journeys answer through: one
/// call with three questions, the third answered as free text on its
/// words row. Same surface as the `journeys.rs` question-form journey.
fn byte_form_step() -> Value {
    json!({"tool_calls": [{"name": "ask_user", "arguments": {"questions": [
        {"header": "Scope", "question": "Which crates should the change touch?",
         "multiSelect": true, "options": [
            {"label": "hub", "description": "the session host"},
            {"label": "tui", "description": "the terminal"}]},
        {"header": "Tests", "question": "Which tests cover it?",
         "options": [{"label": "unit"}, {"label": "pty"}]},
        {"header": "Docs", "question": "Which docs change?"},
    ]}}]})
}

/// Whether the grid draws the three-question form's title and first
/// question after the `run it` turn reaches the scripted step.
fn byte_form_drawn(grid: &Grid) -> bool {
    shows(
        grid,
        "Question from run it · 3 questions, any can be skipped",
    ) && shows(grid, "Which crates should the change touch?")
}

/// A TUI journey's terminal: ready, kitty flags pushed, the unseeded
/// home's first-run import offer declined, so regression bytes go out
/// only once the binary is ready for input.
fn start_byte_journey(setup: &Setup, bin: &Path) -> Run {
    let mut run = Run::spawn_journey(setup, 160, 48, bin, &[], &support::TRUECOLOUR);
    run.ready();
    run.wait_bytes(0, support::pty::KITTY_PUSH, "kitty keyboard flags pushed");
    dismiss_first_run_offer(&mut run);
    run
}

/// The question form open on its free-text words row: the `run it` turn
/// asked, the first question's option toggled, then Right and Tab across
/// to the Docs words row, exactly like the `journeys.rs` form journey.
fn open_words_row(run: &mut Run) {
    run.write(b"run it\r");
    run.wait_screen("the question form", byte_form_drawn);
    run.write(b"2");
    run.wait_screen("the toggled option", |grid| shows(grid, "2 [x] tui"));
    run.write(b"\x1b[C");
    run.wait_screen("the Tests tab", |grid| shows(grid, "Which tests cover it?"));
    run.write(b"\t");
    run.wait_screen("the Docs words row", |grid| {
        shows(grid, "Which docs change?") && shows(grid, "Type an answer")
    });
}

/// The declined form, then the person-visible exit: Esc declines the
/// open form, and the usual sequence quits from the declined turn.
fn decline_and_quit(mut run: Run) {
    run.write(b"\x1b[27u");
    run.wait_screen("the declined form", |grid| {
        shows(grid, "declined") && !shows(grid, "Question from")
    });
    quit_journey(run);
}

/// The line start and end bytes through the real decoder (#3751): a
/// two-line input-box draft, then the entry's bytes move on the
/// cursor's line exactly like the `line_start_end.rs` journey, asserting
/// only inside wait predicates.
fn line_start_end_bytes_journey(entry: &TerminalBytes) {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["Complete."]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": support::journey::tui_off()}),
    );
    let bin = journey_bin();
    let mut run = start_byte_journey(&setup, &bin);
    // A two-line draft with the cursor at its end.
    run.write(b"qx\x0ajz");
    run.wait_screen("the two-line draft", |grid| {
        shows(grid, "qx") && shows(grid, "jz")
    });
    // The entry's bytes move to the start of the cursor's line: typing
    // lands before it, not at the draft's start.
    let mut start = entry.line_start.to_vec();
    start.push(b'X');
    run.write(&start);
    run.wait_screen("the line-start edit", |grid| {
        shows(grid, "Xjz") && !shows(grid, "Xqx")
    });
    // The entry's bytes move to the end of the cursor's line.
    let mut end = entry.line_end.to_vec();
    end.push(b'Y');
    run.write(&end);
    run.wait_screen("the line-end edit", |grid| shows(grid, "XjzY"));
    // From the first line, the end bytes stay on it instead of jumping
    // to the draft's end.
    let mut first = b"\x1b[A".to_vec();
    first.extend_from_slice(entry.line_end);
    first.push(b'Q');
    run.write(&first);
    run.wait_screen("the first line's end", |grid| {
        shows(grid, "qxQ")
            && !grid
                .rows
                .iter()
                .map(|row| row.replace('█', ""))
                .any(|row| row.contains("YQ"))
    });
    // One Ctrl+C clears the draft, and the usual sequence quits from
    // the empty box.
    run.write(b"\x03");
    run.wait_screen("the cleared draft", |grid| !shows(grid, "XjzY"));
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    assert_eq!(run.wait().status.code(), Some(0));
}

/// The delete-to-line-start bytes on a question form's words row
/// (#3754): "one two three" typed, the entry's bytes empty the row.
fn delete_to_line_start_bytes_journey(entry: &TerminalBytes) {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [byte_form_step(), {"text": ["Thanks."]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": support::journey::tui_off()}),
    );
    let bin = journey_bin();
    let mut run = start_byte_journey(&setup, &bin);
    open_words_row(&mut run);
    run.write(b"one two three");
    run.wait_screen("the typed words row", |grid| shows(grid, "✎ one two three"));
    run.write(entry.delete_to_line_start);
    run.wait_screen("the emptied words row", |grid| {
        shows(grid, "Type an answer") && !shows(grid, "one two three")
    });
    decline_and_quit(run);
}

/// The delete-word bytes on a question form's words row (#3754): "one
/// two" typed, the entry's bytes cut "two" while "one" stays. The entry
/// must have a word-delete byte; Ghostty has none, so it has no test.
fn delete_word_bytes_journey(entry: &TerminalBytes) {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [byte_form_step(), {"text": ["Thanks."]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": support::journey::tui_off()}),
    );
    let bin = journey_bin();
    let mut run = start_byte_journey(&setup, &bin);
    open_words_row(&mut run);
    run.write(b"one two");
    run.wait_screen("the typed words row", |grid| shows(grid, "✎ one two"));
    let delete_word = entry
        .delete_word
        .expect("tabled terminal has a word-delete byte");
    run.write(delete_word);
    run.wait_screen("the cut words row", |grid| {
        shows(grid, "✎ one") && !shows(grid, "one two")
    });
    decline_and_quit(run);
}

/// The closed terminal table: every entry names its terminal and cites
/// its bytes' source, so a new terminal joins the table with its
/// provenance instead of a branch.
#[test]
fn terminal_bytes_table_names_each_terminal_source() {
    for entry in TERMINAL_TABLE {
        assert!(!entry.terminal.is_empty(), "each entry names its terminal");
        assert!(
            !entry.provenance.is_empty(),
            "each entry cites its bytes' source"
        );
    }
    assert_eq!(
        TERMINAL_TABLE.len(),
        4,
        "one entry per tabled terminal: Ghostty, iTerm2, Terminal.app, Linux"
    );
}

/// Ghostty's #3751 bytes through the decoder: Super+Left/Right arrive
/// as Ctrl+A/Ctrl+E and move on the cursor's line.
#[test]
fn terminal_bytes_ghostty_line_start_end() {
    run_journey(
        "terminal_bytes_ghostty_line_start_end",
        std::panic::AssertUnwindSafe(|| line_start_end_bytes_journey(&GHOSTTY_BYTES)),
    );
}

/// Ghostty's #3754 line bytes through the decoder: Super+Backspace
/// arrives as Ctrl+U and empties the words row.
#[test]
fn terminal_bytes_ghostty_delete_to_line_start() {
    run_journey(
        "terminal_bytes_ghostty_delete_to_line_start",
        std::panic::AssertUnwindSafe(|| delete_to_line_start_bytes_journey(&GHOSTTY_BYTES)),
    );
}

// Ghostty has no `delete_word` default to feed: Option+Backspace is plain
// `0x7f` since 1.2 and `ESC 0x7f` is a user keybind, so the word row is
// covered for iTerm2, Terminal.app and Linux only.

/// iTerm2's factory line bytes through the decoder: Ctrl+A/Ctrl+E pass
/// through unmapped and move on the cursor's line.
#[test]
fn terminal_bytes_iterm2_line_start_end() {
    run_journey(
        "terminal_bytes_iterm2_line_start_end",
        std::panic::AssertUnwindSafe(|| line_start_end_bytes_journey(&ITERM2_BYTES)),
    );
}

/// iTerm2's factory line bytes through the decoder: Ctrl+U passes through
/// unmapped and empties the words row.
#[test]
fn terminal_bytes_iterm2_delete_to_line_start() {
    run_journey(
        "terminal_bytes_iterm2_delete_to_line_start",
        std::panic::AssertUnwindSafe(|| delete_to_line_start_bytes_journey(&ITERM2_BYTES)),
    );
}

/// iTerm2's factory word bytes through the decoder: Ctrl+W passes through
/// unmapped and cuts the words row back to "one".
#[test]
fn terminal_bytes_iterm2_delete_word() {
    run_journey(
        "terminal_bytes_iterm2_delete_word",
        std::panic::AssertUnwindSafe(|| delete_word_bytes_journey(&ITERM2_BYTES)),
    );
}

/// Terminal.app's line bytes through the decoder: Ctrl+A/Ctrl+E are
/// forwarded as C0 bytes and move on the cursor's line.
#[test]
fn terminal_bytes_terminal_app_line_start_end() {
    run_journey(
        "terminal_bytes_terminal_app_line_start_end",
        std::panic::AssertUnwindSafe(|| line_start_end_bytes_journey(&TERMINAL_APP_BYTES)),
    );
}

/// Terminal.app's line bytes through the decoder: Ctrl+U is forwarded as
/// a C0 byte and empties the words row.
#[test]
fn terminal_bytes_terminal_app_delete_to_line_start() {
    run_journey(
        "terminal_bytes_terminal_app_delete_to_line_start",
        std::panic::AssertUnwindSafe(|| delete_to_line_start_bytes_journey(&TERMINAL_APP_BYTES)),
    );
}

/// Terminal.app's word bytes through the decoder: Ctrl+W is forwarded as
/// a C0 byte and cuts the words row back to "one".
#[test]
fn terminal_bytes_terminal_app_delete_word() {
    run_journey(
        "terminal_bytes_terminal_app_delete_word",
        std::panic::AssertUnwindSafe(|| delete_word_bytes_journey(&TERMINAL_APP_BYTES)),
    );
}

/// Linux terminals' line bytes through the decoder: Ctrl+A/Ctrl+E arrive
/// as C0 bytes and move on the cursor's line.
#[test]
fn terminal_bytes_linux_line_start_end() {
    run_journey(
        "terminal_bytes_linux_line_start_end",
        std::panic::AssertUnwindSafe(|| line_start_end_bytes_journey(&LINUX_BYTES)),
    );
}

/// Linux terminals' line bytes through the decoder: Ctrl+U arrives as a
/// C0 byte and empties the words row.
#[test]
fn terminal_bytes_linux_delete_to_line_start() {
    run_journey(
        "terminal_bytes_linux_delete_to_line_start",
        std::panic::AssertUnwindSafe(|| delete_to_line_start_bytes_journey(&LINUX_BYTES)),
    );
}

/// Linux terminals' word bytes through the decoder: Ctrl+W arrives as a
/// C0 byte and cuts the words row back to "one".
#[test]
fn terminal_bytes_linux_delete_word() {
    run_journey(
        "terminal_bytes_linux_delete_word",
        std::panic::AssertUnwindSafe(|| delete_word_bytes_journey(&LINUX_BYTES)),
    );
}

/// Installs a provider `fake` with model `m` on `anthropic-messages` at
/// the fake server, and makes `fake/m` the configured model: the replay
/// half of `Setup::provider_on` (`failed_tool_result_text.rs`),
/// file-local because each test binary owns its helpers.
fn install_replay_anthropic_provider(setup: &Setup, server: &ProviderServer) {
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
            "models": [{"id": "m", "protocol": "anthropic-messages", "base_url": format!("{}/v1", server.url()), "context_window": 100000, "input": ["text", "image"]}]
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
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m"}),
    );
}

/// Installs the copied `databricks` package with its origin rewritten to
/// the fake server: the replay half of `install` (`databricks.rs`),
/// file-local because each test binary owns its helpers. Returns the
/// fake host the `DATABRICKS_HOST` environment carries.
fn install_replay_databricks_package(setup: &Setup, server: &ProviderServer) -> String {
    let to = setup.root.path().join("pkg-databricks");
    support::package::copy_package("databricks", &to, "https://", "http://");
    std::fs::write(
        to.join("thinking.lua"),
        include_str!("../../../providers/databricks/thinking.lua"),
    )
    .unwrap();
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(to),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    server.url().strip_prefix("http://").unwrap().to_owned()
}

/// The discovery's two answers, in the order `models()` asks for them:
/// only the Claude entries this journey needs (`databricks.rs` serves
/// three services with full metadata), file-local. The row keeps what
/// `entry_for` reads: the chat mode and name match, the ANTHROPIC type
/// for the bearer route, and the window a model needs to count.
fn replay_discovery() -> [Response; 2] {
    [
        Response::status(
            200,
            json!({"model_services": [
                {"name": "model-services/system.ai.claude-opus-5",
                 "supported_api_types": ["mlflow/v1/chat/completions"]},
            ]})
            .to_string(),
        ),
        Response::status(
            200,
            json!({"models": [
                {"model_name": "claude-opus-5", "mode": "chat", "provider_type": "ANTHROPIC",
                 "context_window": {"max_input": 200000, "max_output": 64000}},
            ]})
            .to_string(),
        ),
    ]
}

/// A scripted `anthropic-messages` text reply with `text`.
fn replay_text_reply(text: &str) -> Response {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1"}}),
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 10, "output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ];
    Response::stream(
        events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

/// A scripted `anthropic-messages` reply calling built-in tool `name`
/// with `arguments`.
fn replay_tool_call(id: &str, name: &str, arguments: &Value) -> Response {
    let mut all = vec![json!({"type": "message_start", "message": {"id": "msg_1"}})];
    all.push(
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": id, "name": name, "input": arguments}}),
    );
    all.push(json!({"type": "content_block_stop", "index": 0}));
    all.push(
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
        "usage": {"input_tokens": 10, "output_tokens": 3}}),
    );
    all.push(json!({"type": "message_stop"}));
    Response::stream(
        all.iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

/// User messages decoded from one captured Anthropic Messages request:
/// each `role: user` message's text parts joined, mirroring
/// `user_messages` (`handoff_kept.rs`) for the Responses envelope.
fn replay_user_messages(body: &[u8]) -> Vec<String> {
    let request: Value = serde_json::from_slice(body).unwrap();
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| {
            if let Some(text) = message["content"].as_str() {
                text.to_owned()
            } else {
                message["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<String>()
            }
        })
        .collect()
}

/// The `seq` of the logged turn whose input contains `text`, mirroring
/// `turn_started_seq` (`handoff_kept.rs`) over the session's events file.
fn replay_turn_seq(lines: &[Value], text: &str) -> u64 {
    lines
        .iter()
        .find(|line| {
            line["kind"] == "turn_started"
                && line["payload"]["input"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["content"]
                            .as_array()
                            .is_some_and(|parts| parts.iter().any(|part| part["text"] == text))
                    })
                })
        })
        .and_then(|line| line["seq"].as_u64())
        .expect("turn_started with the requested text and seq")
}

/// Types `text` as one journey turn and waits for `wait_for` on screen,
/// allowing every approval prompt on the way: a tool turn's approval
/// arrives before its reply, so the wait names either.
fn replay_ask(run: &mut Run, text: &str, wait_for: &str) {
    let from = run.output().len();
    run.write(format!("{text}\r").as_bytes());
    loop {
        let grid = run.wait_screen("approval or the reply", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains(wait_for)
        });
        // settled: the approval-or-reply wait returned this grid, with no input since.
        if grid.contents.contains(wait_for) {
            break;
        }
        let approval = run.wait_bytes(from, b"Allow once", "the approval prompt");
        run.wait_bytes(approval, FRAME_END, "the approval frame");
        run.write(b"\r");
    }
    run.turn_finished(from);
}

/// The recorded #3969 exchange replayed: the person's messages that still
/// bind survive a handoff on the Databricks Anthropic route. Discovery
/// answers the two Unity Catalog endpoints once at startup, then the
/// turns run the scripted pre-handoff replies with the recorded text
/// reply answering the first post-handoff request; the journey asserts
/// the labelled pair blocks handoff_kept pins. Evidence for #3969, whose
/// parent dropped kept messages' replies from the post-handoff request.
#[test]
fn replay_databricks_handoff_keeps_labelled_pairs() {
    run_journey(
        "replay_databricks_handoff_keeps_labelled_pairs",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let skill_path = setup.workspace().join(".agents/skills/review/SKILL.md");
            fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
            fs::write(
                &skill_path,
                "---\nname: review\ndescription: Review this work.\n---\nUse the current review skill instructions.\n",
            )
            .unwrap();
            let mut script: Vec<Response> = replay_discovery().into_iter().collect();
            script.push(replay_text_reply("Which color should I use?"));
            script.push(replay_text_reply("Which number should I use?"));
            script.push(replay_tool_call(
                "toolu_1",
                "skill",
                &json!({"name": "review"}),
            ));
            script.push(replay_text_reply("The skill is loaded."));
            script.push(replay_tool_call(
                "toolu_2",
                "handoff",
                &json!({"note": "The handoff note."}),
            ));
            script.push(Response::stream(
                include_bytes!("fixtures/provider_replay/3969_post_handoff_text.sse").as_slice(),
            ));
            let server = ProviderServer::start(script).unwrap();
            let host = install_replay_databricks_package(&setup, &server);
            support::write_json(
                &setup.workspace().join("r.json"),
                &json!({"steps": [{"text": "2, 3"}]}),
            );
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "databricks/databricks-claude-opus-5",
                    "hub": {"idle_exit_ms": 200},
                    "reviewer": {"model": "scripted/r.json"}}),
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &[],
                &[
                    ("DATABRICKS_TOKEN", "dapi-test"),
                    ("DATABRICKS_HOST", host.as_str()),
                ],
            );
            run.ready();
            dismiss_first_run_offer(&mut run);
            replay_ask(&mut run, "Which color?", "Which color should I use?");
            replay_ask(&mut run, "Blue.", "Which number should I use?");
            replay_ask(&mut run, "Seven.", "The skill is loaded.");
            replay_ask(&mut run, "Please hand off now.", "lovely human");
            // Confirm the reviewer, skill tool and handoff all ran before
            // inspecting the request bytes, so a broken setup cannot
            // masquerade as the regression.
            let sessions = sessions(&setup);
            assert_eq!(sessions.len(), 1, "one session ran");
            let events = std::fs::read_to_string(sessions[0].join("events.jsonl")).unwrap();
            let lines: Vec<Value> = events
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let color_seq = replay_turn_seq(&lines, "Which color?");
            let blue_seq = replay_turn_seq(&lines, "Blue.");
            let seven_seq = replay_turn_seq(&lines, "Seven.");
            let kept = lines
                .iter()
                .find(|line| line["kind"] == "reviewer_kept")
                .expect("the reviewer selected messages");
            assert_eq!(
                kept["payload"]["kept"],
                json!([
                    {"seq": blue_seq, "item": 0},
                    {"seq": seven_seq, "item": 0}
                ]),
                "the selection keeps the two answers, not the questions"
            );
            assert_ne!(color_seq, blue_seq);
            let loaded = lines
                .iter()
                .find(|line| {
                    line["kind"] == "tool_call_completed"
                        && line["payload"]["control"]["skill"]["name"] == "review"
                })
                .expect("the real skill tool loaded review");
            assert_eq!(loaded["payload"]["status"], "completed");
            let resent = lines
                .iter()
                .find(|line| line["kind"] == "skills_resent")
                .expect("the loaded skill was resent after handoff");
            assert_eq!(resent["payload"]["skills"].as_array().unwrap().len(), 1);
            let completed = lines
                .iter()
                .find(|line| line["kind"] == "handoff_completed")
                .expect("the handoff completed");
            assert_eq!(completed["payload"]["outcome"], "completed");
            let requests = server.requests();
            assert!(requests.len() >= 8, "{}", requests.len());
            let post = requests
                .iter()
                .rfind(|request| request.path == "/ai-gateway/anthropic/v1/messages")
                .expect("a model request");
            assert_eq!(
                post.header("authorization"),
                Some(fingerprint("Bearer dapi-test").as_str())
            );
            let before: String = requests
                .iter()
                .filter(|request| {
                    request.path == "/ai-gateway/anthropic/v1/messages"
                        && !std::ptr::eq(*request, post)
                })
                .map(|request| replay_user_messages(&request.body).join(""))
                .collect();
            assert!(before.contains("Which color?"));
            assert!(before.contains("Blue."));
            assert!(before.contains("Seven."));
            assert!(before.contains("Please hand off now."));
            assert!(
                !before.contains("The person's earlier message:")
                    && !before.contains("The person's new message:")
                    && !before.contains("Skills reloaded from the previous context:"),
                "handoff-only framing must not leak into ordinary requests"
            );
            let messages = replay_user_messages(&post.body);
            let flattened = messages.join("");
            // docs/handoff.md, "What the model sees after a handoff", item 4:
            // "each in its own labelled block" — two kept answers, two blocks.
            // The Anthropic envelope carries them in one user message, so
            // the count and the order below read the flattened text.
            assert_eq!(
                flattened.matches("The person's earlier message:").count(),
                2,
                "each kept answer has its own block"
            );
            let pair_one = "The person's earlier message:\nThe agent: Which color should I use?\nThe person: Blue.";
            let pair_two = "The person's earlier message:\nThe agent: Which number should I use?\nThe person: Seven.";
            let current = "The person's new message:\nPlease hand off now.";
            let note = "The handoff note.";
            let skill = &resent["payload"]["skills"][0];
            let skill_block = format!(
                "Skills reloaded from the previous context:\n{} — {}\n\n{}",
                skill["name"].as_str().unwrap(),
                skill["path"].as_str().unwrap(),
                skill["content"].as_str().unwrap(),
            );
            // docs/handoff.md, "What the model sees after a handoff", "In order:":
            // each block present once, oldest first, then the new message,
            // the note and the reloaded skill.
            let positions: Vec<usize> = [pair_one, pair_two, current, note, skill_block.as_str()]
                .iter()
                .map(|text| {
                    flattened
                        .find(text)
                        .unwrap_or_else(|| panic!("flattened request lost {text:?}"))
                })
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "block positions were {positions:?}"
            );
            quit_journey(run);
        }),
    );
}

/// Installs the copied `bedrock` package with its endpoint rewritten to
/// the fake server: the replay half of `bedrock_answering` (`bedrock.rs`),
/// file-local because each test binary owns its helpers. Serves the
/// recorded Converse text-turn bytes with the eventstream content type,
/// never `Response::stream`. Returns the fake region host the `AWS_REGION`
/// environment carries.
fn install_replay_bedrock_package(setup: &Setup) -> (ProviderServer, String) {
    let server = ProviderServer::start([Response {
        status: 200,
        headers: vec![(
            "content-type".to_owned(),
            "application/vnd.amazon.eventstream".to_owned(),
        )],
        body: include_bytes!("fixtures/provider_replay/bedrock_converse_text.eventstream").to_vec(),
        drop_connection: false,
        stall: false,
    }])
    .unwrap();
    let to = setup.root.path().join("pkg-bedrock");
    support::package::copy_package(
        "bedrock",
        &to,
        "https://bedrock-runtime.{region}.amazonaws.com",
        "http://{region}",
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(to),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    // The region setting is the per-account host, read from `AWS_REGION`.
    let host = server.url().strip_prefix("http://").unwrap().to_owned();
    (server, host)
}

/// The recorded Bedrock Converse shape replayed: one text turn on the
/// copied `bedrock` package against the fake server answers from the
/// recorded eventstream bytes, and the screen shows the reply. It claims
/// no pre-fix failure: no Bedrock bug stands behind it.
#[test]
fn replay_bedrock_converse_text_turn() {
    run_journey(
        "replay_bedrock_converse_text_turn",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let (server, host) = install_replay_bedrock_package(&setup);
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "bedrock/mistral.ministral-3-8b-instruct",
                    "hub": {"idle_exit_ms": 200}}),
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &[],
                &[
                    ("AWS_BEARER_TOKEN_BEDROCK", "bedrock-test-key"),
                    ("AWS_REGION", host.as_str()),
                ],
            );
            run.ready();
            dismiss_first_run_offer(&mut run);
            let from = run.output().len();
            run.write(b"hi\r");
            run.wait_screen("the answered turn", |grid| shows(grid, "goin"));
            run.turn_finished(from);
            let requests = server.requests();
            assert_eq!(requests.len(), 1, "{requests:?}");
            let sent = &requests[0];
            assert_eq!(
                sent.path,
                "/model/mistral.ministral-3-8b-instruct/converse-stream"
            );
            assert_eq!(
                sent.header("authorization"),
                Some(fingerprint("Bearer bedrock-test-key").as_str())
            );
            quit_journey(run);
        }),
    );
}

/// The recorded #3841 exchange replayed: a hanging MCP call's timeout
/// reaches the provider as the tool result's text. The fake answers the
/// first request with the scripted hang call, an empty `is_error` result
/// with the recorded gateway 400, and any other result with the recorded
/// text reply; the journey asserts the recorded result carries the
/// timeout message. Evidence for #3841, whose parent sent the timed-out
/// result empty, which the gateway rejects with that 400.
#[test]
fn replay_empty_failed_tool_result_carries_timeout_text() {
    run_journey(
        "replay_empty_failed_tool_result_carries_timeout_text",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new_with_mcp_fixture();
            let dir = setup.root.path().join("fx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("tools.json"),
                json!([{"name": "hang", "annotations": {"readOnlyHint": true, "openWorldHint": false}}])
                    .to_string(),
            )
            .unwrap();
            std::fs::write(dir.join("call-hang.json"), "hang").unwrap();
            let server = ProviderServer::start_responding(|request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                let result = body["messages"]
                    .as_array()
                    .into_iter()
                    .flat_map(|messages| messages.iter())
                    .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
                    .find(|block| block["type"] == "tool_result");
                match result {
                    // The wire the recorded `stream tool use` exchange
                    // cannot serve, since bare `get_weather` never invokes
                    // the hanging fixture.
                    None => replay_tool_call("toolu_1", "mcp__fx__hang", &json!({})),
                    Some(block)
                        if block["is_error"] == json!(true)
                            && (block["content"] == json!("") || block["content"] == json!([])) =>
                    {
                        Response::status(
                            400,
                            include_bytes!("fixtures/provider_replay/3841_gateway_400.json")
                                .as_slice(),
                        )
                    }
                    Some(_) => Response::stream(
                        include_bytes!("fixtures/provider_replay/3841_text.sse").as_slice(),
                    ),
                }
            })
            .unwrap();
            install_replay_anthropic_provider(&setup, &server);
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}, "mcp": {"servers": {"fx": {
                    "command": "/bin/bash",
                    "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
                    "startup_timeout_ms": 600000,
                    "timeout_ms": 200,
                }}}}),
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
            let from = run.output().len();
            run.write(b"hang\r");
            // The turn's end paint, whatever its outcome: the failed turn
            // paints its failed card, so waiting for the completed paint
            // alone would stall past the failure the replay exists to show.
            loop {
                let grid = run.wait_screen("approval or the turn's end", |grid| {
                    grid.contents.contains("Allow once")
                        || grid.contents.contains("▣ completed")
                        || grid.contents.contains("▣ failed")
                });
                // settled: the approval-or-end wait returned this grid, with no input since.
                if grid.contents.contains("▣ completed") || grid.contents.contains("▣ failed") {
                    break;
                }
                let approval = run.wait_bytes(from, b"Allow once", "the approval prompt");
                run.wait_bytes(approval, FRAME_END, "the approval frame");
                run.write(b"\r");
            }
            // The hub announces the turn's end by its title on every
            // outcome; the grid paint above already told which one.
            run.wait_bytes(from, support::pty::FINISHED_TITLE, "the finished title");
            let results: Vec<Value> = server
                .requests()
                .iter()
                .filter_map(|request| {
                    let body: Value = serde_json::from_slice(&request.body).ok()?;
                    body["messages"]
                        .as_array()?
                        .iter()
                        .flat_map(|message| {
                            message["content"].as_array().cloned().unwrap_or_default()
                        })
                        .find(|block| block["type"] == "tool_result")
                })
                .collect();
            let result = results
                .into_iter()
                .find(|block| block["is_error"] == json!(true))
                .expect("the timed-out result reached the provider");
            assert_eq!(result["is_error"], json!(true));
            let content = result["content"].as_str().unwrap();
            assert!(
                !content.is_empty(),
                "the timed-out result reached the provider empty: 400 messages.164.content.0.tool_result: content cannot be empty if is_error is true"
            );
            assert!(
                content.contains("did not answer"),
                "the timed-out result carries the timeout message, got: {content}"
            );
            run.wait_screen("the answered turn", |grid| shows(grid, "lovely human"));
            quit_journey(run);
        }),
    );
}

/// Rows one overlay line wraps to at `room` columns: the
/// `wrap_joined_runs` rule (`tui/src/view/notice_rows.rs`) — greedy
/// words, a word wider than the row breaking at the edge, consecutive
/// spaces reading as one. ASCII-only here, so one char is one column
/// and no cut splits a char.
fn wrapped_rows(line: &str, room: usize) -> usize {
    let room = room.max(1);
    let mut rows = 1;
    let mut taken = 0;
    for word in line.split(' ') {
        if word.is_empty() {
            continue;
        }
        let mut rest = word;
        let mut leading = taken > 0;
        while !rest.is_empty() {
            let gap = usize::from(leading);
            if taken + gap + rest.len() <= room {
                taken += gap + rest.len();
                break;
            }
            if taken > 0 {
                rows += 1;
                taken = 0;
            }
            let take = rest.len().min(room);
            rest = &rest[take..];
            if rest.is_empty() {
                taken += take;
            } else {
                rows += 1;
            }
            leading = false;
        }
    }
    rows
}

/// One shadow pair's overlay paths with the lengths discovery logs:
/// `/.agents/skills/<name>/SKILL.md` beside the home copy under
/// `/h/skills/<name>/SKILL.md`, both under the canonical root.
fn shadowed_paths(name: &str, root_len: usize) -> (String, String) {
    let root = "r".repeat(root_len);
    (
        format!("{root}/.agents/skills/{name}/SKILL.md"),
        format!("{root}/h/skills/{name}/SKILL.md"),
    )
}

/// The grouped journey's overlay text in shape: the lead naming the
/// first shadow pair, then one `- ` bullet per pair (`group_notices`
/// with `grouped_lead` in `contract/src/events/context.rs`, shadow
/// message in `loop/src/skills.rs`). Only word lengths reach the wrap
/// count, so the stand-in paths carry the logged lengths.
fn overlay_text(root_len: usize) -> String {
    let message = |index: usize| {
        let name = format!("dup{index:02}");
        let (agents, home) = shadowed_paths(&name, root_len);
        format!("Skill {name} at {agents} is shadowed by {home}, which is used.")
    };
    let bullets: Vec<String> = (0..25)
        .map(|index| format!("- {}", message(index)))
        .collect();
    format!(
        "25 skills are shadowed: {}\n{}",
        message(0),
        bullets.join("\n")
    )
}

/// The pty rows a 120-column run needs to show the whole grouped notice:
/// the overlay keeps the first rows and drops body rows from the bottom
/// when the stack exceeds the area, so a short pty clips names and the
/// journey fails. The count replays the overlay's own wrap over the
/// overlay text, so it holds on any temporary directory: `root_len` is
/// the canonical fixture root length. `room` stays at the width the old
/// divisor assumed (the overlay wraps wider): a narrower room only adds
/// rows, so the count never underfits, and a taller pty changes no wait.
/// 8 chrome slots hold the frame around the lead and bullets, and 16
/// slack covers the page chrome.
fn overlay_rows(root_len: usize) -> u16 {
    const ROOM: usize = 50;
    let body: usize = overlay_text(root_len)
        .split('\n')
        .map(|line| wrapped_rows(line, ROOM))
        .sum();
    u16::try_from(16 + 8 + body).expect("the overlay fits the pty")
}

/// `wrapped_rows` replays the overlay's wrap: one row for a short
/// line, consecutive spaces reading as one, a word wider than the
/// row breaking at the edge.
#[test]
fn wrapped_rows_replays_the_overlay_wrap() {
    assert_eq!(wrapped_rows("ab cd", 50), 1);
    assert_eq!(wrapped_rows("ab  cd", 50), 1);
    assert_eq!(wrapped_rows("ab cdefgh", 5), 3);
}

/// The overlay height fits the wrap it replays: at `root_len` 41 the
/// lead takes 5 rows and each bullet 5, for 16 + 8 + 130.
#[test]
fn overlay_rows_fits_the_replayed_wrap() {
    assert_eq!(overlay_rows(41), 154);
}

/// Sessions under `dir` with an events file: an unreadable directory
/// counts none, so the `wait_until` poll keeps waiting, as the inline
/// `is_ok_and` it replaces did.
fn opened_sessions(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("events.jsonl").is_file())
                .count()
        })
        .unwrap_or(0)
}

/// Twenty-five shadowed skills show one grouped row, once per terminal
/// run: from the lived-in fixture a first session stacks the grouped
/// row, the overlay lists all 25 names, and a second session in the same
/// run shows no row. Evidence for #3679, whose parent showed every
/// shadowed skill on its own row and repeated them across sessions.
#[test]
fn lived_home_twenty_five_shadowed_notices_show_once_per_run() {
    run_journey(
        "lived_home_twenty_five_shadowed_notices_show_once_per_run",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = ProviderServer::start([support::hello(), support::hello()]).unwrap();
            install_fake_provider_extension(&setup, &server);
            std::fs::create_dir_all(setup.home().join("credentials/fake")).unwrap();
            std::fs::write(setup.home().join("credentials/fake/work"), "test-key").unwrap();
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}}),
            );
            support::lived_home::lived_home(setup.root.path(), 25);
            let root_len = fs::canonicalize(setup.root.path())
                .unwrap()
                .to_string_lossy()
                .len();
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                overlay_rows(root_len),
                &bin,
                &[],
                &[("FIBER_TEST_FAKE_KEY", "sk-test")],
            );
            run.ready();
            dismiss_first_run_offer(&mut run);
            // Home starts no session: the first prompt opens the run's
            // first session, whose opening stacks the grouped row.
            let from = run.output().len();
            run.write(b"say hi\r");
            // One grouped row for the run's first session: the count
            // pins the neither-zero-nor-25 edges, since a repeat or an
            // ungrouped pass never shows exactly one grouped row.
            run.wait_screen("the grouped shadow notice", |grid| {
                grid.rows
                    .iter()
                    .filter(|row| row.contains("25 skills are shadowed"))
                    .count()
                    == 1
            });
            // Enter on the focused box opens its whole text in the
            // overlay (`docs/tui.md`, "Notices"): a pointer click would
            // target a row the still-running turn can move, while the
            // focus walk re-reads the row each step and waits the focus
            // look per step, so it is readiness-safe. `focus_notice`
            // returns only on the box, and Enter there opens (see its
            // doc); Enter on the cross would dismiss instead.
            focus_notice(&mut run, "skills are shadowed");
            run.write(b"\r");
            run.wait_screen("the overlay lists all 25", |grid| {
                (0..25).all(|index| shows(grid, &format!("dup{index:02}")))
            });
            // The open overlay pauses the 10 s expiry, so the slow reply
            // cannot expire the notice before dismissal; Esc closes the
            // overlay once the turn finished behind it.
            run.turn_finished(from);
            run.write(b"\x1b[27u");
            run.wait_screen("the closed overlay", |grid| {
                shows(grid, "25 skills are shadowed") && !shows(grid, "dup01")
            });
            // Dismiss the row through keyboard focus on its dismiss
            // cross, exactly as the terminal test does: dismissal, not
            // the overlay close, removes the stacked notice, so the
            // second session's absence is observable without waiting for
            // the fade.
            focus_notice(&mut run, "skills are shadowed");
            run.wait_screen("the notice focused", |grid| {
                grid.rows
                    .iter()
                    .any(|row| row.contains("25 skills are shadowed"))
            });
            run.write(b"\x1b[B");
            // The cross's own look, or the row already gone: the ✕ cell
            // alone reversed (the box look reverses the whole row, so a
            // full row never counts). Any other whole-grid move — an
            // overlay close or a fade paints transient frames — is not
            // the cross, and Enter on the box would reopen the overlay.
            // A fading row clears on its own, so only Enter on a focused
            // cross.
            let mut cross = false;
            run.wait_screen("the dismiss cross focused or the row gone", |grid| {
                if !grid.contents.contains("skills are shadowed") {
                    return true;
                }
                cross = grid
                    .text_at("25 skills are shadowed")
                    .is_some_and(|(_, y)| {
                        let (cols, _) = grid.size();
                        let mut found = false;
                        for x in 0..cols {
                            if grid.cell(x, y).inverse {
                                if x <= 1 || found {
                                    return false;
                                }
                                found = true;
                            }
                        }
                        found
                    });
                cross
            });
            if cross {
                run.write(b"\r");
            }
            run.wait_screen("the dismissed row clears", |grid| {
                !grid.contents.contains("skills are shadowed")
            });
            run.write(b"\x1b[27u");
            run.wait_screen("the input box back", |grid| reversed_cells(grid).is_empty());
            // `/new` returns home with the cursor in the input box; the
            // next prompt starts the second session beside the first. Its
            // identical opening notice shows no row again, since the run
            // already showed it (`docs/tui.md`, "Notices").
            run.write(b"/new\r");
            run.wait_screen("home for the second session", |grid| {
                grid.contents.contains("Enter starts a session")
            });
            run.write(b"say hi\r");
            run.wait_screen("the second session's reply", |grid| {
                grid.contents.contains("Hello.")
            });
            let dir = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
            fakes::wait_until("the second session", setup.deadline, move || {
                opened_sessions(&dir) == 2
            });
            // settled: the reply painted and the session count beside it
            // proves the second session ran, so the frame below is final.
            let grid = run.wait_screen("a settled frame", |grid| grid.contents.contains("›"));
            // The reply and the count above prove the session ran, so this
            // settled: absence needs no wait of its own.
            let repeat = grid.contents.contains("skills are shadowed");
            assert!(!repeat, "no repeat in a second session");
            quit_journey(run);
        }),
    );
}

/// Import leaves already-read skills behind: from the same lived-in
/// fixture with no shadow pairs, `fiber import` copies the source-only
/// skills, leaves the already-read ones with their reason, and a
/// follow-up start shows no `skill_shadowed` row. Evidence for #3680,
/// whose parent copied skills discovery already read elsewhere.
#[test]
fn lived_home_import_leaves_already_read_skills() {
    run_journey(
        "lived_home_import_leaves_already_read_skills",
        // As in the earlier journeys: the body borrows the fake server,
        // so the standard `catch_unwind` wrapper applies.
        std::panic::AssertUnwindSafe(|| {
            let setup = Setup::new();
            let server = ProviderServer::start([support::hello()]).unwrap();
            install_fake_provider_extension(&setup, &server);
            std::fs::create_dir_all(setup.home().join("credentials/fake")).unwrap();
            std::fs::write(setup.home().join("credentials/fake/work"), "test-key").unwrap();
            support::write_json(
                &setup.home().join("config.json"),
                &json!({"model": "fake/m", "hub": {"idle_exit_ms": 200}}),
            );
            support::lived_home::lived_home(setup.root.path(), 0);
            // Discovery logs the canonical path, which on macOS resolves
            // the `/var` symlink to `/private/var`: canonicalize the
            // fixture path before expecting it.
            let agents_skill =
                fs::canonicalize(setup.root.path().join(".agents/skills/review/SKILL.md")).unwrap();
            let mine = format!(
                "left behind skills/mine/SKILL.md: already read from {}",
                agents_skill.display()
            );
            let review = format!(
                "left behind skills/review/SKILL.md: already read from {}",
                agents_skill.display()
            );
            let bin = journey_bin();
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &["import", "claude-code"],
                &[("FIBER_TEST_FAKE_KEY", "sk-test")],
            );
            // A logical line longer than 120 columns wraps across grid
            // rows on long canonical temp paths, so the check joins the
            // rows instead of matching one row only.
            run.wait_screen("the import lines", |grid| {
                let joined = grid.rows.join("");
                joined.contains("imported skill lint")
                    && joined.contains(mine.as_str())
                    && joined.contains(review.as_str())
            });
            let exited = run.wait();
            assert!(exited.status.success(), "import exits 0");
            assert!(
                server.requests().is_empty(),
                "import makes no provider calls"
            );
            // Nothing is written to `skills/` in Fiber home but the two
            // copied skills.
            assert!(!setup.home().join("skills/review").exists());
            assert!(!setup.home().join("skills/mine").exists());
            assert!(setup.home().join("skills/lint/SKILL.md").is_file());
            assert!(setup.home().join("skills/solo/SKILL.md").is_file());
            // A follow-up start raises no `skill_shadowed`: the copied
            // skills stand alone, with no Fiber-home copy to shadow the
            // agents-side skills. The import wrote the receipt, so no
            // first-run offer needs dismissing.
            let mut run = Run::spawn_journey(
                &setup,
                120,
                32,
                &bin,
                &[],
                &[("FIBER_TEST_FAKE_KEY", "sk-test")],
            );
            run.ready();
            run.write(b"say hi\r");
            run.wait_screen("the follow-up reply", |grid| {
                grid.contents.contains("Hello.")
            });
            let dir = log::sessions_dir(&setup.home(), &worktree::project(&setup.workspace()));
            fakes::wait_until("the first session", setup.deadline, move || {
                opened_sessions(&dir) == 1
            });
            // settled: the reply painted and the session count beside it
            // proves the session ran, so the frame below is final.
            let grid = run.wait_screen("a settled frame", |grid| grid.contents.contains("›"));
            // The reply and the count above prove the session ran, so this
            // settled: absence needs no wait of its own.
            let shadowed = grid.contents.contains("skill_shadowed");
            assert!(!shadowed, "no skill_shadowed row after import");
            quit_journey(run);
        }),
    );
}
