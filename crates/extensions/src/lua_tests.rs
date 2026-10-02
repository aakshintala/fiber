use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::mpsc;

use contract::ErrorCode;

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
    vm.deadline
        .restore(Instant::now().checked_add(LOAD_TIMEOUT));
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        vm.resume(f, "boom", LOAD_TIMEOUT, mlua::Value::Nil)
    }));
    assert!(outcome.is_err(), "the panic became {outcome:?}");
}

/// The fixture's VM with no deadline on its entry script, so how fast the
/// runner loads it cannot fail the test.
fn fixture_vm() -> Vm {
    match Vm::load(
        "fixture",
        &fakes::lua_fixture(),
        Path::new("/nonexistent-fiber-home"),
        None,
    ) {
        Ok(vm) => vm,
        Err(e) => panic!("{e:?}"),
    }
}

/// Resumes the fixture's command `name` on its own thread with its deadline
/// already passed, and sends the error's message once the resume returns. The hook first looks
/// at the clock `CHECK_EVERY` instructions in, past the few each command
/// runs before its loop, so only the hook's reach decides whether the
/// command stops, not how fast the runner is.
fn resume_past_its_deadline(name: &'static str) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let vm = fixture_vm();
        let run = vm
            .commands
            .get::<Table>(name)
            .and_then(|spec| spec.get::<Function>("run"))
            .unwrap();
        let thread = vm.lua.create_thread(run).unwrap();
        vm.deadline.arm(&thread).unwrap();
        vm.deadline.restore(Some(ago(1)));
        tx.send(thread.resume::<()>(()).map_err(|e| setup::message(&e)))
    });
    rx
}

/// The hook stops a Lua loop past its deadline. It cannot stop a `__gc`
/// finalizer (Lua turns hooks off there) or a backtracking `string.find`
/// (one C call): both still run a grace period later, when the caller
/// abandons the VM. A slow runner can only make this pass, never fail.
#[test]
fn the_hook_stops_a_loop_but_not_a_finalizer_or_a_long_c_call() {
    let unstoppable = ["spin_gc", "spin_find"].map(|name| (name, resume_past_its_deadline(name)));
    let err = resume_past_its_deadline("spin")
        .recv_timeout(WAIT)
        .expect("the hook did not stop the loop")
        .unwrap_err();
    assert_eq!(err, "the callback passed its timeout");
    let end = Instant::now() + GRACE;
    for (name, rx) in unstoppable {
        let left = end.saturating_duration_since(Instant::now());
        let got = rx.recv_timeout(left);
        assert!(
            matches!(got, Err(mpsc::RecvTimeoutError::Timeout)),
            "{name} ended within the grace: {got:?}"
        );
    }
}

/// Past the memory cap an allocation is a Lua error, and the VM still runs
/// the next callback. No deadline, so only the cap can stop `grow`.
#[test]
fn unbounded_allocation_is_an_error_in_that_vm_and_the_vm_stays_usable() {
    let vm = fixture_vm();
    let arg = Value::String(String::new());
    for _ in 0..2 {
        let err = match vm.step(&command("grow"), &arg, Duration::ZERO, None) {
            Err(e) => e,
            Ok(_) => panic!("grow returned"),
        };
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert!(message.contains("not enough memory"), "{message}");
        let echo = vm.step(
            &command("echo"),
            &Value::String("alive".to_owned()),
            Duration::ZERO,
            None,
        );
        assert!(matches!(echo, Ok(Step::Done(Value::String(s))) if s == "alive"));
    }
}

/// What loading an extension whose entry script requires `big.lua`, of
/// `len` bytes, fails with. No deadline, so how fast the runner reads it
/// cannot fail the test.
fn load_with_module_of(tag: &str, len: usize) -> String {
    let dir = extension(tag, "local m = require(\"big\")\n");
    let big = std::fs::File::create(dir.join("big.lua")).unwrap();
    big.set_len(u64::try_from(len).unwrap()).unwrap();
    let loaded = Vm::load("ext", &dir, Path::new("/nonexistent-fiber-home"), None);
    std::fs::remove_dir_all(&dir).unwrap();
    match loaded {
        Err(Error::Lua { message, .. }) => message,
        Err(e) => panic!("{e:?}"),
        Ok(_) => panic!("the entry script loaded"),
    }
}

#[test]
fn a_module_larger_than_the_memory_cap_is_not_read() {
    let message = load_with_module_of("big-over", MEMORY_CAP + 1);
    assert!(
        message.starts_with("init.lua:1: `big.lua` is larger than"),
        "{message}"
    );
    assert!(message.contains("memory cap"), "{message}");
}

