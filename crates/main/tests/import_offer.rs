//! Binary-level test of the first-run import offer: a fresh home with a
//! fake `~/.claude` shows the offer on home with no session, pressing
//! `1` imports Claude Code through the hub, and the receipt lands.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::json;
use support::pty::Run;
use support::{Setup, TRUECOLOUR, write_json};

/// A fake `~/.claude` under the run's `HOME`: one skill and one
/// instruction, nothing secret.
fn claude_person(setup: &Setup) {
    for (rel, content) in [
        (
            ".claude/skills/review/SKILL.md",
            "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
        ),
        (".claude/CLAUDE.md", "Be kind.\n"),
    ] {
        let path = setup.root.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

#[test]
fn the_offer_imports_the_pressed_source_and_its_receipt_closes_it() {
    let setup = Setup::new();
    claude_person(&setup);
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    // The hub idles out after the quit, so the run reaps it: without
    // this a graceful quit still leaves `hub serve` behind.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn_unseeded(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.wait_screen("the first-run import offer", |grid| {
        grid.rows.iter().any(|row| row.contains("Import settings"))
    });
    // The key goes out with no session: pressing again only repeats
    // the send until the hub answers, so polling the receipt is the
    // wait, under the run's deadline.
    let receipt = setup.home().join("import-claude-code.json");
    loop {
        if receipt.is_file() {
            break;
        }
        assert!(!setup.deadline.left().is_zero(), "the import receipt lands");
        run.write(b"1");
        // One press per terminal reaction: the terminal folds the
        // answer between chunks, so retries never flood its input.
        let _ = run.wait_chunk(std::time::Duration::from_secs(1));
    }
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&receipt).unwrap()).unwrap();
    assert_eq!(report["source"], "claude-code");
    assert_eq!(report["imported"]["skills"], json!(["review"]));
    assert!(setup.home().join("skills/review/SKILL.md").is_file());
    assert!(
        !setup.home().join("import-codex.json").is_file(),
        "only the pressed source ran"
    );
    run.wait_screen("the answered offer closes", |grid| {
        grid.rows.iter().all(|row| !row.contains("Import settings"))
    });
    // Quitting gracefully lets the hub exit with the terminal: a
    // dropped run kills only the terminal, orphaning `hub serve`.
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
