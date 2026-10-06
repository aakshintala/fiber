//! `host.fs` and `host.data_dir` (`docs/extensions.md`, "Host calls").

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::files::PathLock;
use mlua::{Lua, LuaString, Value as LuaValue};
use serde_json::Value;

use super::{FakeLock, Fs, install};

/// How long a test waits for a lock or a thread before failing.
const DEADLINE: Duration = Duration::from_secs(10);

struct Setup {
    root: fakes::TempDir,
    locks: Arc<FakeLock>,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-host-fs");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self {
            root,
            locks: Arc::new(FakeLock::new()),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn locks(&self) -> Arc<dyn PathLock> {
        Arc::clone(&self.locks) as Arc<dyn PathLock>
    }

    fn fs(&self) -> Fs {
        Fs {
            workspace: self.workspace(),
            locks: Some(self.locks()),
            machine_dir: Some(self.home().join("data").join("fiber.test-notes")),
            project_dir: Some(
                self.home()
                    .join("projects")
                    .join("p")
                    .join("data")
                    .join("fiber.test-notes"),
            ),
        }
    }

    /// Lua with `host.fs` and `host.data_dir` for `fiber.test/notes` in
    /// project `p`, locking through the shared fake.
    fn lua(&self) -> Lua {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        install(
            &lua,
            &host,
            self.workspace(),
            self.home(),
            "fiber.test/notes",
            Some("p"),
            Some(self.locks()),
        )
        .unwrap();
        lua.globals().set("host", host).unwrap();
        lua
    }

    /// Lua with no session: relative paths resolve against the process's
    /// current directory, `lock = true` takes no lock, and `host.data_dir`
    /// raises.
    fn lua_without_session(&self) -> Lua {
        let lua = Lua::new();
        let host = lua.create_table().unwrap();
        install(
            &lua,
            &host,
            std::env::current_dir().unwrap(),
            self.home(),
            "fiber.test/notes",
            None,
            None,
        )
        .unwrap();
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

#[test]
fn a_write_reads_back_what_it_wrote_as_bytes() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.mkdir(\"notes\")").exec().unwrap();
    lua.load("host.fs.write(\"notes/a.md\", \"hi\")")
        .exec()
        .unwrap();
    assert!(setup.workspace().join("notes/a.md").exists());
    assert_eq!(eval(&lua, "return host.fs.read(\"notes/a.md\")"), "hi");
    // Raw bytes, never required to be UTF-8.
    lua.load("host.fs.write(\"notes/raw.bin\", string.char(255, 254, 0))")
        .exec()
        .unwrap();
    let back: LuaString = lua
        .load("return host.fs.read(\"notes/raw.bin\")")
        .eval()
        .unwrap();
    assert_eq!(back.as_bytes(), [255, 254, 0]);
}

#[test]
fn a_relative_path_resolves_against_the_workspace() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(\"rel.md\", \"here\")")
        .exec()
        .unwrap();
    assert_eq!(fs::read(setup.workspace().join("rel.md")).unwrap(), b"here");
    fs::write(setup.workspace().join("abs-src.md"), "found").unwrap();
    let absolute = setup.workspace().join("abs-src.md");
    lua.globals()
        .set("abs", absolute.to_str().unwrap())
        .unwrap();
    assert_eq!(eval(&lua, "return host.fs.read(abs)"), "found");
}

#[test]
fn stat_reports_each_kind_and_nil_for_a_missing_path() {
    let setup = Setup::new();
    let lua = setup.lua();
    assert_eq!(eval(&lua, "return host.fs.stat(\"nope\")"), Value::Null);
    lua.load("host.fs.write(\"f.md\", \"hello\")")
        .exec()
        .unwrap();
    let file = eval(&lua, "return host.fs.stat(\"f.md\")");
    assert_eq!(file.get("kind"), Some(&Value::from("file")));
    assert_eq!(file.get("size"), Some(&Value::from(5)));
    assert!(file.get("modified_ms").is_some_and(|ms| ms.is_number()));
    fs::create_dir(setup.workspace().join("sub")).unwrap();
    assert_eq!(eval(&lua, "return host.fs.stat(\"sub\").kind"), "dir");
    symlink("f.md", setup.workspace().join("link")).unwrap();
    assert_eq!(eval(&lua, "return host.fs.stat(\"link\").kind"), "symlink");
}

#[test]
fn list_returns_sorted_names_without_dot_entries() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.mkdir(\"box\")").exec().unwrap();
    for name in ["c.md", "B.md", "a.md"] {
        fs::write(setup.workspace().join("box").join(name), "x").unwrap();
    }
    fs::create_dir(setup.workspace().join("box").join("sub")).unwrap();
    assert_eq!(
        eval(&lua, "return host.fs.list(\"box\")"),
        serde_json::json!(["B.md", "a.md", "c.md", "sub"])
    );
}