#[test]
fn a_module_exactly_the_memory_cap_is_read() {
    // Read and compiled: its zero bytes are not Lua.
    let message = load_with_module_of("big-at", MEMORY_CAP);
    assert!(message.starts_with("init.lua:1: big.lua:"), "{message}");
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
    Instant::now()
        .checked_sub(Duration::from_millis(ms))
        .unwrap()
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

/// A call asked during registration keeps that instant once the entry
/// script publishes the callback's own timeout.
#[test]
fn a_call_queued_during_registration_keeps_its_declared_timeout() {
    let asked = ago(50);
    let abandon_at = Instant::now().checked_add(Duration::from_secs(30));
    let mut shared = in_phase(Phase::Registering { abandon_at });
    let id = shared.push(command("work"), Value::Null, asked);
    let Next::Sleep(until) = shared.judge("ext", id, &command("work"), asked) else {
        panic!("a waiter returned during registration")
    };
    assert_eq!(until, abandon_at);
    let mut published = ready(&[("work", 5000)]);
    shared.phase = std::mem::replace(&mut published.phase, Phase::Idle);
    let Next::Sleep(until) = shared.judge("ext", id, &command("work"), asked) else {
        panic!("work returned")
    };
    assert_eq!(until, asked.checked_add(Duration::from_millis(5000)));
}

/// A command whose name is `<provider>.<function>` keeps the timeout it
/// declared. Asked 100 ms ago, that is still ahead, and the provider
/// function's 50 ms timeout has already passed.
#[test]
fn a_command_named_like_a_provider_function_keeps_its_own_timeout() {
    let dir = extension(
        "collide",
        "fiber.command(\"p.sign\", { timeout = 5000, run = function() return \"ok\" end })\n\
         fiber.provider(\"p\", { sign = { timeout = 50, run = function() return {} end } })\n",
    );
    let vm = Vm::load(
        "ext",
        &dir,
        Path::new("/nonexistent-fiber-home"),
        Instant::now().checked_add(LOAD_TIMEOUT),
    )
    .unwrap();
    let command = command("p.sign");
    let provider = Target::Provider {
        name: "p".to_owned(),
        function: "sign",
    };
    let timeouts = vm.declared();
    assert_eq!(
        timeouts.timeout(&command),
        Some(Duration::from_millis(5000))
    );
    assert_eq!(timeouts.timeout(&provider), Some(Duration::from_millis(50)));
    let asked = ago(100);
    let mut shared = in_phase(Phase::Ready(timeouts));
    let command_id = shared.push(command.clone(), Value::Null, asked);
    let Next::Sleep(until) = shared.judge("ext", command_id, &command, asked) else {
        panic!("the command used the provider function's 50 ms timeout")
    };
    assert_eq!(until, asked.checked_add(Duration::from_millis(5000)));
    let provider_id = shared.push(provider.clone(), Value::Null, asked);
    let err = returned(shared.judge("ext", provider_id, &provider, asked)).unwrap_err();
    assert!(
        matches!(err, Error::Timeout { timeout_ms: 50, .. }),
        "{err:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
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
    assert_eq!(
        shared.queue.iter().map(|job| job.id).collect::<Vec<_>>(),
        [slow]
    );
    let asked = ago(150);
    let Next::Sleep(until) = shared.judge("ext", slow, &command("slow"), asked) else {
        panic!("slow returned")
    };
    assert_eq!(until, asked.checked_add(Duration::from_secs(10)));
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
    let id = shared.push(command("park"), Value::Null, ago(60_000));
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(60_000)),
            parked: true,
        },
    );
    let err = returned(shared.judge("ext", id, &command("park"), ago(60_000))).unwrap_err();
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

/// A parked call inside its grace is not failed by its caller. The thread
/// fails it at the deadline; the caller waits the grace out.
#[test]
fn a_parked_call_inside_its_grace_keeps_waiting() {
    let mut shared = ready(&[("park", 100)]);
    let asked = ago(100);
    let id = shared.push(command("park"), Value::Null, asked);
    shared.queue.clear();
    let deadline = Instant::now();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(deadline),
            parked: true,
        },
    );
    let Next::Sleep(until) = shared.judge("ext", id, &command("park"), asked) else {
        panic!("the caller failed a parked call inside its grace")
    };
    assert_eq!(until, deadline.checked_add(GRACE));
    assert!(matches!(shared.phase, Phase::Ready(_)));
}

/// A running call waits its grace, then abandons the VM.
#[test]
fn a_running_call_past_its_grace_abandons_the_vm() {
    let mut shared = ready(&[("spin", 100)]);
    let asked = Instant::now();
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    // Still ahead of its deadline, so a stall cannot enter the grace.
    // Far past the grace is the second half.
    let deadline = Instant::now() + Duration::from_secs(60);
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(deadline),
            parked: false,
        },
    );
    let Next::Sleep(until) = shared.judge("ext", id, &command("spin"), asked) else {
        panic!("returned before its deadline")
    };
    assert_eq!(until, deadline.checked_add(GRACE));
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(60_000)),
            parked: false,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), asked)).unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    assert!(matches!(
        shared.phase,
        Phase::Stopped(Error::Stopped { .. })
    ));
}

