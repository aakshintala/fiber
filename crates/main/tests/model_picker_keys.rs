//! Binary-level tests of the model picker's keys (`docs/tui.md`, "Swapped
//! views"): the real binary on a 120x32 pseudo-terminal opens the picker,
//! filters as it types, and Ctrl+S chooses for this session only, saving
//! nothing to the global config (`docs/testing.md`, "Levels").
//!
//! The waits read the shared driver's grid, and every assertion reads the
//! run's grid after its wait returns: the grid's text and rows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::Setup;
use support::pty::{Grid, Run};

/// Installs a provider `fake` with the models `alpha`, `beta` and `gamma`
/// on `openai-responses` at the fake server, reading its key from
/// `FIBER_TEST_FAKE_KEY`, and makes `fake/alpha` the configured model.
fn install_picker_provider(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    let models: Vec<Value> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|id| {
            json!({"id": id, "protocol": "openai-responses",
                   "base_url": format!("{}/v1", server.url()),
                   "context_window": 100000})
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
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha", "hub": {"idle_exit_ms": 1000}}),
    );
}

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

#[test]
fn ctrl_s_chooses_the_filtered_model_for_this_session_only() {
    let setup = Setup::new();
    let server = ProviderServer::start([support::hello()]).unwrap();
    install_picker_provider(&setup, &server);
    let mut run = Run::spawn(
        &setup,
        120,
        32,
        &[],
        &[
            ("TERM", "xterm-256color"),
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
        ],
    );
    // The end of the first frame proves the input reader runs before
    // the prompt goes out; the reply stays drawn, so the grid check
    // before the finished title is order-free.
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    let saved = fs::read(setup.home().join("config.json")).unwrap();
    // Ctrl+L opens the picker: the wait names the open, drawn catalogue.
    // The open picker draws from the terminal's own view once the turn
    // closed, so the finished turn above proves the key is taken.
    run.write(b"\x0c");
    run.wait_screen("the open picker with its catalogue", |screen| {
        shows(screen, "Type to search") && shows(screen, "gamma")
    });
    let opened = run.screen().rows.clone();
    assert!(
        opened.iter().any(|row| row.contains("Type to search")),
        "the picker shows its filter: {opened:?}"
    );
    // Typing narrows the list to `beta`: the count names one row of the
    // three installed models, and `gamma` drops out. The open picker's
    // catalogue above proves the filter is taken.
    run.write(b"bet");
    run.wait_screen("the filtered list", |screen| {
        shows(screen, "1 of 3 models") && !shows(screen, "gamma")
    });
    let filtered = run.screen().rows.clone();
    assert!(
        filtered.iter().any(|row| row.contains("1 of 3 models")),
        "the count names one of three: {filtered:?}"
    );
    assert!(
        filtered.iter().all(|row| !row.contains("gamma")),
        "gamma dropped out: {filtered:?}"
    );
    // Ctrl+S chooses at once for this session only: the picker closes and
    // the panel names the session's new model. The filtered list above
    // proves the choice is taken.
    run.write(b"\x13");
    run.wait_screen("the session on fake/beta", |screen| {
        shows(screen, "fake/beta") && !shows(screen, "Type to search")
    });
    let switched = run.screen().rows.clone();
    assert!(
        switched.iter().any(|row| row.contains("model  fake/beta")),
        "the panel names the session model: {switched:?}"
    );
    // The session's log holds the switch, from the driver.
    let sessions = log::sessions_dir(&setup.home(), &doors::project(&setup.workspace()));
    let dirs: Vec<_> = fs::read_dir(&sessions)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(dirs.len(), 1, "one session ran: {dirs:?}");
    let log = fs::read_to_string(dirs[0].join("events.jsonl")).unwrap();
    let changed: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["kind"] == "model_changed")
        .collect();
    assert_eq!(changed.len(), 1, "one switch: {changed:?}");
    assert_eq!(changed[0]["payload"]["after"]["model"], "fake/beta");
    assert_eq!(changed[0]["payload"]["source"], "driver");
    // Nothing reached the global config: byte for byte, and no `models`.
    assert_eq!(fs::read(setup.home().join("config.json")).unwrap(), saved);
    let config: Value =
        serde_json::from_slice(&fs::read(setup.home().join("config.json")).unwrap()).unwrap();
    assert!(config.get("models").is_none(), "no level saved: {config}");
    // Quitting is taken in any state; the session-on-beta grid above
    // proves the terminal drew before the quit goes out.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
