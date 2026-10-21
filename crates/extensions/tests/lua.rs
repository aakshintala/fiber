//! The Lua runtime through its public API, against the fixture extension in
//! `fakes` (`docs/extensions.md`, "Lua extensions", "How an extension runs",
//! "Loading, and cost when nothing is loaded" and "When an extension
//! misbehaves").

mod common;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{Setup, go_module, write};
use contract::ErrorCode;
use contract::clock::Clock;
use extensions::{Error, LuaExtension};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{ProviderServer, Response};

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
#[track_caller]
fn call(ext: &Arc<LuaExtension>, command: &str, text: &str) -> Result<String, Error> {
    let (tx, rx) = mpsc::channel();
    let (ext, name, text) = (Arc::clone(ext), command.to_owned(), text.to_owned());
    std::thread::spawn(move || tx.send(ext.command(&name, &text)));
    match Deadline::after(WAIT).recv(&rx) {
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

/// `require("hold")` blocks in the loader's read of this fifo. The receiver
/// fires once that read has opened the file, which is after the VM's last
/// clock check: the instruction hook does not run during the read. Sending
/// on the returned sender, or dropping it, ends the read.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn hold_open(dir: &std::path::Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let path = dir.join("hold.lua");
    fakes::fifo(&path);
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        match release_rx.recv() {
            Ok(()) | Err(mpsc::RecvError) => {}
        }
        drop(held);
    });
    (rx, release_tx)
}

/// True once `count` threads are parked in `wait_until` at `until`.
fn parked_at(clock: &Arc<FakeClock>, until: std::time::Instant, count: usize) -> bool {
    clock.await_parked_count(until, count, fakes::Deadline::after(WAIT))
}

#[allow(
    clippy::unwrap_used,
    reason = "a test helper; fixture creation must succeed"
)]
fn lazy_fixture(
    init: &str,
    provider: &str,
    clock: Arc<FakeClock>,
) -> (fakes::TempDir, Arc<LuaExtension>) {
    let dir = fakes::TempDir::new("lazy-provider-load");
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    std::fs::write(dir.path().join("providers/p.lua"), provider).unwrap();
    let ext = extension("ext", dir.path(), dir.path(), clock);
    (dir, ext)
}

fn start_provider(ext: &Arc<LuaExtension>) -> mpsc::Receiver<Result<Vec<String>, Error>> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    std::thread::spawn(move || tx.send(ext.provider_functions("p")));
    rx
}

#[test]
fn a_failed_provider_load_is_sticky_and_runs_once() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture("", "", clock);
    let marker = dir.path().join("counter");
    let path = serde_json::to_string(&marker.to_string_lossy()).unwrap();
    std::fs::write(dir.path().join("providers/p.lua"), format!(
        "local ok, old = pcall(host.fs.read, {path})\n host.fs.write({path}, (ok and old or '') .. 'x')\n error('sticky failure')"
    )).unwrap();
    let first = Deadline::after(WAIT)
        .recv_or_fail(&start_provider(&ext), "first failed load")
        .unwrap_err();
    let second = Deadline::after(WAIT)
        .recv_or_fail(&start_provider(&ext), "sticky failed load")
        .unwrap_err();
    assert_eq!(first.code(), ErrorCode::ExtensionFailed);
    assert_eq!(first.to_string(), second.to_string());
    assert!(first.to_string().contains("providers/p.lua:3:"));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
}

#[test]
fn a_queued_provider_load_expires_at_two_seconds_and_retries() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture(
        "fiber.command('hold', {timeout=10000, run=function() require('hold') return 'held' end})",
        "fiber.provider('p', {models={timeout=100, run=function() return {} end}})",
        Arc::clone(&clock),
    );
    let (held, release) = hold_open(dir.path());
    let command = start(&ext, "hold");
    Deadline::after(WAIT).recv_or_fail(&held, "held command");
    let asked = clock.now();
    let provider = start_provider(&ext);
    let park = asked + LOAD;
    let parked = clock
        .mark_parked(park, fakes::Deadline::after(WAIT))
        .expect("the provider caller parks at its load deadline");
    let parked = parked.threads();
    assert_eq!(
        parked.len(),
        1,
        "only the provider caller parks at its load deadline"
    );
    let provider_caller = parked
        .into_iter()
        .next()
        .expect("the provider caller thread");
    let mark = clock.advance_when_parked(
        Some(park),
        Some(provider_caller),
        LOAD.checked_sub(Duration::from_millis(1)).unwrap(),
        &Deadline::after(WAIT),
    );
    assert!(clock.await_parked_since(&mark, Some(park), fakes::Deadline::after(WAIT)));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    clock.advance_when_parked(
        Some(park),
        Some(provider_caller),
        Duration::from_millis(1),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv_or_fail(&provider, "expired queued load")
        .unwrap_err();
    assert!(
        matches!(err, Error::Timeout { callback, timeout_ms: 2000, .. } if callback == "providers/p.lua")
    );
    release.send(()).unwrap();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&command, "released command")
            .unwrap(),
        "held"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&start_provider(&ext), "retried provider load")
            .unwrap(),
        ["models"]
    );
    assert!(ext.is_running());
}

