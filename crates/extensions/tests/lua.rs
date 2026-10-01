//! The Lua runtime through its public API, against the fixture extension in
//! `fakes` (`docs/extensions.md`, "Lua extensions", "How an extension runs",
//! "Loading, and cost when nothing is loaded" and "When an extension
//! misbehaves").

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, write};
use contract::ErrorCode;
use extensions::{Error, LuaExtension};

/// How long a test waits for one call before failing. Far past every timeout
/// the fixture declares, so it fires only when a callback is never stopped.
const WAIT: Duration = Duration::from_secs(10);

fn fixture() -> Arc<LuaExtension> {
    Arc::new(LuaExtension::new("fixture", fakes::lua_fixture()))
}

/// Runs one command on its own thread under `WAIT`, so a callback the runtime
/// fails to stop fails the test instead of hanging it.
#[allow(clippy::panic, reason = "a test helper; a hang is the test's failure")]
fn call(ext: &Arc<LuaExtension>, command: &str, text: &str) -> Result<String, Error> {
    let (tx, rx) = mpsc::channel();
    let (ext, name, text) = (Arc::clone(ext), command.to_owned(), text.to_owned());
    std::thread::spawn(move || tx.send(ext.command(&name, &text)));
    match rx.recv_timeout(WAIT) {
        Ok(result) => result,
        Err(_) => panic!("`{command}` did not return within {WAIT:?}"),
    }
}

#[test]
fn no_vm_exists_until_the_extension_is_first_called() {
    let ext = fixture();
    assert!(!ext.is_running());
    assert_eq!(call(&ext, "echo", "hi").unwrap(), "hi");
    assert!(ext.is_running());
}

#[test]
fn only_the_stripped_library_and_the_host_globals_exist() {
    let names = call(&fixture(), "globals", "").unwrap();
    assert_eq!(
        names,
        "_G,_VERSION,assert,collectgarbage,coroutine,error,fiber,getmetatable,ipairs,load,\
         math,next,pairs,pcall,rawequal,rawget,rawlen,rawset,require,select,setmetatable,\
         string,table,tonumber,tostring,type,utf8,xpcall"
    );
}

#[test]
fn require_loads_a_module_from_the_extensions_directory_once() {
    assert_eq!(
        call(&fixture(), "greet", "fiber").unwrap(),
        "hello, fiber (cached)"
    );
}

#[cfg(unix)]
#[test]
fn require_cannot_leave_the_extensions_directory() {
    let setup = Setup::new();
    let outside = setup.workspace().join("outside.lua");
    write(&outside, "return {}");
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "local m = require(\"outside\")\n");
    std::os::unix::fs::symlink(&outside, dir.join("outside.lua")).unwrap();

    let err = call(&Arc::new(LuaExtension::new("ext", dir)), "x", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.starts_with("init.lua:1: "), "{message}");
    assert!(
        message.contains("outside the extension's directory"),
        "{message}"
    );
}

#[test]
fn an_error_names_the_extensions_file_and_line() {
    let line = std::fs::read_to_string(fakes::lua_fixture().join("init.lua"))
        .unwrap()
        .lines()
        .position(|l| l.contains("error(\"boom\")"))
        .unwrap()
        + 1;
    let ext = fixture();
    let err = call(&ext, "fail", "").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    let Error::Lua { extension, message } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(extension, "fixture");
    assert_eq!(message, &format!("init.lua:{line}: boom"));
    // The session survives and the VM stays usable.
    assert_eq!(call(&ext, "echo", "after").unwrap(), "after");
}

#[test]
fn a_registration_without_a_timeout_is_an_error_at_its_line() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        "-- no timeout\nfiber.command(\"x\", { run = function() end })\n",
    );
    let err = call(&Arc::new(LuaExtension::new("ext", dir)), "x", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.starts_with("init.lua:2: fiber.command"),
        "{message}"
    );
}

#[test]
fn a_command_the_extension_never_registered_is_unknown() {
    let err = call(&fixture(), "nope", "").unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownCommand);
}

#[test]
fn a_loop_is_stopped_at_its_timeout_in_a_callback_a_coroutine_and_under_pcall() {
    let ext = fixture();
    for command in [
        "spin",
        "spin_pcall",
        "spin_create",
        "spin_wrap",
        "spin_nested",
    ] {
        let err = call(&ext, command, "").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        let Error::Timeout {
            callback,
            timeout_ms,
            ..
        } = &err
        else {
            panic!("{command}: {err:?}")
        };
        assert_eq!((callback.as_str(), *timeout_ms), (command, 50));
        assert_eq!(call(&ext, "echo", command).unwrap(), command);
    }
}

#[test]
fn a_wrapped_coroutine_still_yields_values() {
    assert_eq!(call(&fixture(), "count", "").unwrap(), "6");
}

#[test]
fn unbounded_allocation_is_an_error_in_that_vm_and_the_vm_stays_usable() {
    let ext = fixture();
    for _ in 0..2 {
        let err = call(&ext, "grow", "").unwrap_err();
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert!(message.contains("not enough memory"), "{message}");
        assert_eq!(call(&ext, "echo", "alive").unwrap(), "alive");
    }
}

#[test]
fn a_callback_the_hook_cannot_stop_abandons_the_vm_and_the_session_survives() {
    for command in ["spin_gc", "spin_find"] {
        let ext = fixture();
        let err = call(&ext, command, "").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{command}: {err:?}")
        };
        assert_eq!(callback, command);
        assert!(!ext.is_running());
        let err = call(&ext, "echo", "").unwrap_err();
        assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
        // The rest of the session goes on: another extension still runs.
        assert_eq!(call(&fixture(), "echo", "on").unwrap(), "on");
    }
}

#[test]
fn a_lua_error_releases_the_extensions_lock() {
    let ext = fixture();
    assert!(matches!(call(&ext, "fail", ""), Err(Error::Lua { .. })));
    // A second thread takes the lock the failed call held.
    let other = Arc::clone(&ext);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(other.is_running()));
    assert_eq!(rx.recv_timeout(WAIT), Ok(true));
    assert_eq!(call(&ext, "echo", "free").unwrap(), "free");
}

#[test]
fn a_module_larger_than_the_memory_cap_is_not_read() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "local m = require(\"big\")\n");
    let big = std::fs::File::create(dir.join("big.lua")).unwrap();
    big.set_len(u64::try_from(extensions::MEMORY_CAP).unwrap() + 1)
        .unwrap();

    let err = call(&Arc::new(LuaExtension::new("ext", dir)), "x", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.starts_with("init.lua:1: `big.lua` is larger than"),
        "{message}"
    );
    assert!(message.contains("memory cap"), "{message}");
}

#[test]
fn a_module_exactly_the_memory_cap_is_read() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "local m = require(\"big\")\n");
    let big = std::fs::File::create(dir.join("big.lua")).unwrap();
    big.set_len(u64::try_from(extensions::MEMORY_CAP).unwrap())
        .unwrap();

    // Read and compiled: its zero bytes are not Lua.
    let err = call(&Arc::new(LuaExtension::new("ext", dir)), "x", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.starts_with("init.lua:1: big.lua:"), "{message}");
}
