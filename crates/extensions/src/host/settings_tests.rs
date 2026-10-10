//! `host.config` (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use fakes::Deadline;
use mlua::{Lua, Value as LuaValue};
use serde_json::Value;

use super::install;

/// How long a test waits for a thread before failing.
const DEADLINE: Duration = Duration::from_secs(10);

const EXTENSION: &str = "fiber.test/notes";
const SLUG: &str = "fiber.test-notes";

struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-host-settings");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().join("workspace")
    }

    fn write(&self, path: &std::path::Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn config_file(&self, scope: &str) -> std::path::PathBuf {
        match scope {
            "machine" => self.home().join("config").join(format!("{SLUG}.json")),
            _ => self
                .home()
                .join("projects")
                .join("p")
                .join("config")
                .join(format!("{SLUG}.json")),
        }
    }

    fn load(&self, overrides: &[&str]) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap()
    }

    fn session(&self, config: Config, repo_settings: &[&str]) -> crate::host::Session {
        crate::host::Session {
            config,
            repo_settings: repo_settings.iter().map(|s| (*s).to_owned()).collect(),
            locks: Arc::new(super::super::FakeLock::new()),
        }
    }

    /// Lua with `host.config` for `fiber.test/notes`.
    fn lua(&self, session: Option<crate::host::Session>) -> Lua {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        let failure = crate::host::failure::install(&lua).unwrap().failure;
        install(&lua, &host, EXTENSION, session, &failure).unwrap();
        lua.globals().set("host", host).unwrap();
        lua
    }
}

fn eval(lua: &Lua, code: &str) -> Value {
    crate::host::to_json(&lua.load(code).eval::<LuaValue>().unwrap()).unwrap()
}

fn fails(lua: &Lua, code: &str) -> String {
    lua.load(code).eval::<LuaValue>().unwrap_err().to_string()
}

fn on_disk(setup: &Setup, scope: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(setup.config_file(scope)).unwrap()).unwrap()
}

#[test]
fn get_returns_the_value_merged_across_global_project_and_run() {
    let setup = Setup::new();
    setup.write(
        &setup.config_file("machine"),
        r#"{"picker": {"model": "global/m", "keep": 1}}"#,
    );
    setup.write(
        &setup.config_file("project"),
        r#"{"picker": {"model": "project/m"}}"#,
    );
    let config = setup.load(&[
        "extensions.\"fiber.test/notes\".settings.picker.theme=\"dark\"",
        "model=a/b",
    ]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    assert_eq!(
        eval(&lua, "return host.config.get(\"picker.model\")"),
        "project/m"
    );
    assert_eq!(eval(&lua, "return host.config.get(\"picker.keep\")"), 1);
    assert_eq!(
        eval(&lua, "return host.config.get(\"picker.theme\")"),
        "dark"
    );
    assert_eq!(
        eval(&lua, "return host.config.get(\"picker.missing\")"),
        Value::Null
    );
}

#[test]
fn a_repository_key_outside_repo_settings_is_ignored() {
    let setup = Setup::new();
    setup.write(
        &setup
            .workspace()
            .join(".fiber")
            .join("config")
            .join(format!("{SLUG}.json")),
        r#"{"workspace_url": "https://repo", "token_command": "curl evil"}"#,
    );
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &["workspace_url"])));
    assert_eq!(
        eval(&lua, "return host.config.get(\"workspace_url\")"),
        "https://repo"
    );
    assert_eq!(
        eval(&lua, "return host.config.get(\"token_command\")"),
        Value::Null
    );
}

#[test]
fn set_writes_the_named_layer_and_get_sees_it_at_once() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    lua.load("host.config.set(\"picker.model\", \"x/y\", \"machine\")")
        .exec()
        .unwrap();
    assert_eq!(
        on_disk(&setup, "machine"),
        serde_json::json!({"picker": {"model": "x/y"}})
    );
    assert_eq!(
        eval(&lua, "return host.config.get(\"picker.model\")"),
        "x/y"
    );
    // The project layer starts absent: `set` creates it.
    assert!(!setup.config_file("project").exists());
    lua.load("host.config.set(\"picker.model\", \"p/q\", \"project\")")
        .exec()
        .unwrap();
    assert_eq!(
        on_disk(&setup, "project"),
        serde_json::json!({"picker": {"model": "p/q"}})
    );
    assert_eq!(
        eval(&lua, "return host.config.get(\"picker.model\")"),
        "p/q"
    );
}

#[test]
fn a_table_with_mixed_keys_is_an_object_with_string_keys() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    lua.load("host.config.set(\"mixed\", {[1] = \"a\", foo = \"b\"}, \"machine\")")
        .exec()
        .unwrap();
    assert_eq!(
        on_disk(&setup, "machine"),
        serde_json::json!({"mixed": {"1": "a", "foo": "b"}})
    );
}

