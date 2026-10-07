//! `docs/configuration.md`, "A provider's data", and `docs/state.md`,
//! "Cache": a `models()` list is cached at `cache/models/<name>.json`,
//! replaced whole, and a copy that does not read is no copy.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

mod common;

use common::Setup;
use config::{read_model_cache, write_model_cache};
use contract::clock::Clock;
use serde_json::json;

#[test]
fn a_written_list_is_read_back_from_its_file() {
    let setup = Setup::new();
    let home = setup.home();
    assert_eq!(read_model_cache(&home, "acme").unwrap(), None);
    let list = json!([{ "id": "m1", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000 }]);
    write_model_cache(&home, "acme", &list).unwrap();
    let file = home.join("cache/models/acme.json");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&file).unwrap()).unwrap(),
        list
    );
    let models = read_model_cache(&home, "acme").unwrap().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "m1");

    write_model_cache(&home, "acme", &json!([])).unwrap();
    assert_eq!(read_model_cache(&home, "acme").unwrap(), Some(Vec::new()));
}

#[test]
fn a_copy_that_is_not_a_model_list_is_no_copy() {
    let setup = Setup::new();
    let home = setup.home();
    let file = home.join("cache/models/acme.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "{ not json").unwrap();
    assert_eq!(read_model_cache(&home, "acme").unwrap(), None);
    std::fs::write(&file, r#"[{ "id": "m1" }]"#).unwrap();
    assert_eq!(read_model_cache(&home, "acme").unwrap(), None);
}

#[test]
fn a_name_that_is_not_one_file_name_is_refused() {
    let setup = Setup::new();
    let home = setup.home();
    for name in ["../x", "", "a/b"] {
        assert!(read_model_cache(&home, name).is_err(), "{name}");
        assert!(
            write_model_cache(&home, name, &json!([])).is_err(),
            "{name}"
        );
    }
}

#[test]
fn a_cache_that_cannot_be_read_is_io_failed() {
    let setup = Setup::new();
    let home = setup.home();
    std::fs::create_dir_all(home.join("cache/models/acme.json")).unwrap();
    let err = read_model_cache(&home, "acme").unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::IoFailed);
}

/// Sets the cache file's mtime, so its age reads against the fake clock.
fn set_mtime(home: &std::path::Path, mtime: std::time::SystemTime) {
    let file = home.join("cache/models/acme.json");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

#[test]
fn a_missing_list_has_no_age() {
    let setup = Setup::new();
    let home = setup.home();
    let now = fakes::clock::FakeClock::new().wall();
    assert_eq!(config::model_cache_age(&home, "acme", now).unwrap(), None);
}

#[test]
fn a_lists_age_is_now_minus_its_mtime() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    let list = json!([{ "id": "m1", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000 }]);
    write_model_cache(&home, "acme", &list).unwrap();
    set_mtime(&home, clock.wall() - std::time::Duration::from_secs(90));
    assert_eq!(
        config::model_cache_age(&home, "acme", clock.wall()).unwrap(),
        Some(std::time::Duration::from_secs(90))
    );
}

#[test]
fn a_list_newer_than_the_clock_is_fresh() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = fakes::clock::FakeClock::new();
    let list = json!([{ "id": "m1", "protocol": "openai-responses", "base_url": "http://x/v1", "context_window": 1000 }]);
    write_model_cache(&home, "acme", &list).unwrap();
    set_mtime(&home, clock.wall() + std::time::Duration::from_secs(60));
    assert_eq!(
        config::model_cache_age(&home, "acme", clock.wall()).unwrap(),
        Some(std::time::Duration::ZERO)
    );
}