#[test]
fn expiring_one_queued_provider_load_keeps_the_other_load_queued() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture(
        "fiber.command('hold', {timeout=10000, run=function() require('hold') return 'held' end})",
        "fiber.provider('p', {})",
        Arc::clone(&clock),
    );
    std::fs::write(
        dir.path().join("providers/q.lua"),
        "fiber.provider('q', {sign={timeout=100, run=function() return {} end}})",
    )
    .unwrap();
    let (held, release) = hold_open(dir.path());
    let command = start(&ext, "hold");
    Deadline::after(WAIT).recv_or_fail(&held, "held command");
    let asked = clock.now();
    let first = start_provider(&ext);
    let park = asked + LOAD;
    let parked = clock
        .mark_parked(park, fakes::Deadline::after(WAIT))
        .expect("the first caller parks at its load deadline");
    let parked = parked.threads();
    assert_eq!(
        parked.len(),
        1,
        "only the first caller parks at its load deadline"
    );
    let first_caller = parked.into_iter().next().expect("the first caller thread");
    let offset = Duration::from_millis(1);
    let mark = clock.advance_when_parked(
        Some(park),
        Some(first_caller),
        offset,
        &Deadline::after(WAIT),
    );
    assert!(clock.await_parked_since(&mark, Some(park), fakes::Deadline::after(WAIT)));
    let (tx, second) = mpsc::channel();
    let caller = Arc::clone(&ext);
    std::thread::spawn(move || tx.send(caller.provider_functions("q")));
    assert!(parked_at(&clock, asked + offset + LOAD, 1));
    clock.advance_when_parked(
        Some(park),
        Some(first_caller),
        LOAD.checked_sub(offset).unwrap(),
        &Deadline::after(WAIT),
    );
    let error = Deadline::after(WAIT)
        .recv_or_fail(&first, "first queued load expiry")
        .unwrap_err();
    assert!(
        matches!(error, Error::Timeout { callback, timeout_ms:2000, .. } if callback == "providers/p.lua")
    );
    release.send(()).unwrap();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&command, "released command")
            .unwrap(),
        "held"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&second, "the other queued provider load")
            .unwrap(),
        ["sign"]
    );
    assert!(ext.is_running());
}

