use super::super::host_calls::{Pause, pause_settle, unpause_settle};
use super::*;
use fakes::Deadline;
use std::sync::mpsc::{self, Receiver, Sender};

struct SettlePause;

impl Drop for SettlePause {
    fn drop(&mut self) {
        unpause_settle();
    }
}

fn hold_exec(stage: Pause) -> (Receiver<()>, Sender<()>, SettlePause) {
    let (arrived, arrival) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    pause_settle(Arc::new(move |target, pause| {
        if pause == stage {
            // Admission is observed only for the timer's exec, not the
            // command that registers the timer.
            if matches!(target, Target::Timer { .. }) || pause != Pause::Admission {
                let _sent = arrived.send(());
                let _released = released.lock().unwrap().recv();
            }
        }
    }));
    (arrival, release, SettlePause)
}

#[test]
fn stop_for_shutdown_queues_the_extension_exec_before_the_callback_returns() {
    let (arrival, release, _pause) = hold_exec(Pause::ExecFinished);
    let running = running_exec();
    let (tx, rx) = mpsc::channel();
    running.extension.deliver_to(tx);
    running.extension.hub.stop_for_shutdown();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrival)
        .expect("the exec finished its hub registration");
    let result = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.completed)
        .expect("the stopped callback returned");
    assert!(matches!(result, Err(crate::Error::Stopped { .. })));
    let Delivery::ExtensionExec(line) = rx.try_recv().expect("the line precedes the callback")
    else {
        panic!("expected extension_exec");
    };
    assert_eq!(line.program, "sh");
    assert!(line.process.signal.is_some());
    release.send(()).unwrap();
    running
        .watchdog
        .stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

fn timer_exec(
    dir: &std::path::Path,
    ready: &std::path::Path,
    clock: &Arc<fakes::clock::FakeClock>,
) -> Arc<super::super::LuaExtension> {
    let init = format!(
        r#"fiber.command("arm", {{ timeout = 5000, run = function()
  host.after(100, function()
    host.exec("sh", {{"-c", "echo $$ > \"$0\"", {ready:?}}})
    return "fired"
  end, {{ timeout = 5000 }})
  return "armed"
end }})"#,
        ready = ready.display().to_string(),
    );
    std::fs::write(dir.join("init.lua"), init).unwrap();
    Arc::new(super::super::LuaExtension::new(
        SHUTDOWN_EXTENSION,
        dir,
        dir,
        clock.clone(),
    ))
}

fn fire_timer(extension: &Arc<super::super::LuaExtension>, clock: &fakes::clock::FakeClock) {
    let caller = Arc::clone(extension);
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(caller.command("arm", ""));
    });
    assert_eq!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&result)
            .unwrap()
            .unwrap(),
        "armed"
    );
    clock.advance_when_parked(
        Some(clock.origin() + Duration::from_millis(100)),
        None,
        Duration::from_millis(100),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
}

#[test]
fn wait_for_execs_waits_for_a_timer_run_to_queue_its_line() {
    let dir = fakes::TempDir::new("fiber-hub-timer-barrier");
    let ready = fakes::children::Ready::new(dir.path());
    let clock = fakes::clock::FakeClock::new();
    let (arrival, release, _pause) = hold_exec(Pause::ExecEnding);
    let extension = timer_exec(dir.path(), ready.path(), &clock);
    let (tx, rx) = mpsc::channel();
    extension.deliver_to(tx);
    fire_timer(&extension, &clock);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrival)
        .expect("timer exec ended");
    extension.hub.stop_for_shutdown();
    let waiter = Arc::clone(&extension);
    let (started_tx, started) = mpsc::channel();
    let (done, returned) = mpsc::channel();
    std::thread::spawn(move || {
        started_tx.send(std::thread::current().id()).unwrap();
        waiter.hub.wait_for_execs();
        let _sent = done.send(());
    });
    let waiter_id = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&started)
        .expect("the waiter started");
    assert!(
        clock.await_thread_parked(
            waiter_id,
            None,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the barrier parks while the run is held"
    );
    assert!(
        returned.try_recv().is_err(),
        "the barrier waits for the held run"
    );
    release.send(()).unwrap();
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&returned)
        .expect("the barrier returned");
    let Delivery::ExtensionExec(line) = rx
        .try_recv()
        .expect("the run queued its line before returning")
    else {
        panic!("expected extension_exec");
    };
    assert_eq!(line.program, "sh");
    assert_eq!(line.process.exit_code, Some(0));
}

