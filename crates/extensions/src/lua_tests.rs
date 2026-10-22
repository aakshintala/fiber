use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use fakes::Deadline;
use fakes::clock::FakeClock;

use super::hub::{Progress, Run, Unsent};
use super::*;

/// Wall-clock bound on a wait for the VM, a server, or a thread.
const WAIT: Duration = Duration::from_secs(5);

/// Bound on a wait for the hub's state, a held request to reach its server,
/// or the extension thread to quit.
const WAIT_UNTIL: Duration = fakes::MUST_SUCCEED_WITHIN;

/// Bound on a wait for a test server to accept, or a caller to park.
const WAIT_SERVER: Duration = fakes::MUST_SUCCEED_WITHIN;

fn registration_result(init: &str, command: Option<&str>) -> Result<Vec<(String, String)>, Error> {
    let dir = extension("registration-guards", init);
    let ext = LuaExtension::new("ext", dir.path(), dir.path(), FakeClock::new());
    let command = command.map(str::to_owned);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = match command {
            Some(command) => ext
                .command(&command, "")
                .map(|reply| vec![(command, reply)]),
            None => ext.commands(),
        };
        tx.send(result).unwrap();
        drop(dir);
    });
    Deadline::after(WAIT_UNTIL).recv_or_fail(&rx, "registration guard result")
}

#[test]
fn provider_registration_in_init_is_refused_plain_and_caught() {
    for init in [
        "fiber.provider('acme', {})",
        "pcall(function() fiber.provider('acme', {}) end)",
    ] {
        let err = registration_result(init, None).unwrap_err();
        let Error::Lua { message, .. } = err else {
            panic!("{err:?}")
        };
        assert!(message.contains("init.lua"), "{message}");
        assert!(message.contains("providers/acme.lua"), "{message}");
    }
}

#[test]
fn registration_from_a_command_is_refused_for_each_registration_kind() {
    for call in [
        "fiber.command('late', {timeout=100, run=function() end})",
        "fiber.hook('turn_start', {timeout=100, on_failure='blocking', run=function() end})",
        "fiber.provider('acme', {})",
    ] {
        let init = format!("fiber.command('test', {{timeout=1000, run=function() {call} end}})");
        let err = registration_result(&init, Some("test")).unwrap_err();
        let Error::Lua { message, .. } = err else {
            panic!("{err:?}")
        };
        assert!(message.contains("init.lua:1:"), "{message}");
        assert!(message.contains("only while"), "{message}");
    }
}

#[test]
fn registration_from_a_deferred_timer_never_makes_a_command_callable() {
    let dir = extension(
        "timer-registration",
        r#"
        visited = false
        fiber.command('probe', {timeout=1000, run=function() return tostring(visited) .. ' ' .. tostring(reason) end})
        host.after(0, function()
            visited = true
            local ok
            ok, reason = pcall(function()
                fiber.command('late', {timeout=1000, run=function() return 'late' end})
            end)
            host.status('timer-ready')
        end, {timeout=1000})
    "#,
    );
    let ext = LuaExtension::new("ext", dir.path(), dir.path(), FakeClock::new());
    struct TimerSignal(mpsc::Sender<()>);
    impl contract::emit::Emit for TimerSignal {
        fn emit(&self, _: &contract::events::Event) {
            match self.0.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
        }
    }
    let (ready_tx, ready_rx) = mpsc::channel();
    ext.set_emit(Arc::new(TimerSignal(ready_tx)));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        ext.commands().unwrap();
        Deadline::after(WAIT).recv_or_fail(&ready_rx, "timer completed its registration attempt");
        let probe = ext.command("probe", "");
        let late = ext.command("late", "");
        tx.send((probe, late)).unwrap();
        drop(dir);
    });
    let (probe, late) = Deadline::after(WAIT_UNTIL).recv_or_fail(&rx, "timer registration result");
    let probe = probe.unwrap();
    assert!(probe.starts_with("true "), "{probe}");
    assert!(probe.contains("init.lua:"), "{probe}");
    assert!(probe.contains("only while"), "{probe}");
    assert!(matches!(late, Err(Error::UnknownCommand { command, .. }) if command == "late"));
}

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
            deps: Default::default(),
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

#[test]
fn load_completion_wins_at_the_abandonment_boundary() {
    provider_load_race(true);
}

#[test]
fn load_abandonment_wins_before_recording() {
    provider_load_race(false);
}

