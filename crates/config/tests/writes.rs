//! `docs/configuration.md`, "When Fiber writes" and "Extension settings", and
//! `docs/state.md`, "Concurrent access": a write takes the lock, changes one
//! key, keeps the rest, and renames a whole file into place.

mod common;

use std::fs;
use std::sync::Arc;
use std::thread;

use common::{PROJECT, Setup, key};
use config::{Layer, Scope, remove_extension_settings, set, set_global, set_global_if_unset};
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

#[test]
fn an_unset_key_is_written_once_and_a_set_key_is_left_alone() {
    let setup = Setup::new();
    let key = "providers.acme.credential";
    assert!(set_global_if_unset(&setup.home(), key, json!("first")).unwrap());
    assert!(!set_global_if_unset(&setup.home(), key, json!("second")).unwrap());
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.get(key, None).unwrap().0, json!("first"));
}

#[test]
fn a_write_if_unset_keeps_the_keys_around_it_and_names_a_dotted_provider() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"model": "a/b"}"#);
    let key = r#"providers."acme.api".credential"#;
    assert!(set_global_if_unset(&setup.home(), key, json!("default")).unwrap());
    let text = fs::read_to_string(setup.global()).unwrap();
    let written: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        written,
        json!({"model": "a/b", "providers": {"acme.api": {"credential": "default"}}})
    );
}

#[test]
fn a_project_layer_value_does_not_stop_the_global_write_if_unset() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"providers": {"acme": {"credential": "work"}}}"#,
    );
    assert!(
        set_global_if_unset(&setup.home(), "providers.acme.credential", json!("default")).unwrap()
    );
    let global: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.global()).unwrap()).unwrap();
    assert_eq!(global["providers"]["acme"]["credential"], "default");
}

#[test]
fn a_write_if_unset_of_the_wrong_type_or_a_bad_key_changes_nothing() {
    let setup = Setup::new();
    let e = set_global_if_unset(&setup.home(), "model", json!(3)).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    let e = set_global_if_unset(&setup.home(), "a..b", json!(1)).unwrap_err();
    assert_eq!(e.code(), ErrorCode::Usage);
    assert!(!setup.global().exists());
}

#[test]
fn concurrent_writes_if_unset_let_exactly_one_win() {
    let setup = Arc::new(Setup::new());
    let wins: usize = (0..8)
        .map(|n| {
            let setup = Arc::clone(&setup);
            thread::spawn(move || {
                set_global_if_unset(
                    &setup.home(),
                    "providers.acme.credential",
                    json!(n.to_string()),
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum();
    assert_eq!(wins, 1);
}

#[test]
fn set_writes_each_layer_s_file_sorted_and_keeps_other_keys() {
    let setup = Setup::new();
    setup.write(&setup.global(), r#"{"model": "a/b"}"#);
    setup.write(&setup.project(), r#"{"model": "a/b"}"#);
    setup.write(&setup.repository(), r#"{"model": "a/b"}"#);
    set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Global,
        "handoff.tokens",
        json!(200000),
    )
    .unwrap();
    set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        "handoff.tokens",
        json!(100),
    )
    .unwrap();
    set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        "handoff.tokens",
        json!(50),
    )
    .unwrap();
    for file in [setup.global(), setup.project(), setup.repository()] {
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("\"handoff\": {\n    \"tokens\""),
            "{file:?}: {text:?}"
        );
        assert!(text.contains("\"model\": \""), "{file:?}: {text:?}");
    }
    assert_eq!(
        setup
            .load(&[])
            .unwrap()
            .get("handoff.tokens", None)
            .unwrap()
            .0,
        json!(100),
        "the project layer beats the repository layer"
    );
}

#[test]
fn set_with_a_person_only_key_at_the_repository_layer_is_refused_and_writes_nothing() {
    let setup = Setup::new();
    let e = set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        "session.idle_exit_ms",
        json!(60000),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Usage);
    assert!(e.to_string().contains("`session.idle_exit_ms`"), "{e}");
    assert!(e.to_string().contains("a repository may not set it"), "{e}");
    assert!(
        e.to_string()
            .contains(&setup.repository().display().to_string()),
        "{e}"
    );
    assert!(!setup.repository().exists());
    assert!(!setup.workspace().join(".fiber").exists());
}

#[test]
fn set_of_diagnostics_level_outside_the_global_layer_is_refused() {
    for (layer, why) in [
        (Layer::Project, "only Fiber home's `config.json` may set it"),
        (Layer::Repository, "a repository may not set it"),
    ] {
        let setup = Setup::new();
        let e = set(
            &setup.home(),
            &setup.workspace(),
            &key(),
            layer,
            "diagnostics.level",
            json!("debug"),
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage, "{layer:?}");
        assert!(
            e.to_string().contains("`diagnostics.level`"),
            "{layer:?}: {e}"
        );
        assert!(e.to_string().contains(why), "{layer:?}: {e}");
        assert!(!setup.project().exists(), "{layer:?}");
        assert!(!setup.repository().exists(), "{layer:?}");
    }
    let setup = Setup::new();
    set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Global,
        "diagnostics.level",
        json!("debug"),
    )
    .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.global()).unwrap()).unwrap();
    assert_eq!(written, json!({"diagnostics": {"level": "debug"}}));
}