#[test]
fn wait_for_execs_closes_admission_before_observing_empty() {
    let dir = fakes::TempDir::new("fiber-hub-admission-barrier");
    let ready = dir.path().join("never-started");
    let clock = fakes::clock::FakeClock::new();
    let (arrival, release, _pause) = hold_exec(Pause::Admission);
    let extension = timer_exec(dir.path(), &ready, &clock);
    let (tx, rx) = mpsc::channel();
    extension.deliver_to(tx);
    fire_timer(&extension, &clock);
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&arrival)
        .expect("timer held before admission");
    assert!(extension.hub.lock().execs.is_empty());
    let waiter = Arc::clone(&extension);
    let (done, returned) = mpsc::channel();
    std::thread::spawn(move || {
        waiter.hub.wait_for_execs();
        let _sent = done.send(());
    });
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&returned)
        .expect("empty barrier returned");
    assert!(
        matches!(extension.hub.lock().phase, Phase::Stopped(_)),
        "the barrier closes admission"
    );
    release.send(()).unwrap();
    // The VM processes the refusal before its thread returns. Disposing it
    // leaves no admission path, even if it has not resumed yet.
    let caller = Arc::clone(&extension);
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(caller.command("arm", ""));
    });
    assert!(matches!(
        Deadline::after(fakes::MUST_SUCCEED_WITHIN)
            .recv(&result)
            .unwrap(),
        Err(crate::Error::Stopped { .. })
    ));
    assert!(!ready.exists(), "the refused exec never starts");
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

const SHUTDOWN_EXTENSION: &str = "fiber.test/shutdown";
const CHILD: &str = "FIBER_HUB_STOP_EVERY_CALLBACK_CHILD";
const CHILD_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A real command callback suspended in an admitted `host.exec` run.
/// The ready FIFO fixes the process-start point; the second FIFO keeps
/// the shell in its process group without a real-clock hold.
struct RunningExec {
    _dir: fakes::TempDir,
    _ready: fakes::children::Ready,
    extension: Arc<super::super::LuaExtension>,
    group: u32,
    completed: std::sync::mpsc::Receiver<Result<String, crate::Error>>,
    watchdog: fakes::Watchdog,
}

fn running_exec() -> RunningExec {
    let dir = fakes::TempDir::new("fiber-lua-hub-shutdown");
    let ready = fakes::children::Ready::new(dir.path());
    let blocked = dir.path().join("blocked.fifo");
    fakes::fifo(&blocked);
    let init = format!(
        r#"fiber.command("run", {{ timeout = 60000, run = function()
  host.exec("sh", {{ "-c", "echo $$ > \"$0\"; read _ < \"$1\"", {ready:?}, {blocked:?} }})
  return "finished"
end }})
"#,
        ready = ready.path().display().to_string(),
        blocked = blocked.display().to_string(),
    );
    std::fs::write(dir.path().join("init.lua"), init).unwrap();
    let extension = Arc::new(super::super::LuaExtension::new(
        SHUTDOWN_EXTENSION,
        dir.path(),
        dir.path(),
        fakes::clock::FakeClock::new(),
    ));
    let caller = Arc::clone(&extension);
    let (tx, completed) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(tx.send(caller.command("run", "")));
    });
    let group = ready.wait(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN))[0];
    let watchdog = fakes::Watchdog::group(group);
    {
        let shared = extension.hub.lock();
        assert!(
            shared.calls.iter().any(|(id, progress)| {
                matches!(progress, Progress::Started { .. }) && shared.exec_pending(*id)
            }),
            "the real callback is waiting on its admitted host.exec run"
        );
    }
    assert!(
        fakes::kill_group(group, "0").unwrap(),
        "the exec process group is live before shutdown"
    );
    RunningExec {
        _dir: dir,
        _ready: ready,
        extension,
        group,
        completed,
        watchdog,
    }
}

fn assert_shutdown_stopped(running: RunningExec) {
    let result = fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&running.completed)
        .expect("the callback completes after shutdown");
    assert!(
        matches!(
            result,
            Err(crate::Error::Stopped { ref extension }) if extension == SHUTDOWN_EXTENSION
        ),
        "the callback answers Stopped: {result:?}"
    );
    assert!(
        !fakes::kill_group(running.group, "0").unwrap(),
        "the exec process group is empty when the callback completes"
    );
    running
        .watchdog
        .stand_down(fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN));
}

