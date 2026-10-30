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
/// quota fetch reports 90% used and signals the test when it lands.
fn start_quota_server(hit: std::sync::mpsc::Sender<()>) -> ProviderServer {
    ProviderServer::start_responding(move |request| match request.path.as_str() {
        "/lua-quota" => {
            let _sent = hit.send(());
            fakes::Response::status(200, "90")
        }
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
            let (quota_hit, quota_rx) = std::sync::mpsc::channel();
            let server = start_quota_server(quota_hit);
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
            // The fake signals when its quota response is ready, so the
            // focus-in below runs after the fetch it started: the named
            // screen wait then draws the fetched share.
            setup.deadline.recv_or_fail(
                &quota_rx,
                "the fake quota response for the fetch the first turn starts",
            );
            run.write(b"\x1b[I");
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
            quit_journey(run);
        }),
    );
}

/// Installs a provider `fake` with three models in distinct states: `m-ok`
/// is valid and configured, `m-nocontext` declares no `context_window`
/// (`model_invalid`), and `m-host` names the `workspace` placeholder whose
/// setting value is not a host (`model_unconfigured`, as in
/// `model_picker_keys.rs`'s claimed test).
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
                {"id": "m-host", "protocol": "openai-responses", "base_url": "https://{workspace}/v1", "context_window": 100000},
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

/// Installs the Lua extension `badext` with provider `bad` whose models
/// function always fails: the catalogue refresh names the extension with
/// `extension_failed`.
fn install_failing_lua_provider(setup: &Setup, server_url: &str) {
    let source = setup.root.path().join("badsrc");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "badext", "version": "0.1.0", "fiber": "0.1.0", "api": 1,
                "providers": {"bad": [format!("{server_url}/v1")]}}),
    );
    std::fs::create_dir_all(source.join("providers")).unwrap();
    std::fs::write(
        source.join("providers/bad.lua"),
        format!(
            "fiber.provider(\"bad\", {{\n\
               models = {{ timeout = 60000, run = function()\n\
                 error(\"badext models are broken\")\n\
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
/// `model_invalid` itself is claimed by `model_picker_keys.rs`'s existing
/// binary-level test, which pins the negative for its model.
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
            install_failing_lua_provider(&setup, &server.url());
            std::fs::create_dir_all(setup.home().join("credentials/bad")).unwrap();
            std::fs::write(setup.home().join("credentials/bad/work"), "test-key").unwrap();
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
            // Each wait names its code and its model, which survive the
            // 120-column truncation that cuts the message's tail.
            let screen = run.wait_screen("the model_unconfigured notice", |grid| {
                shows(grid, "model_unconfigured") && shows(grid, "fake/m-host")
            });
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
        .map(|y| y as u16)
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
                let letter = (b'a' + n as u8) as char;
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
            // Tall enough that every view still draws, short enough that
            // the context breakdown overflows: thirteen rows against ten
            // visible, room for one full three-row step.
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
            // is the transcript's bottom-anchored answer row.
            let mark_row = find_grid_row(&run.screen().rows, "Hello.").expect("the marker row");
            let mark_text = run.screen().rows[usize::from(mark_row)].clone();
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
