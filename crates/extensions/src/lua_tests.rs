use std::collections::BTreeMap;
use std::io::Read;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use fakes::clock::FakeClock;

use super::hub::Progress;
use super::*;
use contract::inbox::Delivery;

/// Wall-clock bound on a wait for the VM, a server, or a thread.
const WAIT: Duration = Duration::from_secs(5);

/// Bound on a wait for the hub's state, a held request to reach its server,
/// or the extension thread to quit.
const WAIT_UNTIL: Duration = Duration::from_secs(2);

/// Bound on a wait for a test server to accept, or a caller to park.
const WAIT_SERVER: Duration = Duration::from_secs(3);

/// A panic in a host function is never a Lua error the extension's `pcall`
/// can catch (`docs/code-quality.md`, "Panics"). Tests run under unwind,
/// where it reaches the Rust caller past the `pcall`; Fiber's builds abort at
/// the panic.
#[test]
fn a_panic_in_a_host_function_passes_the_extensions_pcall() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let vm = Vm::load(
        &hub,
        &schedule::Start {
            name: "fixture".to_owned(),
            dir: fakes::lua_fixture(),
            home: PathBuf::from("/nonexistent-fiber-home"),
            load_by: Some(clock.now().checked_add(LOAD_TIMEOUT).unwrap()),
            memory_cap: MEMORY_CAP,
            browser: Arc::new(SystemBrowser::default()),
            session: None,
            secrets: Vec::new(),
        },
    )
    .unwrap();
    let boom = vm
        .lua
        .create_function(|_, ()| -> mlua::Result<()> { panic!("host bug") })
        .unwrap();
    vm.lua.globals().set("boom", boom).unwrap();
    let f = vm
        .lua
        .load(r#"local ok, e = pcall(boom) return tostring(ok) .. " " .. tostring(e)"#)
        .into_function()
        .unwrap();
    vm.deadline
        .restore(Some(clock.now().checked_add(LOAD_TIMEOUT).unwrap()));
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        vm.resume(f, "boom", LOAD_TIMEOUT, mlua::Value::Nil)
    }));
    assert!(outcome.is_err(), "the panic became {outcome:?}");
}

fn command(name: &str) -> Target {
    Target::Command(name.to_owned())
}

fn in_phase(phase: Phase) -> Shared {
    let mut shared = Shared::default();
    shared.phase = phase;
    shared
}

fn ready(commands: &[(&str, u64)]) -> Shared {
    let mut timeouts = CallbackTimeouts::default();
    for (name, ms) in commands {
        timeouts.commands.insert(
            (*name).to_owned(),
            crate::lua::declared::DeclaredCommand {
                timeout: Duration::from_millis(*ms),
                description: String::new(),
            },
        );
    }
    in_phase(Phase::Ready(timeouts))
}

fn returned(next: (Next, Vec<Delivery>)) -> Result<Value, Error> {
    match next.0 {
        Next::Return(result) => result,
        Next::Sleep(until) => panic!("slept until {until:?}"),
    }
}

/// An instant `ms` before `now`, on one fake clock's origin.
fn ago(now: Instant, ms: u64) -> Instant {
    now.checked_sub(Duration::from_millis(ms)).unwrap()
}

fn now() -> Instant {
    FakeClock::new().origin()
}

/// A call waiting on registration sleeps until the entry script's grace
/// ends, and no later.
#[test]
fn a_call_waiting_on_registration_sleeps_until_the_entry_scripts_grace_ends() {
    let asked = now();
    let abandon_at = asked.checked_add(Duration::from_secs(5));
    let mut shared = in_phase(Phase::Registering { abandon_at });
    let id = shared.push(command("x"), Value::Null, asked);
    let (Next::Sleep(until), _) = shared.judge("ext", id, &command("x"), asked, asked) else {
        panic!("a waiter returned during registration")
    };
    assert_eq!(until, abandon_at);
    assert!(matches!(shared.phase, Phase::Registering { .. }));
}

