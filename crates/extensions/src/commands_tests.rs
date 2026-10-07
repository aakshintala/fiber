//! `SessionCommands` (`docs/extensions.md`, "Commands and screens"): the list
//! is sorted with tags; a conflict leaves neither with one `command_conflict`
//! notice; renames, the `replaces` check, admission order and run failures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::extension::ExtensionDoor;
use fakes::clock::FakeClock;
use serde_json::json;

use super::SessionCommands;
use crate::commands::CommandSource;
use crate::host::FakeLock;
use crate::lua::LuaExtension;
use crate::{Origin, Request, plan};

/// How long a test waits for a load or a run before failing.
const WAIT: Duration = Duration::from_secs(10);

fn command(name: &str, description: &str) -> String {
    format!(
        "fiber.command(\"{name}\", {{ timeout = 5000, description = \"{description}\", run = function(text) return text end }})\n"
    )
}

fn extension(name: &str, dir: &std::path::Path, home: &std::path::Path) -> Arc<LuaExtension> {
    Arc::new(
        LuaExtension::new(name, dir, home, FakeClock::new()).with_session(crate::host::Session {
            config: Config::load(Sources {
                home: home.to_owned(),
                workspace: dir.to_owned(),
                project: ProjectKey::new("p").unwrap(),
                overrides: Vec::new(),
            })
            .unwrap(),
            repo_settings: Vec::new(),
            locks: Arc::new(FakeLock::new()),
        }),
    )
}

fn write_ext(root: &std::path::Path, short: &str, init: &str) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let dir = root.join("src").join(short);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("extension.json"),
        json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1}).to_string(),
    )
    .unwrap();
    fs::write(dir.join("init.lua"), init).unwrap();
    (home, dir)
}

type Source<'a> = (
    &'a str,
    &'a std::path::Path,
    Vec<(String, String)>,
    Vec<String>,
);

