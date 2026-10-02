use std::io::Write;
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;

use super::hub::Progress;
use super::*;

/// A panic in a host function is never a Lua error the extension's `pcall`
/// can catch (`docs/code-quality.md`, "Panics"). Tests run under unwind,
/// where it reaches the Rust caller past the `pcall`; Fiber's builds abort at
/// the panic.
#[test]
fn a_panic_in_a_host_function_passes_the_extensions_pcall() {
    let vm = Vm::load(
        "fixture",
        &fakes::lua_fixture(),
        Path::new("/nonexistent-fiber-home"),
        Instant::now().checked_add(LOAD_TIMEOUT),
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
    vm.deadline.restore(Instant::now().checked_add(LOAD_TIMEOUT));
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

fn ago(ms: u64) -> Instant {
    Instant::now() - Duration::from_millis(ms)
}

/// A call waiting on registration sleeps until the entry script's grace
/// ends, and no later.
#[test]
fn a_call_waiting_on_registration_sleeps_until_the_entry_scripts_grace_ends() {
    let abandon_at = Instant::now().checked_add(Duration::from_secs(5));
    let mut shared = in_phase(Phase::Registering { abandon_at });
    let id = shared.push(command("x"), Value::Null, Instant::now());
    let Next::Sleep(until) = shared.judge("ext", id, &command("x"), Instant::now()) else {
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
    let mut shared = in_phase(Phase::Registering {
            abandon_at: Some(ago(1)),
        });
    let first = shared.push(command("a"), Value::Null, ago(5));
    let second = shared.push(command("b"), Value::Null, ago(5));
    for (id, name) in [(first, "a"), (second, "b")] {
        let err = returned(shared.judge("ext", id, &command(name), ago(5))).unwrap_err();
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
    let mut shared = ready(&[("quick", 100), ("slow", 10_000)]);
    let quick = shared.push(command("quick"), Value::Null, ago(150));
    let slow = shared.push(command("slow"), Value::Null, ago(150));
    let err = returned(shared.judge("ext", quick, &command("quick"), ago(150))).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 100, .. } if callback == "quick"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    assert_eq!(shared.queue.iter().map(|job| job.id).collect::<Vec<_>>(), [slow]);
    let Next::Sleep(until) = shared.judge("ext", slow, &command("slow"), ago(150)) else {
        panic!("slow returned")
    };
    assert!(until.is_some_and(|at| at > Instant::now() + Duration::from_secs(9)));
}

/// Once registration has published, a callback it did not register fails
/// at once.
#[test]
fn a_call_the_entry_script_did_not_register_is_unknown() {
    let mut shared = ready(&[]);
    let id = shared.push(command("nope"), Value::Null, Instant::now());
    let err = returned(shared.judge("ext", id, &command("nope"), Instant::now())).unwrap_err();
    assert!(matches!(err, Error::UnknownCommand { .. }), "{err:?}");
    assert!(shared.queue.is_empty());
}

/// A parked call that the thread has not failed by its grace, because the
/// thread is busy with another callback, times out alone: the VM stays.
#[test]
fn a_parked_call_past_its_grace_times_out_and_the_vm_stays() {
    let mut shared = ready(&[("park", 100)]);
    let id = shared.push(command("park"), Value::Null, ago(2000));
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(1100)),
            parked: true,
        },
    );
    let err = returned(shared.judge("ext", id, &command("park"), ago(2000))).unwrap_err();
    assert!(matches!(err, Error::Timeout { timeout_ms: 100, .. }), "{err:?}");
    assert!(matches!(shared.phase, Phase::Ready(_)));
}

/// A running call waits its grace, then abandons the VM.
#[test]
fn a_running_call_past_its_grace_abandons_the_vm() {
    let mut shared = ready(&[("spin", 100)]);
    let id = shared.push(command("spin"), Value::Null, ago(600));
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(500)),
            parked: false,
        },
    );
    let Next::Sleep(until) = shared.judge("ext", id, &command("spin"), ago(600)) else {
        panic!("returned inside its grace")
    };
    assert!(until.is_some_and(|at| at > Instant::now()));
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(1100)),
            parked: false,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), ago(1200))).unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    assert!(matches!(shared.phase, Phase::Stopped(Error::Stopped { .. })));
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
        for _ in 0..2 {
            let id = shared.push(command("x"), Value::Null, Instant::now());
            let err = returned(shared.judge("ext", id, &command("x"), Instant::now())).unwrap_err();
            assert_eq!(err.to_string(), text);
        }
    }
}

/// A finished call returns its result, even after the extension stopped.
#[test]
fn a_finished_call_returns_its_result() {
    let mut shared = in_phase(Phase::Stopped(hub::stopped("ext")));
    let id = shared.push(command("x"), Value::Null, Instant::now());
    shared.finish(id, Ok(Value::String("done".to_owned())));
    assert_eq!(
        returned(shared.judge("ext", id, &command("x"), Instant::now())).unwrap(),
        "done"
    );
    assert!(shared.calls.is_empty());
}