/// The verify-3 hang: an entry script the hook cannot stop is abandoned by
/// the first waiter past its grace, and every other waiter gets the same
/// error instead of waiting on its own reply.
#[test]
fn every_call_waiting_on_an_abandoned_registration_gets_abandoned() {
    let at = now();
    let mut shared = in_phase(Phase::Registering {
        abandon_at: Some(ago(at, 1)),
    });
    let first = shared.push(command("a"), Value::Null, ago(at, 5));
    let second = shared.push(command("b"), Value::Null, ago(at, 5));
    for (id, name) in [(first, "a"), (second, "b")] {
        let err = returned(shared.judge("ext", id, &command(name), ago(at, 5), at)).unwrap_err();
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(callback, "init.lua");
    }
    assert!(matches!(shared.phase, Phase::Stopped(_)));
    assert!(shared.queue.is_empty() && shared.calls.is_empty());
}

/// A queued call fails on its own timeout from when it was asked, leaves
/// the queue, and leaves the VM up.
#[test]
fn a_queued_call_past_its_own_timeout_times_out_and_the_vm_stays() {
    let at = now();
    let mut shared = ready(&[("quick", 100), ("slow", 10_000)]);
    let asked = ago(at, 150);
    let quick = shared.push(command("quick"), Value::Null, asked);
    let slow = shared.push(command("slow"), Value::Null, asked);
    let err = returned(shared.judge("ext", quick, &command("quick"), asked, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 100, .. } if callback == "quick"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    assert_eq!(
        shared.queue.iter().map(|job| job.id).collect::<Vec<_>>(),
        [slow]
    );
    let (Next::Sleep(until), _) = shared.judge("ext", slow, &command("slow"), asked, at) else {
        panic!("slow returned")
    };
    assert_eq!(until, Some(asked + Duration::from_secs(10)));
}

/// Once registration has published, a callback it did not register fails
/// at once.
#[test]
fn a_call_the_entry_script_did_not_register_is_unknown() {
    let at = now();
    let mut shared = ready(&[]);
    let id = shared.push(command("nope"), Value::Null, at);
    let err = returned(shared.judge("ext", id, &command("nope"), at, at)).unwrap_err();
    assert!(matches!(err, Error::UnknownCommand { .. }), "{err:?}");
    assert!(shared.queue.is_empty());
}

/// A parked call that the thread has not failed by its grace, because the
/// thread is busy with another callback, times out alone: the VM stays.
#[test]
fn a_parked_call_past_its_grace_times_out_and_the_vm_stays() {
    let at = now();
    let mut shared = ready(&[("park", 100)]);
    let asked = ago(at, 2000);
    let id = shared.push(command("park"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1100)),
            parked: true,
        },
    );
    let err = returned(shared.judge("ext", id, &command("park"), asked, at)).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Timeout {
                timeout_ms: 100,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
}

/// A running call waits its grace, then abandons the VM.
#[test]
fn a_running_call_past_its_grace_abandons_the_vm() {
    let at = now();
    let mut shared = ready(&[("spin", 100)]);
    let asked = ago(at, 600);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    let inside = ago(at, 500);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(inside),
            parked: false,
        },
    );
    let (Next::Sleep(until), _) = shared.judge("ext", id, &command("spin"), asked, at) else {
        panic!("returned inside its grace")
    };
    assert_eq!(until, Some(inside + GRACE));
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1100)),
            parked: false,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), ago(at, 1200), at)).unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    assert!(matches!(
        shared.phase,
        Phase::Stopped(Error::Stopped { .. })
    ));
}

/// Every waiter on a stopped extension gets the error that stopped it, at
/// once.
#[test]
fn every_waiter_on_a_stopped_extension_gets_the_stored_error() {
    let stored = [
        Error::Lua {
            extension: "ext".to_owned(),
            message: "init.lua:1: bad".to_owned(),
        },
        Error::Timeout {
            extension: "ext".to_owned(),
            callback: "init.lua".to_owned(),
            timeout_ms: 2000,
        },
        Error::Io {
            path: "/gone".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        },
        Error::Stopped {
            extension: "ext".to_owned(),
        },
    ];
    for error in stored {
        let text = error.to_string();
        let mut shared = in_phase(Phase::Stopped(error));
        let at = now();
        for _ in 0..2 {
            let id = shared.push(command("x"), Value::Null, at);
            let err = returned(shared.judge("ext", id, &command("x"), at, at)).unwrap_err();
            assert_eq!(err.to_string(), text);
        }
    }
}