#[test]
fn mkdir_makes_parents_and_an_existing_directory_is_not_an_error() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.mkdir(\"deep/nest/dir\")").exec().unwrap();
    assert!(setup.workspace().join("deep/nest/dir").is_dir());
    lua.load("host.fs.mkdir(\"deep/nest/dir\")").exec().unwrap();
}

#[test]
fn remove_takes_a_file_a_symlink_or_an_empty_directory() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(\"gone.md\", \"x\")")
        .exec()
        .unwrap();
    lua.load("host.fs.remove(\"gone.md\")").exec().unwrap();
    assert!(!setup.workspace().join("gone.md").exists());
    lua.load("host.fs.mkdir(\"empty\")").exec().unwrap();
    lua.load("host.fs.remove(\"empty\")").exec().unwrap();
    assert!(!setup.workspace().join("empty").exists());
    symlink("nope", setup.workspace().join("dangling")).unwrap();
    lua.load("host.fs.remove(\"dangling\")").exec().unwrap();
    assert!(!setup.workspace().join("dangling").exists());
}

#[test]
fn rename_moves_the_file() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(\"before.md\", \"moved\")")
        .exec()
        .unwrap();
    lua.load("host.fs.rename(\"before.md\", \"after.md\")")
        .exec()
        .unwrap();
    assert!(!setup.workspace().join("before.md").exists());
    assert_eq!(eval(&lua, "return host.fs.read(\"after.md\")"), "moved");
}

#[test]
fn every_failure_names_its_call_and_its_path() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(\"f.md\", \"x\")").exec().unwrap();
    lua.load("host.fs.mkdir(\"dir\")").exec().unwrap();
    lua.load("host.fs.write(\"dir/child.md\", \"x\")")
        .exec()
        .unwrap();
    for (code, op, path) in [
        (
            "return host.fs.read(\"missing.md\")",
            "host.fs.read",
            "missing.md",
        ),
        (
            "host.fs.write(\"no/parent/x.md\", \"x\")",
            "host.fs.write",
            "no/parent/x.md",
        ),
        ("return host.fs.list(\"f.md\")", "host.fs.list", "f.md"),
        ("host.fs.mkdir(\"f.md/kid\")", "host.fs.mkdir", "f.md"),
        ("host.fs.remove(\"dir\")", "host.fs.remove", "dir"),
        (
            "host.fs.rename(\"gone.md\", \"there.md\")",
            "host.fs.rename",
            "gone.md",
        ),
    ] {
        let message = fails(&lua, code);
        assert!(message.contains(op), "{code}: {message}");
        assert!(message.contains(path), "{code}: {message}");
    }
}

#[test]
fn a_non_utf8_path_is_bytes_and_never_panics() {
    // macOS refuses non-UTF-8 file names, so this names no file: it shows
    // paths travel as bytes and failures name the call, without ever
    // requiring UTF-8.
    let setup = Setup::new();
    let lua = setup.lua();
    assert_eq!(
        eval(
            &lua,
            "return host.fs.stat(\"gone-\" .. string.char(255, 254))"
        ),
        Value::Null
    );
    for code in [
        "return host.fs.read(\"gone-\" .. string.char(255, 254))",
        "host.fs.write(\"gone-\" .. string.char(255, 254), \"x\")",
    ] {
        let message = fails(&lua, code);
        assert!(message.contains("host.fs."), "{code}: {message}");
    }
    let names = eval(&lua, "return host.fs.list(\".\")");
    assert_eq!(names, Value::Array(Vec::new()));
}