/// An extension dropped while running is stopped, so its thread quits.
#[test]
fn dropping_the_extension_stops_it() {
    let ext = LuaExtension::new(
        "fixture",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
    );
    assert_eq!(ext.command("echo", "hi").unwrap(), "hi");
    let hub = Arc::clone(&ext.hub);
    drop(ext);
    assert!(matches!(hub.lock().phase, Phase::Stopped(Error::Stopped { .. })));
}

/// An extension directory in a fresh temporary directory, with `init`.
fn extension(tag: &str, init: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fiber-lua-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("init.lua"), init).unwrap();
    dir
}

/// Runs the thread on `dir`, once a call to `first` is queued.
fn serve_after(dir: &Path, first: &str) -> (Arc<Hub>, u64, thread::JoinHandle<()>) {
    let hub = Arc::<Hub>::default();
    let id = {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        shared.push(command(first), Value::String(String::new()), Instant::now())
    };
    let (thread_hub, dir) = (Arc::clone(&hub), dir.to_owned());
    let handle = thread::spawn(move || {
        schedule::serve(
            "ext",
            &dir,
            Path::new("/nonexistent-fiber-home"),
            &thread_hub,
            Instant::now().checked_add(LOAD_TIMEOUT),
        );
    });
    (hub, id, handle)
}

/// Waits until `check` holds for the hub's state, or two seconds pass.
fn until(hub: &Hub, check: impl Fn(&Shared) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(2);
    let mut shared = hub.lock();
    while !check(&shared) {
        if Instant::now() >= end {
            return false;
        }
        shared = hub.wait(shared, Some(end));
    }
    true
}

fn stop(hub: &Hub) {
    hub.lock().phase = Phase::Stopped(hub::stopped("ext"));
    hub.notify();
}

/// A stopped extension's thread quits while a callback is parked, and the
/// call queued behind it never starts.
#[test]
fn the_thread_quits_once_stopped_with_a_callback_parked() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        accepted_tx.send(()).unwrap();
        thread::sleep(Duration::from_secs(3));
        drop(sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"));
    });
    let dir = extension(
        "parked",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n"
        ),
    );
    let (hub, hold, handle) = serve_after(&dir, "hold");
    accepted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hold never reached the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let later = hub
        .lock()
        .push(command("later"), Value::Null, Instant::now());
    stop(&hub);
    let end = Instant::now() + Duration::from_secs(2);
    while !handle.is_finished() && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.is_finished(), "the thread kept running once stopped");
    assert!(matches!(hub.lock().calls.get(&later), Some(Progress::Queued)));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A thread stopped while a callback runs quits when that callback ends,
/// and does not start the call queued behind it.
#[test]
fn the_thread_quits_after_the_running_callback_once_stopped() {
    let dir = extension(
        "running",
        "fiber.command(\"spin\", { timeout = 300, run = function() while true do end end })\n\
         fiber.command(\"later\", { timeout = 5000, run = function() return \"later\" end })\n",
    );
    let (hub, spin, handle) = serve_after(&dir, "spin");
    let later = hub
        .lock()
        .push(command("later"), Value::Null, Instant::now());
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Started { parked: false, .. })
    )));
    stop(&hub);
    assert!(until(&hub, |s| matches!(s.calls.get(&spin), Some(Progress::Done(_)))));
    let end = Instant::now() + Duration::from_secs(2);
    while !handle.is_finished() && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.is_finished(), "the thread kept running once stopped");
    assert!(matches!(hub.lock().calls.get(&later), Some(Progress::Queued)));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Accepts one connection, signals, and answers after five seconds.
fn hold_server() -> (String, mpsc::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        accepted_tx.send(()).unwrap();
        thread::sleep(Duration::from_secs(5));
        drop(sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"));
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
               sign = {{ timeout = 50, run = function() setmetatable({{}}, {{ __gc = function() while true do end end }}); collectgarbage() end }},\n\
             }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let (tx, rx) = mpsc::channel();
    let run = |what: &'static str, f: fn(&LuaExtension) -> Result<Value, Error>| {
        let (ext, tx) = (Arc::clone(&ext), tx.clone());
        thread::spawn(move || tx.send((what, f(&ext))));
    };
    let started = Instant::now();
    run("park", |e| e.command("park", "").map(Value::String));
    park_accepted.recv_timeout(Duration::from_secs(3)).unwrap();
    run("models", |e| e.provider_call("p", "models", Value::Null));
    models_accepted.recv_timeout(Duration::from_secs(3)).unwrap();
    run("queued", |e| e.command("queued", "").map(Value::String));
    run("sign", |e| e.provider_call("p", "sign", Value::Null));
    let mut seen = BTreeMap::new();
    for _ in 0..4 {
        let (what, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        seen.insert(what, result.unwrap_err());
    }
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the waiters took {:?}, not the abandonment",
        started.elapsed()
    );
    assert!(matches!(seen["sign"], Error::Abandoned { .. }), "{:?}", seen["sign"]);
    for what in ["park", "models", "queued"] {
        assert!(matches!(seen[what], Error::Stopped { .. }), "{what}: {:?}", seen[what]);
    }
    assert!(!ext.is_running());
    std::fs::remove_dir_all(&dir).unwrap();
}
