//! The Lua runtime through its public API, against the fixture extension in
//! `fakes` (`docs/extensions.md`, "Lua extensions", "How an extension runs",
//! "Loading, and cost when nothing is loaded" and "When an extension
//! misbehaves").

mod common;

use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, write};
use contract::ErrorCode;
use contract::clock::Clock;
use extensions::{Error, LuaExtension};
use fakes::clock::FakeClock;

/// How long a test waits for one call before failing. A callback that is
/// stopped returns on the fake clock, so this fires only when one is never
/// stopped.
const WAIT: Duration = Duration::from_secs(5);

/// Reading, compiling and running an entry script is bounded at 2 seconds
/// (`docs/extensions.md`).
const LOAD: Duration = Duration::from_secs(2);

/// When the hook cannot stop the VM, the caller waits 1 second more
/// (`docs/extensions.md`).
const GRACE: Duration = Duration::from_secs(1);

fn fixture() -> Arc<LuaExtension> {
    extension(
        "fixture",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    )
}

fn extension(
    name: &str,
    dir: impl Into<std::path::PathBuf>,
    home: impl Into<std::path::PathBuf>,
    clock: Arc<FakeClock>,
) -> Arc<LuaExtension> {
    Arc::new(LuaExtension::new(name, dir, home, clock))
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

/// Starts `command` on `ext` on its own thread; the result arrives on the
/// receiver.
fn start(ext: &Arc<LuaExtension>, command: &'static str) -> mpsc::Receiver<Result<String, Error>> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.command(command, "")));
    rx
}

/// Reads an HTTP head, through the blank line that ends it.
fn read_head(sock: &mut impl Read) {
    let mut buf = [0; 1];
    let mut seen = Vec::new();
    loop {
        match sock.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => seen.push(buf[0]),
        }
        if seen.ends_with(b"\r\n\r\n") {
            break;
        }
    }
}

/// `require("hold")` blocks in the loader's read of this fifo. The receiver
/// fires once that read has opened the file, which is after the VM's last
/// clock check: the instruction hook does not run during the read. Sending
/// on the returned sender, or dropping it, ends the read.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn hold_open(dir: &std::path::Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let path = dir.join("hold.lua");
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        match release_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        drop(held);
    });
    (rx, release_tx)
}

/// `require("go_<name>")` is a signal, not a barrier. Opening the fifo for
/// write returns only once the loader has opened it for read; the writer
/// reports that and closes the fifo at once, so `require` reads an empty
/// module and the code runs on. Nothing stays blocked in it. Each name is
/// used once per VM, because `require` caches.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn go_module(dir: &std::path::Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        drop(held);
    });
    rx
}

/// True once `count` threads are parked in `wait_until` at `until`.
fn parked_at(clock: &Arc<FakeClock>, until: std::time::Instant, count: usize) -> bool {
    clock.await_parked_count(until, count, WAIT)
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
        &extension("ext", dir, setup.home(), FakeClock::new()),
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
fn a_registration_without_a_timeout_leaves_a_problem_and_registers_nothing() {
    // Ruling 5 on #561: a bad `fiber.command` spec leaves a problem (an
    // `extension_failed` notice) and the entry script goes on, instead of
    // raising. The command is never registered, so calling it is unknown.
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        "-- no timeout\nfiber.command(\"x\", { run = function() end })\n",
    );
    let err = call(
        &extension("ext", dir, setup.home(), FakeClock::new()),
        "x",
        "",
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownCommand);
}

#[test]
fn a_command_the_extension_never_registered_is_unknown() {
    let err = call(&fixture(), "nope", "").unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownCommand);
}

/// A `while true` loop, in `body` after the go module, is stopped at the
/// callback's 50 ms timeout. The go module is the callback's first statement,
/// so the advance lands after the VM's clock check, and the caller is already
/// parked at the grace. One case per test: five of them in one test would put
/// its deadlines over the 60 s sum.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
fn spinning_callback_stops_at_its_timeout(command: &'static str, body: &str) {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"echo\", {{ timeout = 1000, run = function(text) return text end }})\n\
             fiber.command(\"{command}\", {{ timeout = 50, run = function()\n\
             require(\"go_{command}\")\n\
             {body}end }})\n"
        ),
    );
    let went = go_module(&dir, command);
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let asked = clock.now();
    let result = start(&ext, command);
    went.recv_timeout(WAIT)
        .expect("waited for the callback to pass its clock check");
    let parked_at = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(parked_at, WAIT),
        "waited for {command} to park at its grace"
    );
    // The hook stops the loop at the deadline. The grace is the caller's
    // wait, and it has not run out.
    clock.advance(Duration::from_millis(50));
    let err = result
        .recv_timeout(WAIT)
        .expect("waited for the spinning callback")
        .unwrap_err();
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