/// A finished call returns its result while the extension is up.
#[test]
fn a_finished_call_returns_its_result() {
    let at = now();
    let mut shared = ready(&[("x", 1000)]);
    let id = shared.push(command("x"), Value::Null, at);
    shared.finish(id, Ok(Value::String("done".to_owned())));
    assert_eq!(
        returned(shared.judge("ext", id, &command("x"), at, at)).unwrap(),
        "done"
    );
    assert!(shared.calls.is_empty());
}

/// Once the extension is stopped, a waiter holding a finished result gets
/// the stopped error: Stopped is final for every waiter.
#[test]
fn a_finished_call_on_a_stopped_extension_gets_the_stopped_error() {
    let at = now();
    let mut shared = in_phase(Phase::Stopped(hub::stopped("ext")));
    let id = shared.push(command("x"), Value::Null, at);
    shared.finish(id, Ok(Value::String("done".to_owned())));
    let err = returned(shared.judge("ext", id, &command("x"), at, at)).unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
    assert!(shared.calls.is_empty());
}

/// When `provider_functions` abandons registration, it wakes every other
/// waiter. The other waiter here sleeps with no limit, so only that wake
/// ends its wait.
#[test]
fn provider_functions_abandoning_registration_wakes_every_waiter() {
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
        clock.clone(),
    ));
    ext.hub.lock().phase = Phase::Registering {
        abandon_at: Some(clock.origin()),
    };
    let hub = Arc::clone(&ext.hub);
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let (woke_tx, woke_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut shared = hub.lock();
        // Sent while holding the lock, so the abandonment comes after the wait starts.
        waiting_tx.send(()).unwrap();
        while !matches!(shared.phase, Phase::Stopped(_)) {
            shared = hub.wait(shared, None);
        }
        woke_tx.send(()).unwrap();
    });
    waiting_rx
        .recv_timeout(WAIT)
        .expect("waited for the other waiter to take the lock");
    // Its own thread, so a call that never returns fails the test.
    let (done_tx, done_rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    thread::spawn(move || done_tx.send(caller.provider_functions("p")));
    let err = done_rx
        .recv_timeout(WAIT)
        .expect("provider_functions never returned")
        .unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    // A hang guard only: without the wake the waiter never returns.
    woke_rx
        .recv_timeout(WAIT)
        .expect("the other waiter was never woken");
}

/// An extension dropped while running is stopped, so its thread quits.
#[test]
fn dropping_the_extension_stops_it() {
    let ext = Arc::new(LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    ));
    let caller = Arc::clone(&ext);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = caller.command("echo", "hi");
        drop(caller);
        match done_tx.send(result) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        }
    });
    assert_eq!(
        done_rx
            .recv_timeout(WAIT)
            .expect("waited for echo")
            .unwrap(),
        "hi"
    );
    let hub = Arc::clone(&ext.hub);
    drop(ext);
    assert!(matches!(
        hub.lock().phase,
        Phase::Stopped(Error::Stopped { .. })
    ));
}

/// An extension directory in a fresh temporary directory, with `init`.
fn extension(tag: &str, init: &str) -> fakes::TempDir {
    let dir = fakes::TempDir::new(&format!("fiber-lua-{tag}"));
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    dir
}

