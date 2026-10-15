use super::*;
use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use serde_json::json;

fn load(home: &Path, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: home.into(),
        workspace: home.join("workspace"),
        project: ProjectKey::new("test-project").unwrap(),
        overrides: overrides.iter().map(|s| s.to_string()).collect(),
    })
    .unwrap()
}

fn write(file: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, value.to_string()).unwrap();
}

fn listed_name(target: Target) -> String {
    let Target::Listed { name, .. } = target else {
        panic!("expected a listed hub")
    };
    name
}

#[test]
fn target_reads_each_loaded_config_without_caching() {
    let dir = fakes::TempDir::new("fiber-listed-target");
    let home = dir.path();
    assert!(!target(&load(home, &[]), home, None).unwrap().is_listed());
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"a"}, "hubs":{"a":{"address":"unix:/tmp/a"}, "b.x":{"address":"unix:/tmp/b"}}}),
    );
    let cfg = load(home, &[]);
    assert_eq!(listed_name(target(&cfg, home, None).unwrap()), "a");
    assert_eq!(listed_name(target(&cfg, home, Some("b.x")).unwrap()), "b.x");
    assert_eq!(
        listed_name(target(&load(home, &["hub.default=b.x"]), home, None).unwrap()),
        "b.x"
    );
    write(
        &home.join("projects/test-project/config.json"),
        json!({"hub":{"default":"b.x"}}),
    );
    assert_eq!(
        listed_name(target(&load(home, &[]), home, None).unwrap()),
        "b.x"
    );
    std::fs::remove_file(home.join("projects/test-project/config.json")).unwrap();
    write(
        &home.join("workspace/.fiber/config.json"),
        json!({"hub":{"default":"b.x"}}),
    );
    let cfg = load(home, &[]);
    assert!(!cfg.notices().is_empty());
    assert_eq!(listed_name(target(&cfg, home, None).unwrap()), "a");
    write(
        &home.join("config.json"),
        json!({"hub":{"default":"b.x"}, "hubs":{"b.x":{"address":"unix:/tmp/b"}}}),
    );
    assert_eq!(
        listed_name(target(&load(home, &[]), home, None).unwrap()),
        "b.x"
    );
}

#[test]
fn target_errors_do_not_fall_back_to_local() {
    let dir = fakes::TempDir::new("fiber-listed-errors");
    let home = dir.path();
    let error = target(&load(home, &[]), home, Some("nope")).unwrap_err();
    assert!(matches!(error, Error::UnknownHub { .. }));
    assert_eq!(error.code(), ErrorCode::Usage);
    assert_eq!(
        error.to_string(),
        "No hub named `nope` in this client's list; run `fiber hub add <name> <address>`. Run `fiber --help` for usage."
    );
    write(&home.join("config.json"), json!({"hub":{"default":"gone"}}));
    let error = target(&load(home, &[]), home, None).unwrap_err();
    assert!(matches!(error, Error::MissingDefault { .. }));
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("hub.default"));
    write(
        &home.join("config.json"),
        json!({"hubs":{"box":{"address":"http://bad"}}}),
    );
    let error = target(&load(home, &[]), home, Some("box")).unwrap_err();
    assert!(matches!(error, Error::StoredAddress { .. }));
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("hubs.\"box\".address"));
}

#[test]
fn websocket_target_needs_a_token_but_unix_never_reads_one() {
    let dir = fakes::TempDir::new("fiber-listed-token");
    let home = dir.path();
    write(
        &home.join("config.json"),
        json!({"hubs":{"box":{"address":"ws://box"}, "fwd":{"address":"unix:/tmp/fwd"}}}),
    );
    let error = target(&load(home, &[]), home, Some("box")).unwrap_err();
    assert!(matches!(error, Error::MissingToken { .. }));
    assert_eq!(error.code(), ErrorCode::CredentialMissing);
    assert!(error.to_string().contains("fiber hub remove box"));
    assert!(error.to_string().contains("fiber hub add box"));
    config::store_credential(
        home,
        "hubs",
        "box",
        &contract::Secret::new("device-token".into()),
    )
    .unwrap();
    let Target::Listed {
        token: Some(token),
        address: Address::Web(_),
        ..
    } = target(&load(home, &[]), home, Some("box")).unwrap()
    else {
        panic!("expected paired websocket")
    };
    assert_eq!(token.expose(), "device-token");
    config::store_credential(
        home,
        "hubs",
        "fwd",
        &contract::Secret::new("ignored-token".into()),
    )
    .unwrap();
    let Target::Listed {
        token,
        address: Address::Unix(_),
        ..
    } = target(&load(home, &[]), home, Some("fwd")).unwrap()
    else {
        panic!("expected forwarded socket")
    };
    assert!(token.is_none());
}
