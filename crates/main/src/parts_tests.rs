use std::sync::{Arc, Mutex};
use std::time::Duration;

use fakes::Deadline;
use serde_json::json;

use super::start_catalog_refresh;

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

    let handle = start_catalog_refresh(home.clone(), &config, std::time::SystemTime::now(), fetch)
        .expect("a missing catalog is due at session start");
    fakes::within(
        "session-start catalog refresh",
        Deadline::after(Duration::from_secs(10)),
        move || handle.join().expect("refresh thread exits"),
    );

    assert_eq!(*calls.lock().unwrap(), [extensions::models_dev::URL]);
    assert_eq!(
        config::read_models_dev(&home).unwrap(),
        Some(serde_json::to_vec(&json!({"acme": {"m": {"name": "live"}}})).unwrap())
    );
}