/// Runs the thread on `dir`, once a call to `first` is queued. The receiver
/// gets one message when the thread returns.
fn serve_after(
    dir: &Path,
    first: &str,
    clock: &Arc<FakeClock>,
) -> (Arc<Hub>, u64, mpsc::Receiver<()>) {
    let hub = Hub::new(clock.clone());
    let asked = clock.now();
    let id = {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        shared.push(command(first), Value::String(String::new()), asked)
    };
    let (thread_hub, dir) = (Arc::clone(&hub), dir.to_owned());
    let (done_tx, done_rx) = mpsc::channel();
    let load_by = asked.checked_add(LOAD_TIMEOUT);
    thread::spawn(move || {
        schedule::serve(
            Arc::clone(&thread_hub),
            schedule::Start {
                name: "ext".to_owned(),
                dir,
                home: PathBuf::from("/nonexistent-fiber-home"),
                load_by,
                memory_cap: MEMORY_CAP,
                browser: Arc::new(SystemBrowser::default()),
                session: None,
                secrets: Vec::new(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    (hub, id, done_rx)
}

/// Waits until `check` holds for the hub's state, or two seconds pass.
fn until(hub: &Hub, check: impl Fn(&Shared) -> bool) -> bool {
    let shared = hub.lock();
    hub.wait_for(shared, WAIT_UNTIL, check)
}

fn stop(hub: &Hub) {
    hub.lock().phase = Phase::Stopped(hub::stopped("ext"));
    hub.notify();
}

/// A stopped extension's thread quits while a callback is parked, and the
/// call queued behind it never starts.
#[test]
fn the_thread_quits_once_stopped_with_a_callback_parked() {
    let (url, accepted_rx) = hold_server();
    let dir = extension(
        "parked",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    accepted_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for hold to reach the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let later = hub.lock().push(command("later"), Value::Null, clock.now());
    stop(&hub);
    done.recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
}

/// While a command is parked, the next command does not start: a provider
/// call queued behind the queued command still runs, which proves the
/// thread judged the queue with the command in front of it.
#[test]
fn a_command_queued_behind_a_parked_command_does_not_start() {
    let (url, accepted_rx) = hold_server();
    let dir = extension(
        "parked-second",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"second\", {{ timeout = 5000, run = function() return \"second\" end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    accepted_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for hold to reach the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let asked = clock.now();
    let (second, sign) = {
        let mut shared = hub.lock();
        let second = shared.push(command("second"), Value::Null, asked);
        let sign = shared.push(
            Target::Provider {
                name: "p".to_owned(),
                function: "sign",
                credential: None,
            },
            Value::Null,
            asked,
        );
        (second, sign)
    };
    hub.notify();
    assert!(until(&hub, |s| matches!(
        s.calls.get(&sign),
        Some(Progress::Done(_))
    )));
    {
        let shared = hub.lock();
        assert!(
            matches!(shared.calls.get(&second), Some(Progress::Queued)),
            "the second command started while hold was parked"
        );
        assert!(shared.queue.iter().any(|job| job.id == second));
    }
    stop(&hub);
    done.recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
}

/// A thread stopped while a callback runs quits when that callback ends,
/// and does not start the call queued behind it.
#[test]
fn the_thread_quits_after_the_running_callback_once_stopped() {
    let dir = extension(
        "running",
        "fiber.command(\"spin\", { timeout = 1000, run = function() require(\"go_spin\") while true do end end })\n\
         fiber.command(\"later\", { timeout = 5000, run = function() return \"later\" end })\n",
    );
    let went = go_module(dir.path(), "spin");
    let clock = FakeClock::new();
    let (hub, spin, done) = serve_after(dir.path(), "spin", &clock);
    let later = hub.lock().push(command("later"), Value::Null, clock.now());
    went.recv_timeout(WAIT)
        .expect("waited for spin to pass its clock check");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Started { parked: false, .. })
    )));
    stop(&hub);
    clock.advance(Duration::from_millis(1000));
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Done(_))
    )));
    done.recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
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

/// `require("go_<name>")` is a signal, not a barrier. Opening the fifo for
/// write returns only once the loader has opened it for read; the writer
/// reports that and closes the fifo at once, so `require` reads an empty
/// module and the code runs on. Nothing stays blocked in it. Each name is
/// used once per VM, because `require` caches.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn go_module(dir: &Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        drop(held);
    });
    rx
}

