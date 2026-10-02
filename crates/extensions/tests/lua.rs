//! The Lua runtime through its public API, against the fixture extension in
//! `fakes` (`docs/extensions.md`, "Lua extensions", "How an extension runs",
//! "Loading, and cost when nothing is loaded" and "When an extension
//! misbehaves").

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
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

const HTTP_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

/// Reads one request. The bytes are the signal that ureq has set its socket
/// timeout and finished sending: answering at `accept` closes the socket in
/// the gap before that `setsockopt`, and macOS returns EINVAL.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn read_request(sock: &mut TcpStream) {
    let mut got = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = sock.read(&mut buf).expect("reading the request");
        assert!(n > 0, "the client closed before its request");
        got.extend_from_slice(buf.get(..n).unwrap());
        if got.windows(4).any(|w| w == b"\r\n\r\n") {
            return;
        }
        assert!(got.len() <= 8192, "the request header never ended");
    }
}

/// Accepts one connection, reads the request, signals, and answers when
/// `release` arrives, so a parked `host.http` can be finished on purpose.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn answer_when_released(
    listener: std::net::TcpListener,
    accepted: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
) {
    let mut sock = listener.accept().unwrap().0;
    read_request(&mut sock);
    accepted.send(()).unwrap();
    if release.recv_timeout(WAIT).is_ok() {
        sock.write_all(HTTP_OK).unwrap();
    }
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
    let slow = start(&ext, "slow");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("the slow command never reached the server");
    // Under 5s, so a bug that waits out `slow` fails here. Far past 300ms,
    // so a loaded runner can still deliver the timeout.
    let quick = start(&ext, "quick");
    let err = quick
        .recv_timeout(Duration::from_secs(4))
        .expect("quick did not time out on its own deadline")
        .unwrap_err();
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
    assert!(slow.recv_timeout(WAIT).unwrap().is_ok());
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
    let (work_ok_tx, work_ok_rx) = mpsc::channel();
    let (work_release_tx, work_release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(reg, accepted_tx, release_rx));
    std::thread::spawn(move || answer_n(work, work_ok_tx, work_release_rx, 2));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "host.http({{ url = \"{reg_url}\" }})\n\
             fiber.command(\"work\", {{ timeout = 5000, run = function() return host.http({{ url = \"{work_url}\" }}).body end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    // Both calls are queued while the entry script is still in `host.http`.
    // Each then reaches the work server, so the timeout it declared at
    // registration was still ahead: an already-expired deadline never connects.
    let first = start(&ext, "work");
    let second = start(&ext, "work");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    release_tx.send(()).unwrap();
    for _ in 0..2 {
        work_ok_rx
            .recv_timeout(WAIT)
            .expect("work never reached the server");
        work_release_tx.send(()).unwrap();
    }
    assert_eq!(first.recv_timeout(WAIT).unwrap().unwrap(), "ok");
    assert_eq!(second.recv_timeout(WAIT).unwrap().unwrap(), "ok");
    assert!(ext.is_running());
}

#[test]
fn a_call_still_waiting_on_registration_times_out_from_when_it_was_asked() {
    let setup = Setup::new();
    let reg = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hold = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let reg_url = format!("http://{}/", reg.local_addr().unwrap());
    let hold_url = format!("http://{}/", hold.local_addr().unwrap());
    let (reg_ok_tx, reg_ok_rx) = mpsc::channel();
    let (reg_release_tx, reg_release_rx) = mpsc::channel();
    let (hold_ok_tx, hold_ok_rx) = mpsc::channel();
    let (hold_release_tx, hold_release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(reg, reg_ok_tx, reg_release_rx));
    std::thread::spawn(move || answer_when_released(hold, hold_ok_tx, hold_release_rx));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "host.http({{ url = \"{reg_url}\" }})\n\
             fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{hold_url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 400, run = function() return \"ran\" end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    // `hold` is in the entry script before `quick` is asked, so `quick` is
    // queued behind it. Starting both at once lets `quick` win the queue.
    let held = start(&ext, "hold");
    reg_ok_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    let quick = start(&ext, "quick");
    reg_release_tx.send(()).unwrap();
    hold_ok_rx
        .recv_timeout(WAIT)
        .expect("hold never reached the server");
    // Still parked on `hold`, so this is quick's own deadline, not hold's.
    let err = quick
        .recv_timeout(Duration::from_secs(3))
        .expect("quick did not time out while hold was parked")
        .unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 400);
    assert!(ext.is_running());
    hold_release_tx.send(()).unwrap();
    assert!(held.recv_timeout(WAIT).unwrap().is_ok());
}