#[test]
fn a_provider_load_goes_ahead_of_queued_commands() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture(
        "order = ''\n fiber.command('hold', {timeout=10000, run=function() require('hold') return 'held' end})\n fiber.command('next', {timeout=9000, run=function() order = order .. 'command' return order end})",
        "order = order .. 'load,'\n fiber.provider('p', {})",
        Arc::clone(&clock),
    );
    let (held, release) = hold_open(dir.path());
    let command = start(&ext, "hold");
    Deadline::after(WAIT).recv_or_fail(&held, "held command");
    let asked = clock.now();
    let queued = start(&ext, "next");
    assert!(parked_at(&clock, asked + Duration::from_secs(9), 1));
    let provider = start_provider(&ext);
    assert!(parked_at(&clock, asked + LOAD, 1));
    release.send(()).unwrap();
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&command, "held command end")
            .unwrap(),
        "held"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&queued, "queued command")
            .unwrap(),
        "load,command"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv_or_fail(&provider, "priority load")
            .unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_stuck_provider_load_abandons_its_mixed_extension_after_grace() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture(
        "fiber.command('echo', {timeout=1000, run=function() return 'alive' end})",
        "require('hold')\n fiber.provider('p', {})",
        Arc::clone(&clock),
    );
    let (held, release) = hold_open(dir.path());
    assert_eq!(call(&ext, "echo", "").unwrap(), "alive");
    let asked = clock.now();
    let provider = start_provider(&ext);
    Deadline::after(WAIT).recv_or_fail(&held, "provider blocked in C read");
    let park = asked + LOAD + GRACE;
    let mark = clock.advance_when_parked(
        Some(park),
        None,
        (LOAD + GRACE)
            .checked_sub(Duration::from_millis(1))
            .unwrap(),
        &Deadline::after(WAIT),
    );
    assert!(clock.await_parked_since(&mark, Some(park), fakes::Deadline::after(WAIT)));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    clock.advance_when_parked(
        Some(park),
        None,
        Duration::from_millis(1),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv_or_fail(&provider, "abandoned provider")
        .unwrap_err();
    assert!(matches!(err, Error::Abandoned { callback, .. } if callback == "providers/p.lua"));
    assert!(!ext.is_running());
    let err = call(&ext, "echo", "").unwrap_err();
    assert_eq!(err.code(), ErrorCode::ExtensionFailed);
    assert!(matches!(
        err,
        Error::Abandoned { .. } | Error::Stopped { .. }
    ));
    release.send(()).unwrap();
}

#[test]
fn a_looping_provider_load_stops_at_entry_deadline_and_keeps_commands() {
    let clock = FakeClock::new();
    let (dir, ext) = lazy_fixture(
        "fiber.command('echo', {timeout=1000, run=function() return 'alive' end})",
        "require('go_spin')\n while true do end",
        Arc::clone(&clock),
    );
    let entered = go_module(dir.path(), "spin");
    assert_eq!(call(&ext, "echo", "").unwrap(), "alive");
    let asked = clock.now();
    let provider = start_provider(&ext);
    Deadline::after(WAIT).recv_or_fail(&entered, "provider entered its looping file");
    assert!(parked_at(&clock, asked + LOAD + GRACE, 1));
    clock.advance_when_parked(
        Some(asked + LOAD + GRACE),
        None,
        LOAD,
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv_or_fail(&provider, "looping provider stopped")
        .unwrap_err();
    assert!(
        matches!(err, Error::Timeout { callback, timeout_ms: 2000, .. } if callback == "providers/p.lua")
    );
    assert!(ext.is_running());
    assert_eq!(call(&ext, "echo", "").unwrap(), "alive");
}

#[test]
fn a_provider_load_error_leaves_the_mixed_extensions_commands_running() {
    for lua in ["error('broken provider')", "fiber.provider('q', {})"] {
        let (_dir, ext) = lazy_fixture(
            "fiber.command('echo', {timeout=1000, run=function() return 'alive' end})",
            lua,
            FakeClock::new(),
        );
        assert_eq!(call(&ext, "echo", "").unwrap(), "alive");
        let err = Deadline::after(WAIT)
            .recv_or_fail(&start_provider(&ext), "failed provider load")
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        let Error::Lua { message, .. } = err else {
            panic!("{err:?}")
        };
        assert!(message.contains("providers/p.lua:1:"), "{message}");
        assert!(
            message.contains("broken provider") || message.contains("not `q`"),
            "{message}"
        );
        assert!(ext.is_running());
        assert_eq!(call(&ext, "echo", "").unwrap(), "alive");
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
         setmetatable,state,string,table,tonumber,tostring,type,utf8,xpcall"
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
    // Without its required timeout, a `fiber.command` spec leaves a problem
    // (an `extension_failed` notice), and the entry script continues instead
    // of raising. The command is never registered, so calling it is unknown.
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
/// parked at its deadline. One case per test: five of them in one test would put
/// its deadlines over the 60 s sum.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
#[track_caller]
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
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the callback to pass its clock check");
    let parked_at = asked + Duration::from_millis(50);
    assert!(
        clock.await_parked(parked_at, fakes::Deadline::after(WAIT)),
        "waited for {command} to park at its deadline"
    );
    // The interrupt stops the loop at the deadline.
    clock.advance_when_parked(
        Some(parked_at),
        None,
        Duration::from_millis(50),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv(&result)
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

/// The late wake is the behaviour under test: the caller wakes past the
/// asked + 50 ms park, at deadline + grace, after the callback ran past its
/// own deadline.
#[test]
fn a_callback_the_hook_cannot_stop_abandons_the_vm_and_the_session_survives() {
    // The go module is the callback's first statement. `spin_gc` then loops
    // in a finalizer, where the hook is off. `spin_find` is one backtracking
    // C call, which the hook does not interrupt. The advance comes after the
    // go module and after the caller has parked at its deadline.
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
        Deadline::after(WAIT)
            .recv(&went)
            .expect("waited for the callback to pass its clock check");
        let parked_at = asked + Duration::from_millis(50);
        assert!(
            clock.await_parked(parked_at, fakes::Deadline::after(WAIT)),
            "waited for {command} to park at its deadline"
        );
        // Late wake past the asked + 50 ms park, reached at 50 ms + GRACE.
        clock.advance(Duration::from_millis(50) + GRACE);
        let err = Deadline::after(WAIT)
            .recv(&result)
            .expect("waited for the callback the hook cannot stop")
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::ExtensionFailed);
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{command}: {err:?}")
        };
        assert_eq!(callback, command);
        assert!(!ext.is_running());
        let err = call(&ext, "echo", "").unwrap_err();
        assert!(
            matches!(&err, Error::Abandoned { callback, .. } if callback == command),
            "{err:?}"
        );
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
        Deadline::after(WAIT)
            .recv(&rx)
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

/// A command queued behind a parked command runs once the parked command
/// finishes. That it does not start while parked is proved by the unit test
/// `a_command_queued_behind_a_parked_command_does_not_start`.
#[test]
fn a_command_queued_behind_a_parked_command_runs_once_it_finishes() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"first\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"second\", {{ timeout = 5000, run = function() return \"second\" end }})\n"
        ),
    );
    write(
        &dir.join("providers/p.lua"),
        "fiber.provider('p', {sign={timeout=1000, run=function() return {} end}})",
    );
    let ext = extension("ext", dir, setup.home(), FakeClock::new());
    let first = start(&ext, "first");
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "waited for the first command to reach the server"
    );
    let (listed_tx, listed_rx) = mpsc::channel();
    let listed = Arc::clone(&ext);
    std::thread::spawn(move || listed_tx.send(listed.provider_functions("p")));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&listed_rx)
            .expect("waited for the provider's functions")
            .unwrap(),
        ["sign"]
    );
    let second = start(&ext, "second");
    server.release();
    let first = Deadline::after(WAIT)
        .recv(&first)
        .expect("waited for the first command")
        .unwrap();
    let second = Deadline::after(WAIT)
        .recv(&second)
        .expect("waited for the second command")
        .unwrap();
    assert_eq!((first.as_str(), second.as_str()), ("ok", "second"));
    assert!(ext.is_running());
}