/// Accepts one connection, reads its head, signals, and then holds the
/// socket so the call stays parked.
fn hold_server() -> (String, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        read_head(&mut sock);
        match accepted_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
        let (_block_tx, block_rx) = mpsc::channel::<()>();
        match block_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        drop(sock);
    });
    (url, accepted_rx)
}

/// When a running callback abandons the VM, every other waiter, queued or
/// parked, command or provider call, gets `Stopped` at once rather than at
/// its own deadline.
#[test]
fn an_abandoned_vm_wakes_every_queued_and_parked_waiter() {
    let (park_url, park_accepted) = hold_server();
    let (models_url, models_accepted) = hold_server();
    let dir = extension(
        "abandon",
        &format!(
            "fiber.command(\"park\", {{ timeout = 8000, run = function() return host.http({{ url = \"{park_url}\" }}).body end }})\n\
             fiber.command(\"queued\", {{ timeout = 8000, run = function() return \"ran\" end }})\n\
             fiber.provider(\"p\", {{\n\
               models = {{ timeout = 8000, run = function() return host.http({{ url = \"{models_url}\" }}).body end }},\n\
               sign = {{ timeout = 50, run = function() require(\"go_sign\") setmetatable({{}}, {{ __gc = function() while true do end end }}); collectgarbage() end }},\n\
             }})\n"
        ),
    );
    // The go module is sign's first statement, before the finalizer's loop.
    // The finalizer runs with the hook off, so only the caller's grace stops it.
    let went = go_module(dir.path(), "sign");
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        "/nonexistent-fiber-home",
        clock.clone(),
    ));
    let (tx, rx) = mpsc::channel();
    let run = |what: &'static str, f: fn(&LuaExtension) -> Result<Value, Error>| {
        let (ext, tx) = (Arc::clone(&ext), tx.clone());
        thread::spawn(move || tx.send((what, f(&ext))));
    };
    let asked = clock.now();
    run("park", |e| e.command("park", "").map(Value::String));
    park_accepted
        .recv_timeout(WAIT_SERVER)
        .expect("waited for park to reach the server");
    run("models", |e| e.provider_call("p", "models", Value::Null));
    models_accepted
        .recv_timeout(WAIT_SERVER)
        .expect("waited for models to reach the server");
    run("queued", |e| e.command("queued", "").map(Value::String));
    run("sign", |e| e.provider_call("p", "sign", Value::Null));
    went.recv_timeout(WAIT)
        .expect("waited for sign to pass its clock check");
    let abandon = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon, WAIT_SERVER),
        "waited for sign to park at its grace"
    );
    clock.advance(Duration::from_millis(50) + GRACE);
    let mut seen = BTreeMap::new();
    for _ in 0..4 {
        let (what, result) = rx
            .recv_timeout(WAIT)
            .expect("waited for a waiter to return");
        seen.insert(what, result.unwrap_err());
    }
    assert!(
        matches!(seen["sign"], Error::Abandoned { .. }),
        "{:?}",
        seen["sign"]
    );
    for what in ["park", "models", "queued"] {
        assert!(
            matches!(seen[what], Error::Stopped { .. }),
            "{what}: {:?}",
            seen[what]
        );
    }
    assert!(!ext.is_running());
}

/// A call that finds no thread started fails rather than wait forever.
#[test]
fn a_call_on_an_extension_with_no_thread_is_stopped() {
    let at = now();
    let mut shared = Shared::default();
    let id = shared.push(command("x"), Value::Null, at);
    let err = returned(shared.judge("ext", id, &command("x"), at, at)).unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
}

/// A credential lock on a fresh file, and a second handle on it.
fn held_lock(tag: &str) -> (fakes::TempDir, config::CredentialFile, host::Reply) {
    let home = fakes::TempDir::new(&format!("fiber-lua-{tag}"));
    let file = config::CredentialFile::new(home.path(), "acme", "default").unwrap();
    let lock = file.try_lock().unwrap().unwrap();
    (home, file, host::Reply::Lock(Ok(lock)))
}