#[test]
fn a_missing_or_bad_scope_writes_nothing() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    for code in [
        "host.config.set(\"a\", 1)",
        "host.config.set(\"a\", 1, nil)",
        "host.config.set(\"a\", 1, \"global\")",
        "host.config.set(\"a\", 1, 7)",
    ] {
        let message = fails(&lua, code);
        assert!(
            message.contains("host.config.set: scope must be \"machine\" or \"project\""),
            "{code}: {message}"
        );
    }
    assert!(!setup.config_file("machine").exists());
    assert!(!setup.config_file("project").exists());
}

#[test]
fn a_nil_value_and_a_bad_key_write_nothing() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    let message = fails(&lua, "host.config.set(\"a\", nil, \"machine\")");
    assert!(
        message.contains("host.config.set: value must not be nil"),
        "{message}"
    );
    let message = fails(
        &lua,
        "host.config.set(\"unclosed.\\\"quote\", 1, \"machine\")",
    );
    assert!(message.contains("host.config.set"), "{message}");
    assert!(message.contains("not a dotted key"), "{message}");
    let message = fails(&lua, "return host.config.get(\"unclosed.\\\"quote\")");
    assert!(message.contains("host.config.get"), "{message}");
    assert!(message.contains("not a dotted key"), "{message}");
    assert!(!setup.config_file("machine").exists());
}

#[test]
fn without_a_session_both_calls_raise() {
    let setup = Setup::new();
    let lua = setup.lua(None);
    let message = fails(&lua, "return host.config.get(\"a\")");
    assert!(
        message.contains("host.config.get: this extension has no session"),
        "{message}"
    );
    let message = fails(&lua, "host.config.set(\"a\", 1, \"machine\")");
    assert!(
        message.contains("host.config.set: this extension has no session"),
        "{message}"
    );
    assert!(!setup.config_file("machine").exists());
}

#[test]
fn two_sequential_sets_both_land() {
    let setup = Setup::new();
    let home = setup.home();
    let workspace = setup.workspace();
    let key = ProjectKey::new("p").unwrap();
    let (done_a, done_rx_a) = std::sync::mpsc::channel();
    let (done_b, done_rx_b) = std::sync::mpsc::channel();
    let writer = |value: u64, done: std::sync::mpsc::Sender<()>| {
        let (home, workspace, key) = (home.clone(), workspace.clone(), key.clone());
        thread::spawn(move || {
            let config = Config::load(Sources {
                home,
                workspace,
                project: key,
                overrides: Vec::new(),
            })
            .unwrap();
            let session = crate::host::Session {
                config,
                repo_settings: Vec::new(),
                locks: Arc::new(crate::host::FakeLock::new()),
            };
            let lua = Lua::new();
            let host = lua.create_table().unwrap();
            let failure = crate::host::failure::install(&lua).unwrap().failure;
            install(&lua, &host, EXTENSION, Some(session), &failure).unwrap();
            lua.globals().set("host", host).unwrap();
            lua.load(format!(
                "host.config.set(\"slot{value}\", {value}, \"machine\")"
            ))
            .exec()
            .unwrap();
            done.send(()).unwrap();
        })
    };
    writer(1, done_a);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx_a).is_ok(),
        "the first session's set did not finish within {DEADLINE:?}"
    );
    // The second session starts only after the first finished: two
    // sequential sets both land. Contention on one file is forced in
    // `a_second_update_waits_for_the_files_lock_then_keeps_both_writes`
    // in `crates/config/src/write.rs`.
    writer(2, done_b);
    assert!(
        Deadline::after(DEADLINE).recv(&done_rx_b).is_ok(),
        "the second session's set did not finish within {DEADLINE:?}"
    );
    let merged = setup
        .load(&[])
        .extensions()
        .get(EXTENSION, &[], "slot1")
        .unwrap();
    assert_eq!(merged, Some(serde_json::json!(1)));
    let merged = setup
        .load(&[])
        .extensions()
        .get(EXTENSION, &[], "slot2")
        .unwrap();
    assert_eq!(merged, Some(serde_json::json!(2)));
}

/// The failure a prelude `pcall` of `code` catches: its code and message.
fn pcall_of(lua: &Lua, code: &str) -> (String, String) {
    let clock = fakes::clock::FakeClock::new();
    let deadline = crate::lua::Deadline::new(clock);
    let dir = fakes::TempDir::new("fiber-settings-prelude");
    crate::lua::install_prelude(lua, &deadline, dir.path().to_path_buf(), crate::MEMORY_CAP)
        .unwrap();
    let (ok, err): (bool, LuaValue) = lua
        .load(format!("return pcall(function() {code} end)"))
        .eval()
        .unwrap();
    assert!(!ok, "{code} unexpectedly succeeded");
    let LuaValue::Table(failed) = err else {
        panic!("{code} raised no failure table");
    };
    (failed.get("code").unwrap(), failed.get("message").unwrap())
}

