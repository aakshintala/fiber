//! Binary-level `store: false` capture for opencode-go (`docs/testing.md`,
//! "Levels"): the built `fiber` runs in its own process group against the
//! fake opencode-go server, and the test asserts `"store": false` on the
//! `openai-responses` request it sends on the wire.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::Value;
use support::Setup;

#[test]
fn opencode_go_responses_request_sends_store_false() {
    let setup = Setup::new();
    let server = support::package::opencode_go_ask_server(vec![support::hello_inline_completed()]);
    let to = setup.root.path().join("pkg-opencode");
    support::package::copy_package("opencode", &to, "https://opencode.ai", &server.url());
    let installed = support::run_to_exit(
        setup.deadline,
        "`fiber extension install` to exit",
        setup.fiber(&["extension", "install", to.to_str().unwrap()]),
    );
    assert_eq!(
        installed.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&installed.stderr)
    );

    let mut ask = setup.fiber(&[
        "ask",
        "--model",
        "opencode-go/muse-spark-1.3-contributor",
        "hi",
    ]);
    ask.env("OPENCODE_API_KEY", "sk-test-opencode-go");
    ask.current_dir(setup.workspace());
    let run = support::run_to_exit(setup.deadline, "`fiber ask` to exit", ask);
    assert_eq!(
        run.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let requests = server.requests();
    // The background quota() fetch races the turn: it may or may not be
    // recorded yet, so only the turn's own requests are pinned here.
    let calls: Vec<_> = requests
        .iter()
        .filter(|request| request.path == "/zen/go/v1/responses")
        .collect();
    assert!(
        !calls.is_empty(),
        "no /zen/go/v1/responses request reached the fake"
    );
    for request in &calls {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["store"], Value::Bool(false));
    }
}