/// `stop_every_callback` reaches a real command callback and its
/// running `host.exec` process group. This test runs in a child process
/// because shutdown deliberately stops every hub in the process.
#[test]
fn stop_every_callback_stops_an_in_flight_exec_and_its_call() {
    let name = std::thread::current().name().unwrap().to_owned();
    if in_child(&name) {
        return;
    }
    let running = running_exec();
    stop_every_callback();
    assert_shutdown_stopped(running);
}

/// `stop_for_shutdown` stops a real command callback's admitted
/// `host.exec` run, waits for its process group to empty, and completes
/// the callback as `Stopped`.
#[test]
fn stop_for_shutdown_signals_an_in_flight_exec_and_marks_its_call() {
    let running = running_exec();
    running.extension.hub.stop_for_shutdown();
    assert_shutdown_stopped(running);
}

/// Runs the global-shutdown test alone so it cannot stop hubs owned by
/// unrelated unit tests in the parent test process.
fn in_child(name: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(tx.send(child.wait_with_output().unwrap()));
    });
    let output = match fakes::Deadline::after(CHILD_WAIT).recv(&rx) {
        Ok(output) => output,
        Err(_) => {
            fakes::within(
                "kill -KILL on the child",
                Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                move || {
                    drop(fakes::kill_pid(pid, "KILL"));
                },
            );
            panic!("waited {CHILD_WAIT:?} for the isolated shutdown test");
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the isolated shutdown test failed: {stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran exactly the shutdown test: {stdout}"
    );
    true
}

/// `stop_for_shutdown` on an already-stopped hub changes nothing: the
/// stored reason stays, and a queued call the shutdown never started
/// stays uncancelled. Without the `!` the stop would re-run and
/// overwrite the reason with `Stopped`, failing here.
#[test]
fn stop_for_shutdown_leaves_a_stopped_hub_unchanged() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    let id = {
        let mut shared = hub.lock();
        let now = hub.clock().now();
        let id = shared.push(Target::Tool("t".to_owned()), serde_json::Value::Null, now);
        shared.phase = Phase::Stopped(StopReason::Abandoned {
            extension: "ext".to_owned(),
            callback: "hook".to_owned(),
        });
        id
    };
    hub.stop_for_shutdown();
    let shared = hub.lock();
    assert!(
        matches!(&shared.phase, Phase::Stopped(StopReason::Abandoned { .. })),
        "the stored reason stays"
    );
    assert!(
        !shared.cancelled.contains(&id),
        "the queued call stays uncancelled"
    );
}

/// `finish_watcher` clears the abandon watch only for its own id: a finish
/// with another id leaves the watch, its own clears it. Flipping `==` to
/// `!=` would clear on the other id and keep on its own.
#[test]
fn finish_watcher_clears_the_abandon_watch_only_for_its_own_id() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    {
        let mut shared = hub.lock();
        shared.abandon = Some(AbandonWatch {
            id: 7,
            target: Target::Watcher { index: 0 },
            deadline: None,
        });
    }
    hub.finish_watcher("ext", 8, 0, Ok(serde_json::Value::Null));
    assert!(
        hub.lock()
            .abandon
            .as_ref()
            .is_some_and(|watch| watch.id == 7),
        "another watcher's finish leaves the watch"
    );
    hub.finish_watcher("ext", 7, 0, Ok(serde_json::Value::Null));
    assert!(
        hub.lock().abandon.is_none(),
        "its own finish clears the watch"
    );
}

/// `finish_watcher` keeps only the drain's own result for collection: a
/// live delivery finishing while the drain keeps its call is forgotten
/// instead of retained, so its failure still takes the notice path.
/// Flipping `==` to `!=` would retain the live call and drop the drain's.
#[test]
fn finish_watcher_keeps_only_the_drains_result() {
    let hub = Hub::new(fakes::clock::FakeClock::new());
    {
        let mut shared = hub.lock();
        shared.keep_watcher_done = Some(7);
        shared.calls.insert(7, Progress::Queued);
        shared.calls.insert(8, Progress::Queued);
    }
    hub.finish_watcher("ext", 8, 0, Err(stopped("ext")));
    assert!(
        !hub.lock().calls.contains_key(&8),
        "the live call is forgotten while the drain keeps its own"
    );
    hub.finish_watcher("ext", 7, 0, Err(stopped("ext")));
    assert!(
        matches!(hub.lock().calls.get(&7), Some(Progress::Done(_))),
        "the drain's own call is kept for collection"
    );
}