#[test]
fn a_queued_command_times_out_on_its_own_deadline_and_the_vm_stays() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "seen = \"no\"\n\
             fiber.command(\"slow\", {{ timeout = 30000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 300, run = function() seen = \"yes\"; return \"ran\" end }})\n\
             fiber.command(\"after\", {{ timeout = 1000, run = function() return seen end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let slow = start(&ext, "slow");
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "waited for the slow command to reach the server"
    );
    let asked = clock.now();
    let quick = start(&ext, "quick");
    let park = asked + Duration::from_millis(300);
    let parked = clock
        .mark_parked(park, fakes::Deadline::after(WAIT))
        .expect("quick parks at its timeout");
    let parked = parked.threads();
    assert_eq!(parked.len(), 1, "only quick parks at its timeout");
    let quick_caller = parked.into_iter().next().expect("the quick caller thread");
    clock.advance_when_parked(
        Some(park),
        Some(quick_caller),
        Duration::from_millis(300),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv(&quick)
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
    server.release();
    assert!(
        Deadline::after(WAIT)
            .recv(&slow)
            .expect("waited for the slow command")
            .is_ok()
    );
    assert_eq!(call(&ext, "after", "").unwrap(), "no");
}

#[test]
fn a_call_made_before_registration_finishes_uses_its_declared_timeout() {
    let setup = Setup::new();
    // A call that ignores time spent registering runs `work` and gets an
    // answer at once, so the timeout assertion fails.
    let work =
        ProviderServer::start([Response::status(200, "ok"), Response::status(200, "ok")]).unwrap();
    let work_url = work.url();
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
    Deadline::after(WAIT)
        .recv(&held)
        .expect("waited for the entry script to block past its clock checks");
    let second = start(&ext, "work");
    // Both wait on the entry script's grace, which is later than the
    // command's own timeout. Advancing that timeout does not abandon the VM.
    clock.advance_when_parked_threads(
        Some(asked + LOAD + GRACE),
        2,
        Duration::from_millis(1000),
        &Deadline::after(WAIT),
    );
    release.send(()).unwrap();
    for waiter in [first, second] {
        let err = Deadline::after(WAIT)
            .recv(&waiter)
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
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    server.hold();
    let hold_url = server.url();
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
    Deadline::after(WAIT)
        .recv(&opened)
        .expect("waited for the entry script to block past its clock checks");
    let quick = start(&ext, "quick");
    clock.advance_when_parked_threads(
        Some(asked + LOAD + GRACE),
        2,
        Duration::from_millis(400),
        &Deadline::after(WAIT),
    );
    release.send(()).unwrap();
    let err = Deadline::after(WAIT)
        .recv(&quick)
        .expect("waited for quick")
        .unwrap_err();
    let Error::Timeout { timeout_ms, .. } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*timeout_ms, 400);
    assert!(ext.is_running());
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "waited for hold to reach the server"
    );
    server.release();
    assert!(
        Deadline::after(WAIT)
            .recv(&held)
            .expect("waited for hold")
            .is_ok()
    );
}

