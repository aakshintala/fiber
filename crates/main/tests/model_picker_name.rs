//! Binary-level test that the model picker matches on the display
//! name (`docs/tui.md`, "Swapped views"): the real binary on a 120x32
//! pseudo-terminal lists both installed models, then a query only the
//! name carries narrows the list to the named model
//! (`docs/testing.md`, "Levels").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::ProviderServer;
use serde_json::json;
use support::Setup;
use support::pty::{Grid, Run};

/// Installs a provider `fake` with models `alpha-1` (named
/// "Claude Opus 5.5") and `beta-2` (no name) on `openai-responses` at
/// the fake server, reading its key from `FIBER_TEST_FAKE_KEY`, and
/// makes `fake/alpha-1` the configured model. Neither the provider
/// nor either id contains "opus": only the display name can carry the
/// picker's "opus 5.5" query.
fn install_picker_provider(setup: &Setup, server: &ProviderServer) {
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
            "models": [
                {"id": "alpha-1", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000, "name": "Claude Opus 5.5"},
                {"id": "beta-2", "protocol": "openai-responses",
                 "base_url": format!("{}/v1", server.url()),
                 "context_window": 100000},
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
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/alpha-1", "hub": {"idle_exit_ms": 200}}),
    );
}

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

#[test]
fn the_picker_matches_a_model_through_its_display_name() {
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
    // Ctrl+L opens the picker: the wait names the open, drawn
    // catalogue. The open picker's catalogue proves the key is taken.
    // Both models list before any filter: the negative control.
    run.write(b"\x0c");
    let opened_grid = run.wait_screen("the open picker with its catalogue", |screen| {
        shows(screen, "Type to search") && shows(screen, "alpha-1") && shows(screen, "beta-2")
    });
    let opened = opened_grid.rows.clone();
    assert!(
        opened.iter().any(|row| row.contains("Type to search")),
        "the picker shows its filter: {opened:?}"
    );
    assert!(
        opened.iter().any(|row| row.contains("alpha-1")),
        "alpha-1 lists before the filter: {opened:?}"
    );
    assert!(
        opened.iter().any(|row| row.contains("beta-2")),
        "beta-2 lists before the filter: {opened:?}"
    );
    // Typing what only the display name carries narrows the list to
    // `alpha-1`: the count names one row of the two installed models,
    // and `beta-2` drops out. The open picker's catalogue above proves
    // the filter is taken.
    run.write(b"opus 5.5");
    let filtered_grid = run.wait_screen("the name-filtered list", |screen| {
        shows(screen, "1 of 2 models") && shows(screen, "alpha-1") && !shows(screen, "beta-2")
    });
    let filtered = filtered_grid.rows.clone();
    assert!(
        filtered.iter().any(|row| row.contains("1 of 2 models")),
        "the count names one of two: {filtered:?}"
    );
    assert!(
        filtered.iter().any(|row| row.contains("alpha-1")),
        "the named model stays: {filtered:?}"
    );
    assert!(
        filtered.iter().all(|row| !row.contains("beta-2")),
        "beta-2 dropped out: {filtered:?}"
    );
    // Quitting is taken in any state; the filtered grid above proves
    // the terminal drew before the quit goes out.
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
