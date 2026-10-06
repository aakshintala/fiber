//! `host.config` (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::cell::RefCell;
use std::fs;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use mlua::{Lua, Value as LuaValue};
use serde_json::Value;

use super::{Settings, install};

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

    fn settings(&self, config: Config, repo_settings: &[&str]) -> Settings {
        Settings {
            extension: EXTENSION.to_owned(),
            repo_settings: repo_settings.iter().map(|s| (*s).to_owned()).collect(),
            config: Rc::new(RefCell::new(config)),
        }
    }

    /// Lua with `host.config` for `fiber.test/notes`.
    fn lua(&self, settings: Option<Settings>) -> Lua {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        install(&lua, &host, settings).unwrap();
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
    let lua = setup.lua(Some(setup.settings(config, &[])));
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
    let lua = setup.lua(Some(setup.settings(config, &["workspace_url"])));
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
    let lua = setup.lua(Some(setup.settings(config, &[])));
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
    let lua = setup.lua(Some(setup.settings(config, &[])));
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
    let lua = setup.lua(Some(setup.settings(config, &[])));
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
    let lua = setup.lua(Some(setup.settings(config, &[])));
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
fn two_sessions_setting_at_once_lose_no_key() {
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
            let settings = Settings {
                extension: EXTENSION.to_owned(),
                repo_settings: Vec::new(),
                config: Rc::new(RefCell::new(config)),
            };
            let lua = Lua::new();
            let host = lua.create_table().unwrap();
            install(&lua, &host, Some(settings)).unwrap();
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
    writer(2, done_b);
    done_rx_a.recv_timeout(DEADLINE).unwrap();
    done_rx_b.recv_timeout(DEADLINE).unwrap();
    let merged = setup
        .load(&[])
        .extension_setting(EXTENSION, &[], "slot1")
        .unwrap();
    assert_eq!(merged, Some(serde_json::json!(1)));
    let merged = setup
        .load(&[])
        .extension_setting(EXTENSION, &[], "slot2")
        .unwrap();
    assert_eq!(merged, Some(serde_json::json!(2)));
}