/// A running call whose deadline has passed, and whose grace has not, keeps
/// waiting. The grace is one second, so a deadline of this instant is inside
/// it for the rest of that second.
#[test]
fn a_running_call_inside_its_grace_keeps_waiting() {
    let mut shared = ready(&[("spin", 100)]);
    let asked = ago(100);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    let deadline = Instant::now();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(deadline),
            parked: false,
        },
    );
    let Next::Sleep(until) = shared.judge("ext", id, &command("spin"), asked) else {
        panic!("abandoned inside its grace")
    };
    assert_eq!(until, deadline.checked_add(GRACE));
    assert!(matches!(shared.phase, Phase::Ready(_)));
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

/// A finished call returns its result while the extension is up.
#[test]
fn a_finished_call_returns_its_result() {
    let mut shared = ready(&[("x", 1000)]);
    let id = shared.push(command("x"), Value::Null, Instant::now());
    shared.finish(id, Ok(Value::String("done".to_owned())));
    assert_eq!(
        returned(shared.judge("ext", id, &command("x"), Instant::now())).unwrap(),
        "done"
    );
    assert!(shared.calls.is_empty());
}

/// Once the extension is stopped, a waiter holding a finished result gets
/// the stopped error: Stopped is final for every waiter.
#[test]
fn a_finished_call_on_a_stopped_extension_gets_the_stopped_error() {
    let mut shared = in_phase(Phase::Stopped(hub::stopped("ext")));
    let id = shared.push(command("x"), Value::Null, Instant::now());
    shared.finish(id, Ok(Value::String("done".to_owned())));
    let err = returned(shared.judge("ext", id, &command("x"), Instant::now())).unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
    assert!(shared.calls.is_empty());
}

/// When `provider_functions` abandons registration, it wakes every other
/// waiter. The other waiter here sleeps with no limit, so only that wake
/// ends its wait.
#[test]
fn provider_functions_abandoning_registration_wakes_every_waiter() {
    let ext = Arc::new(LuaExtension::new(
        "ext",
        fakes::lua_fixture(),
        "/nonexistent-fiber-home",
    ));
    ext.hub.lock().phase = Phase::Registering {
        abandon_at: Some(ago(1)),
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
        .recv_timeout(Duration::from_secs(10))
        .expect("provider_functions never returned")
        .unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    // A hang guard only: without the wake the waiter never returns.
    woke_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the other waiter was never woken");
}

/// An extension dropped while running is stopped, so its thread quits.
#[test]
fn dropping_the_extension_stops_it() {
    let ext = LuaExtension::new("fixture", fakes::lua_fixture(), "/nonexistent-fiber-home");
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
            &Instant::now,
        );
    });
    (hub, id, handle)
}

/// Waits until `check` holds for the hub's state, or five seconds pass.
fn until(hub: &Hub, check: impl Fn(&Shared) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
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

/// Whether `handle` finishes before the deadline. The join itself is the
/// signal; the deadline is only how long this test waits for it.
fn thread_done(handle: thread::JoinHandle<()>) -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let finished = handle.join().is_ok();
        tx.send(finished)
    });
    matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(true))
}

/// A stopped extension's thread quits while a callback is parked, and the
/// call queued behind it never starts.
#[test]
fn the_thread_quits_once_stopped_with_a_callback_parked() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    thread::spawn(move || hold_until_close(listener, accepted_tx));
    let dir = extension(
        "parked",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n"
        ),
    );
    let (hub, hold, handle) = serve_after(&dir, "hold");
    accepted_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("hold never reached the server");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started { parked: true, .. })
    )));
    let later = hub
        .lock()
        .push(command("later"), Value::Null, Instant::now());
    stop(&hub);
    assert!(thread_done(handle), "the thread kept running once stopped");
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
    let (hub, spin, handle) = serve_after(&dir, "spin");
    let later = hub
        .lock()
        .push(command("later"), Value::Null, Instant::now());
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Started { parked: false, .. })
    )));
    stop(&hub);
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Done(_))
    )));
    assert!(thread_done(handle), "the thread kept running once stopped");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Reads until the client drops. The first bytes are the signal that the
/// request was sent, which is past the point where the socket timeout is set.
fn hold_until_close(listener: TcpListener, accepted: mpsc::Sender<()>) {
    let mut sock = listener.accept().unwrap().0;
    let mut buf = [0u8; 1024];
    let mut signaled = false;
    while let Ok(n) = sock.read(&mut buf) {
        if n == 0 {
            break;
        }
        if !signaled {
            signaled = true;
            accepted.send(()).unwrap();
        }
    }
}

