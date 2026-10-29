//! Binary-level test that a placeholder with no value stays silent at
//! launch while the model picker counts the left-out model
//! (`docs/model-routing.md`, "A per-account host"): the real binary on
//! a 120x32 pseudo-terminal raises no `model_unconfigured` notice for
//! the valueless placeholder, and the picker shows its one count row.
//!
//! The waits read the shared driver's grid, and every assertion reads
//! the run's grid after its wait returns: the grid's text and rows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::json;
use support::Setup;
use support::pty::{Grid, Run};

/// Whether any grid row holds `needle`.
fn shows(screen: &Grid, needle: &str) -> bool {
    screen.rows.iter().any(|row| row.contains(needle))
}

/// Installs extension `fake` registering provider `fake` with one model
/// whose `base_url` fills `{workspace}`, declared with no environment
/// variable and no setting, so the terminal's own read leaves the
/// model out; `fake/alpha` stays the configured model.
fn install_placeholder_provider(setup: &Setup) {
    let source = setup.root.path().join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": ["https://{workspace}/v1"]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "placeholders": {"workspace": {}},
            "models": [{"id": "alpha", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 100000}],
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
        &json!({"model": "fake/alpha", "hub": {"idle_exit_ms": 200}}),
    );
}

#[test]
fn a_placeholder_with_no_value_stays_silent_and_the_picker_counts_it() {
    let setup = Setup::new();
    install_placeholder_provider(&setup);
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
    // the background catalogue read answers.
    run.ready();
    // Ctrl+L opens the picker: the open wait names the picker frame,
    // so a stall says whether the picker never opened.
    run.write(b"\x0c");
    run.wait_screen("the open picker", |screen| shows(screen, "Type to search"));
    // The count row proves the grouped read answered with the `fake`
    // group: the same read that raises per-model notices at launch,
    // so a valueless placeholder had its chance to raise one.
    run.wait_screen("the picker count row", |screen| {
        shows(screen, "1 model needs workspace: run `fiber login fake`")
    });
    let counted = run.screen().rows.clone();
    assert!(
        counted
            .iter()
            .any(|row| row.contains("1 model needs workspace: run `fiber login fake`")),
        "the picker counts the left-out model: {counted:?}"
    );
    // Silence: the launch raised no notice for the valueless
    // placeholder, and the model is not invalid either.
    assert!(
        counted
            .iter()
            .all(|row| !row.contains("model_unconfigured")),
        "no launch notice for the valueless placeholder: {counted:?}"
    );
    assert!(
        counted
            .iter()
            .all(|row| !(row.contains("model_invalid") && row.contains("fake/alpha"))),
        "no model_invalid row carries it: {counted:?}"
    );
    // Esc closes the picker at once with no query typed, and the quit
    // goes out at home.
    run.write(b"\x1b[27u");
    run.wait_screen("home again", |screen| !shows(screen, "Type to search"));
    run.write(b"\x03\x03");
    run.wait_screen("the restored terminal", |screen| {
        !screen.alternate_screen && !screen.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