#[test]
fn a_command_named_like_a_provider_function_keeps_its_own_timeout() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = setup.home().join("ext");
    write(
        &dir.join("init.lua"),
        &format!(
            "fiber.command(\"p.sign\", {{ timeout = 30000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n"
        ),
    );
    write(
        &dir.join("providers/p.lua"),
        "fiber.provider('p', {sign={timeout=50, run=function() return {} end}})",
    );
    let clock = FakeClock::new();
    let ext = extension("ext", dir, setup.home(), clock.clone());
    let asked = clock.now();
    let ran = start(&ext, "p.sign");
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "waited for p.sign to reach the server"
    );
    let grace = asked + Duration::from_millis(30000) + GRACE;
    let parked = clock
        .mark_parked(grace, fakes::Deadline::after(WAIT))
        .expect("p.sign parks at its own grace");
    let parked = parked.threads();
    assert_eq!(parked.len(), 1, "only p.sign parks at its own grace");
    let p_sign = parked.into_iter().next().expect("the p.sign caller thread");
    // The provider function's 50 ms would already have failed a parked call.
    // Advancing wakes the caller; it parks again at the same grace.
    let mark = clock.advance_when_parked(
        Some(grace),
        Some(p_sign),
        Duration::from_millis(50),
        &Deadline::after(WAIT),
    );
    assert!(
        clock.await_parked_since(&mark, Some(grace), fakes::Deadline::after(WAIT)),
        "p.sign left its own deadline"
    );
    assert!(
        ran.try_recv().is_err(),
        "p.sign returned on the provider function's timeout"
    );
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&ran)
            .expect("waited for p.sign")
            .unwrap(),
        "ok"
    );
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
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the entry script to pass its clock check");
    clock.advance_when_parked_threads(Some(abandon), 2, LOAD + GRACE, &Deadline::after(WAIT));
    for waiter in waiters {
        let err = Deadline::after(WAIT)
            .recv(&waiter)
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
    Deadline::after(WAIT)
        .recv(&held)
        .expect("waited for the entry script to block past its clock checks");
    let second = start(&ext, "b");
    assert!(
        parked_at(&clock, abandon, 2),
        "waited for both callers to park on registration"
    );
    release.send(()).unwrap();
    for waiter in [first, second] {
        let err = Deadline::after(WAIT)
            .recv(&waiter)
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
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the entry script to pass its clock check");
    assert!(
        clock.await_parked(abandon, fakes::Deadline::after(WAIT)),
        "waited for the caller to park until the load grace"
    );
    // The hook's deadline is the 2 second load bound, before the grace.
    clock.advance_when_parked(Some(abandon), None, LOAD, &Deadline::after(WAIT));
    let err = Deadline::after(WAIT)
        .recv(&first)
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

/// An extension whose commands fail `host.http` caught and uncaught, and
/// with a missing argument.
fn http_extension(setup: &Setup) -> Arc<LuaExtension> {
    let dir = setup.root().join("extensions").join("http");
    write(
        &dir.join("init.lua"),
        r#"
fiber.command("fetch", { timeout = 60000, run = function(text)
  local ok, err = pcall(host.http, { url = text })
  if ok then return "ok" end
  return err.code .. "\n" .. err.message
end })
fiber.command("fetch_raw", { timeout = 60000, run = function(text)
  return tostring(host.http({ url = text }).status)
end })
fiber.command("fetch_arg", { timeout = 60000, run = function()
  local ok, err = pcall(host.http, {})
  return tostring(ok) .. ":" .. type(err) .. ":" .. tostring(err)
end })
"#,
    );
    extension("http", dir, setup.home(), FakeClock::new())
}

#[test]
fn a_failed_host_http_raises_code_and_message_that_pcall_catches() {
    let setup = Setup::new();
    let ext = http_extension(&setup);
    let url = "http://127.0.0.1:1/";
    let caught = call(&ext, "fetch", url).unwrap();
    let (code, message) = caught.split_once('\n').unwrap();
    assert_eq!(code, "connection_failed");
    assert!(message.starts_with("host.http: "), "{message}");
    // Uncaught, the callback fails with the message as its text.
    let Error::Lua {
        message: uncaught, ..
    } = &call(&ext, "fetch_raw", url).unwrap_err()
    else {
        panic!("an uncaught host.http failure is the callback's Lua error")
    };
    assert_eq!(uncaught, message);
}

#[test]
fn a_host_http_with_no_url_stays_a_string_error() {
    let setup = Setup::new();
    let ext = http_extension(&setup);
    let caught = call(&ext, "fetch_arg", "").unwrap();
    let (shape, message) = caught.split_once(":host.http: ").unwrap();
    assert_eq!(shape, "false:string");
    assert!(message.contains("`url` must be a string"), "{message}");
}

/// An extension whose commands fail `host.exec` and `host.fs` caught and uncaught.
fn failing_extension(setup: &Setup) -> Arc<LuaExtension> {
    let dir = setup.root().join("extensions").join("failing");
    write(
        &dir.join("init.lua"),
        r#"
fiber.command("run", { timeout = 60000, run = function()
  local ok, err = pcall(host.exec, "fiber-definitely-missing-xyz", {})
  if ok then return "ok" end
  return err.code .. "\n" .. err.message
end })
fiber.command("run_raw", { timeout = 60000, run = function()
  return tostring(host.exec("fiber-definitely-missing-xyz", {}))
end })
fiber.command("read", { timeout = 60000, run = function()
  local ok, err = pcall(host.fs.read, "fiber-definitely-missing-fs.md")
  if ok then return "ok" end
  return err.code .. "\n" .. err.message
end })
fiber.command("read_raw", { timeout = 60000, run = function()
  return tostring(host.fs.read("fiber-definitely-missing-fs.md"))
end })
"#,
    );
    extension("failing", dir, setup.home(), FakeClock::new())
}

#[test]
fn a_failed_host_exec_raises_code_and_message_that_pcall_catches() {
    let setup = Setup::new();
    let ext = failing_extension(&setup);
    let caught = call(&ext, "run", "").unwrap();
    let (code, message) = caught.split_once('\n').unwrap();
    assert_eq!(code, "not_found");
    assert!(message.starts_with("host.exec: "), "{message}");
    let Error::Lua {
        message: uncaught, ..
    } = &call(&ext, "run_raw", "").unwrap_err()
    else {
        panic!("an uncaught host.exec failure is the callback's Lua error")
    };
    assert_eq!(uncaught, message);
}

#[test]
fn a_failed_host_fs_raises_code_and_message_that_pcall_catches() {
    let setup = Setup::new();
    let ext = failing_extension(&setup);
    let caught = call(&ext, "read", "").unwrap();
    let (code, message) = caught.split_once('\n').unwrap();
    assert_eq!(code, "not_found");
    assert!(message.starts_with("host.fs.read: "), "{message}");
    let Error::Lua {
        message: uncaught, ..
    } = &call(&ext, "read_raw", "").unwrap_err()
    else {
        panic!("an uncaught host.fs failure is the callback's Lua error")
    };
    assert_eq!(uncaught, message);
}