#[allow(
    clippy::unwrap_used,
    reason = "a race fixture must be created and each bounded wait must finish"
)]
fn provider_load_race(completion_first: bool) {
    use super::hub::Window;
    use contract::clock::Wake;
    struct ClockMoved(mpsc::Sender<()>);
    impl Wake for ClockMoved {
        fn wake(&self) {
            match self.0.send(()) {
                Ok(()) | Err(mpsc::SendError(())) => {}
            }
        }
    }
    let clock = FakeClock::new();
    let (moved_tx, moved_rx) = mpsc::channel();
    let moved: Arc<dyn Wake> = Arc::new(ClockMoved(moved_tx));
    clock.subscribe(Arc::downgrade(&moved));
    let dir = extension("load-race", "");
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(
        dir.path().join("providers/p.lua"),
        "require('hold')\n fiber.provider('p', {})",
    )
    .unwrap();
    let fifo = dir.path().join("hold.lua");
    fakes::fifo(&fifo);
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    thread::spawn(move || {
        let held = std::fs::OpenOptions::new().write(true).open(fifo).unwrap();
        held_tx.send(()).unwrap();
        match release_rx.recv() {
            Ok(()) | Err(mpsc::RecvError) => {}
        }
        drop(held);
    });
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        clock.clone(),
    ));
    let (recorded_tx, recorded_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = std::sync::Mutex::new(resume_rx);
    ext.hub.pause_at_windows(Arc::new(move |_, window| {
        if window == Window::LoadRecorded {
            recorded_tx.send(()).unwrap();
            match resume_rx.lock().unwrap().recv() {
                Ok(()) | Err(mpsc::RecvError) => {}
            }
        }
    }));
    let (result_tx, result_rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    let asked = clock.now();
    let abandon_at = asked + LOAD_TIMEOUT + GRACE;
    let caller_thread = thread::spawn(move || result_tx.send(caller.provider_functions("p")));
    Deadline::after(WAIT).recv_or_fail(&held_rx, "provider held in read");
    assert!(clock.await_parked(abandon_at, fakes::Deadline::after(WAIT)));
    if completion_first {
        release_tx.send(()).unwrap();
        Deadline::after(WAIT).recv_or_fail(&recorded_rx, "load result recorded before abandonment");
        let (advanced_tx, advanced_rx) = mpsc::channel();
        let advance_clock = Arc::clone(&clock);
        let caller_id = caller_thread.thread().id();
        thread::spawn(move || {
            advance_clock.advance_when_parked(
                Some(abandon_at),
                Some(caller_id),
                LOAD_TIMEOUT + GRACE,
                &Deadline::after(WAIT),
            );
            advanced_tx.send(()).unwrap();
        });
        Deadline::after(WAIT).recv_or_fail(&moved_rx, "clock moved before hub wake");
        resume_tx.send(()).unwrap();
        Deadline::after(WAIT).recv_or_fail(&advanced_rx, "clock advance completed");
        assert_eq!(
            Deadline::after(WAIT)
                .recv_or_fail(&result_rx, "completed load result")
                .unwrap(),
            Vec::<String>::new()
        );
        assert!(ext.is_running());
    } else {
        clock.advance_when_parked(
            Some(abandon_at),
            Some(caller_thread.thread().id()),
            LOAD_TIMEOUT + GRACE,
            &Deadline::after(WAIT),
        );
        let err = Deadline::after(WAIT)
            .recv_or_fail(&result_rx, "abandonment before recording")
            .unwrap_err();
        assert!(matches!(err, Error::Abandoned { callback, .. } if callback == "providers/p.lua"));
        release_tx.send(()).unwrap();
        Deadline::after(WAIT).recv_or_fail(&recorded_rx, "late load result dropped");
        resume_tx.send(()).unwrap();
        assert!(!ext.is_running());
        let (retry_tx, retry_rx) = mpsc::channel();
        let retry = Arc::clone(&ext);
        thread::spawn(move || retry_tx.send(retry.provider_functions("p")));
        assert!(matches!(
            Deadline::after(WAIT).recv_or_fail(&retry_rx, "abandonment stays final"),
            Err(Error::Abandoned { .. })
        ));
    }
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

fn returned(next: (Next, Unsent)) -> Result<Value, Error> {
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
            run: Run::Parked,
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

#[test]
fn a_login_wait_past_its_grace_reports_authentication_failed() {
    let at = now();
    let mut shared = ready(&[("login", 100)]);
    let asked = ago(at, 2000);
    let target = command("login");
    let id = shared.push(target.clone(), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1100)),
            run: Run::LoginWait,
        },
    );
    let error = returned(shared.judge("ext", id, &target, asked, at)).unwrap_err();
    assert!(
        matches!(
            error,
            Error::LoginTimedOut {
                timeout_ms: 100,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
}

/// A running command past its deadline times out and arms the abandon watch,
/// then past its grace abandons the VM.
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
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), asked, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 100, .. } if callback == "spin"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    assert!(
        shared.abandon.as_ref().is_some_and(|watch| watch.id == id),
        "the Timeout arms the abandon watch"
    );
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1100)),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), ago(at, 1200), at)).unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    assert!(matches!(
        shared.phase,
        Phase::Stopped(hub::StopReason::Abandoned { .. })
    ));
}

/// Every waiter on a stopped extension gets the error that stopped it, at
/// once.
#[test]
fn every_waiter_on_a_stopped_extension_gets_the_stored_error() {
    let stored = [
        hub::StopReason::Lua {
            extension: "ext".to_owned(),
            message: "init.lua:1: bad".to_owned(),
        },
        hub::StopReason::Timeout {
            extension: "ext".to_owned(),
            callback: "init.lua".to_owned(),
            timeout_ms: 2000,
        },
        hub::StopReason::Io {
            path: "/gone".into(),
            kind: std::io::ErrorKind::NotFound,
            message: std::io::Error::from(std::io::ErrorKind::NotFound).to_string(),
        },
        hub::StopReason::Stopped,
    ];
    for reason in stored {
        let text = reason.error("ext").to_string();
        let mut shared = in_phase(Phase::Stopped(reason));
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
    let mut shared = in_phase(Phase::Stopped(hub::StopReason::Stopped));
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
    Deadline::after(WAIT)
        .recv(&waiting_rx)
        .expect("waited for the other waiter to take the lock");
    // Its own thread, so a call that never returns fails the test.
    let (done_tx, done_rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    thread::spawn(move || done_tx.send(caller.provider_functions("fixture")));
    let err = Deadline::after(WAIT)
        .recv(&done_rx)
        .expect("provider_functions never returned")
        .unwrap_err();
    assert!(matches!(err, Error::Abandoned { .. }), "{err:?}");
    // A hang guard only: without the wake the waiter never returns.
    Deadline::after(WAIT)
        .recv(&woke_rx)
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
        Deadline::after(WAIT)
            .recv(&done_rx)
            .expect("waited for echo")
            .unwrap(),
        "hi"
    );
    let hub = Arc::clone(&ext.hub);
    drop(ext);
    assert!(matches!(
        hub.lock().phase,
        Phase::Stopped(hub::StopReason::Stopped)
    ));
}

/// A mixed fixture's provider registration is the last block of its script.
fn mixed_extension(tag: &str, script: &str) -> fakes::TempDir {
    let (init, provider) = script.split_once("fiber.provider").unwrap();
    let dir = extension(tag, init);
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    std::fs::write(
        dir.path().join("providers/p.lua"),
        format!("fiber.provider{provider}"),
    )
    .unwrap();
    dir
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
    let provider_file = dir.join("providers/p.lua").is_file();
    let id = {
        let mut shared = hub.lock();
        shared.phase = Phase::Registering { abandon_at: None };
        if provider_file {
            shared
                .loads
                .insert("p".to_owned(), hub::ProviderLoad::Queued { asked });
            shared.pending_loads.push_back("p".to_owned());
        }
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
                deps: Default::default(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    if provider_file {
        assert!(until(&hub, |shared| matches!(
            shared.loads.get("p"),
            Some(hub::ProviderLoad::Loaded)
        )));
    }
    (hub, id, done_rx)
}

/// Waits until `check` holds for the hub's state, or the must-succeed bound passes.
fn until(hub: &Hub, check: impl Fn(&Shared) -> bool) -> bool {
    let shared = hub.lock();
    hub.wait_for(shared, WAIT_UNTIL, check)
}

fn stop(hub: &Hub) {
    hub.lock().phase = Phase::Stopped(hub::StopReason::Stopped);
    hub.notify();
}

/// A stopped extension's thread quits while a callback is parked, and the
/// call queued behind it never starts.
#[test]
fn the_thread_quits_once_stopped_with_a_callback_parked() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = extension(
        "parked",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"later\", {{ timeout = 5000, run = function() return \"later\" end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for hold to reach the server"
    );
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started {
            run: Run::Parked,
            ..
        })
    )));
    let later = hub.lock().push(command("later"), Value::Null, clock.now());
    stop(&hub);
    Deadline::after(WAIT_UNTIL)
        .recv(&done)
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
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = mixed_extension(
        "parked-second",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.command(\"second\", {{ timeout = 5000, run = function() return \"second\" end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for hold to reach the server"
    );
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started {
            run: Run::Parked,
            ..
        })
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
        Some(Progress::Done(Ok(_)))
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
    Deadline::after(WAIT_UNTIL)
        .recv(&done)
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
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for spin to pass its clock check");
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Started {
            run: Run::Running,
            ..
        })
    )));
    stop(&hub);
    // No thread of the code under test waits on the clock: the spinning callback reads it in its interrupt and never parks (ruling 6179406086).
    clock.advance(Duration::from_millis(1000));
    assert!(until(&hub, |s| matches!(
        s.calls.get(&spin),
        Some(Progress::Done(_))
    )));
    Deadline::after(WAIT_UNTIL)
        .recv(&done)
        .expect("waited for the extension thread to quit");
    assert!(matches!(
        hub.lock().calls.get(&later),
        Some(Progress::Queued)
    ));
}