/// The failure a raw `coroutine.resume` of `code` catches: its code and
/// message. The value is the table at its source, not userdata.
fn resume_of(lua: &Lua, code: &str) -> (String, String) {
    let (ok, err): (bool, LuaValue) = lua
        .load(format!(
            "return coroutine.resume(coroutine.create(function() {code} end))"
        ))
        .eval()
        .unwrap();
    assert!(!ok, "{code} unexpectedly succeeded");
    let LuaValue::Table(failed) = err else {
        panic!("{code} raised no failure table");
    };
    (failed.get("code").unwrap(), failed.get("message").unwrap())
}

#[test]
fn a_resumed_coded_failure_is_the_table_at_its_source() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    // Another session left invalid JSON on disk after this one loaded.
    setup.write(&setup.config_file("machine"), "{invalid");
    let (code, message) = resume_of(&lua, "host.config.set(\"a\", 1, \"machine\")");
    assert_eq!(code, "config_invalid");
    assert!(message.starts_with("host.config.set: "), "{message}");
}

#[test]
fn a_set_over_an_invalid_file_is_config_invalid() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    // Another session left invalid JSON on disk after this one loaded.
    setup.write(&setup.config_file("machine"), "{invalid");
    let (code, message) = pcall_of(&lua, "host.config.set(\"a\", 1, \"machine\")");
    assert_eq!(code, "config_invalid");
    assert!(message.starts_with("host.config.set: "), "{message}");
}

#[test]
fn a_set_where_no_file_can_be_written_is_io_failed() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    // A file where the settings directory goes leaves no file to write.
    setup.write(&setup.home().join("config"), "in the way");
    let (code, message) = pcall_of(&lua, "host.config.set(\"a\", 1, \"machine\")");
    assert_eq!(code, "io_failed");
    assert!(message.starts_with("host.config.set: "), "{message}");
}

#[test]
fn wrong_arguments_are_strings_and_coded_failures_are_tables() {
    let setup = Setup::new();
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    let clock = fakes::clock::FakeClock::new();
    let deadline = crate::lua::Deadline::new(clock);
    let dir = fakes::TempDir::new("fiber-settings-types");
    crate::lua::install_prelude(&lua, &deadline, dir.path().to_path_buf(), crate::MEMORY_CAP)
        .unwrap();
    let kind_of = |code: &str| -> String {
        lua.load(format!(
            "local ok, err = pcall(function() {code} end); return type(err)"
        ))
        .eval()
        .unwrap()
    };
    // Every error in the calling code stays a string (ruling 17): a key that
    // is not a string, not UTF-8 or not a dotted key, a nil or non-JSON
    // value, and a missing or bad scope.
    for code in [
        "return host.config.get(123)",
        "return host.config.get(\"\\255\")",
        "return host.config.get(\"unclosed.\\\"quote\")",
        "host.config.set(123, 1, \"machine\")",
        "host.config.set(\"\\255\", 1, \"machine\")",
        "host.config.set(\"unclosed.\\\"quote\", 1, \"machine\")",
        "host.config.set(\"a\", nil, \"machine\")",
        "host.config.set(\"a\", print, \"machine\")",
        "host.config.set(\"a\", 1)",
        "host.config.set(\"a\", 1, \"bogus\")",
        "host.config.set(\"a\", 1, 123)",
    ] {
        assert_eq!(kind_of(code), "string", "{code}");
    }
    assert!(!setup.config_file("machine").exists());
    // Every operational failure is a table: an invalid file, and a file
    // that cannot be written.
    setup.write(&setup.config_file("machine"), "{invalid");
    assert_eq!(kind_of("host.config.set(\"a\", 1, \"machine\")"), "table");
    setup.write(
        &setup.home().join("projects").join("p").join("config"),
        "in the way",
    );
    assert_eq!(kind_of("host.config.set(\"a\", 1, \"project\")"), "table");
    // No session stays a string too.
    let lua = setup.lua(None);
    let clock = fakes::clock::FakeClock::new();
    let deadline = crate::lua::Deadline::new(clock);
    let dir = fakes::TempDir::new("fiber-settings-types-nosession");
    crate::lua::install_prelude(&lua, &deadline, dir.path().to_path_buf(), crate::MEMORY_CAP)
        .unwrap();
    for code in [
        "return host.config.get(\"a\")",
        "host.config.set(\"a\", 1, \"machine\")",
    ] {
        let kind: String = lua
            .load(format!(
                "local ok, err = pcall(function() {code} end); return type(err)"
            ))
            .eval()
            .unwrap();
        assert_eq!(kind, "string", "{code}");
    }
}

#[test]
fn a_get_over_a_file_the_session_read_as_invalid_is_a_table() {
    let setup = Setup::new();
    setup.write(&setup.config_file("machine"), "{invalid");
    let config = setup.load(&[]);
    let lua = setup.lua(Some(setup.session(config, &[])));
    let (code, message) = pcall_of(&lua, "return host.config.get(\"a\")");
    assert_eq!(code, "config_invalid");
    assert!(message.starts_with("host.config.get: "), "{message}");
}
