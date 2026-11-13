use contract::clock::Clock;
use fakes::Deadline;
use serde_json::json;
use std::sync::{Arc, Mutex};

use super::{parts_in, start_catalog_refresh};
use crate::credential::Labels;
use crate::session_extensions::RepositoryScope;
use crate::test_support::install_extension;
use contract::ErrorCode;

#[test]
fn a_session_can_start_by_deprecated_id_and_gets_one_named_notice() {
    let temp = fakes::TempDir::new("fiber-session-deprecated-model");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let models = json!([
        {"id":"retired", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000},
        {"id":"other", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000}
    ]);
    let mut data = json!({"name":"acme", "models":models});
    data["models_dev"] = json!({"source":"acme"});
    install_extension(
        &home,
        "extensions/acme-ext",
        json!({"name":"acme-ext", "version":"v1.0.0", "fiber":"0.1.0", "api":extensions::API}),
        &[("acme", data)],
    );
    config::store_credential(
        &home,
        "acme",
        "default",
        &config::Secret::new("test".into()),
    )
    .unwrap();
    std::fs::write(
        home.join("config.json"),
        json!({"model":"acme/retired"}).to_string(),
    )
    .unwrap();
    config::write_models_dev(&home, br#"{"acme":{"retired":{"status":"deprecated"}}}"#).unwrap();

    let parts = parts_in(
        home,
        workspace,
        Vec::new(),
        None,
        Labels::default(),
        None,
        fakes::clock::FakeClock::new(),
        None,
        None,
        Arc::new(crate::switch::Reads::default()),
        log::diag::Process::Session,
        extensions::StateSeed::new(),
        RepositoryScope::default(),
    )
    .unwrap();

    assert_eq!(parts.model.reference, "acme/retired");
    let notices = parts
        .mcp
        .notices
        .iter()
        .filter(|notice| notice.code == ErrorCode::ModelDeprecated)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].message.contains("acme/retired"));
}

#[test]
fn session_start_spawns_a_due_catalog_fetch() {
    let temp = fakes::TempDir::new("fiber-session-models-dev");
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let (_, project) = cli::project_of(&home, &workspace).unwrap();
    let config = config::Config::load(config::Sources {
        home: home.clone(),
        workspace,
        project,
        overrides: Vec::new(),
    })
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let bytes = serde_json::to_vec(&json!({"acme": {"m": {"name": "live"}}})).unwrap();
    let fetch: extensions::models_dev::Fetch = Arc::new(move |url| {
        recorded.lock().unwrap().push(url.to_owned());
        Ok(bytes.clone())
    });

    let handle = start_catalog_refresh(
        home.clone(),
        &config,
        fakes::clock::FakeClock::new().wall(),
        fetch,
    )
    .expect("a missing catalog is due at session start");
    fakes::within(
        "session-start catalog refresh",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || handle.join().expect("refresh thread exits"),
    );

    assert_eq!(*calls.lock().unwrap(), [extensions::models_dev::URL]);
    assert_eq!(
        config::read_models_dev(&home).unwrap(),
        Some(serde_json::to_vec(&json!({"acme": {"m": {"name": "live"}}})).unwrap())
    );
}