/// `require("go_<name>")` is a signal, not a barrier. The loader runs the
/// registered observer before it opens the file; the observer reports that
/// and `require` reads an empty module, so the code runs on. Nothing stays
/// blocked in it. Each name is used once per VM, because `require` caches.
#[allow(clippy::unwrap_used, reason = "a test helper; a failure is the test's")]
fn go_module(dir: &Path, name: &str) -> mpsc::Receiver<()> {
    let path = dir.join(format!("go_{name}.lua"));
    std::fs::File::create(&path).unwrap();
    let (tx, rx) = mpsc::channel();
    crate::lua::observe_load(
        path.canonicalize().unwrap(),
        Box::new(move || match tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }),
    );
    rx
}

/// When a running callback abandons the VM, every other waiter, queued or
/// parked, command or provider call, gets `Stopped` at once rather than at
/// its own deadline.
#[test]
fn an_abandoned_vm_wakes_every_queued_and_parked_waiter() {
    let park = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    park.hold();
    let models = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    models.hold();
    let (park_url, models_url) = (park.url(), models.url());
    let dir = mixed_extension(
        "abandon",
        &format!(
            "fiber.command(\"park\", {{ timeout = 30000, run = function() return host.http({{ url = \"{park_url}\" }}).body end }})\n\
             fiber.command(\"queued\", {{ timeout = 8000, run = function() return \"ran\" end }})\n\
             fiber.provider(\"p\", {{\n\
               models = {{ timeout = 30000, run = function() return host.http({{ url = \"{models_url}\" }}).body end }},\n\
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
        thread::spawn(move || tx.send((what, f(&ext))))
            .thread()
            .id()
    };
    let asked = clock.now();
    run("park", |e| e.command("park", "").map(Value::String));
    assert!(
        park.await_requests(1, Deadline::after(WAIT_SERVER)),
        "waited for park to reach the server"
    );
    run("models", |e| e.provider_call("p", "models", Value::Null));
    assert!(
        models.await_requests(1, Deadline::after(WAIT_SERVER)),
        "waited for models to reach the server"
    );
    run("queued", |e| e.command("queued", "").map(Value::String));
    let sign = run("sign", |e| e.provider_call("p", "sign", Value::Null));
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for sign to pass its clock check");
    let abandon = asked + Duration::from_millis(50) + GRACE;
    assert!(
        clock.await_parked(abandon, fakes::Deadline::after(WAIT_SERVER)),
        "waited for sign to park at its grace"
    );
    clock.advance_when_parked(
        Some(abandon),
        Some(sign),
        Duration::from_millis(50) + GRACE,
        &Deadline::after(WAIT),
    );
    let mut seen = BTreeMap::new();
    // One deadline for the whole wait: the loop drains one stream.
    let wait = Deadline::after(WAIT);
    for _ in 0..4 {
        let (what, result) = wait.recv(&rx).expect("waited for a waiter to return");
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
    (
        home,
        file,
        host::Reply::Lock(Ok(crate::oauth::Holder::File(lock))),
    )
}

#[test]
fn a_lock_reply_for_a_stopped_extension_is_dropped_and_releases_the_lock() {
    let (_home, file, reply) = held_lock("deliver-stopped");
    let hub = Hub::new(FakeClock::new());
    hub.lock().phase = Phase::Stopped(hub::StopReason::Stopped);
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
    hub.lock().stop(hub::StopReason::Stopped);
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

/// A hook queued while a command is parked on `host.http` starts only after
/// the command finishes: hooks, watcher deliveries and commands form one
/// stream, in the order they happened.
#[test]
fn a_hook_queued_behind_a_parked_command_does_not_start() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = mixed_extension(
        "hook-behind-parked-command",
        &format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() return host.http({{ url = \"{url}\" }}).body end }})\n\
             fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"non-blocking\", run = function(call) return {{}} end }})\n\
             fiber.provider(\"p\", {{ sign = {{ timeout = 1000, run = function() return {{}} end }} }})\n"
        ),
    );
    let clock = FakeClock::new();
    let (hub, hold, done) = serve_after(dir.path(), "hold", &clock);
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for hold to reach the server"
    );
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hold),
        Some(Progress::Started {
            run: Run::Parked,
            ..
        })
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
        Some(Progress::Done(Ok(_)))
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
    Deadline::after(WAIT_UNTIL)
        .recv(&done)
        .expect("waited for the extension thread to quit");
}

/// A command queued while a hook is parked likewise waits for the hook.
#[test]
fn a_command_queued_behind_a_parked_hook_does_not_start() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = mixed_extension(
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
        shared
            .loads
            .insert("p".to_owned(), hub::ProviderLoad::Queued { asked });
        shared.pending_loads.push_back("p".to_owned());
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
                deps: Default::default(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    assert!(until(&hub, |shared| matches!(
        shared.loads.get("p"),
        Some(hub::ProviderLoad::Loaded)
    )));
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for the hook to reach the server"
    );
    assert!(until(&hub, |s| matches!(
        s.calls.get(&hook),
        Some(Progress::Started {
            run: Run::Parked,
            ..
        })
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
        Some(Progress::Done(Ok(_)))
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
    Deadline::after(WAIT_UNTIL)
        .recv(&done_rx)
        .expect("waited for the extension thread to quit");
}

/// A held command does not start, and a hook queued after it waits too,
/// until `release`; after `release` it runs.
#[test]
fn a_held_command_blocks_the_stream_until_released() {
    let dir = mixed_extension(
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
        shared
            .loads
            .insert("p".to_owned(), hub::ProviderLoad::Queued { asked });
        shared.pending_loads.push_back("p".to_owned());
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
                deps: Default::default(),
            },
        );
        match done_tx.send(()) {
            Ok(()) | Err(mpsc::SendError(())) => {}
        }
    });
    // Wait until the entry script has run: the held job is still queued.
    assert!(until(&hub, |s| matches!(s.phase, Phase::Ready(_))));
    assert!(until(&hub, |shared| matches!(
        shared.loads.get("p"),
        Some(hub::ProviderLoad::Loaded)
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
        Some(Progress::Done(Ok(_)))
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
    Deadline::after(WAIT_UNTIL)
        .recv(&done_rx)
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

/// A hook target at `point`, first in registration order.
fn hook(point: &str) -> Target {
    Target::Hook {
        point: point.to_owned(),
        index: 0,
    }
}

/// Ready with one hook at `point` timing out after `ms`.
fn ready_hook(point: &str, ms: u64) -> Shared {
    let mut shared = in_phase(Phase::Ready(CallbackTimeouts::default()));
    let Phase::Ready(timeouts) = &mut shared.phase else {
        panic!("not ready");
    };
    timeouts.hooks.by_point.insert(
        point.to_owned(),
        vec![declared::DeclaredHook {
            phase: HookPhase::Transform,
            blocking: false,
            timeout: Duration::from_millis(ms),
        }],
    );
    shared
}

/// A running hook at its deadline has failed: the caller takes the timeout
/// at once, without waiting the grace, and the VM stays up.
#[test]
fn a_running_hook_at_its_deadline_times_out_and_leaves_the_vm_up() {
    let at = now();
    let asked = ago(at, 50);
    let mut shared = ready_hook("before_message", 50);
    let id = shared.push(hook("before_message"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &hook("before_message"), asked, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "before_message"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    assert!(shared.calls.is_empty() && shared.queue.is_empty());
}

/// A running hook just before its deadline still waits: flipping the
/// deadline comparison to answer early would return here.
#[test]
fn a_running_hook_before_its_deadline_waits() {
    let at = now();
    let asked = ago(at, 49);
    let mut shared = ready_hook("before_message", 50);
    let id = shared.push(hook("before_message"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Running,
        },
    );
    let (Next::Sleep(until), _) =
        shared.judge("ext", id, &hook("before_message"), asked, ago(at, 1))
    else {
        panic!("a hook returned before its deadline")
    };
    assert_eq!(until, Some(at));
}

/// A hook still on the thread past its grace abandons the VM: the
/// interrupt could not stop it. The next call fails with the stored
/// abandoned error.
#[test]
fn a_running_hook_past_its_grace_abandons_the_vm() {
    let at = now();
    let asked = ago(at, 1200);
    let mut shared = ready_hook("before_message", 50);
    let id = shared.push(hook("before_message"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1100)),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &hook("before_message"), asked, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Abandoned { callback, .. } if callback == "before_message"),
        "{err:?}"
    );
    assert!(matches!(
        shared.phase,
        Phase::Stopped(hub::StopReason::Abandoned { .. })
    ));
    let next = shared.push(hook("before_message"), Value::Null, at);
    let err = returned(shared.judge("ext", next, &hook("before_message"), at, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Abandoned { callback, .. } if callback == "before_message"),
        "{err:?}"
    );
}

/// A hook that outruns its caller's `Timeout` is abandoned by the next call
/// past its grace: the worker may be stuck in the VM, so the next judge
/// stops it. Within its grace the VM stays up.
#[test]
fn a_timed_out_hook_still_running_past_its_grace_abandons_on_the_next_call() {
    let at = now();
    let asked = ago(at, 1200);
    let mut shared = ready_hook("before_message", 50);
    let id = shared.push(hook("before_message"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1150)),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &hook("before_message"), asked, ago(at, 200)))
        .unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "before_message"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    let next = shared.push(hook("before_message"), Value::Null, at);
    let err = returned(shared.judge("ext", next, &hook("before_message"), at, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Abandoned { callback, .. } if callback == "before_message"),
        "{err:?}"
    );
    assert!(matches!(
        shared.phase,
        Phase::Stopped(hub::StopReason::Abandoned { .. })
    ));
}

/// A parked hook at its deadline still waits out its grace: only a hook on
/// the thread answers at its deadline, which a flipped park check would
/// break here.
#[test]
fn a_parked_hook_at_its_deadline_waits_out_its_grace() {
    let at = now();
    let asked = ago(at, 50);
    let mut shared = ready_hook("before_message", 50);
    let id = shared.push(hook("before_message"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Parked,
        },
    );
    let (Next::Sleep(until), _) = shared.judge("ext", id, &hook("before_message"), asked, at)
    else {
        panic!("a parked hook returned at its deadline")
    };
    assert_eq!(until, Some(at + GRACE));
}

/// A running command at its deadline has failed: the caller takes the timeout
/// at once, without waiting the grace, and the VM stays up.
#[test]
fn a_running_command_at_its_deadline_times_out_and_leaves_the_vm_up() {
    let at = now();
    let asked = ago(at, 50);
    let mut shared = ready(&[("spin", 50)]);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), asked, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "spin"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    assert!(
        shared.abandon.as_ref().is_some_and(|watch| watch.id == id),
        "the Timeout arms the abandon watch"
    );
}

/// A running command just before its deadline still waits: flipping the
/// deadline comparison to answer early would return here.
#[test]
fn a_running_command_before_its_deadline_waits() {
    let at = now();
    let asked = ago(at, 49);
    let mut shared = ready(&[("spin", 50)]);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Running,
        },
    );
    let (Next::Sleep(until), _) = shared.judge("ext", id, &command("spin"), asked, ago(at, 1))
    else {
        panic!("a command returned before its deadline")
    };
    assert_eq!(until, Some(at));
}

/// A running command inside its grace times out, and at its grace it is
/// abandoned: both sides of the grace boundary.
#[test]
fn a_running_command_one_millisecond_inside_its_grace_times_out() {
    let deadline = now();
    let asked = ago(deadline, 50);
    // One millisecond inside the grace: the caller takes the timeout and the
    // VM stays up.
    {
        let at = deadline
            .checked_add(GRACE)
            .and_then(|at| at.checked_sub(Duration::from_millis(1)))
            .unwrap();
        let mut shared = ready(&[("spin", 50)]);
        let id = shared.push(command("spin"), Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            id,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        let err = returned(shared.judge("ext", id, &command("spin"), asked, at)).unwrap_err();
        assert!(
            matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "spin"),
            "{err:?}"
        );
        assert!(matches!(shared.phase, Phase::Ready(_)));
    }
    // Exactly at the grace: the VM is abandoned.
    {
        let at = deadline + GRACE;
        let mut shared = ready(&[("spin", 50)]);
        let id = shared.push(command("spin"), Value::Null, asked);
        shared.queue.clear();
        shared.calls.insert(
            id,
            Progress::Started {
                deadline: Some(deadline),
                run: Run::Running,
            },
        );
        let err = returned(shared.judge("ext", id, &command("spin"), asked, at)).unwrap_err();
        assert!(
            matches!(&err, Error::Abandoned { callback, .. } if callback == "spin"),
            "{err:?}"
        );
        assert!(matches!(
            shared.phase,
            Phase::Stopped(hub::StopReason::Abandoned { .. })
        ));
    }
}

/// A parked command at its deadline still waits out its grace: only a command
/// on the thread answers at its deadline.
#[test]
fn a_parked_command_at_its_deadline_waits_out_its_grace() {
    let at = now();
    let asked = ago(at, 50);
    let mut shared = ready(&[("spin", 50)]);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(at),
            run: Run::Parked,
        },
    );
    let (Next::Sleep(until), _) = shared.judge("ext", id, &command("spin"), asked, at) else {
        panic!("a parked command returned at its deadline")
    };
    assert_eq!(until, Some(at + GRACE));
}