#[test]
fn a_loop_is_stopped_at_its_timeout_in_a_callback() {
    spinning_callback_stops_at_its_timeout("spin", "while true do end\n");
}

#[test]
fn a_loop_is_stopped_at_its_timeout_under_pcall() {
    spinning_callback_stops_at_its_timeout(
        "spin_pcall",
        "while true do pcall(function() while true do end end) end\n",
    );
}

#[test]
fn a_loop_is_stopped_at_its_timeout_in_a_created_coroutine() {
    spinning_callback_stops_at_its_timeout(
        "spin_create",
        "local co = coroutine.create(function() while true do end end)\n\
         return tostring(coroutine.resume(co))\n",
    );
}

#[test]
fn a_loop_is_stopped_at_its_timeout_in_a_wrapped_coroutine() {
    spinning_callback_stops_at_its_timeout(
        "spin_wrap",
        "coroutine.wrap(function()\n\
           while true do pcall(function() while true do end end) end\n\
         end)()\n",
    );
}

#[test]
fn a_loop_is_stopped_at_its_timeout_in_nested_coroutines() {
    spinning_callback_stops_at_its_timeout(
        "spin_nested",
        "return coroutine.wrap(function()\n\
           local inner = coroutine.create(function() while true do end end)\n\
           return tostring(coroutine.resume(inner))\n\
         end)()\n",
    );
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
    // The go module is the callback's first statement. `spin_gc` then loops
    // in a finalizer, where the hook is off. `spin_find` is one backtracking
    // C call, which the hook does not interrupt. The advance comes after the
    // go module and after the caller has parked at the grace.
    for (command, body) in [
        (
            "spin_gc",
            "setmetatable({}, { __gc = function() while true do end end })\ncollectgarbage()\n",
        ),
        (
            "spin_find",
            "return tostring(string.find(string.rep(\"a\", 100000), \"a*a*a*a*b\"))\n",
        ),
    ] {
        let setup = Setup::new();
        let dir = setup.home().join("ext");
        write(
            &dir.join("init.lua"),
            &format!(
                "fiber.command(\"{command}\", {{ timeout = 50, run = function()\n\
                 require(\"go_{command}\")\n\
                 {body}end }})\n"
            ),
        );
        let went = go_module(&dir, command);
        let clock = FakeClock::new();
        let ext = extension("ext", dir, setup.home(), clock.clone());
        let asked = clock.now();
        let result = start(&ext, command);
        went.recv_timeout(WAIT)
            .expect("waited for the callback to pass its clock check");
        let parked_at = asked + Duration::from_millis(50) + GRACE;
        assert!(
            clock.await_parked(parked_at, WAIT),
            "waited for {command} to park at its grace"
        );
        clock.advance(Duration::from_millis(50) + GRACE);
        let err = result
            .recv_timeout(WAIT)
            .expect("waited for the callback the hook cannot stop")
            .unwrap_err();
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
    assert!(
        rx.recv_timeout(WAIT)
            .expect("waited for the lock to be taken")
    );
    assert_eq!(call(&ext, "echo", "free").unwrap(), "free");
}

#[test]
fn the_default_memory_cap_is_one_mib() {
    assert_eq!(extensions::MEMORY_CAP, 1 << 20);
}

#[test]
fn allocation_past_the_default_cap_fails_and_a_raised_cap_allows_it() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        r#"fiber.command("alloc", { timeout = 5000, run = function()
  return #string.rep("x", 1536 * 1024)
end })"#,
    );
    let clock = FakeClock::new();
    let ext = extension("ext", dir.clone(), setup.home(), clock.clone());
    let err = call(&ext, "alloc", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.contains("not enough memory"), "{message}");
    let raised = Arc::new(
        LuaExtension::new("ext", dir, setup.home(), clock)
            .with_memory_cap(NonZeroUsize::new(4 << 20).unwrap()),
    );
    assert_eq!(call(&raised, "alloc", "").unwrap(), "1572864");
}

#[test]
fn a_raised_cap_reads_a_file_past_the_default_cap() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "local m = require(\"big\")\n");
    let big = std::fs::File::create(dir.join("big.lua")).unwrap();
    big.set_len(u64::try_from(extensions::MEMORY_CAP).unwrap() + 1)
        .unwrap();
    let ext = Arc::new(
        LuaExtension::new("ext", dir, setup.home(), FakeClock::new())
            .with_memory_cap(NonZeroUsize::new(2 << 20).unwrap()),
    );
    let err = call(&ext, "x", "").unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.starts_with("init.lua:1: big.lua:"), "{message}");
    assert!(!message.contains("is larger than"), "{message}");
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
        &extension("ext", dir, setup.home(), FakeClock::new()),
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