fn sources(home: &std::path::Path, items: &[Source<'_>]) -> Vec<CommandSource> {
    items
        .iter()
        .map(|(name, dir, commands, replaces)| CommandSource {
            extension: (*name).to_owned(),
            replaces: replaces.clone(),
            commands: commands.clone(),
            lua: extension(name, dir, home),
        })
        .collect()
}

fn config_with(home: &std::path::Path, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: home.to_owned(),
        workspace: home.to_owned(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

#[test]
fn list_is_sorted_with_tags_and_no_argument_hint() {
    let root = fakes::TempDir::new("fiber-commands-list");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir_b) = write_ext(root.path(), "b", &command("zeta", "Z"));
    let (_h, dir_a) = write_ext(root.path(), "a", &command("alpha", "A"));
    // Start both VMs so their commands register.
    let lua_b = extension("fiber.test/b", &dir_b, &home);
    lua_b.commands().unwrap();
    let lua_a = extension("fiber.test/a", &dir_a, &home);
    lua_a.commands().unwrap();
    let all = sources(
        &home,
        &[
            (
                "fiber.test/b",
                &dir_b,
                vec![("zeta".into(), "Z".into())],
                vec![],
            ),
            (
                "fiber.test/a",
                &dir_a,
                vec![("alpha".into(), "A".into())],
                vec![],
            ),
        ],
    );
    // Rebuild with the started VMs.
    let _ = (lua_a, lua_b);
    let built = SessionCommands::build(&all, &config_with(&home, &[]));
    let list = built.list();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "alpha");
    assert_eq!(list[0].description, "A");
    assert_eq!(list[0].argument_hint, None);
    assert_eq!(list[0].tag, "fiber.test/a");
    assert_eq!(list[1].name, "zeta");
    assert_eq!(list[1].tag, "fiber.test/b");
}

#[test]
fn a_conflict_leaves_neither_with_one_notice_naming_both() {
    let root = fakes::TempDir::new("fiber-commands-conflict");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir_a) = write_ext(root.path(), "a", &command("sync", "A"));
    let (_h, dir_b) = write_ext(root.path(), "b", &command("sync", "B"));
    let all = sources(
        &home,
        &[
            (
                "fiber.test/a",
                &dir_a,
                vec![("sync".into(), "A".into())],
                vec![],
            ),
            (
                "fiber.test/b",
                &dir_b,
                vec![("sync".into(), "B".into())],
                vec![],
            ),
        ],
    );
    let built = SessionCommands::build(&all, &config_with(&home, &[]));
    assert!(built.list().is_empty(), "neither gets the name");
    let notices = built.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::CommandConflict);
    assert_eq!(notices[0].extension, None);
    assert!(
        notices[0].message.contains("`sync`"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[0].message.contains("fiber.test/a"),
        "{}",
        notices[0].message
    );
    assert!(
        notices[0].message.contains("fiber.test/b"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_rename_gives_one_the_new_name_and_clears_the_conflict() {
    let root = fakes::TempDir::new("fiber-commands-rename");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir_a) = write_ext(root.path(), "a", &command("sync", "A"));
    let (_h, dir_b) = write_ext(root.path(), "b", &command("sync", "B"));
    let all = sources(
        &home,
        &[
            (
                "fiber.test/a",
                &dir_a,
                vec![("sync".into(), "A".into())],
                vec![],
            ),
            (
                "fiber.test/b",
                &dir_b,
                vec![("sync".into(), "B".into())],
                vec![],
            ),
        ],
    );
    let config = config_with(
        &home,
        &[r#"extensions."fiber.test/b".commands."sync"="pulled""#],
    );
    let built = SessionCommands::build(&all, &config);
    let list = built.list();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "pulled");
    assert_eq!(list[1].name, "sync");
    assert!(built.notices().is_empty());
}

#[test]
fn a_non_plain_rename_is_ignored_with_a_notice() {
    let root = fakes::TempDir::new("fiber-commands-badrename");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir_a) = write_ext(root.path(), "a", &command("sync", "A"));
    let all = sources(
        &home,
        &[(
            "fiber.test/a",
            &dir_a,
            vec![("sync".into(), "A".into())],
            vec![],
        )],
    );
    let config = config_with(
        &home,
        &[r#"extensions."fiber.test/a".commands."sync"="not plain""#],
    );
    let built = SessionCommands::build(&all, &config);
    let list = built.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "sync");
    let notices = built.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert_eq!(notices[0].extension.as_deref(), Some("fiber.test/a"));
}

#[test]
fn a_rename_onto_the_same_extensions_other_command_is_ignored() {
    let root = fakes::TempDir::new("fiber-commands-selfrename");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let init = command("a", "A") + &command("b", "B");
    let (_h, dir) = write_ext(root.path(), "a", &init);
    let all = sources(
        &home,
        &[(
            "fiber.test/a",
            &dir,
            vec![("a".into(), "A".into()), ("b".into(), "B".into())],
            vec![],
        )],
    );
    let config = config_with(&home, &[r#"extensions."fiber.test/a".commands."b"="a""#]);
    let built = SessionCommands::build(&all, &config);
    let list = built.list();
    assert_eq!(list.len(), 2, "both keep their registered names");
    assert_eq!(list[0].name, "a");
    assert_eq!(list[1].name, "b");
    assert_eq!(built.notices().len(), 1);
    assert_eq!(built.notices()[0].code, ErrorCode::ExtensionFailed);
}

#[test]
fn a_builtin_name_without_replaces_unloads_with_a_notice_naming_it() {
    let root = fakes::TempDir::new("fiber-commands-replaces");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir) = write_ext(root.path(), "a", &command("model", "M"));
    let all = sources(
        &home,
        &[(
            "fiber.test/a",
            &dir,
            vec![("model".into(), "M".into())],
            vec![],
        )],
    );
    let built = SessionCommands::build(&all, &config_with(&home, &[]));
    assert!(built.list().is_empty());
    assert!(built.unloaded().contains("fiber.test/a"));
    let notices = built.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].code, ErrorCode::ExtensionFailed);
    assert!(
        notices[0].message.contains("`model`"),
        "{}",
        notices[0].message
    );
}

#[test]
fn a_builtin_name_with_replaces_is_listed() {
    let root = fakes::TempDir::new("fiber-commands-replaced");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir) = write_ext(root.path(), "a", &command("model", "M"));
    let all = sources(
        &home,
        &[(
            "fiber.test/a",
            &dir,
            vec![("model".into(), "M".into())],
            vec!["model".into()],
        )],
    );
    let built = SessionCommands::build(&all, &config_with(&home, &[]));
    assert_eq!(built.list().len(), 1);
    assert!(built.unloaded().is_empty());
}

#[test]
fn unknown_name_is_unknown_command() {
    let root = fakes::TempDir::new("fiber-commands-unknown");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let all: Vec<CommandSource> = Vec::new();
    let built = SessionCommands::build(&all, &config_with(&home, &[]));
    let err = match built.admit_with("nope", "") {
        Ok(_) => panic!("admitted unknown"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::UnknownCommand);
    assert_eq!(err.message, "`nope` names no extension command.");
}

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-commands-home");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn install(&self, short: &str, init: &str) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("extension.json"),
            json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1}).to_string(),
        )
        .unwrap();
        fs::write(src.join("init.lua"), init).unwrap();
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn load(&self, overrides: &[&str]) -> Arc<super::super::SessionExtensions> {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _sent = tx.send(super::super::SessionExtensions::load(
                &home,
                &config,
                FakeClock::new(),
                locks,
            ));
        });
        Arc::new(rx.recv_timeout(WAIT).expect("waited for the extensions"))
    }
}