/// A command that outruns its caller's `Timeout` is abandoned by the next call
/// past its grace: the worker may be stuck in the VM, so the next judge
/// stops it. Within its grace the VM stays up.
#[test]
fn a_timed_out_command_still_running_past_its_grace_abandons_on_the_next_call() {
    let at = now();
    let asked = ago(at, 1200);
    let mut shared = ready(&[("spin", 50)]);
    let id = shared.push(command("spin"), Value::Null, asked);
    shared.queue.clear();
    shared.calls.insert(
        id,
        Progress::Started {
            deadline: Some(ago(at, 1150)),
            run: Run::Running,
        },
    );
    let err = returned(shared.judge("ext", id, &command("spin"), asked, ago(at, 200))).unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "spin"),
        "{err:?}"
    );
    assert!(matches!(shared.phase, Phase::Ready(_)));
    let next = shared.push(command("spin"), Value::Null, at);
    let err = returned(shared.judge("ext", next, &command("spin"), at, at)).unwrap_err();
    assert!(
        matches!(&err, Error::Abandoned { callback, .. } if callback == "spin"),
        "{err:?}"
    );
    assert!(matches!(
        shared.phase,
        Phase::Stopped(hub::StopReason::Abandoned { .. })
    ));
}

/// While one extension's hook completion is withheld, other extensions, other
/// points and commands all complete: the pause passes everything else
/// through, which a widened check would hold up here. Nothing is released.
#[test]
fn a_withheld_hook_completion_passes_other_traffic() {
    let dir = extension(
        "other",
        "fiber.command(\"quick\", { timeout = 5000, run = function() return \"quick\" end })\n\
         fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\",\n\
           run = function(call) return {} end })\n\
         fiber.hook(\"before_message\", { timeout = 5000, on_failure = \"non-blocking\",\n\
           run = function(call) return {} end })\n",
    );
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/other",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock,
    ));
    let (_hold_tx, hold_rx) = mpsc::channel();
    super::schedule::withhold_hook_completion("fiber.test/withheld", "before_message", hold_rx);
    // Each blocking call runs on a thread, received here with a deadline
    // naming the wait, so a pause that wrongly captured it fails here
    // instead of hanging the test.
    let (quick_tx, quick_rx) = mpsc::channel();
    let quick_caller = Arc::clone(&ext);
    thread::spawn(move || quick_tx.send(quick_caller.command("quick", "")));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&quick_rx)
            .expect("waited for the quick command")
            .unwrap(),
        "quick"
    );
    let (after_tx, after_rx) = mpsc::channel();
    let after_caller = Arc::clone(&ext);
    thread::spawn(move || after_tx.send(after_caller.hook("after_tool", 0, serde_json::json!({}))));
    assert!(
        Deadline::after(WAIT)
            .recv(&after_rx)
            .expect("waited for the after_tool hook")
            .is_ok()
    );
    let (before_tx, before_rx) = mpsc::channel();
    let before_caller = Arc::clone(&ext);
    thread::spawn(move || {
        before_tx.send(before_caller.hook("before_message", 0, serde_json::json!({})))
    });
    assert!(
        Deadline::after(WAIT)
            .recv(&before_rx)
            .expect("waited for the before_message hook")
            .is_ok()
    );
    super::schedule::release_hook_completion();
}

