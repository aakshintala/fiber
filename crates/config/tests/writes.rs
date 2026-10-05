//! `docs/configuration.md`, "When Fiber writes" and "Extension settings", and
//! `docs/state.md`, "Concurrent access": a write takes the lock, changes one
//! key, keeps the rest, and renames a whole file into place.

mod common;

use std::fs;
use std::sync::Arc;
use std::thread;

use common::{PROJECT, Setup};
use config::{Scope, remove_extension_settings, set_global};
use contract::ErrorCode;
use serde_json::json;

const ACME: &str = "github.com/acme/fiber-acme";

#[test]
fn a_write_creates_the_global_file_sorted_with_a_two_space_indent() {
    let setup = Setup::new();
    set_global(&setup.home(), "model", json!("openai/gpt-5.6")).unwrap();
    set_global(&setup.home(), "handoff.tokens", json!(200000)).unwrap();
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"handoff\": {\n    \"tokens\": 200000\n  },\n  \"model\": \"openai/gpt-5.6\"\n}\n"
    );
}

#[test]
fn a_write_keeps_every_key_it_does_not_touch() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"zeta": [3, 1], "handoff": {"nudge": false}, "alpha": {"keep": "me"}}"#,
    );
    set_global(&setup.home(), "handoff.tokens", json!(5)).unwrap();
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        concat!(
            "{\n",
            "  \"alpha\": {\n    \"keep\": \"me\"\n  },\n",
            "  \"handoff\": {\n    \"nudge\": false,\n    \"tokens\": 5\n  },\n",
            "  \"zeta\": [\n    3,\n    1\n  ]\n",
            "}\n"
        )
    );
}

#[test]
fn a_written_value_is_what_the_next_read_sees() {
    let setup = Setup::new();
    set_global(
        &setup.home(),
        "models.\"openai/gpt-5.6\".cache.lifetime",
        json!("5m"),
    )
    .unwrap();
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config
            .get("cache.lifetime", Some("openai/gpt-5.6"))
            .unwrap()
            .0,
        json!("5m")
    );
}

#[test]
fn a_write_of_the_wrong_type_is_refused_and_changes_nothing() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"model": "a/b"}"#);
    let e = set_global(&setup.home(), "handoff.tokens", json!("many")).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        e.to_string(),
        format!(
            "{}: `handoff.tokens` must be a whole number of zero or more.",
            setup.global().display()
        )
    );
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        r#"{"model": "a/b"}"#
    );
}

#[test]
fn a_write_checks_only_the_type_so_an_unknown_key_is_written() {
    let setup = Setup::new();
    set_global(&setup.home(), "future.key", json!(1)).unwrap();
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"future\": {\n    \"key\": 1\n  }\n}\n"
    );
}

#[test]
fn a_write_to_a_file_that_is_not_json_is_refused_and_leaves_it() {
    let setup = Setup::new();
    setup.write(&setup.global(), "{\"model\": \"a/b\",}");
    let e = set_global(&setup.home(), "model", json!("c/d")).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\"model\": \"a/b\",}"
    );
}

#[test]
fn a_write_to_a_key_that_is_not_dotted_is_a_usage_error() {
    let setup = Setup::new();
    assert_eq!(
        set_global(&setup.home(), "a..b", json!(1))
            .unwrap_err()
            .code(),
        ErrorCode::Usage
    );
    let mut config = setup.load(&[]).unwrap();
    assert_eq!(
        config
            .set_extension_setting(ACME, Scope::Machine, "", json!(1))
            .unwrap_err()
            .code(),
        ErrorCode::Usage
    );
    assert!(!setup.global().exists());
}

#[test]
fn a_write_that_cannot_rename_into_place_fails_and_leaves_no_temporary_file() {
    let setup = Setup::new();
    fs::create_dir_all(setup.global().join("occupied")).unwrap();
    let e = set_global(&setup.home(), "model", json!("a/b")).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
    let mut left: Vec<_> = fs::read_dir(setup.home())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["config.json", "config.json.lock"]);
}

#[test]
fn concurrent_writers_lose_no_key() {
    let setup = Arc::new(Setup::new());
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let setup = Arc::clone(&setup);
            thread::spawn(move || {
                for n in 0..20 {
                    set_global(&setup.home(), &format!("roles.w{w}n{n}"), json!("a/b")).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let config = setup.load(&[]).unwrap();
    let roles = config.get("roles", None).unwrap().0;
    assert_eq!(roles.as_object().unwrap().len(), 160);
    let mut left: Vec<_> = fs::read_dir(setup.home())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["config.json", "config.json.lock"]);
}

#[test]
fn an_extension_setting_is_written_to_the_scope_it_names() {
    let setup = Setup::new();
    let mut config = setup.load(&[]).unwrap();
    config
        .set_extension_setting(ACME, Scope::Machine, "region", json!("eu"))
        .unwrap();
    config
        .set_extension_setting(ACME, Scope::Project, "model", json!("m"))
        .unwrap();
    assert_eq!(
        fs::read_to_string(setup.home().join("config/github.com-acme-fiber-acme.json")).unwrap(),
        "{\n  \"region\": \"eu\"\n}\n"
    );
    assert_eq!(
        fs::read_to_string(
            setup
                .home()
                .join("projects")
                .join(PROJECT)
                .join("config/github.com-acme-fiber-acme.json")
        )
        .unwrap(),
        "{\n  \"model\": \"m\"\n}\n"
    );
    let expected = json!({"model": "m", "region": "eu"});
    assert_eq!(config.extension_settings(ACME, &[]).unwrap().0, expected);
    let reloaded = setup.load(&[]).unwrap();
    assert_eq!(reloaded.extension_settings(ACME, &[]).unwrap().0, expected);
    assert!(
        !setup.global().exists(),
        "an extension never rewrites config.json"
    );
}

#[test]
fn another_session_sees_a_setting_only_at_its_next_reload() {
    let setup = Setup::new();
    let mut writer = setup.load(&[]).unwrap();
    let other = setup.load(&[]).unwrap();
    writer
        .set_extension_setting(ACME, Scope::Machine, "region", json!("eu"))
        .unwrap();
    assert_eq!(other.extension_settings(ACME, &[]).unwrap().0, json!({}));
    let reloaded = setup.load(&[]).unwrap();
    assert_eq!(
        reloaded.extension_settings(ACME, &[]).unwrap().0,
        json!({"region": "eu"})
    );
}

#[test]
fn a_setting_written_on_disk_after_loading_is_not_seen_until_reload() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    setup.write(
        &setup.home().join("config/github.com-acme-fiber-acme.json"),
        r#"{"a": 1}"#,
    );
    assert_eq!(config.extension_settings(ACME, &[]).unwrap().0, json!({}));
}

#[test]
fn a_write_keeps_the_extension_s_other_settings_in_the_session() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join("config/github.com-acme-fiber-acme.json"),
        r#"{"a": 1}"#,
    );
    let mut config = setup.load(&[]).unwrap();
    config
        .set_extension_setting(ACME, Scope::Machine, "b", json!(2))
        .unwrap();
    assert_eq!(
        config.extension_settings(ACME, &[]).unwrap().0,
        json!({"a": 1, "b": 2})
    );
}

