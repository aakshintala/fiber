use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;

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
        &|_| Ok(()),
        &Mutex::new(BTreeMap::new()),
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
    vm.deadline.start(LOAD_TIMEOUT);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        vm.resume(f, "boom", LOAD_TIMEOUT, mlua::Value::Nil)
    }));
    assert!(outcome.is_err(), "the panic became {outcome:?}");
}

/// A job whose caller left before it ran is skipped. That does not abandon
/// the VM: a later job still runs.
#[test]
fn a_job_whose_caller_left_before_it_ran_is_skipped() {
    let (inbox, jobs) = mpsc::channel();
    let (gone, _) = mpsc::channel();
    let (later, answers) = mpsc::channel();
    for reply in [gone, later] {
        inbox
            .send(schedule::Msg::Job(Job {
                target: Target::Command("echo".to_owned()),
                arg: Value::String("x".to_owned()),
                reply,
                asked: Instant::now(),
            }))
            .unwrap();
    }
    let http = inbox.clone();
    drop(inbox);
    let timeouts = Mutex::new(BTreeMap::new());
    std::thread::spawn(move || {
        schedule::serve(
            "fixture",
            &fakes::lua_fixture(),
            Path::new("/nonexistent-fiber-home"),
            &jobs,
            &http,
            &timeouts,
        );
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut served = false;
    while Instant::now() < deadline {
        match answers.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Reply::Done(Ok(value))) => {
                assert_eq!(value, Value::String("x".to_owned()));
                served = true;
                break;
            }
            Ok(Reply::Done(Err(e))) => panic!("the later job failed: {e}"),
            Ok(Reply::Deadline(_)) => {}
            Err(_) => break,
        }
    }
    assert!(served, "the later job was not served");
}

/// A caller that leaves after the callback started abandoned the VM. The
/// thread quits when that callback ends in an error, and does not run a
/// later job.
#[test]
fn the_worker_quits_when_a_started_callbacks_caller_leaves() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut sock = listener.accept().unwrap().0;
        accepted_tx.send(()).unwrap();
        match release_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(())
            | Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        use std::io::Write;
        drop(
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"),
        );
    });
    let dir = std::env::temp_dir().join(format!("fiber-lua-quit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.command(\"hold\", {{ timeout = 5000, run = function() host.http({{ url = \"{url}\" }}); error(\"after\") end }})\n\
             fiber.command(\"later\", {{ timeout = 1000, run = function() return \"later\" end }})\n"
        ),
    )
    .unwrap();
    let (inbox, jobs) = mpsc::channel();
    let (reply, answers) = mpsc::channel();
    inbox
        .send(schedule::Msg::Job(Job {
            target: Target::Command("hold".to_owned()),
            arg: Value::String(String::new()),
            reply,
            asked: Instant::now(),
        }))
        .unwrap();
    let http = inbox.clone();
    let timeouts = Mutex::new(BTreeMap::new());
    let serve_dir = dir.clone();
    std::thread::spawn(move || {
        schedule::serve(
            "hold",
            &serve_dir,
            Path::new("/nonexistent-fiber-home"),
            &jobs,
            &http,
            &timeouts,
        );
    });
    accepted_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hold never reached the server");
    assert!(matches!(
        answers.recv_timeout(Duration::from_secs(2)).unwrap(),
        Reply::Deadline(_)
    ));
    drop(answers);
    let (later, later_rx) = mpsc::channel();
    inbox
        .send(schedule::Msg::Job(Job {
            target: Target::Command("later".to_owned()),
            arg: Value::String(String::new()),
            reply: later,
            asked: Instant::now(),
        }))
        .unwrap();
    release_tx.send(()).unwrap();
    match later_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Reply::Deadline(_)) | Ok(Reply::Done(_)) => {
            panic!("a later job ran after the caller left")
        }
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// [`Deadline::at`] is the instant [`Deadline::start`] stored.
#[test]
fn the_deadline_reports_the_instant_it_was_started() {
    let deadline = Deadline::default();
    let started = deadline.start(Duration::from_secs(5));
    let now = Instant::now();
    assert_eq!(deadline.at(), started);
    assert!(
        started.is_some_and(|at| at >= now && at <= now + Duration::from_secs(5)),
        "the deadline is not five seconds ahead: {started:?}"
    );
}