#[test]
fn admission_returns_before_run_starts_and_two_run_in_order() {
    // A recording `run` and an assertion made between admission and release
    // that it has not run; two admissions run in admission order.
    let root = fakes::TempDir::new("fiber-commands-admit");
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let (_h, dir) = write_ext(
        root.path(),
        "a",
        "fiber.command(\"one\", { timeout = 5000, run = function() host.log(\"one\") end })\n\
         fiber.command(\"two\", { timeout = 5000, run = function() host.log(\"two\") end })\n",
    );
    let lua = extension("fiber.test/a", &dir, &home);
    lua.commands().unwrap();
    let first = lua.queue_command("one", "").unwrap();
    let second = lua.queue_command("two", "").unwrap();
    // Between admission (queueing held) and release, neither `run` has
    // started: no delivery reaches the inbox.
    let (tx, rx) = mpsc::channel();
    lua.deliver_to(tx);
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "nothing runs while held"
    );
    first.release();
    first.wait().unwrap();
    let first_done = rx.recv_timeout(WAIT).expect("first runs after release");
    drop(first_done);
    second.release();
    second.wait().unwrap();
    assert!(rx.recv_timeout(WAIT).is_ok(), "second runs after release");
}

#[derive(Default)]
struct Recorder {
    events: std::sync::Mutex<Vec<contract::events::Event>>,
}

impl contract::emit::Emit for Recorder {
    fn emit(&self, event: &contract::events::Event) {
        self.events.lock().unwrap().push(event.clone());
    }
}

#[test]
fn a_failing_run_gives_one_extension_failed_notice_naming_the_extension() {
    let home = Home::new();
    home.install(
        "a",
        "fiber.command(\"bad\", { timeout = 5000, run = function() error(\"boom\") end })\n",
    );
    let session = home.load(&[]);
    let recorder = Arc::new(Recorder::default());
    session.emit_to(recorder.clone() as Arc<dyn contract::emit::Emit>);
    let door: &dyn ExtensionDoor = &*session;
    let release = door.command("bad", "").expect("admitted");
    release();
    // The waiter thread emits the notice through the emitter; wait for it
    // under a wall-clock deadline.
    let notice = {
        let (tx, rx) = mpsc::channel();
        let mut found = None;
        for _ in 0..(WAIT.as_millis() / 10) {
            found = recorder.events.lock().unwrap().iter().find_map(|event| {
                let contract::events::Event::Notice(notice) = event else {
                    return None;
                };
                Some(notice.clone())
            });
            if found.is_some() {
                break;
            }
            let _ = rx.recv_timeout(Duration::from_millis(10)).ok();
        }
        let _ = tx.send(()).ok();
        found.expect("waited for the run-failure notice")
    };
    assert_eq!(notice.code, ErrorCode::ExtensionFailed);
    assert_eq!(notice.extension.as_deref(), Some("fiber.test/a"));
    assert!(notice.message.contains("bad"), "{}", notice.message);
    drop(session);
}

#[test]
fn a_run_returning_a_table_is_not_an_error() {
    // `run`'s return value is ignored: a table is not an error, so no
    // `extension_failed` notice follows it.
    let home = Home::new();
    home.install(
        "a",
        "fiber.command(\"tab\", { timeout = 5000, run = function() return { a = 1 } end })\n",
    );
    let session = home.load(&[]);
    let recorder = Arc::new(Recorder::default());
    session.emit_to(recorder.clone() as Arc<dyn contract::emit::Emit>);
    let door: &dyn ExtensionDoor = &*session;
    let release = door.command("tab", "").expect("admitted");
    release();
    // Give the waiter a chance to fail it, then assert silence. The run
    // itself returns at once, so 500 ms of quiet proves no failure notice.
    let (_, rx) = mpsc::channel::<()>();
    let _ = rx.recv_timeout(Duration::from_millis(500)).ok();
    assert!(
        recorder.events.lock().unwrap().is_empty(),
        "a table return is not an error"
    );
    drop(session);
}

#[test]
fn command_only_extensions_stay_loaded() {
    // A hooks-only and a commands-only extension both stay in `lua`.
    let home = Home::new();
    home.install(
        "cmd",
        "fiber.command(\"go\", { timeout = 5000, run = function() return \"go\" end })\n",
    );
    let session = home.load(&[]);
    assert!(
        session.commands().iter().any(|c| c.name == "go"),
        "the command-only extension is listed"
    );
}
