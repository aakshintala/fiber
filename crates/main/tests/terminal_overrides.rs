//! Binary-level tests that bare `fiber --model`/`-c` open the terminal
//! and ride each hub `start` in `overrides`, while any subcommand keeps
//! them a usage error (`docs/invocation.md`, "Sessions").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::write_json;

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, with no model configured and no retry in the file: the
/// run's `--model` is the session's only model, and its `-c` is the only
/// retry layer. The hub idles out a second after its last client leaves,
/// so no hub lingers.
fn provider_without_model(setup: &support::Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    write_json(
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
    write_json(
        &setup.home().join("config.json"),
        &json!({"hub": {"idle_exit_ms": 1000}, "retry": {
            "attempts": 0, "initial_delay_ms": 0, "max_delay_ms": 0,
        }}),
    );
}

/// Spawns `fiber` with `args` on a `cols` by `rows` terminal through the
/// shared driver, with the fake key in the child's environment.
fn terminal(
    setup: &support::Setup,
    cols: u16,
    rows: u16,
    args: &[&str],
) -> support::pty::Run {
    support::pty::Run::spawn(
        setup,
        cols,
        rows,
        args,
        &[("FIBER_TEST_FAKE_KEY", "sk-test")],
    )
}

/// The project's sessions directory, through the canonical workspace,
/// as the project key names it.
fn sessions(setup: &support::Setup) -> std::path::PathBuf {
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    setup.home().join("projects").join(key).join("sessions")
}

/// Every event line of the one session in [`sessions`], in order.
fn session_events(setup: &support::Setup) -> Vec<Value> {
    let mut ids: Vec<String> = fs::read_dir(sessions(setup))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "one session");
    let log = sessions(setup)
        .join(ids.pop().unwrap())
        .join("events.jsonl");
    let text = fs::read_to_string(&log).unwrap();
    text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn bare_fiber_with_model_and_dash_c_starts_each_session_with_both_overrides() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([Response::status(503, "{}"), support::hello()]).unwrap();
    provider_without_model(&setup, &server);
    let mut run = terminal(
        &setup,
        120,
        32,
        &["--model", "fake/m", "-c", "retry.attempts=2"],
    );
    // The first frame draws the input line: with no model configured,
    // only the run's `--model` sets the chip, so a prompt starts a
    // session instead of opening the picker.
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    // The reply streams in; the turn's close says it finished. Attempts
    // 0 in the file would fail the first 503 at once, so a finished
    // turn proves the `-c` layer rode the `start`.
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
    run.turn_finished(from);
    // The turn just ended, so quitting either exits at once or asks
    // first; the Enter leaves the session running, and exiting prints
    // its resume line.
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
    // The `start` carried both overrides: the session ran on the run's
    // model, and retried the 503 under the run's retry layer.
    let events = session_events(&setup);
    assert!(
        events.iter().any(|line| line["kind"] == "usage_recorded"
            && line["payload"]["model"] == "fake/m"),
        "the session ran on the run's model: {events:?}"
    );
    let scheduled: Vec<&Value> = events
        .iter()
        .filter(|line| line["kind"] == "retry_scheduled")
        .collect();
    assert_eq!(scheduled.len(), 1, "{events:?}");
    assert_eq!(scheduled[0]["payload"]["last_attempt"], 3);
}

#[test]
fn model_and_dash_c_with_a_subcommand_stay_a_usage_error() {
    for args in [
        ["--model", "fake/m", "sessions"],
        ["-c", "retry.attempts=2", "sessions"],
    ] {
        let setup = support::Setup::new();
        let mut command = setup.fiber(&args);
        command.current_dir(setup.workspace());
        let output = support::run_to_exit(
            setup.deadline,
            &format!("`fiber {}`", args.join(" ")),
            command,
        );
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with("fiber: "), "{stderr}");
        assert!(
            stderr.ends_with("Run `fiber --help` for usage.\n"),
            "{stderr}"
        );
    }
}