/// The `before_message` flake (#2256) forced on every run: a hook spinning
/// on its first call, whose completion the test withholds past the answer
/// wait. The timeout answer must arrive off the clock advance, without the
/// worker's delivery; the worker's late completion is dropped and the VM
/// stays usable. With the delivery withheld and no fix, the answer never
/// comes and this fails at the wait below, every run and under no load.
#[test]
fn a_non_blocking_before_message_hook_past_its_timeout_is_dropped_while_its_completion_is_withheld()
{
    let dir = extension(
        "withheld",
        "local calls = 0\n\
         fiber.hook(\"before_message\", { timeout = 50, on_failure = \"non-blocking\",\n\
           run = function(call)\n\
             calls = calls + 1\n\
             if calls == 1 then require(\"go_spin\") while true do end end\n\
             return {} end })\n",
    );
    let went = go_module(dir.path(), "spin");
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/withheld",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock.clone(),
    ));
    // Calling code that blocks runs on a thread, received here with a
    // deadline naming the wait.
    let (tx, rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    let caller_thread =
        thread::spawn(move || tx.send(caller.hook("before_message", 0, serde_json::json!({}))));
    // Withhold the worker's completion past the answer wait below.
    let (hold_tx, hold_rx) = mpsc::channel();
    super::schedule::withhold_hook_completion("fiber.test/withheld", "before_message", hold_rx);
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the hook to start");
    // The worker runs the hook, past the arming of its deadline: the signal
    // the advance below waits on.
    assert!(
        until(&ext.hub, |shared| shared.calls.values().any(
            |progress| matches!(
                progress,
                Progress::Started {
                    run: Run::Running,
                    ..
                }
            )
        )),
        "waited for the worker to run the hook"
    );
    let timeout_at = clock.origin() + Duration::from_millis(50);
    let caller_id = caller_thread.thread().id();
    assert!(
        clock.await_thread_parked(caller_id, Some(timeout_at), fakes::Deadline::after(WAIT)),
        "waited for the caller to park at its timeout"
    );
    clock.advance_when_parked(
        Some(timeout_at),
        Some(caller_id),
        Duration::from_millis(50),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the timeout answer without the worker's delivery")
        .unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "before_message"),
        "{err:?}"
    );
    assert!(err.to_string().contains("50 ms timeout"), "{err:?}");
    // The answer arrived while the worker was still withheld, so this
    // release only lets its late completion land, dropped, and the VM stays
    // usable: the next call runs the hook again.
    drop(hold_tx);
    super::schedule::release_hook_completion();
    let (again_tx, again_rx) = mpsc::channel();
    let again_caller = Arc::clone(&ext);
    thread::spawn(move || {
        again_tx.send(again_caller.hook("before_message", 0, serde_json::json!({})))
    });
    Deadline::after(WAIT)
        .recv(&again_rx)
        .expect("waited for the hook to run again after its timeout")
        .unwrap();
}