/// Accepts one connection, reads its head, signals, and answers when
/// `release` arrives or is dropped, so a parked `host.http` stays parked
/// until the test says.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn answer_when_released(
    listener: std::net::TcpListener,
    accepted: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
) {
    let mut sock = listener.accept().unwrap().0;
    read_head(&mut sock);
    accepted.send(()).unwrap();
    match release.recv_timeout(Duration::from_secs(5)) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
    }
    drop(sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"));
}

/// A command queued behind a parked command runs once the parked command
/// finishes. That it does not start while parked is proved by the unit test
/// `a_command_queued_behind_a_parked_command_does_not_start`.
#[test]
fn a_command_queued_behind_a_parked_command_runs_once_it_finishes() {
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
    let ext = extension("ext", dir, setup.home(), FakeClock::new());
    let first = start(&ext, "first");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("waited for the first command to reach the server");
    let (listed_tx, listed_rx) = mpsc::channel();
    let listed = Arc::clone(&ext);
    std::thread::spawn(move || listed_tx.send(listed.provider_functions("p")));
    assert_eq!(
        listed_rx
            .recv_timeout(WAIT)
            .expect("waited for the provider's functions")
            .unwrap(),
        ["sign"]
    );
    let second = start(&ext, "second");
    release_tx.send(()).unwrap();
    let first = first
        .recv_timeout(WAIT)
        .expect("waited for the first command")
        .unwrap();
    let second = second
        .recv_timeout(WAIT)
        .expect("waited for the second command")
        .unwrap();
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
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let slow = start(&ext, "slow");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("waited for the slow command to reach the server");
    let asked = clock.now();
    let quick = start(&ext, "quick");
    assert!(
        clock.await_parked(asked + Duration::from_millis(300), WAIT),
        "waited for quick to park at its timeout"
    );
    clock.advance(Duration::from_millis(300));
    let err = quick
        .recv_timeout(WAIT)
        .expect("waited for quick")
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
    assert!(
        slow.recv_timeout(WAIT)
            .expect("waited for the slow command")
            .is_ok()
    );
    assert_eq!(call(&ext, "after", "").unwrap(), "no");
}

#[test]
fn a_call_made_before_registration_finishes_uses_its_declared_timeout() {
    let setup = Setup::new();
    let work = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let work_url = format!("http://{}/", work.local_addr().unwrap());
    let (work_ok_tx, _work_ok_rx) = mpsc::channel();
    let (work_release_tx, work_release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_n(work, work_ok_tx, work_release_rx, 2));
    // A call that ignores time spent registering runs `work` and gets an
    // answer at once, so the timeout assertion fails.
    drop(work_release_tx);
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "require(\"hold\")\n\
             fiber.command(\"work\", {{ timeout = 1000, run = function() return host.http({{ url = \"{work_url}\" }}).body end }})\n"
        ),
    );
    // The entry script's own `host.http` is bounded by the 2s load deadline
    // plus the grace, as wall time. A fifo read is not, and it is past the
    // entry script's clock checks.
    let (held, release) = hold_open(&dir);
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let asked = clock.now();
    let first = start(&ext, "work");
    held.recv_timeout(WAIT)
        .expect("waited for the entry script to block past its clock checks");
    let second = start(&ext, "work");
    // Both wait on the entry script's grace, which is later than the
    // command's own timeout. Advancing that timeout does not abandon the VM.
    assert!(
        parked_at(&clock, asked + LOAD + GRACE, 2),
        "waited for both calls to park on registration"
    );
    clock.advance(Duration::from_millis(1000));
    release.send(()).unwrap();
    for waiter in [first, second] {
        let err = waiter
            .recv_timeout(WAIT)
            .expect("waited for work")
            .unwrap_err();
        let Error::Timeout { timeout_ms, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(*timeout_ms, 1000);
    }
    assert!(ext.is_running(), "a queued timeout stopped the extension");
}