#[test]
fn a_lock_reply_for_a_stopped_extension_is_dropped_and_releases_the_lock() {
    let (_home, file, reply) = held_lock("deliver-stopped");
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Stopped(hub::stopped("ext"));
    assert!(file.try_lock().unwrap().is_none());
    hub.deliver(7, reply);
    assert!(hub.lock().replies.is_empty());
    assert!(file.try_lock().unwrap().is_some());
}

#[test]
fn a_lock_reply_queued_when_the_extension_stops_is_dropped_and_releases_the_lock() {
    let (_home, file, reply) = held_lock("stop-queued");
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Ready(CallbackTimeouts::default());
    hub.deliver(7, reply);
    assert_eq!(hub.lock().replies.len(), 1);
    assert!(file.try_lock().unwrap().is_none());
    hub.lock().stop(hub::stopped("ext"));
    assert!(hub.lock().replies.is_empty());
    assert!(file.try_lock().unwrap().is_some());
}

#[test]
fn a_lock_reply_for_a_ready_extension_is_kept_with_its_lock() {
    let (_home, file, reply) = held_lock("deliver-ready");
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Ready(CallbackTimeouts::default());
    hub.deliver(7, reply);
    assert_eq!(hub.lock().replies.len(), 1);
    assert!(file.try_lock().unwrap().is_none());
}

/// A timer target names its id: `timer <id>`.
#[test]
fn a_timer_target_displays_its_id() {
    assert_eq!(Target::Timer { id: 7 }.to_string(), "timer 7");
}

/// The scheduler starts the earliest due timer that waits: due, neither
/// cancelled nor firing.
#[test]
fn timer_fire_starts_the_earliest_due_timer_that_waits() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let mut shared = hub.lock();
    let now = clock.now();
    shared.next_timer = 3;
    for (id, due, cancelled, firing) in [
        (0, now, false, true),
        (1, now, true, false),
        (
            2,
            now.checked_add(Duration::from_secs(60)).unwrap(),
            false,
            false,
        ),
    ] {
        shared.next_timer = shared.next_timer.max(id + 1);
        shared.timers.insert(
            id,
            hub::Timer {
                id,
                every: None,
                due,
                timeout: Duration::from_millis(100),
                cancelled,
                firing,
            },
        );
    }
    assert!(shared.timer_fire(now).is_none(), "nothing due waits");
    let late = now.checked_add(Duration::from_secs(60)).unwrap();
    let (id, _) = shared.timer_fire(late).unwrap();
    assert_eq!(id, 2, "the future one is due at its time");
    shared.timer_end(2, late);
    shared.timers.get_mut(&1).unwrap().cancelled = false;
    let (id, timeout) = shared.timer_fire(late).unwrap();
    assert_eq!(id, 1, "the earliest due id fires");
    assert_eq!(timeout, Duration::from_millis(100));
    assert!(shared.timers.get(&1).unwrap().firing);
}

/// An `every` ended uncancelled fires again `ms` after its end; anything
/// else leaves, freeing its Lua function on the extension's thread.
#[test]
fn timer_end_reschedules_an_uncancelled_every_and_removes_the_rest() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let mut shared = hub.lock();
    let now = clock.now();
    for (id, every) in [
        (0, None),
        (1, Some(Duration::from_millis(50))),
        (2, Some(Duration::from_millis(50))),
    ] {
        shared.timers.insert(
            id,
            hub::Timer {
                id,
                every,
                due: now,
                timeout: Duration::from_millis(100),
                cancelled: id == 2,
                firing: true,
            },
        );
    }
    shared.timer_end(0, now);
    shared.timer_end(1, now);
    shared.timer_end(2, now);
    assert!(!shared.timers.contains_key(&0), "an `after` leaves");
    assert!(
        !shared.timers.contains_key(&2),
        "a cancelled `every` leaves"
    );
    let timer = shared.timers.get(&1).unwrap();
    assert_eq!(
        timer.due,
        now.checked_add(Duration::from_millis(50)).unwrap()
    );
    assert!(!timer.firing);
    assert_eq!(shared.timer_cleanup, vec![0, 2]);
}