#[test]
fn removing_an_extension_deletes_its_global_and_every_per_project_file() {
    let setup = Setup::new();
    let file = "github.com-acme-fiber-acme.json";
    for project in ["p1", "p2"] {
        let dir = setup.home().join("projects").join(project).join("config");
        setup.write(&dir.join(file), "{}");
        setup.write(&dir.join("other.json"), "{}");
    }
    fs::create_dir_all(setup.home().join("projects/p3")).unwrap();
    setup.write(&setup.home().join("config").join(file), "{}");
    remove_extension_settings(&setup.home(), ACME).unwrap();
    assert!(!setup.home().join("config").join(file).exists());
    for project in ["p1", "p2"] {
        let dir = setup.home().join("projects").join(project).join("config");
        assert!(!dir.join(file).exists());
        assert!(dir.join("other.json").exists());
    }
}

#[test]
fn removing_an_extension_from_an_empty_home_does_nothing() {
    let setup = Setup::new();
    remove_extension_settings(&setup.home(), ACME).unwrap();
    assert_eq!(fs::read_dir(setup.home()).unwrap().count(), 0);
}

#[test]
fn removing_an_extension_whose_file_cannot_be_deleted_fails() {
    let setup = Setup::new();
    fs::create_dir_all(
        setup
            .home()
            .join("config/github.com-acme-fiber-acme.json/x"),
    )
    .unwrap();
    let e = remove_extension_settings(&setup.home(), ACME).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
}

#[test]
fn removing_an_extension_when_projects_is_not_a_directory_fails() {
    let setup = Setup::new();
    setup.write(&setup.home().join("projects"), "");
    let e = remove_extension_settings(&setup.home(), ACME).unwrap_err();
    assert_eq!(e.code(), ErrorCode::IoFailed);
    assert!(e.to_string().contains("projects"));
}

#[test]
fn a_write_of_an_object_key_checks_the_object_and_what_it_holds() {
    let setup = Setup::new();
    for (key, value) in [
        ("handoff", json!(5)),
        ("handoff", json!({"tokens": "many"})),
        ("mcp", json!({"servers": {"gh": {"required": "yes"}}})),
        (
            "providers.x",
            json!({"credentials": {"work": {"command": []}}}),
        ),
    ] {
        let e = set_global(&setup.home(), key, value).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{key}");
        assert!(!setup.global().exists(), "{key}");
    }
    set_global(&setup.home(), "handoff", json!({"tokens": 5})).unwrap();
    assert_eq!(
        setup
            .load(&[])
            .unwrap()
            .get("handoff.tokens", None)
            .unwrap()
            .0,
        json!(5)
    );
}

#[test]
fn a_session_s_own_write_does_not_bring_in_another_session_s() {
    let setup = Setup::new();
    let mut a = setup.load(&[]).unwrap();
    let mut b = setup.load(&[]).unwrap();
    b.set_extension_setting(ACME, Scope::Machine, "region", json!("eu"))
        .unwrap();
    a.set_extension_setting(ACME, Scope::Machine, "model", json!("m"))
        .unwrap();
    assert_eq!(
        a.extension_settings(ACME, &[]).unwrap().0,
        json!({"model": "m"})
    );
    assert_eq!(
        setup
            .load(&[])
            .unwrap()
            .extension_settings(ACME, &[])
            .unwrap()
            .0,
        json!({"model": "m", "region": "eu"})
    );
}

#[test]
fn a_write_over_settings_this_session_read_as_invalid_fails_before_touching_disk() {
    let setup = Setup::new();
    let file = setup.home().join("config/github.com-acme-fiber-acme.json");
    setup.write(&file, "{\"a\": 1,}");
    let mut config = setup.load(&[]).unwrap();
    setup.write(&file, r#"{"a": 1}"#);
    let e = config
        .set_extension_setting(ACME, Scope::Machine, "b", json!(2))
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(fs::read_to_string(&file).unwrap(), r#"{"a": 1}"#);
    let mut reloaded = setup.load(&[]).unwrap();
    reloaded
        .set_extension_setting(ACME, Scope::Machine, "b", json!(2))
        .unwrap();
    assert_eq!(
        reloaded.extension_settings(ACME, &[]).unwrap().0,
        json!({"a": 1, "b": 2})
    );
}