/// An extension whose hub says registration published `timeouts`, with no
/// thread: the test starts and parks calls itself.
fn ready_extension(timeouts: CallbackTimeouts) -> Arc<LuaExtension> {
    let ext = Arc::new(LuaExtension::new(
        "ext",
        "/nonexistent-extension",
        "/nonexistent-fiber-home",
    ));
    ext.hub.lock().phase = Phase::Ready(timeouts);
    ext
}

/// Moves the queued call to `name` onto the thread, as the scheduler does,
/// with `deadline`. `parked` as `settle` leaves a call on `host.http`.
fn start_queued(shared: &mut Shared, name: &str, deadline: Instant, parked: bool) {
    let pos = shared
        .queue
        .iter()
        .position(|job| job.target.to_string() == name)
        .unwrap_or_else(|| panic!("{name} is not queued"));
    let job = shared.queue.remove(pos).unwrap();
    shared.calls.insert(
        job.id,
        Progress::Started {
            deadline: Some(deadline),
            parked,
        },
    );
}

/// When a running callback abandons the VM, every other waiter, queued or
/// parked, command or provider call, gets `Stopped` at once rather than at
/// its own deadline, and so does the next caller. The test plays the
/// thread: the running call's deadline and grace are long past and every
/// other deadline is far ahead, so no clock decides the outcome.
#[test]
fn an_abandoned_vm_wakes_every_queued_and_parked_waiter() {
    let minute = Duration::from_secs(60);
    let mut timeouts = CallbackTimeouts::default();
    for name in ["park", "queued"] {
        timeouts.commands.insert(name.to_owned(), minute);
    }
    timeouts.providers.insert(
        "p".to_owned(),
        BTreeMap::from([("models".to_owned(), minute), ("sign".to_owned(), minute)]),
    );
    let ext = ready_extension(timeouts);
    let (tx, rx) = mpsc::channel();
    let run = |what: &'static str, f: fn(&LuaExtension) -> Result<Value, Error>| {
        let (ext, tx) = (Arc::clone(&ext), tx.clone());
        thread::spawn(move || tx.send((what, f(&ext))));
    };
    run("park", |e| e.command("park", "").map(Value::String));
    run("models", |e| e.provider_call("p", "models", Value::Null));
    run("queued", |e| e.command("queued", "").map(Value::String));
    run("sign", |e| e.provider_call("p", "sign", Value::Null));
    assert!(
        until(&ext.hub, |s| s.queue.len() == 4),
        "the four calls were not queued"
    );
    {
        let mut shared = ext.hub.lock();
        let ahead = Instant::now() + minute;
        start_queued(&mut shared, "park", ahead, true);
        start_queued(&mut shared, "p.models", ahead, true);
        start_queued(&mut shared, "p.sign", ago(60_000), false);
    }
    ext.hub.notify();
    let mut seen = BTreeMap::new();
    for _ in 0..4 {
        let (what, result) = rx.recv_timeout(WAIT).expect("a waiter did not return");
        seen.insert(what, result.unwrap_err());
    }
    let Error::Abandoned { callback, .. } = &seen["sign"] else {
        panic!("{:?}", seen["sign"])
    };
    assert_eq!(callback, "p.sign");
    assert_eq!(seen["sign"].code(), ErrorCode::ExtensionFailed);
    for what in ["park", "models", "queued"] {
        assert!(
            matches!(seen[what], Error::Stopped { .. }),
            "{what}: {:?}",
            seen[what]
        );
    }
    assert!(!ext.is_running());
    let err = start(&ext, "queued")
        .recv_timeout(WAIT)
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
}

/// A call that finds no thread started fails rather than wait forever.
#[test]
fn a_call_on_an_extension_with_no_thread_is_stopped() {
    let mut shared = Shared::default();
    let id = shared.push(command("x"), Value::Null, Instant::now());
    let err = returned(shared.judge("ext", id, &command("x"), Instant::now())).unwrap_err();
    assert!(matches!(err, Error::Stopped { .. }), "{err:?}");
}

/// How long a test waits for one call before failing.
const WAIT: Duration = Duration::from_secs(10);

const HTTP_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

fn read_request(sock: &mut TcpStream) {
    let mut got = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = sock.read(&mut buf).expect("reading the request");
        assert!(n > 0, "the client closed before its request");
        got.extend_from_slice(buf.get(..n).unwrap());
        if got.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
        assert!(got.len() <= 8192, "the request header never ended");
    }
}