#[test]
fn a_call_still_waiting_on_registration_times_out_from_when_it_was_asked() {
    let setup = Setup::new();
    let hold = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hold_url = format!("http://{}/", hold.local_addr().unwrap());
    let (hold_ok_tx, hold_ok_rx) = mpsc::channel();
    let (hold_release_tx, hold_release_rx) = mpsc::channel();
    std::thread::spawn(move || answer_when_released(hold, hold_ok_tx, hold_release_rx));
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "require(\"hold\")\n\
             fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{hold_url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 400, run = function() return \"ran\" end }})\n"
        ),
    );
    let (opened, release) = hold_open(&dir);
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let asked = clock.now();
    let held = start(&ext, "hold");
    opened
        .recv_timeout(WAIT)
        .expect("waited for the entry script to block past its clock checks");
    let quick = start(&ext, "quick");
    assert!(
        parked_at(&clock, asked + LOAD + GRACE, 2),
        "waited for hold and quick to park on registration"
    );
    clock.advance(Duration::from_millis(400));
    release.send(()).unwrap();
    let err = quick
        .recv_timeout(WAIT)
        .expect("waited for quick")
        .unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 400);
    assert!(ext.is_running());
    hold_ok_rx
        .recv_timeout(WAIT)
        .expect("waited for hold to reach the server");
    hold_release_tx.send(()).unwrap();
    assert!(held.recv_timeout(WAIT).expect("waited for hold").is_ok());
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
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let asked = clock.now();
    let ran = start(&ext, "p.sign");
    ok_rx
        .recv_timeout(WAIT)
        .expect("waited for p.sign to reach the server");
    let grace = asked + Duration::from_millis(5000) + GRACE;
    assert!(
        clock.await_parked(grace, WAIT),
        "waited for p.sign to park at its own grace"
    );
    // The provider function's 50 ms would already have failed a parked call.
    // Advancing wakes the caller; it parks again at the same grace.
    clock.advance(Duration::from_millis(50));
    assert!(
        clock.await_parked(grace, WAIT),
        "p.sign left its own deadline"
    );
    assert!(
        ran.try_recv().is_err(),
        "p.sign returned on the provider function's timeout"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        ran.recv_timeout(WAIT).expect("waited for p.sign").unwrap(),
        "ok"
    );
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
        read_head(&mut sock);
        accepted.send(()).unwrap();
        match release.recv_timeout(Duration::from_secs(8)) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
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
        &extension("ext", dir, setup.home(), FakeClock::new()),
        "x",
        "",
    )
    .unwrap_err();
    let Error::Lua { message, .. } = &err else {
        panic!("{err:?}")
    };
    assert!(message.starts_with("init.lua:1: big.lua:"), "{message}");
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
        "require(\"go_entry\")\n\
         setmetatable({}, { __gc = function() while true do end end })\n\
         collectgarbage()\n",
    );
    let went = go_module(&dir, "entry");
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let abandon = clock.now() + LOAD + GRACE;
    let waiters = [start(&ext, "a"), start(&ext, "b")];
    went.recv_timeout(WAIT)
        .expect("waited for the entry script to pass its clock check");
    assert!(
        parked_at(&clock, abandon, 2),
        "waited for both callers to park until the load grace"
    );
    clock.advance(LOAD + GRACE);
    for waiter in waiters {
        let err = waiter
            .recv_timeout(WAIT)
            .expect("waited for a caller of the entry script")
            .unwrap_err();
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(callback, "init.lua");
    }
    assert!(!ext.is_running());
    assert!(matches!(call(&ext, "a", ""), Err(Error::Abandoned { .. })));
}

/// An entry script that errors stops the extension with its error, for
/// every call that waited on it and every later call.
#[test]
fn every_call_waiting_on_an_entry_script_that_errors_gets_its_error() {
    let setup = Setup::new();
    let dir = setup.home().join("ext");
    write(&dir.join("init.lua"), "require(\"hold\")\nerror(\"bad\")\n");
    let (held, release) = hold_open(&dir);
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let abandon = clock.now() + LOAD + GRACE;
    let first = start(&ext, "a");
    held.recv_timeout(WAIT)
        .expect("waited for the entry script to block past its clock checks");
    let second = start(&ext, "b");
    assert!(
        parked_at(&clock, abandon, 2),
        "waited for both callers to park on registration"
    );
    release.send(()).unwrap();
    for waiter in [first, second] {
        let err = waiter
            .recv_timeout(WAIT)
            .expect("waited for a caller of the entry script")
            .unwrap_err();
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
    write(
        &dir.join("init.lua"),
        "require(\"go_init\")\nwhile true do end\n",
    );
    let went = go_module(&dir, "init");
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let abandon = clock.now() + LOAD + GRACE;
    let first = start(&ext, "a");
    went.recv_timeout(WAIT)
        .expect("waited for the entry script to pass its clock check");
    assert!(
        clock.await_parked(abandon, WAIT),
        "waited for the caller to park until the load grace"
    );
    // The hook's deadline is the 2 second load bound, before the grace.
    clock.advance(LOAD);
    let err = first
        .recv_timeout(WAIT)
        .expect("waited for the entry script")
        .unwrap_err();
    let Error::Timeout {
        callback,
        timeout_ms,
        ..
    } = &err
    else {
        panic!("{err:?}")
    };
    assert_eq!((callback.as_str(), *timeout_ms), ("init.lua", 2000));
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
    assert!(!ext.is_running());
}

/// An extension whose directory is gone fails every call with that.
#[test]
fn an_extension_whose_directory_is_gone_fails_every_call() {
    let setup = Setup::new();
    let ext = extension(
        "ext",
        setup.home().join("gone"),
        setup.home(),
        FakeClock::new(),
    );
    for _ in 0..2 {
        let err = call(&ext, "a", "").unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
    }
}