/// A command spinning past its timeout is answered while its completion is
/// withheld: the timeout answer must arrive off the clock advance, without
/// the worker's delivery; the worker's late completion is dropped and the VM
/// stays usable. Without the fix the answer waits on the worker's delivery,
/// so this fails at the wait below on every run.
#[test]
fn a_running_command_past_its_timeout_is_answered_while_its_completion_is_withheld() {
    let dir = extension(
        "withheld-command",
        "fiber.command(\"echo\", { timeout = 1000, run = function(text) return text end })\n\
         fiber.command(\"spin\", { timeout = 50, run = function()\n\
           require(\"go_spin\") while true do end end })\n",
    );
    let went = go_module(dir.path(), "spin");
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/withheld-command",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock.clone(),
    ));
    // Withhold the worker's completion past the answer wait below.
    let (hold_tx, hold_rx) = mpsc::channel();
    super::schedule::withhold_command_completion("fiber.test/withheld-command", "spin", hold_rx);
    // Calling code that blocks runs on a thread, received here with a
    // deadline naming the wait.
    let (tx, rx) = mpsc::channel();
    let caller = Arc::clone(&ext);
    let caller_thread = thread::spawn(move || tx.send(caller.command("spin", "")));
    Deadline::after(WAIT)
        .recv(&went)
        .expect("waited for the command to start");
    // The worker runs the command, past the arming of its deadline: the
    // signal the advance below waits on.
    assert!(
        until(&ext.hub, |shared| shared.calls.values().any(
            |progress| matches!(
                progress,
                Progress::Started {
                    run: Run::Running,
                    ..
                }
            )
        )),
        "waited for the worker to run the command"
    );
    let timeout_at = clock.origin() + Duration::from_millis(50);
    let caller_id = caller_thread.thread().id();
    assert!(
        clock.await_thread_parked(caller_id, Some(timeout_at), fakes::Deadline::after(WAIT)),
        "waited for the caller to park at its timeout"
    );
    clock.advance_when_parked(
        Some(timeout_at),
        Some(caller_id),
        Duration::from_millis(50),
        &Deadline::after(WAIT),
    );
    let err = Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the timeout answer without the worker's delivery")
        .unwrap_err();
    assert!(
        matches!(&err, Error::Timeout { callback, timeout_ms: 50, .. } if callback == "spin"),
        "{err:?}"
    );
    assert!(err.to_string().contains("50 ms timeout"), "{err:?}");
    // The answer arrived while the worker was still withheld, so this
    // release only lets its late completion land, dropped, and the VM stays
    // usable: the next call runs again.
    drop(hold_tx);
    super::schedule::release_hook_completion();
    let (again_tx, again_rx) = mpsc::channel();
    let again_caller = Arc::clone(&ext);
    thread::spawn(move || again_tx.send(again_caller.command("echo", "alive")));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&again_rx)
            .expect("waited for the next command after the timeout")
            .unwrap(),
        "alive"
    );
}

/// The watchers one entry script registered, loaded on a thread under
/// [`WAIT`]: the fake clock never ends a wait the runtime does not end
/// itself.
#[track_caller]
fn watchers_result(init: &str) -> Result<DeclaredWatchers, Error> {
    let dir = extension("watch-registration", init);
    let ext = LuaExtension::new("ext", dir.path(), dir.path(), FakeClock::new());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = ext.watchers();
        tx.send(result).unwrap();
        drop(dir);
    });
    Deadline::after(WAIT).recv_or_fail(&rx, "watch registration result")
}

/// One `host.status` line per watcher run, in run order: the fake emitter
/// the tests install, so no file or clock is involved.
struct StatusSignal(mpsc::Sender<String>);

impl contract::emit::Emit for StatusSignal {
    fn emit(&self, event: &contract::events::Event) {
        if let contract::events::Event::ExtensionUi(ui) = event
            && let contract::events::Ui::Status { status } = &ui.ui
        {
            match self.0.send(status.clone()) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        }
    }
}

/// Installs the status emitter on `ext` and boots it on a thread: the
/// receiver gets the registration once the entry script ran.
#[track_caller]
fn boot_watching(
    ext: &Arc<LuaExtension>,
    statuses: mpsc::Sender<String>,
) -> mpsc::Receiver<Result<DeclaredWatchers, Error>> {
    ext.set_emit(Arc::new(StatusSignal(statuses)));
    let caller = Arc::clone(ext);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(caller.watchers()).unwrap();
    });
    rx
}

#[test]
fn fiber_watch_registers_kinds_and_timeout() {
    let declared = watchers_result(
        "fiber.watch({\"notice\", \"step_started\"}, { timeout = 2000, run = function(ev) end })\n",
    )
    .unwrap();
    assert!(declared.problems.is_empty(), "{:?}", declared.problems);
    assert_eq!(declared.watchers.len(), 1);
    assert_eq!(
        declared.watchers[0].kinds,
        vec!["notice".to_owned(), "step_started".to_owned()]
    );
    assert_eq!(declared.watchers[0].timeout, Duration::from_millis(2000));
}

#[test]
fn a_watch_missing_its_timeout_is_not_registered_and_says_why() {
    let declared =
        watchers_result("fiber.watch({\"notice\"}, { run = function(ev) end })\n").unwrap();
    assert!(declared.watchers.is_empty());
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`watcher:0`")
            && declared.problems[0].contains("missing `timeout`"),
        "{}",
        declared.problems[0]
    );
}

#[test]
fn a_watch_with_a_non_table_kinds_is_not_registered_and_says_why() {
    let declared =
        watchers_result("fiber.watch(\"notice\", { timeout = 100, run = function(ev) end })\n")
            .unwrap();
    assert!(declared.watchers.is_empty());
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`watcher:0`") && declared.problems[0].contains("`kinds`"),
        "{}",
        declared.problems[0]
    );
}

#[test]
fn a_watch_with_an_empty_kinds_list_is_not_registered_and_says_why() {
    let declared =
        watchers_result("fiber.watch({}, { timeout = 100, run = function(ev) end })\n").unwrap();
    assert!(declared.watchers.is_empty());
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`watcher:0`") && declared.problems[0].contains("`kinds`"),
        "{}",
        declared.problems[0]
    );
}

#[test]
fn a_watch_with_a_non_string_kind_is_not_registered_and_says_why() {
    let declared =
        watchers_result("fiber.watch({42}, { timeout = 100, run = function(ev) end })\n").unwrap();
    assert!(declared.watchers.is_empty());
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`watcher:0`") && declared.problems[0].contains("`kinds`"),
        "{}",
        declared.problems[0]
    );
}

#[test]
fn a_watch_on_an_unknown_kind_is_not_registered_and_names_the_kind() {
    let declared = watchers_result(
        "fiber.watch({\"frobnicate\"}, { timeout = 100, run = function(ev) end })\n",
    )
    .unwrap();
    assert!(declared.watchers.is_empty());
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`watcher:0`")
            && declared.problems[0].contains("`frobnicate`"),
        "{}",
        declared.problems[0]
    );
}

