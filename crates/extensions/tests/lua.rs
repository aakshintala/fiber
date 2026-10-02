//! The Lua runtime through its public API, against the fixture extension in
//! `fakes` (`docs/extensions.md`, "Lua extensions", "How an extension runs",
//! "Loading, and cost when nothing is loaded" and "When an extension
//! misbehaves").

mod common;

use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{Setup, write};
use contract::ErrorCode;
use extensions::{Error, LuaExtension};

/// How long a test waits for one call before failing. Far past every timeout
/// the fixture declares, so it fires only when a callback is never stopped.
const WAIT: Duration = Duration::from_secs(10);

fn fixture() -> Arc<LuaExtension> {
    Arc::new(LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
    ))
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
        "_G,_VERSION,assert,collectgarbage,coroutine,error,fiber,getmetatable,host,ipairs,\
         json,load,math,next,pairs,pcall,rawequal,rawget,rawlen,rawset,require,select,\
         setmetatable,string,table,tonumber,tostring,type,utf8,xpcall"
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

    let err = call(
        &Arc::new(LuaExtension::new("ext", dir, setup.home())),
        "x",
        "",
    )
    .unwrap_err();
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
    let err = call(
        &Arc::new(LuaExtension::new("ext", dir, setup.home())),
        "x",
        "",
    )
    .unwrap_err();
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

    let err = call(
        &Arc::new(LuaExtension::new("ext", dir, setup.home())),
        "x",
        "",
    )
    .unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(
        message.starts_with("init.lua:1: `big.lua` is larger than"),
        "{message}"
    );
    assert!(message.contains("memory cap"), "{message}");
}

/// Accepts one connection, signals, and answers only when `release` arrives
/// or five seconds pass, so a parked `host.http` can be finished on purpose.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn answer_when_released(
    listener: std::net::TcpListener,
    accepted: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
) {
    let mut sock = listener.accept().unwrap().0;
    accepted.send(()).unwrap();
    match release.recv_timeout(Duration::from_secs(5)) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
    }
    drop(sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"));
}

#[test]
fn a_second_command_waits_until_the_parked_command_finishes() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(listener, accepted_tx, release_rx));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"first\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"second\", {{ timeout = 5000, run = function() return \"second\" end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let first = Arc::clone(&ext);
    let (first_tx, first_rx) = mpsc::channel();
    std::thread::spawn(move || first_tx.send(first.command("first", "")));
    accepted_rx
        .recv_timeout(WAIT)
        .expect("the first command never reached the server");
    assert_eq!(ext.provider_functions("p").unwrap(), ["sign"]);
    let second = Arc::clone(&ext);
    let (second_tx, second_rx) = mpsc::channel();
    std::thread::spawn(move || second_tx.send(second.command("second", "")));
    assert!(
        second_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "the second command started while the first was parked"
    );
    release_tx.send(()).unwrap();
    let first = first_rx.recv_timeout(WAIT).unwrap().unwrap();
    let second = second_rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!((first.as_str(), second.as_str()), ("ok", "second"));
    assert!(ext.is_running());
}

#[test]
fn a_queued_command_times_out_on_its_own_deadline_and_the_vm_stays() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(listener, accepted_tx, release_rx));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "seen = \"no\"\n\
             fiber.command(\"slow\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 300, run = function() seen = \"yes\"; return \"ran\" end }})\n\
             fiber.command(\"after\", {{ timeout = 1000, run = function() return seen end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let slow = Arc::clone(&ext);
    let slow = std::thread::spawn(move || slow.command("slow", ""));
    accepted_rx
        .recv_timeout(WAIT)
        .expect("the slow command never reached the server");
    let started = Instant::now();
    let err = call(&ext, "quick", "").unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "quick waited {:?}, not its own 300 ms",
        started.elapsed()
    );
    let Error::Timeout {
        callback,
        timeout_ms,
        ..
    } = &err
    else {
        panic!("{err:?}")
    };
    assert_eq!((callback.as_str(), *timeout_ms), ("quick", 300));
    assert!(ext.is_running(), "the queued timeout stopped the extension");
    release_tx.send(()).unwrap();
    assert!(slow.join().unwrap().is_ok());
    assert_eq!(call(&ext, "after", "").unwrap(), "no");
}

#[test]
fn a_call_made_before_registration_finishes_uses_its_declared_timeout() {
    let setup = Setup::new();
    let reg = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let work = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let reg_url = format!("http://{}/", reg.local_addr().unwrap());
    let work_url = format!("http://{}/", work.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(reg, accepted_tx, release_rx));
    std::thread::spawn(move || answer_after(work, Duration::from_millis(1200), 2));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "host.http({{ url = \"{reg_url}\" }})\n\
             fiber.command(\"work\", {{ timeout = 5000, run = function() return host.http({{ url = \"{work_url}\" }}).body end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let first = Arc::clone(&ext);
    let first = std::thread::spawn(move || first.command("work", ""));
    accepted_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    let started = Instant::now();
    let second = Arc::clone(&ext);
    let second = std::thread::spawn(move || second.command("work", ""));
    release_tx.send(()).unwrap();
    assert_eq!(second.join().unwrap().unwrap(), "ok");
    assert!(
        started.elapsed() > Duration::from_secs(1),
        "work returned in {:?}",
        started.elapsed()
    );
    assert!(first.join().unwrap().is_ok());
    assert!(ext.is_running());
}

#[test]
fn a_command_named_like_a_provider_function_keeps_its_own_timeout() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    std::thread::spawn(move || answer_after(listener, Duration::from_millis(300), 1));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"p.sign\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 50, run = function() return {{}} end }} }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let started = Instant::now();
    assert_eq!(call(&ext, "p.sign", "").unwrap(), "ok");
    let elapsed = started.elapsed();
    assert!(
        elapsed > Duration::from_millis(200) && elapsed < Duration::from_secs(2),
        "p.sign took {elapsed:?}, not its own timeout"
    );
}

#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn answer_after(listener: std::net::TcpListener, delay: Duration, times: usize) {
    for _ in 0..times {
        let mut sock = listener.accept().unwrap().0;
        std::thread::sleep(delay);
        drop(
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"),
        );
    }
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
    let err = call(
        &Arc::new(LuaExtension::new("ext", dir, setup.home())),
        "x",
        "",
    )
    .unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.starts_with("init.lua:1: big.lua:"), "{message}");
}