/// Accepts `times` connections. Each is answered when `release` arrives,
/// after the request is on the socket.
fn answer_n(
    listener: TcpListener,
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

fn start(ext: &Arc<LuaExtension>, command: &'static str) -> mpsc::Receiver<Result<String, Error>> {
    let (tx, rx) = mpsc::channel();
    let ext = Arc::clone(ext);
    thread::spawn(move || tx.send(ext.command(command, "")));
    rx
}

fn command_queued(shared: &Shared, name: &str) -> usize {
    shared
        .queue
        .iter()
        .filter(|job| matches!(&job.target, Target::Command(got) if got == name))
        .count()
}

fn parked(shared: &Shared) -> usize {
    shared
        .calls
        .values()
        .filter(|progress| matches!(progress, Progress::Started { parked: true, .. }))
        .count()
}

/// Registration is still running, and the queued commands are `names`, in order.
fn queued_during_registration(shared: &Shared, names: &[&str]) -> bool {
    let queued: Vec<&str> = shared
        .queue
        .iter()
        .filter_map(|job| match &job.target {
            Target::Command(name) => Some(name.as_str()),
            Target::Provider { .. } => None,
        })
        .collect();
    matches!(shared.phase, Phase::Registering { .. }) && queued == names
}

#[test]
fn a_second_command_waits_until_the_parked_command_finishes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(listener, accepted_tx, release_rx, 1));
    let dir = extension(
        "second",
        &format!(
            "fiber.command(\"first\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"second\", {{ timeout = 5000, run = function() return \"second\" end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let first = start(&ext, "first");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("the first command never reached the server");
    assert_eq!(ext.provider_functions("p").unwrap(), ["sign"]);
    let second = start(&ext, "second");
    assert!(
        until(&ext.hub, |s| parked(s) == 1
            && command_queued(s, "second") == 1),
        "the second command was not queued behind the parked first"
    );
    release_tx.send(()).unwrap();
    assert_eq!(first.recv_timeout(WAIT).unwrap().unwrap(), "ok");
    assert_eq!(second.recv_timeout(WAIT).unwrap().unwrap(), "second");
    assert!(ext.is_running());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Whether a parked call ended on its own terms: its reply, or its own
/// timeout of `timeout_ms` if a slow runner released it too late. Never
/// another call's error.
fn ended_on_its_own(result: &Result<String, Error>, timeout_ms: u64) -> bool {
    match result {
        Ok(_) => true,
        Err(Error::Timeout {
            timeout_ms: got, ..
        }) => *got == timeout_ms,
        Err(_) => false,
    }
}

#[test]
fn a_queued_command_times_out_on_its_own_deadline_and_the_vm_stays() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(listener, accepted_tx, release_rx, 1));
    let dir = extension(
        "queued-timeout",
        &format!(
            "seen = \"no\"\n\
             fiber.command(\"slow\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 300, run = function() seen = \"yes\"; return \"ran\" end }})\n\
             fiber.command(\"after\", {{ timeout = 1000, run = function() return seen end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let slow = start(&ext, "slow");
    accepted_rx
        .recv_timeout(WAIT)
        .expect("the slow command never reached the server");
    let quick = start(&ext, "quick");
    assert!(
        until(&ext.hub, |s| {
            parked(s) == 1 && command_queued(s, "quick") == 1
        }),
        "quick was not queued behind the parked slow command"
    );
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
    let slow = slow.recv_timeout(WAIT).unwrap();
    assert!(ended_on_its_own(&slow, 5000), "{slow:?}");
    assert_eq!(
        start(&ext, "after").recv_timeout(WAIT).unwrap().unwrap(),
        "no"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_call_made_before_registration_finishes_uses_its_declared_timeout() {
    // The scheduler assigns the deadline. A shorter one fails here, before
    // the public call below waits out the real five seconds.
    scheduler_keeps_the_deadline_declared_while_registering();
    let reg = TcpListener::bind("127.0.0.1:0").unwrap();
    let work = TcpListener::bind("127.0.0.1:0").unwrap();
    let reg_url = format!("http://{}/", reg.local_addr().unwrap());
    let work_url = format!("http://{}/", work.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (work_ok_tx, work_ok_rx) = mpsc::channel();
    // Held open: dropping it closes the socket, and the call fails before its timeout.
    let (work_release_tx, work_release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(reg, accepted_tx, release_rx, 1));
    thread::spawn(move || answer_n(work, work_ok_tx, work_release_rx, 1));
    let dir = extension(
        "reg-timeout",
        &format!(
            "host.http({{ url = \"{reg_url}\" }})\n\
             fiber.command(\"work\", {{ timeout = 5000, run = function() return host.http({{ url = \"{work_url}\" }}).body end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let first = start(&ext, "work");
    let second = start(&ext, "work");
    assert!(
        until(&ext.hub, |s| queued_during_registration(
            s,
            &["work", "work"]
        )),
        "both work calls were not queued during registration"
    );
    accepted_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    release_tx.send(()).unwrap();
    work_ok_rx
        .recv_timeout(WAIT)
        .expect("work never reached the server");
    // The reply stays unsent. A shorter timeout comes back as that timeout.
    for waiter in [first, second] {
        let err = waiter.recv_timeout(WAIT).unwrap().unwrap_err();
        let Error::Timeout { timeout_ms, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(*timeout_ms, 5000);
    }
    assert!(ext.is_running());
    drop(work_release_tx);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_call_still_waiting_on_registration_times_out_from_when_it_was_asked() {
    let reg = TcpListener::bind("127.0.0.1:0").unwrap();
    let hold = TcpListener::bind("127.0.0.1:0").unwrap();
    let reg_url = format!("http://{}/", reg.local_addr().unwrap());
    let hold_url = format!("http://{}/", hold.local_addr().unwrap());
    let (reg_ok_tx, reg_ok_rx) = mpsc::channel();
    let (reg_release_tx, reg_release_rx) = mpsc::channel();
    let (hold_ok_tx, hold_ok_rx) = mpsc::channel();
    let (hold_release_tx, hold_release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(reg, reg_ok_tx, reg_release_rx, 1));
    thread::spawn(move || answer_n(hold, hold_ok_tx, hold_release_rx, 1));
    let dir = extension(
        "reg-quick",
        &format!(
            "host.http({{ url = \"{reg_url}\" }})\n\
             fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{hold_url}\" }}).body end }})\n\
             fiber.command(\"quick\", {{ timeout = 400, run = function() return \"ran\" end }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    // `hold` is queued before `quick` is asked, so `quick` cannot win the queue.
    let held = start(&ext, "hold");
    assert!(
        until(&ext.hub, |s| queued_during_registration(s, &["hold"])),
        "hold was not queued during registration"
    );
    let quick = start(&ext, "quick");
    assert!(
        until(&ext.hub, |s| queued_during_registration(
            s,
            &["hold", "quick"]
        )),
        "quick was not queued behind hold during registration"
    );
    reg_ok_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    reg_release_tx.send(()).unwrap();
    hold_ok_rx
        .recv_timeout(WAIT)
        .expect("hold never reached the server");
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
    let held = held.recv_timeout(WAIT).unwrap();
    assert!(ended_on_its_own(&held, 5000), "{held:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The verify-3 hang: an entry script the hook cannot stop, with two calls
/// waiting on it. Both get `Abandoned` once its grace ends, neither waits
/// forever, and a later call gets it at once. The test plays the thread:
/// registration's grace ends when the test moves it into the past, so no
/// clock decides the outcome. That the hook cannot stop such a script is
/// `the_hook_stops_a_loop_but_not_a_finalizer_or_a_long_c_call`.
#[test]
fn every_call_waiting_on_an_entry_script_the_hook_cannot_stop_is_abandoned() {
    let ext = Arc::new(LuaExtension::new(
        "ext",
        "/nonexistent-extension",
        "/nonexistent-fiber-home",
    ));
    ext.hub.lock().phase = Phase::Registering {
        abandon_at: Some(Instant::now() + Duration::from_secs(60)),
    };
    let first = start(&ext, "a");
    let second = start(&ext, "b");
    assert!(
        until(&ext.hub, |s| s.queue.len() == 2),
        "the two calls were not queued during registration"
    );
    ext.hub.lock().phase = Phase::Registering {
        abandon_at: Some(ago(1)),
    };
    ext.hub.notify();
    for waiter in [first, second] {
        let err = waiter.recv_timeout(WAIT).unwrap().unwrap_err();
        let Error::Abandoned { callback, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(callback, "init.lua");
    }
    assert!(!ext.is_running());
    let err = start(&ext, "a").recv_timeout(WAIT).unwrap().unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
}

/// An entry script that errors stops the extension with its error, for
/// every call that waited on it and every later call.
#[test]
fn every_call_waiting_on_an_entry_script_that_errors_gets_its_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(listener, accepted_tx, release_rx, 1));
    let dir = extension(
        "reg-error",
        &format!("host.http({{ url = \"{url}\" }})\nerror(\"bad\")\n"),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let first = start(&ext, "a");
    assert!(
        until(&ext.hub, |s| queued_during_registration(s, &["a"])),
        "the first call was not queued during registration"
    );
    accepted_rx
        .recv_timeout(WAIT)
        .expect("registration never reached the server");
    let second = start(&ext, "b");
    assert!(
        until(&ext.hub, |s| queued_during_registration(s, &["a", "b"])),
        "the second call was not queued during registration"
    );
    release_tx.send(()).unwrap();
    for waiter in [first, second] {
        let err = waiter.recv_timeout(WAIT).unwrap().unwrap_err();
        let Error::Lua { message, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(message, "init.lua:2: bad");
    }
    assert!(!ext.is_running());
    let err = start(&ext, "a").recv_timeout(WAIT).unwrap().unwrap_err();
    assert!(matches!(err, Error::Lua { .. }), "{err:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The command `p.sign` runs through `LuaExtension::command` and returns what
/// it ran. Its 5 s timeout is not the provider function's 50 ms.
#[test]
fn a_command_named_like_a_provider_function_returns_what_it_ran() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (ok_tx, ok_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || answer_n(listener, ok_tx, release_rx, 1));
    let dir = extension(
        "collide-public",
        &format!(
            "fiber.command(\"p.sign\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 50, run = function() return {{}} end }} }})\n"
        ),
    );
    let ext = Arc::new(LuaExtension::new("ext", &dir, "/nonexistent-fiber-home"));
    let ran = start(&ext, "p.sign");
    ok_rx
        .recv_timeout(WAIT)
        .expect("the command never reached the server");
    release_tx.send(()).unwrap();
    assert_eq!(ran.recv_timeout(WAIT).unwrap().unwrap(), "ok");
    assert!(ext.is_running());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A parked `host.http` callback ends when `next` sees its deadline. That
/// instant is still inside the caller's grace, so a grace delay leaves it
/// running. A millisecond later also ends it.
#[test]
fn a_parked_http_call_ends_at_its_deadline_not_its_grace() {
    let (dir, vm, hub) = loaded(
        "park-deadline",
        "fiber.command(\"hold\", { timeout = 60000, run = function() return host.http({ url = \"http://127.0.0.1:9/\" }).body end })\n\
         fiber.command(\"later\", { timeout = 60000, run = function() return host.http({ url = \"http://127.0.0.1:9/\" }).body end })\n",
    );
    let asked = Instant::now();
    publish(&hub, &vm);
    enqueue(&hub, command("hold"), asked);
    enqueue(&hub, command("later"), asked);
    let hold = park_next(&hub, &vm, 60_000);
    let later = park_next(&hub, &vm, 60_000);
    let hold_deadline = hold.deadline.expect("hold has a deadline");
    let later_deadline = later.deadline.expect("later has a deadline");
    waits_until(
        &hub,
        hold,
        hold_deadline
            .checked_sub(Duration::from_millis(1))
            .expect("before the deadline"),
        hold_deadline,
        60_000,
    );
    publish(&hub, &vm);
    ends_at(
        &hub,
        later,
        later_deadline
            .checked_add(Duration::from_millis(1))
            .expect("after the deadline"),
        60_000,
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `p.sign` the command and `p.sign` the provider function each end on the
/// timeout they declared. A second after they were asked, only the 50 ms
/// function is past its deadline.
#[test]
fn a_command_and_a_provider_function_that_share_a_name_end_on_their_own_deadlines() {
    let (dir, vm, hub) = loaded(
        "name-deadlines",
        "fiber.command(\"p.sign\", { timeout = 60000, run = function() return host.http({ url = \"http://127.0.0.1:9/\" }).body end })\n\
         fiber.provider(\"p\", { sign = { timeout = 50, run = function() return host.http({ url = \"http://127.0.0.1:9/\" }).body end } })\n",
    );
    let asked = Instant::now();
    publish(&hub, &vm);
    enqueue(&hub, command("p.sign"), asked);
    enqueue(
        &hub,
        Target::Provider {
            name: "p".to_owned(),
            function: "sign",
        },
        asked,
    );
    let command_parked = park_next(&hub, &vm, 60_000);
    let provider_parked = park_next(&hub, &vm, 50);
    let command_deadline = command_parked.deadline.expect("the command has a deadline");
    ends_at(
        &hub,
        provider_parked,
        asked
            .checked_add(Duration::from_secs(1))
            .expect("a second later"),
        50,
    );
    publish(&hub, &vm);
    waits_until(
        &hub,
        command_parked,
        command_deadline
            .checked_sub(Duration::from_millis(1))
            .expect("before the command's deadline"),
        command_deadline,
        60_000,
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Asked while the entry script is still running. The deadline `next` stores
/// is that instant plus the 5 s the command declares: a shorter deadline has
/// already ended a millisecond early, and the deadline itself has ended while
/// the caller's grace has not.
fn scheduler_keeps_the_deadline_declared_while_registering() {
    let (dir, vm, hub) = loaded(
        "reg-deadline",
        "fiber.command(\"work\", { timeout = 5000, run = function() return host.http({ url = \"http://127.0.0.1:9/\" }).body end })\n",
    );
    let asked = ago(1000);
    {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        shared.push(command("work"), Value::Null, asked);
    }
    publish(&hub, &vm);
    let parked = park_next(&hub, &vm, 5_000);
    waits_until(
        &hub,
        parked,
        asked
            .checked_add(Duration::from_millis(4_999))
            .expect("before the declared deadline"),
        asked
            .checked_add(Duration::from_millis(5_000))
            .expect("the declared deadline"),
        5_000,
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

fn loaded(tag: &str, script: &str) -> (PathBuf, Vm, Arc<Hub>) {
    let dir = extension(tag, script);
    let vm = Vm::load(
        "ext",
        &dir,
        Path::new("/nonexistent-fiber-home"),
        Instant::now().checked_add(LOAD_TIMEOUT),
    )
    .unwrap();
    (dir, vm, Arc::<Hub>::default())
}

fn publish(hub: &Hub, vm: &Vm) {
    hub.lock().phase = Phase::Ready(vm.declared());
}

fn enqueue(hub: &Hub, target: Target, asked: Instant) {
    hub.lock().push(target, Value::Null, asked);
}

/// Starts the queued call and parks it where `settle` would. The deadline is
/// the one `next` assigned from when the call was asked.
fn park_next(hub: &Hub, vm: &Vm, timeout_ms: u64) -> schedule::Parked {
    let mut parked = Vec::new();
    let Some(work) = schedule::next("ext", hub, &mut parked, &Instant::now) else {
        panic!("the scheduler did not start the queued call");
    };
    let schedule::Work::Start(job, timeout, deadline) = work else {
        panic!("the scheduler resumed a parked call");
    };
    assert!(parked.is_empty());
    assert_eq!(timeout, Duration::from_millis(timeout_ms));
    assert_eq!(
        deadline,
        job.asked.checked_add(Duration::from_millis(timeout_ms))
    );
    let step = vm
        .step(&job.target, &job.arg, timeout, deadline)
        .expect("the callback failed");
    let Step::Suspend {
        thread,
        target,
        deadline,
        timeout,
        ..
    } = step
    else {
        panic!("host.http returned instead of suspending");
    };
    let id = job.id;
    hub.lock().calls.insert(
        id,
        Progress::Started {
            deadline,
            parked: true,
        },
    );
    schedule::Parked {
        id,
        thread,
        target,
        deadline,
        timeout,
    }
}

fn assert_timed_out(shared: &Shared, id: u64, timeout_ms: u64) {
    match shared.calls.get(&id) {
        Some(Progress::Done(Err(Error::Timeout {
            timeout_ms: got, ..
        }))) => assert_eq!(*got, timeout_ms),
        Some(Progress::Done(Err(err))) => panic!("the call failed another way: {err:?}"),
        Some(Progress::Done(Ok(_))) => panic!("the call returned a value"),
        Some(Progress::Started { .. }) => panic!("the call was still running"),
        Some(Progress::Queued) => panic!("the call was still queued"),
        None => panic!("the call was dropped"),
    }
}

/// `next` runs on this thread. `check` runs once the scheduler has read the
/// clock, and stopping the hub on the way out — panic included — makes `next`
/// return.
fn watch(
    hub: &Arc<Hub>,
    parked: schedule::Parked,
    clock: Arc<Mutex<Instant>>,
    check: impl FnOnce(&mpsc::Receiver<()>, &Arc<Hub>) + Send + 'static,
) {
    let (saw_tx, saw_rx) = mpsc::channel();
    let now = move || {
        let at = *clock.lock().unwrap();
        match saw_tx.send(()) {
            Ok(()) => at,
            Err(mpsc::SendError(())) => at,
        }
    };
    let watched = Arc::clone(hub);
    let handle = thread::spawn(move || {
        let _stop = StopOnDrop {
            hub: Arc::clone(&watched),
        };
        check(&saw_rx, &watched);
    });
    let mut parked = vec![parked];
    assert!(schedule::next("ext", hub, &mut parked, &now).is_none());
    match handle.join() {
        Ok(()) => {}
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

struct StopOnDrop {
    hub: Arc<Hub>,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        stop(&self.hub);
    }
}

fn ends_at(hub: &Arc<Hub>, parked: schedule::Parked, at: Instant, timeout_ms: u64) {
    let id = parked.id;
    watch(hub, parked, Arc::new(Mutex::new(at)), move |saw, hub| {
        saw.recv_timeout(Duration::from_secs(5))
            .expect("the scheduler never read the clock");
        assert_timed_out(&hub.lock(), id, timeout_ms);
    });
}

fn waits_until(
    hub: &Arc<Hub>,
    parked: schedule::Parked,
    early: Instant,
    late: Instant,
    timeout_ms: u64,
) {
    let id = parked.id;
    let clock = Arc::new(Mutex::new(early));
    let move_clock = Arc::clone(&clock);
    watch(hub, parked, clock, move |saw, hub| {
        saw.recv_timeout(Duration::from_secs(5))
            .expect("the scheduler never read the clock");
        let waiting = matches!(
            hub.lock().calls.get(&id),
            Some(Progress::Started { parked: true, .. })
        );
        assert!(waiting, "the call ended before its deadline");
        *move_clock.lock().unwrap() = late;
        hub.notify();
        saw.recv_timeout(Duration::from_secs(5))
            .expect("the scheduler did not read the moved clock");
        assert_timed_out(&hub.lock(), id, timeout_ms);
    });
}