#[test]
fn a_watch_keeps_its_known_kinds_and_drops_the_unknown_one() {
    let declared = watchers_result(
        "fiber.watch({\"notice\", \"frobnicate\"}, { timeout = 100, run = function(ev) end })\n",
    )
    .unwrap();
    assert_eq!(declared.watchers.len(), 1);
    assert_eq!(declared.watchers[0].kinds, vec!["notice".to_owned()]);
    assert_eq!(declared.problems.len(), 1);
    assert!(
        declared.problems[0].contains("`frobnicate`"),
        "{}",
        declared.problems[0]
    );
}

/// A pushed watcher job runs under the timeout it declared, and one the
/// entry script never registered is refused by name.
#[test]
fn a_watcher_job_serves_its_declared_timeout() {
    let dir = extension(
        "watch-serve",
        "fiber.watch({\"notice\"}, { timeout = 2000, run = function(ev) host.status('watcher-ran') end })\n",
    );
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        clock.clone(),
    ));
    let (status_tx, status_rx) = mpsc::channel();
    let boot_rx = boot_watching(&ext, status_tx);
    assert!(
        until(&ext.hub, |shared| matches!(shared.phase, Phase::Ready(_))),
        "waited for the extension to be ready"
    );
    let asked = clock.now();
    {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 0 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            asked,
        );
    }
    ext.hub.notify();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the watcher to run"),
        "watcher-ran"
    );
    assert!(
        until(&ext.hub, |shared| !shared.watcher_busy.contains(&0)),
        "waited for the watcher's busy bit to clear"
    );
    // A watcher the entry script never registered is refused by name.
    let missing = {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 3 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            clock.now(),
        )
    };
    ext.hub.notify();
    assert!(
        until(&ext.hub, |shared| matches!(
            shared.calls.get(&missing),
            Some(Progress::Done(Err(Error::UnknownCallback { callback, .. })))
            if callback == "watcher:3"
        )),
        "waited for the unregistered watcher to be refused"
    );
    assert!(
        Deadline::after(WAIT)
            .recv(&boot_rx)
            .expect("waited for the registration")
            .is_ok()
    );
}

/// A hook queued behind a watcher parked on a host call does not start:
/// the watcher holds the ordered stream.
#[test]
fn a_hook_queued_behind_a_parked_watcher_does_not_start() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = extension(
        "hook-behind-parked-watcher",
        &format!(
            "fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"non-blocking\", run = function(call) host.status('hook-ran') return {{}} end }})\n\
             fiber.watch({{\"notice\"}}, {{ timeout = 5000, run = function(ev) host.http({{ url = \"{url}\" }}) host.status('watcher-ran') end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        clock.clone(),
    ));
    let (status_tx, status_rx) = mpsc::channel();
    let _boot_rx = boot_watching(&ext, status_tx);
    assert!(
        until(&ext.hub, |shared| matches!(shared.phase, Phase::Ready(_))),
        "waited for the extension to be ready"
    );
    let asked = clock.now();
    {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 0 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            asked,
        );
    }
    ext.hub.notify();
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for the watcher to reach the server"
    );
    assert!(
        until(&ext.hub, |shared| shared.calls.values().any(
            |progress| matches!(
                progress,
                Progress::Started {
                    run: Run::Parked,
                    ..
                }
            )
        )),
        "waited for the watcher to park"
    );
    let hook = {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            clock.now(),
        )
    };
    ext.hub.notify();
    // The hook may not start while the watcher is parked: judged under the
    // hub lock, so a racing start cannot slip between the check and this.
    {
        let shared = ext.hub.lock();
        assert!(
            matches!(shared.calls.get(&hook), Some(Progress::Queued)),
            "the hook started while the watcher was parked"
        );
        assert!(shared.queue.iter().any(|job| job.id == hook));
    }
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the watcher to run"),
        "watcher-ran"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the hook to run"),
        "hook-ran"
    );
}

/// A watcher queued behind a parked hook likewise waits for the stream.
#[test]
fn a_watcher_queued_behind_a_parked_hook_does_not_start() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let url = server.url();
    let dir = extension(
        "watcher-behind-parked-hook",
        &format!(
            "fiber.hook(\"after_tool\", {{ timeout = 5000, on_failure = \"non-blocking\", run = function(call) host.http({{ url = \"{url}\" }}) host.status('hook-ran') return {{}} end }})\n\
             fiber.watch({{\"notice\"}}, {{ timeout = 5000, run = function(ev) host.status('watcher-ran') end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "ext",
        dir.path(),
        dir.path(),
        clock.clone(),
    ));
    let (status_tx, status_rx) = mpsc::channel();
    let _boot_rx = boot_watching(&ext, status_tx);
    assert!(
        until(&ext.hub, |shared| matches!(shared.phase, Phase::Ready(_))),
        "waited for the extension to be ready"
    );
    let asked = clock.now();
    {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            asked,
        );
    }
    ext.hub.notify();
    assert!(
        server.await_requests(1, Deadline::after(WAIT_UNTIL)),
        "waited for the hook to reach the server"
    );
    assert!(
        until(&ext.hub, |shared| shared.calls.values().any(
            |progress| matches!(
                progress,
                Progress::Started {
                    run: Run::Parked,
                    ..
                }
            )
        )),
        "waited for the hook to park"
    );
    let watcher = {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 0 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            clock.now(),
        )
    };
    ext.hub.notify();
    {
        let shared = ext.hub.lock();
        assert!(
            matches!(shared.calls.get(&watcher), Some(Progress::Queued)),
            "the watcher started while the hook was parked"
        );
        assert!(shared.queue.iter().any(|job| job.id == watcher));
    }
    server.release();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the hook to run"),
        "hook-ran"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the watcher to run"),
        "watcher-ran"
    );
}