#[test]
fn a_command_named_like_a_provider_function_keeps_its_own_timeout() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (ok_tx, ok_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_n(listener, ok_tx, release_rx, 1));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"p.sign\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 50, run = function() return {{}} end }} }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let ran = start(&ext, "p.sign");
    ok_rx
        .recv_timeout(WAIT)
        .expect("p.sign never reached the server");
    // Past the provider function's 50ms, and the command has not returned:
    // it is still the command's own timeout.
    assert!(
        ran.recv_timeout(Duration::from_millis(300)).is_err(),
        "p.sign used the provider function's 50 ms timeout"
    );
    release_tx.send(()).unwrap();
    assert_eq!(ran.recv_timeout(WAIT).unwrap().unwrap(), "ok");
}

#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn answer_n(
    listener: std::net::TcpListener,
    accepted: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    times: usize,
) {
    for _ in 0..times {
        let mut sock = listener.accept().unwrap().0;
        read_request(&mut sock);
        accepted.send(()).unwrap();
        if release.recv_timeout(WAIT).is_ok() {
            sock.write_all(HTTP_OK).unwrap();
        }
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

/// Starts `command` on `ext` on its own thread; the result arrives on the
/// receiver.
fn start(ext: &Arc<LuaExtension>, command: &'static str) -> mpsc::Receiver<Result<String, Error>> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.command(command, "")));
    rx
}

/// The verify-3 hang: an entry script the hook cannot stop, with two calls
/// waiting on it. Both get `Abandoned` past its deadline and grace, neither
/// waits forever, and a later call gets it at once.
#[test]
fn every_call_waiting_on_an_entry_script_the_hook_cannot_stop_is_abandoned() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        "setmetatable({}, { __gc = function() while true do end end })\ncollectgarbage()\n",
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let waiters = [start(&ext, "a"), start(&ext, "b")];
    for waiter in waiters {
        let err = waiter.recv_timeout(WAIT).unwrap().unwrap_err();
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(callback, "init.lua");
    }
    assert!(!ext.is_running());
    let again = start(&ext, "a");
    let err = again.recv_timeout(WAIT).unwrap().unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
}

/// An entry script that errors stops the extension with its error, for
/// every call that waited on it and every later call.
#[test]
fn every_call_waiting_on_an_entry_script_that_errors_gets_its_error() {
    let setup = Setup::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(listener, accepted_tx, release_rx));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!("host.http({{ url = \"{url}\" }})\nerror(\"bad\")\n"),
    );
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    let first = start(&ext, "a");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    let second = start(&ext, "b");
    release_tx.send(()).unwrap();
    for waiter in [first, second] {
        let err = waiter.recv_timeout(WAIT).unwrap().unwrap_err();
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(message, "init.lua:2: bad");
    }
    assert!(!ext.is_running());
    assert!(matches!(call(&ext, "a", ""), Err(Error::Lua { .. })));
}

/// An entry script the hook stops at its deadline stops the extension with
/// that timeout.
#[test]
fn an_entry_script_past_its_deadline_stops_the_extension_with_its_timeout() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "while true do end\n");
    let ext = Arc::new(LuaExtension::new("ext", dir, setup.home()));
    for _ in 0..2 {
        let err = call(&ext, "a", "").unwrap_err();
        let Error::Timeout {
            callback,
            timeout_ms,
            ..
        } = &err
        else {
            panic!("{err:?}")
        };
        assert_eq!((callback.as_str(), *timeout_ms), ("init.lua", 2000));
    }
    assert!(!ext.is_running());
}

/// An extension whose directory is gone fails every call with that.
#[test]
fn an_extension_whose_directory_is_gone_fails_every_call() {
    let setup = Setup::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        setup.home().join("gone"),
        setup.home(),
    ));
    for _ in 0..2 {
        let err = call(&ext, "a", "").unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
    }
}