#[test]
fn data_dir_names_both_directories_and_creates_neither() {
    let setup = Setup::new();
    let lua = setup.lua();
    let machine: String = lua
        .load("return host.data_dir(\"machine\")")
        .eval()
        .unwrap();
    let project: String = lua
        .load("return host.data_dir(\"project\")")
        .eval()
        .unwrap();
    assert_eq!(
        machine,
        setup.home().join("data/fiber.test-notes").to_str().unwrap()
    );
    assert_eq!(
        project,
        setup
            .home()
            .join("projects/p/data/fiber.test-notes")
            .to_str()
            .unwrap()
    );
    assert!(!setup.home().join("data").exists());
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_write_inside_each_data_directory_creates_it() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(host.data_dir(\"machine\") .. \"/m.md\", \"m\")")
        .exec()
        .unwrap();
    assert_eq!(
        fs::read(setup.home().join("data/fiber.test-notes/m.md")).unwrap(),
        b"m"
    );
    lua.load("host.fs.mkdir(host.data_dir(\"project\") .. \"/deep\")")
        .exec()
        .unwrap();
    assert!(
        setup
            .home()
            .join("projects/p/data/fiber.test-notes/deep")
            .is_dir()
    );
}

#[test]
fn a_write_elsewhere_creates_nothing_and_a_sibling_prefix_is_outside() {
    let setup = Setup::new();
    let lua = setup.lua();
    let message = fails(&lua, "host.fs.write(\"no/such/parent/x.md\", \"x\")");
    assert!(message.contains("host.fs.write"), "{message}");
    assert!(!setup.workspace().join("no").exists());
    // `data/fiber.test-notes-2` merely shares a prefix with the data
    // directory; it is not inside it.
    lua.globals()
        .set(
            "sibling",
            setup
                .home()
                .join("data/fiber.test-notes-2/x.md")
                .to_str()
                .unwrap(),
        )
        .unwrap();
    let message = fails(&lua, "host.fs.write(sibling, \"x\")");
    assert!(message.contains("host.fs.write"), "{message}");
    assert!(!setup.home().join("data").exists());
    // A `..` that leaves the data directory is not inside it either.
    lua.globals()
        .set(
            "escape",
            setup
                .home()
                .join("data/fiber.test-notes/../other/x.md")
                .to_str()
                .unwrap(),
        )
        .unwrap();
    let message = fails(&lua, "host.fs.write(escape, \"x\")");
    assert!(message.contains("host.fs.write"), "{message}");
    assert!(!setup.home().join("data/fiber.test-notes").exists());
    assert!(!setup.home().join("data/other").exists());
}

#[test]
fn a_bad_or_missing_scope_is_the_scope_error() {
    let setup = Setup::new();
    let lua = setup.lua();
    for code in [
        "return host.data_dir(\"global\")",
        "return host.data_dir()",
        "return host.data_dir(nil)",
    ] {
        let message = fails(&lua, code);
        assert!(
            message.contains("host.data_dir: scope must be \"machine\" or \"project\""),
            "{code}: {message}"
        );
    }
}

#[test]
fn without_a_session_data_dir_raises_and_fs_takes_no_lock() {
    let setup = Setup::new();
    let lua = setup.lua_without_session();
    let message = fails(&lua, "return host.data_dir(\"machine\")");
    assert!(
        message.contains("host.data_dir: this extension has no session"),
        "{message}"
    );
    let absolute = setup.workspace().join("plain.md");
    lua.globals()
        .set("abs", absolute.to_str().unwrap())
        .unwrap();
    lua.load("host.fs.write(abs, \"hi\", { lock = true })")
        .exec()
        .unwrap();
    assert_eq!(fs::read(&absolute).unwrap(), b"hi");
    assert!(setup.locks.calls().is_empty());
}

#[test]
fn a_locked_write_waits_while_the_same_key_is_held() {
    let setup = Setup::new();
    let fs = setup.fs();
    let target = setup.workspace().join("notes/a.md");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let key = target.clone();
    let locks = Arc::clone(&setup.locks);
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let holder = thread::spawn(move || {
        locks.hold(&key, &mut || {
            entered.send(()).unwrap();
            release_rx.recv_timeout(DEADLINE).unwrap();
        });
    });
    entered_rx.recv_timeout(DEADLINE).unwrap();
    let (done, done_rx) = mpsc::channel();
    let writer = thread::spawn(move || {
        fs.write(b"notes/a.md", b"hi", true).unwrap();
        done.send(()).unwrap();
    });
    let probe = Arc::clone(&setup.locks);
    let (waiting, waiting_rx) = mpsc::channel();
    thread::spawn(move || {
        while probe.waiting() != 1 {
            thread::yield_now();
        }
        waiting.send(()).unwrap();
    });
    waiting_rx.recv_timeout(DEADLINE).unwrap();
    assert!(
        done_rx.try_recv().is_err(),
        "the locked write ran while the key was held"
    );
    release.send(()).unwrap();
    done_rx.recv_timeout(DEADLINE).unwrap();
    writer.join().unwrap();
    holder.join().unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"hi");
}

#[test]
fn rename_locks_both_keys_sorted_and_an_unlocked_write_asks_for_none() {
    let setup = Setup::new();
    let lua = setup.lua();
    lua.load("host.fs.write(\"z.txt\", \"x\")").exec().unwrap();
    assert!(setup.locks.calls().is_empty());
    let workspace = setup.workspace();
    lua.load("host.fs.rename(\"z.txt\", \"a.txt\", { lock = true })")
        .exec()
        .unwrap();
    assert_eq!(
        setup.locks.calls(),
        [workspace.join("a.txt"), workspace.join("z.txt")]
    );
}

#[test]
fn lock_keys_are_absolute_paths_under_the_workspace() {
    let setup = Setup::new();
    let lua = setup.lua();
    fs::create_dir(setup.workspace().join("notes")).unwrap();
    lua.load("host.fs.write(\"notes/a.md\", \"x\", { lock = true })")
        .exec()
        .unwrap();
    assert_eq!(setup.locks.calls(), [setup.workspace().join("notes/a.md")]);
}