/// A hook queued while a command is parked on `host.http` starts only after
/// the command finishes: hooks, watcher deliveries and commands form one
/// stream, in the order they happened.
#[test]
fn a_hook_queued_behind_a_parked_command_does_not_start() {
    let (url, accepted_rx) = hold_server();
    let dir = extension(
        "hook-behind-parked-command",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"non-blocking\", run = function(call) return {{}} end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    accepted_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for hold to reach the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let asked = clock.now();
    let (hook, sign) = {
        let mut shared = hub.lock();
        let hook = shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            asked,
        );
        // A provider function queued behind the hook still runs, which proves
        // the thread judged the queue with the hook in front of it.
        let sign = shared.push(
            Target::Provider {
                name: "p".to_owned(),
                function: "sign",
                credential: None,
            },
            Value::Null,
            asked,
        );
        (hook, sign)
    };
    hub.notify();
    assert!(until(&hub, |s| matches!(
        s.calls.get(&sign),
        Some(Progress::Done(_))
    )));
    {
        let shared = hub.lock();
        assert!(
            matches!(shared.calls.get(&hook), Some(Progress::Queued)),
            "the hook started while hold was parked"
        );
        assert!(shared.queue.iter().any(|job| job.id == hook));
    }
    stop(&hub);
    done.recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
}

