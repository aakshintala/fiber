//! `parts_in` over a scripted session (`docs/model-routing.md`, "The
//! scripted provider"): startup, resume and reviewer resolution through
//! the session parts, which need `loop` types and stay in `main`.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::sync::Arc;

use contract::ErrorCode;
use contract::clock::Clock;
use serde_json::json;

/// `parts_in` over an empty home and a workspace holding `files`, with
/// `config.json` holding `global`.
fn parts(
    global: &serde_json::Value,
    files: &[(&str, &str)],
    recorded: Option<&str>,
    labels: crate::credential::Labels<'_>,
) -> Result<crate::Parts, contract::shapes::Failure> {
    let root = fakes::TempDir::new("fiber-scripted-parts");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(home.join("config.json"), global.to_string()).unwrap();
    for (name, text) in files {
        std::fs::write(workspace.join(name), text).unwrap();
    }
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let reads = Arc::new(crate::switch::Reads::default());
    crate::parts_in(
        home,
        workspace,
        Vec::new(),
        recorded,
        labels,
        None,
        clock,
        None,
        None,
        reads,
        log::diag::Process::Session,
        extensions::StateSeed::new(),
        crate::session_extensions::RepositoryScope::default(),
    )
}

const ONE_STEP: &str = r#"{"steps": [{"text": "Hi."}]}"#;

#[test]
fn a_scripted_session_starts_with_no_credential_and_no_warming() {
    let parts = parts(
        &json!({"model": "scripted/s.json", "cache": {"warm_idle": true}}),
        &[("s.json", ONE_STEP)],
        None,
        crate::credential::Labels::default(),
    )
    .unwrap();
    assert_eq!(parts.model.reference, "scripted/s.json");
    assert!(parts.model.cost.is_none());
    assert!(!parts.model.subscription);
    assert_eq!(parts.warm, None);
    // No `reviewer.model`, and the scripted provider names no reviewer.
    match &parts.reviewer {
        Err(failure) => assert_eq!(failure.code, ErrorCode::NoModel),
        Ok(reviewer) => panic!("a reviewer resolved: {}", reviewer.model.reference),
    }
}

#[test]
fn a_resumed_scripted_session_starts_from_its_recorded_model() {
    let parts = parts(
        &json!({"model": "scripted/other.json"}),
        &[("s.json", ONE_STEP)],
        Some("scripted/s.json"),
        crate::credential::Labels::default(),
    )
    .unwrap();
    assert_eq!(parts.model.reference, "scripted/s.json");
}

#[test]
fn a_scripted_reviewer_resolves_to_its_own_script() {
    let parts = parts(
        &json!({"model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"}}),
        &[("s.json", ONE_STEP), ("r.json", ONE_STEP)],
        None,
        crate::credential::Labels::default(),
    )
    .unwrap();
    match &parts.reviewer {
        Ok(reviewer) => assert_eq!(reviewer.model.reference, "scripted/r.json"),
        Err(failure) => panic!("no reviewer: {}", failure.message),
    }
}

#[test]
fn a_missing_script_fails_io_failed_and_a_malformed_one_config_invalid() {
    let missing = parts(
        &json!({"model": "scripted/s.json"}),
        &[],
        None,
        crate::credential::Labels::default(),
    )
    .err()
    .unwrap();
    assert_eq!(missing.code, ErrorCode::IoFailed);
    assert!(missing.message.contains("s.json"), "{}", missing.message);
    let malformed = parts(
        &json!({"model": "scripted/s.json"}),
        &[("s.json", r#"{"steps": [{"text": "a"}, {"colour": 1}]}"#)],
        None,
        crate::credential::Labels::default(),
    )
    .err()
    .unwrap();
    assert_eq!(malformed.code, ErrorCode::ConfigInvalid);
    assert!(
        malformed.message.contains("s.json"),
        "{}",
        malformed.message
    );
    assert!(
        malformed.message.contains("step 2"),
        "{}",
        malformed.message
    );
}

#[test]
fn parts_in_starts_a_scripted_session_with_no_label() {
    let started = parts(
        &json!({"model": "scripted/s.json"}),
        &[("s.json", ONE_STEP)],
        None,
        crate::credential::Labels::new(None, Some("default")),
    )
    .unwrap();
    assert_eq!(started.prompt.credential, None);
}

#[test]
fn parts_in_rejects_an_asked_label_on_a_scripted_session() {
    let failure = parts(
        &json!({"model": "scripted/s.json"}),
        &[("s.json", ONE_STEP)],
        None,
        crate::credential::Labels::new(Some("x"), None),
    )
    .err()
    .unwrap();
    assert_eq!(failure.code, ErrorCode::CredentialMissing);
    assert!(
        failure.message.ends_with("are: none"),
        "{}",
        failure.message
    );
}