#[test]
fn set_of_a_repository_only_key_outside_the_repository_layer_is_refused() {
    for layer in [Layer::Global, Layer::Project] {
        let setup = Setup::new();
        let e = set(
            &setup.home(),
            &setup.workspace(),
            &key(),
            layer,
            "repository_extensions",
            json!([{"path": "pkg"}]),
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage, "{layer:?}");
        assert!(
            e.to_string().contains("`repository_extensions`"),
            "{layer:?}: {e}"
        );
        assert!(
            e.to_string()
                .contains("only a repository's own file may set it"),
            "{layer:?}: {e}"
        );
        assert!(!setup.global().exists(), "{layer:?}");
    }
}

#[test]
fn set_of_an_unknown_key_is_refused_and_writes_nothing() {
    for layer in [Layer::Global, Layer::Project, Layer::Repository] {
        let setup = Setup::new();
        setup.write(&setup.global(), r#"{"model": "a/b"}"#);
        let e = set(
            &setup.home(),
            &setup.workspace(),
            &key(),
            layer,
            "hub.port",
            json!(8080),
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::Usage, "{layer:?}");
        assert!(e.to_string().contains("`hub.port`"), "{layer:?}: {e}");
        assert!(
            e.to_string().contains("this Fiber does not know it"),
            "{layer:?}: {e}"
        );
        assert_eq!(
            fs::read_to_string(setup.global()).unwrap(),
            r#"{"model": "a/b"}"#,
            "{layer:?}"
        );
        assert!(!setup.project().exists(), "{layer:?}");
        assert!(!setup.repository().exists(), "{layer:?}");
    }
}

#[test]
fn set_of_a_repo_settable_key_at_the_repository_layer_is_written() {
    let setup = Setup::new();
    set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        "handoff.tokens",
        json!(50),
    )
    .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.repository()).unwrap()).unwrap();
    assert_eq!(written, json!({"handoff": {"tokens": 50}}));
}

#[test]
fn set_of_a_wrongly_typed_value_is_config_invalid_and_writes_nothing() {
    let setup = Setup::new();
    let e = set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        "handoff.tokens",
        json!("many"),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(!setup.project().exists());
}

#[test]
fn set_at_the_repository_layer_refuses_a_symlinked_dot_fiber() {
    for linked in [".fiber", ".fiber/config.json"] {
        let setup = Setup::new();
        let target = setup.root().join("elsewhere");
        fs::create_dir_all(&target).unwrap();
        let link = setup.workspace().join(linked);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let e = set(
            &setup.home(),
            &setup.workspace(),
            &key(),
            Layer::Repository,
            "model",
            json!("a/b"),
        )
        .unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{linked}");
    }
}

#[test]
fn set_into_a_file_holding_invalid_json_leaves_it() {
    let setup = Setup::new();
    setup.write(&setup.global(), "{\"model\": \"a/b\",}");
    let e = set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Global,
        "model",
        json!("c/d"),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\"model\": \"a/b\",}"
    );
}

#[test]
fn set_of_a_warm_cap_of_twelve_is_config_invalid_and_eleven_is_written() {
    let setup = Setup::new();
    let e = set(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        "cache.warm_cap",
        json!(12),
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(!setup.repository().exists());
    for (name, value) in [
        ("cache.warm_cap", json!(11)),
        ("cache.warm_idle", json!(true)),
    ] {
        set(
            &setup.home(),
            &setup.workspace(),
            &key(),
            Layer::Repository,
            name,
            value,
        )
        .unwrap();
    }
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.repository()).unwrap()).unwrap();
    assert_eq!(
        written,
        json!({"cache": {"warm_cap": 11, "warm_idle": true}})
    );
}