/// A command queued while a hook is parked likewise waits for the hook.
#[test]
fn a_command_queued_behind_a_parked_hook_does_not_start() {
    let (url, accepted_rx) = hold_server();
    let dir = extension(
        "command-behind-parked-hook",
        &format!(
            "fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n\
             fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"non-blocking\", run = function(call) return host.http({{ url = \"{url}\" }}) end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let asked = clock.now();
    let hook = {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            asked,
        )
    };
    let (thread_hub, dir) = (Arc::clone(&hub), dir.path().to_owned());
    let (done_tx, done_rx) = mpsc::channel();
    let load_by = asked.checked_add(LOAD_TIMEOUT);
    thread::spawn(move || {
        schedule::serve(
            Arc::clone(&thread_hub),
            schedule::Start {
                name: "ext".to_owned(),
                dir,
                home: PathBuf::from("/nonexistent-fiber-home"),
                load_by,
                memory_cap: MEMORY_CAP,
                browser: Arc::new(SystemBrowser::default()),
                session: None,
                secrets: Vec::new(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    accepted_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for the hook to reach the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hook),
        Some(Progress::Started { parked: true, .. })
    )));
    let asked = clock.now();
    let (later, sign) = {
        let mut shared = hub.lock();
        let later = shared.push(command("later"), Value::Null, asked);
        let sign = shared.push(
            Target::Provider {
                name: "p".to_owned(),
                function: "sign",
                credential: None,
            },
            Value::Null,
            asked,
        );
        (later, sign)
    };
    hub.notify();
    assert!(until(&hub, |s| matches!(
        s.calls.get(&sign),
        Some(Progress::Done(_))
    )));
    {
        let shared = hub.lock();
        assert!(
            matches!(shared.calls.get(&later), Some(Progress::Queued)),
            "the command started while the hook was parked"
        );
        assert!(shared.queue.iter().any(|job| job.id == later));
    }
    stop(&hub);
    done_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
}

/// A held command does not start, and a hook queued after it waits too,
/// until `release`; after `release` it runs.
#[test]
fn a_held_command_blocks_the_stream_until_released() {
    let dir = extension(
        "held-blocks-stream",
        "fiber.command(\"held\", { timeout = 5000, run = function() host.log(\"held ran\") end })\n\
         fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\", run = function(call) return {} end })\n\
         fiber.provider(\"p\", { sign = { timeout = 1000, run = function() return {} end } })\n",
    );
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let asked = clock.now();
    let held = {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        shared.push_held(command("held"), Value::Null, asked)
    };
    let (thread_hub, dir) = (Arc::clone(&hub), dir.path().to_owned());
    let (done_tx, done_rx) = mpsc::channel();
    let load_by = asked.checked_add(LOAD_TIMEOUT);
    thread::spawn(move || {
        schedule::serve(
            Arc::clone(&thread_hub),
            schedule::Start {
                name: "ext".to_owned(),
                dir,
                home: PathBuf::from("/nonexistent-fiber-home"),
                load_by,
                memory_cap: MEMORY_CAP,
                browser: Arc::new(SystemBrowser::default()),
                session: None,
                secrets: Vec::new(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    // Wait until the entry script has run: the held job is still queued.
    assert!(until(&hub, |s| matches!(s.phase, Phase::Ready(_))));
    let asked = clock.now();
    let (hook, sign) = {
        let mut shared = hub.lock();
        let hook = shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            asked,
        );
        let sign = shared.push(
            Target::Provider {
                name: "p".to_owned(),
                function: "sign",
                credential: None,
            },
            Value::Null,
            asked,
        );
        (hook, sign)
    };
    hub.notify();
    // The provider runs in the gap, proving the thread judged the queue; the
    // held command and the hook behind it stay queued.
    assert!(until(&hub, |s| matches!(
        s.calls.get(&sign),
        Some(Progress::Done(_))
    )));
    {
        let shared = hub.lock();
        assert!(
            matches!(shared.calls.get(&held), Some(Progress::Queued)),
            "the held command started before release"
        );
        assert!(
            matches!(shared.calls.get(&hook), Some(Progress::Queued)),
            "the hook queued after a held command started"
        );
    }
    hub.release(held);
    assert!(until(&hub, |s| !matches!(
        s.calls.get(&held),
        Some(Progress::Queued)
    )));
    stop(&hub);
    done_rx
        .recv_timeout(WAIT_UNTIL)
        .expect("waited for the extension thread to quit");
}

/// Each bad `fiber.command` spec leaves one problem naming it, and the other
/// registrations stand.
#[test]
fn each_bad_command_spec_leaves_one_problem_and_the_rest_stand() {
    let dir = extension(
        "bad-command-specs",
        "fiber.command(\"\", { timeout = 1000, run = function() end })\n\
         fiber.command(\"has space\", { timeout = 1000, run = function() end })\n\
         fiber.command(\"a/b\", { timeout = 1000, run = function() end })\n\
         fiber.command(\"a:b\", { timeout = 1000, run = function() end })\n\
         fiber.command(\"no-timeout\", { run = function() end })\n\
         fiber.command(\"no-run\", { timeout = 1000 })\n\
         fiber.command(\"bad-desc\", { timeout = 1000, run = function() end, description = \"two\\nlines\" })\n\
         fiber.command(\"good\", { timeout = 1000, description = \"fine\", run = function() return \"good\" end })\n",
    );
    let clock = FakeClock::new();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock,
    );
    let declared = ext.hooks().expect("the entry script runs");
    assert_eq!(
        declared.problems.len(),
        7,
        "one problem per bad spec: {:?}",
        declared.problems
    );
    for name in [
        "``",
        "`has space`",
        "`a/b`",
        "`a:b`",
        "`no-timeout`",
        "`no-run`",
        "`bad-desc`",
    ] {
        assert!(
            declared.problems.iter().any(|p| p.contains(name)),
            "a problem names {name}: {:?}",
            declared.problems
        );
    }
    let commands = ext.commands().expect("commands list");
    assert_eq!(
        commands,
        vec![("good".to_owned(), "fine".to_owned())],
        "the good registration stands"
    );
}

/// A command registers with and without `description`; absent defaults to `""`.
#[test]
fn a_command_registers_with_and_without_description() {
    let dir = extension(
        "command-descriptions",
        "fiber.command(\"plain\", { timeout = 1000, run = function() end })\n\
         fiber.command(\"noted\", { timeout = 1000, description = \"Does things.\", run = function() end })\n",
    );
    let clock = FakeClock::new();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock,
    );
    let mut commands = ext.commands().expect("commands list");
    commands.sort();
    assert_eq!(
        commands,
        vec![
            ("noted".to_owned(), "Does things.".to_owned()),
            ("plain".to_owned(), String::new()),
        ]
    );
}
