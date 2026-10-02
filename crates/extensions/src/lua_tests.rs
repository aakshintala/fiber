use std::io::Read;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;

use fakes::clock::FakeClock;

use super::hub::Progress;
use super::*;

/// Wall-clock bound on a wait for the VM, a server, or a thread. The load
/// soak runs this process at background priority, so a few seconds is not
/// enough for the VM to reach a test server.
const WAIT: Duration = Duration::from_secs(60);

/// A callback timeout whose `host.http` must stay open across [`WAIT`].
/// ureq's bound is this plus the grace, and that bound is wall time.
const HOLD_MS: u64 = 180_000;

/// A panic in a host function is never a Lua error the extension's `pcall`
/// can catch (`docs/code-quality.md`, "Panics"). Tests run under unwind,
/// where it reaches the Rust caller past the `pcall`; Fiber's builds abort at
/// the panic.
#[test]
fn a_panic_in_a_host_function_passes_the_extensions_pcall() {
    let clock = FakeClock::new();
    let vm = Vm::load(
        "fixture",
        &fakes::lua_fixture(),
        Path::new("/nonexistent-fiber-home"),
        clock.clone(),
        Some(clock.now().checked_add(LOAD_TIMEOUT).unwrap()),
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
        timeouts
            .commands
            .insert((*name).to_owned(), Duration::from_millis(*ms));
    }
    in_phase(Phase::Ready(timeouts))
}

fn returned(next: Next) -> Result<Value, Error> {
    match next {
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
    let Next::Sleep(until) = shared.judge("ext", id, &command("x"), asked, asked) else {
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
    let Next::Sleep(until) = shared.judge("ext", slow, &command("slow"), asked, at) else {
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
    let Next::Sleep(until) = shared.judge("ext", id, &command("spin"), asked, at) else {
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
    waiting_rx.recv().unwrap();
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
    let ext = LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
        FakeClock::new(),
    );
    assert_eq!(ext.command("echo", "hi").unwrap(), "hi");
    let hub = Arc::clone(&ext.hub);
    drop(ext);
    assert!(matches!(
        hub.lock().phase,
        Phase::Stopped(Error::Stopped { .. })
    ));
}

/// An extension directory in a fresh temporary directory, with `init`.
fn extension(tag: &str, init: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fiber-lua-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.lua"), init).unwrap();
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
            "ext",
            &dir,
            Path::new("/nonexistent-fiber-home"),
            &thread_hub,
            load_by,
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
    hub.wait_for(shared, WAIT, check)
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
            "fiber.command(\"hold\", {{ timeout = {HOLD_MS}, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(&dir, "hold", &clock);
    accepted_rx
        .recv_timeout(WAIT)
        .expect("waited for hold to reach the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let later = hub.lock().push(command("later"), Value::Null, clock.now());
    stop(&hub);
    done.recv_timeout(WAIT)
        .expect("waited for the extension thread to quit");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A thread stopped while a callback runs quits when that callback ends,
/// and does not start the call queued behind it.
#[test]
fn the_thread_quits_after_the_running_callback_once_stopped() {
    let dir = extension(
        "running",
        "fiber.command(\"spin\", { timeout = 1000, run = function() while true do end end })\n\
         fiber.command(\"later\", { timeout = 5000, run = function() return \"later\" end })\n",
    );
    let clock = FakeClock::new();
    let (hub, spin, done) = serve_after(&dir, "spin", &clock);
    let later = hub.lock().push(command("later"), Value::Null, clock.now());
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
    done.recv_timeout(WAIT)
        .expect("waited for the extension thread to quit");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
    std::fs::remove_dir_all(&dir).unwrap();
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
/// fires once that read has opened the file. Sending on the returned sender,
/// or dropping it, ends the read.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn hold_open(dir: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let path = dir.join("hold.lua");
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || {
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
        match block_rx.recv() {
            Ok(()) | Err(mpsc::RecvError) => {}
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
            "fiber.command(\"park\", {{ timeout = {HOLD_MS}, run = function() return host.http({{ url = \"{park_url}\" }}).body end }})\n\
             fiber.command(\"queued\", {{ timeout = 8000, run = function() return \"ran\" end }})\n\
             fiber.provider(\"p\", {{\n\
               models = {{ timeout = {HOLD_MS}, run = function() return host.http({{ url = \"{models_url}\" }}).body end }},\n\
               sign = {{ timeout = 50, run = function() setmetatable({{}}, {{ __gc = function() require(\"hold\") end }}); collectgarbage() end }},\n\
             }})\n"
        ),
    );
    // The finalizer blocks in `require("hold")`, after the hook is off and
    // after the callback's last clock check.
    let (held, _release) = hold_open(&dir);
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        &dir,
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
        .recv_timeout(WAIT)
        .expect("waited for park to reach the server");
    run("models", |e| e.provider_call("p", "models", Value::Null));
    models_accepted
        .recv_timeout(WAIT)
        .expect("waited for models to reach the server");
    run("queued", |e| e.command("queued", "").map(Value::String));
    run("sign", |e| e.provider_call("p", "sign", Value::Null));
    held.recv_timeout(WAIT)
        .expect("waited for sign to block past its clock checks");
    let abandon = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon, WAIT),
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
    std::fs::remove_dir_all(&dir).unwrap();
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