/// While one watcher's completion is withheld, the hook, the later watcher
/// and the command behind it all wait; released, they run in stream order.
#[test]
fn a_parked_watcher_holds_a_hook_a_later_watcher_and_a_command() {
    let dir = extension(
        "withheld-watcher",
        "fiber.command(\"later\", { timeout = 5000, run = function(text) host.status('command-ran') return \"later\" end })\n\
         fiber.hook(\"after_tool\", { timeout = 5000, on_failure = \"non-blocking\", run = function(call) host.status('hook-ran') return {} end })\n\
         fiber.watch({\"notice\"}, { timeout = 5000, run = function(ev) host.status('watcher-zero') end })\n\
         fiber.watch({\"notice\"}, { timeout = 5000, run = function(ev) host.status('watcher-one') end })\n",
    );
    let clock = FakeClock::new();
    let ext = Arc::new(LuaExtension::new(
        "fiber.test/withheld-watcher",
        dir.path(),
        dir.path(),
        clock.clone(),
    ));
    let (status_tx, status_rx) = mpsc::channel();
    let _boot_rx = boot_watching(&ext, status_tx);
    assert!(
        until(&ext.hub, |shared| matches!(shared.phase, Phase::Ready(_))),
        "waited for the extension to be ready"
    );
    // Withhold the first watcher's completion past everything queued below.
    let (hold_tx, hold_rx) = mpsc::channel();
    super::schedule::withhold_watcher_completion("fiber.test/withheld-watcher", 0, hold_rx);
    let asked = clock.now();
    {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 0 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            asked,
        );
    }
    ext.hub.notify();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the first watcher to run"),
        "watcher-zero"
    );
    // A hook, a later watcher and a command queue behind the withheld one.
    // The hook and the later watcher queue through the hub; the command
    // through its caller, which blocks until the stream reaches it.
    let hook = {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Hook {
                point: "after_tool".to_owned(),
                index: 0,
            },
            serde_json::json!({}),
            clock.now(),
        )
    };
    let second = {
        let mut shared = ext.hub.lock();
        shared.push(
            Target::Watcher { index: 1 },
            serde_json::json!({"kind": "notice", "payload": {}}),
            clock.now(),
        )
    };
    let (command_tx, command_rx) = mpsc::channel();
    let command_caller = Arc::clone(&ext);
    thread::spawn(move || {
        command_tx
            .send(command_caller.command("later", ""))
            .unwrap();
    });
    ext.hub.notify();
    // None of the three starts while the first watcher is withheld: judged
    // under the hub lock, so a racing start cannot slip between the checks.
    assert!(
        until(&ext.hub, |shared| shared.queue.len() == 3),
        "waited for the hook, the later watcher and the command to queue"
    );
    {
        let shared = ext.hub.lock();
        assert!(
            matches!(shared.calls.get(&hook), Some(Progress::Queued)),
            "the hook started while the watcher was withheld"
        );
        assert!(
            matches!(shared.calls.get(&second), Some(Progress::Queued)),
            "the later watcher started while the first was withheld"
        );
    }
    // Released, they run in stream order behind the first watcher: dropping
    // the hold sender lets the withheld completion land at once.
    drop(hold_tx);
    super::schedule::release_hook_completion();
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&command_rx)
            .expect("waited for the command to run")
            .unwrap(),
        "later"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the hook's line"),
        "hook-ran"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the later watcher's line"),
        "watcher-one"
    );
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&status_rx)
            .expect("waited for the command's line"),
        "command-ran"
    );
}

/// Every `fiber.hook` refusal names the entry when one is declared, and
/// matches today's string without one.
#[test]
fn hook_refusals_name_the_entry_when_given() {
    // Each refusal as the point expression, the spec's fields and the
    // reason: emitted once without an entry and once declaring `fmt`.
    let cases: &[(&str, &str, &str)] = &[
        (
            "\"after_tool\"",
            "on_failure = \"blocking\", run = run",
            "missing `timeout`",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, run = run",
            "missing `on_failure`",
        ),
        (
            "\"after_call\"",
            "timeout = 100, on_failure = \"blocking\", run = run",
            "`after_call` is not a hook point",
        ),
        (
            "7",
            "timeout = 100, on_failure = \"blocking\", run = run",
            "`7` is not a hook point",
        ),
        (
            "\"after_tool\"",
            "phase = \"late\", timeout = 100, on_failure = \"blocking\", run = run",
            "`phase` must be `sanitize`, `transform` or `check`",
        ),
        (
            "\"after_tool\"",
            "phase = \"check\", timeout = 100, on_failure = \"blocking\", run = run",
            "a `check` hook exists only at `before_message`, `before_tool` and `before_model_call`",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = \"no\"",
            "`run` must be a function",
        ),
        (
            "\"after_tool\"",
            "timeout = 0, on_failure = \"blocking\", run = run",
            "`timeout` must be a whole number of milliseconds above 0",
        ),
        (
            "\"after_tool\"",
            "timeout = -5, on_failure = \"blocking\", run = run",
            "`timeout` must be a whole number of milliseconds above 0",
        ),
        (
            "\"after_tool\"",
            "timeout = 1.5, on_failure = \"blocking\", run = run",
            "`timeout` must be a whole number of milliseconds above 0",
        ),
        (
            "\"after_tool\"",
            "timeout = \"100\", on_failure = \"blocking\", run = run",
            "`timeout` must be a whole number of milliseconds above 0",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"maybe\", run = run",
            "`on_failure` must be `blocking` or `non-blocking`",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = 1, run = run",
            "`on_failure` must be `blocking` or `non-blocking`",
        ),
        (
            "\"session_start\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {\"read\"}",
            "`tools` is only allowed at `before_tool` and `after_tool`",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {}",
            "`tools` must name at least one tool",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = \"read\"",
            "`tools` must be a list of tool names",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {[2] = \"read\"}",
            "`tools` must be a list of tool names",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {[1] = \"read\", [3] = \"write\"}",
            "`tools` must be a list of tool names",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {[0] = \"read\"}",
            "`tools` must be a list of tool names",
        ),
        (
            "\"after_tool\"",
            "timeout = 100, on_failure = \"blocking\", run = run, tools = {read = \"yes\"}",
            "`tools` must be a list of tool names",
        ),
    ];
    let mut init = String::from("local run = function() end\n");
    let mut expected = Vec::new();
    for (point, fields, why) in cases {
        init.push_str(&format!("fiber.hook({point}, {{ {fields} }})\n"));
        let name = point.trim_matches('"');
        expected.push(format!("`{name}` hook not registered: {why}"));
        init.push_str(&format!(
            "fiber.hook({point}, {{ entry = \"fmt\", {fields} }})\n"
        ));
        expected.push(format!(
            "`{name}` hook not registered for entry `fmt`: {why}"
        ));
    }
    init.push_str("fiber.hook(\"after_tool\", \"no\")\n");
    expected.push(
        "`after_tool` hook not registered: it takes a table of `timeout`, `on_failure` and `run`"
            .to_owned(),
    );
    init.push_str(
        "fiber.hook(\"before_tool\", { entry = \"fmt\", phase = \"check\", timeout = 1, on_failure = \"blocking\", run = run })\n",
    );
    let dir = extension("entry-labeled-refusals", &init);
    let clock = FakeClock::new();
    let ext = LuaExtension::new(
        "ext",
        dir.path(),
        PathBuf::from("/nonexistent-fiber-home"),
        clock,
    );
    let declared = ext.hooks().expect("the entry script runs");
    assert_eq!(declared.problems, expected);
    // The valid entry-carrying hook registered: the label changes only
    // refusals.
    assert_eq!(declared.by_point.get("before_tool").map(Vec::len), Some(1));
}
