use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use contract::keeper::{CommandHost, Exit, HostError, HostEvent, Spawn, Started, Stream};
use contract::tool::Tool;
use contract::{ErrorCode, JobId, events::Outcome};
use fakes::{CancelToken, Deadline, Recorder};

use super::kept_end;
use crate::Shell;

struct Host {
    calls: mpsc::Sender<(Spawn, mpsc::Sender<HostEvent>)>,
    stops: Mutex<Vec<String>>,
    stopped: Option<mpsc::Sender<String>>,
    adopted: Option<mpsc::Sender<(JobId, std::path::PathBuf)>>,
}
impl CommandHost for Host {
    fn start(&self, spec: Spawn, events: mpsc::Sender<HostEvent>) -> Result<Started, HostError> {
        self.calls.send((spec, events)).expect("start observed");
        Ok(Started { pgid: u32::MAX })
    }
    fn stop(&self, id: &str) -> bool {
        self.stops.lock().expect("stops").push(id.into());
        if let Some(stopped) = &self.stopped {
            stopped.send(id.into()).expect("stop observed");
        }
        true
    }
    fn write(&self, _: &str, _: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn adopt(&self, _: &str, job: &JobId, path: &Path, _: Option<&Path>) -> Result<(), HostError> {
        if let Some(adopted) = &self.adopted {
            adopted
                .send((job.clone(), path.to_path_buf()))
                .expect("adopt observed");
        }
        Ok(())
    }
}
fn end() -> Exit {
    Exit {
        code: Some(0),
        signal: None,
        timed_out: false,
        output_cap: false,
    }
}

#[test]
fn a_hosted_foreground_call_streams_and_leaves_no_local_group() {
    let (tx, rx) = mpsc::channel();
    let host = Arc::new(Host {
        calls: tx,
        stops: Mutex::new(Vec::new()),
        stopped: None,
        adopted: None,
    });
    let shell =
        Shell::new(std::env::temp_dir(), Arc::new(fakes::clock::SystemClock)).with_host(host);
    let before = support::group::live().len();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let args = serde_json::json!({"command":"echo hosted", "timeout_ms":1234});
        let output = shell.run(
            args.as_object().expect("object"),
            &CancelToken::default(),
            &Recorder::default(),
        );
        done.send(output).expect("result");
    });
    let (spec, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&rx, "host start");
    assert_eq!(spec.timeout_ms, Some(1234));
    assert_eq!(support::group::live().len(), before);
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: b"hosted\n".to_vec(),
        })
        .expect("output");
    events.send(HostEvent::Exit(end())).expect("exit");
    drop(events);
    let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "hosted end");
    assert!(output.error.is_none(), "{:?}", output.error);
    assert!(format!("{:?}", output.content).contains("hosted"));
}

#[test]
fn hosted_exit_flags_and_control_eof_reach_the_call() {
    for (exit, expected) in [
        (
            Some(Exit {
                timed_out: true,
                ..end()
            }),
            ErrorCode::Timeout,
        ),
        (
            Some(Exit {
                output_cap: true,
                ..end()
            }),
            ErrorCode::OutputCap,
        ),
        (None, ErrorCode::Indeterminate),
    ] {
        let (calls, started) = mpsc::channel();
        let host = Arc::new(Host {
            calls,
            stops: Mutex::new(Vec::new()),
            stopped: None,
            adopted: None,
        });
        let shell =
            Shell::new(std::env::temp_dir(), Arc::new(fakes::clock::SystemClock)).with_host(host);
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            done.send(
                shell.run(
                    serde_json::json!({"command":"held"})
                        .as_object()
                        .expect("object"),
                    &CancelToken::default(),
                    &Recorder::default(),
                ),
            )
            .expect("result");
        });
        let (_, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "start");
        if let Some(exit) = exit {
            events.send(HostEvent::Exit(exit)).expect("exit");
        }
        drop(events);
        let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "end");
        assert_eq!(output.error.expect("failure").code, expected);
    }
}

#[test]
fn cancellation_stops_the_host_once_without_a_local_signal() {
    let (calls, started) = mpsc::channel();
    let (stopped, stops) = mpsc::channel();
    let host = Arc::new(Host {
        calls,
        stops: Mutex::new(Vec::new()),
        stopped: Some(stopped),
        adopted: None,
    });
    let shell = Shell::new(std::env::temp_dir(), Arc::new(fakes::clock::SystemClock))
        .with_host(host.clone());
    let cancel = Arc::new(support::cancel::Token::default());
    let worker_cancel = Arc::clone(&cancel);
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(
            shell.run(
                serde_json::json!({"command":"held"})
                    .as_object()
                    .expect("object"),
                &*worker_cancel,
                &Recorder::default(),
            ),
        )
        .expect("result");
    });
    let (spec, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "start");
    cancel.cancel();
    assert_eq!(
        Deadline::after(Duration::from_secs(10)).recv_or_fail(&stops, "stop"),
        spec.id
    );
    events
        .send(HostEvent::Exit(Exit {
            code: None,
            signal: Some(15),
            ..end()
        }))
        .expect("exit");
    let _ = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "cancelled end");
    // docs/tools.md, "Stopping a command": later cancels never start a second stop.
    assert_eq!(*host.stops.lock().expect("stops"), vec![spec.id]);
}

#[test]
fn a_background_call_adopts_after_job_started_and_keeps_writing_its_file() {
    let dir = fakes::TempDir::new("fiber-hosted-adopt");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let (calls, started) = mpsc::channel();
    let (adopted, adopts) = mpsc::channel();
    let host = Arc::new(Host {
        calls,
        stops: Mutex::new(Vec::new()),
        stopped: None,
        adopted: Some(adopted),
    });
    let shell = Shell::new(
        dir.path().to_path_buf(),
        Arc::new(fakes::clock::SystemClock),
    )
    .with_host(host)
    .with_jobs(jobs.clone());
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(
            shell.run(
                serde_json::json!({"command":"held", "run_in_background":true})
                    .as_object()
                    .expect("object"),
                &CancelToken::default(),
                &Recorder::default(),
            ),
        )
        .expect("result");
    });
    let (spec, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "start");
    assert_eq!(spec.timeout_ms, None);
    let (job, path) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&adopts, "adopt");
    // docs/tools.md, "The keeper": adoption follows the durable job start.
    assert_eq!(
        jobs.started()
            .iter()
            .map(|start| &start.job_id)
            .collect::<Vec<_>>(),
        vec![&job]
    );
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: b"after adoption\n".to_vec(),
        })
        .expect("output");
    events.send(HostEvent::Exit(end())).expect("exit");
    let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "receipt");
    assert!(output.error.is_none());
    let completed = jobs
        .ended(Deadline::after(Duration::from_secs(10)))
        .expect("job end");
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(
        std::fs::read(path).expect("output file"),
        b"after adoption\n"
    );
    assert!(adopts.try_recv().is_err());
}

#[test]
fn a_detached_call_bypasses_the_host() {
    let dir = fakes::TempDir::new("fiber-hosted-detached");
    let ready = fakes::children::Ready::new(dir.path());
    let script = dir.path().join("detached-host-bypass.sh");
    std::fs::write(
        &script,
        format!(
            "printf '%s\\n' $$ > '{}'\nexec sleep 60\n",
            ready.path().display()
        ),
    )
    .expect("script");
    let marker = script.to_string_lossy().into_owned();
    let _watchdog = fakes::Watchdog::matching(&marker);
    let (calls, started) = mpsc::channel();
    let host = Arc::new(Host {
        calls,
        stops: Mutex::new(Vec::new()),
        stopped: None,
        adopted: None,
    });
    let shell = Shell::new(
        dir.path().to_path_buf(),
        Arc::new(fakes::clock::SystemClock),
    )
    .with_host(host)
    .with_detached(dir.path().join("detached"));
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(
            shell.run(
                serde_json::json!({"command":format!("sh '{}'", script.display()), "detach":true})
                    .as_object()
                    .expect("object"),
                &CancelToken::default(),
                &Recorder::default(),
            ),
        )
        .expect("receipt");
    });
    let pids = ready.wait(Deadline::after(Duration::from_secs(10)));
    let pgid = pids[0];
    let _group = fakes::Watchdog::group(pgid);
    let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "detached receipt");
    assert!(output.error.is_none());
    assert!(format!("{:?}", output.content).contains("detached"));
    assert!(support::group::alive(pgid));
    assert!(started.try_recv().is_err());
    fakes::kill_group(pgid, "KILL").expect("kill detached group");
    assert!(fakes::group_empties(
        pgid,
        Deadline::after(Duration::from_secs(10))
    ));
}

#[test]
fn hosted_terminal_execute_uses_pty_and_installs_terminal_input() {
    hosted_move_policy(super::command::MovePolicy::Terminal);
}

#[test]
fn hosted_monitor_execute_installs_the_monitor_sink() {
    hosted_move_policy(super::command::MovePolicy::Monitor);
}

fn hosted_move_policy(policy: super::command::MovePolicy) {
    use super::command::{MovePolicy, Ran};
    use contract::keeper::HostStdin;

    let dir = fakes::TempDir::new("fiber-hosted-policy");
    let _watchdog = fakes::Watchdog::matching(&dir.path().to_string_lossy());
    let (calls, started) = mpsc::channel();
    let host = Arc::new(Host {
        calls,
        stops: Mutex::new(Vec::new()),
        stopped: None,
        adopted: None,
    });
    let workdir = dir.path().to_path_buf();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let settings = super::super::sandbox::Settings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: workdir.clone(),
        };
        let ran = super::execute(
            host,
            Path::new("/bin/sh"),
            "held",
            &workdir,
            None,
            &fakes::clock::SystemClock,
            &CancelToken::default(),
            &Recorder::default(),
            policy,
            None,
            None,
            None,
            &settings,
            &workdir,
            None,
        );
        done.send(ran).expect("execute result");
    });
    let (spec, events) =
        Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "hosted policy start");
    assert_eq!(
        spec.stdin,
        if policy == MovePolicy::Terminal {
            HostStdin::Pty
        } else {
            HostStdin::Null
        }
    );
    let ran = Deadline::after(Duration::from_secs(10))
        .recv_or_fail(&result, "hosted policy move")
        .expect("execute");
    let Ran::Moved(moved) = ran else {
        panic!("call must move while host holds its exit")
    };
    assert_eq!(moved.take_input().is_some(), policy == MovePolicy::Terminal);
    {
        let shared = moved.shared();
        let inner = super::super::output::lock(&shared.inner);
        assert_eq!(inner.errors.is_some(), policy == MovePolicy::Monitor);
        assert_eq!(inner.lines.is_some(), policy == MovePolicy::Monitor);
    }
    events.send(HostEvent::Exit(end())).expect("exit");
    drop(events);
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(moved.resume(
            &fakes::clock::SystemClock,
            &CancelToken::default(),
            &Recorder::default(),
        ))
        .expect("resumed result");
    });
    let _ = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "hosted policy reaped");
}

#[test]
fn keeper_end_maps_every_process_failure_without_a_tail() {
    let cases = [
        (end(), Outcome::Completed, None),
        (
            Exit {
                code: Some(3),
                ..end()
            },
            Outcome::Failed,
            Some(ErrorCode::NonzeroExit),
        ),
        (
            Exit {
                code: None,
                signal: Some(15),
                ..end()
            },
            Outcome::Failed,
            Some(ErrorCode::Signal),
        ),
        (
            Exit {
                timed_out: true,
                ..end()
            },
            Outcome::Failed,
            Some(ErrorCode::Timeout),
        ),
        (
            Exit {
                output_cap: true,
                ..end()
            },
            Outcome::Failed,
            Some(ErrorCode::OutputCap),
        ),
    ];
    for (exit, status, code) in cases {
        let kept = kept_end(JobId("j_one".into()), &exit);
        assert_eq!(kept.status, status);
        assert_eq!(kept.error.as_ref().map(|error| &error.code), code.as_ref());
        assert!(kept.output_tail.is_none());
        assert!(kept.notice.is_none());
        assert!(kept.changed_by.is_none());
        assert!(kept.protected_paths.is_none());
    }
}

#[test]
fn a_hosted_background_receipt_names_the_kept_timeout_stop() {
    // The fake clock's wall starts at 2023-11-14T22:13:20Z, so a keeper-held
    // 60 s timeout stops the job at 2023-11-14T22:14:20Z. The drive keeps no
    // local deadline for a hosted command, so the receipt reads the wall the
    // host handoff stored.
    let dir = fakes::TempDir::new("fiber-hosted-receipt-deadline");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let (calls, started) = mpsc::channel();
    let host = Arc::new(Host {
        calls,
        stops: Mutex::new(Vec::new()),
        stopped: None,
        adopted: None,
    });
    let clock = fakes::clock::FakeClock::new();
    let shell = Shell::new(dir.path().to_path_buf(), clock)
        .with_host(host)
        .with_jobs(jobs.clone());
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(
            shell.run(
                serde_json::json!({"command":"held", "run_in_background":true, "timeout_ms":60000})
                    .as_object()
                    .expect("object"),
                &CancelToken::default(),
                &Recorder::default(),
            ),
        )
        .expect("result");
    });
    let (spec, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "start");
    assert_eq!(spec.timeout_ms, Some(60_000));
    let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "receipt");
    let text = format!("{:?}", output.content);
    assert!(
        text.contains("Fiber stops it at 2023-11-14T22:14:20Z (timeout_ms 60000)"),
        "{text}"
    );
    assert!(text.contains("run_in_background"), "{text}");
    events.send(HostEvent::Exit(end())).expect("exit");
    drop(events);
}

/// What a hosted run's emitter and host saw, in order: `starting` from the
/// emitter, `start` from the host.
type Order = Arc<Mutex<Vec<&'static str>>>;

/// An emitter that answers the start hook, notes it in `order` and records
/// every delta.
struct Probe {
    order: Order,
    answer: bool,
    paced: bool,
    recorder: Recorder,
}

impl contract::emit::Emit for Probe {
    fn emit(&self, event: &contract::events::Event) {
        self.recorder.emit(event);
    }

    fn starting(&self) -> bool {
        self.order.lock().expect("order").push("starting");
        self.answer
    }

    fn paced(&self) -> bool {
        self.paced
    }
}

/// A host that notes `start` in `order`, then refuses it with `refusal` or
/// hands the test the run's event sender.
struct Noting {
    order: Order,
    refusal: Option<String>,
    calls: mpsc::Sender<mpsc::Sender<HostEvent>>,
}

impl CommandHost for Noting {
    fn start(&self, _: Spawn, events: mpsc::Sender<HostEvent>) -> Result<Started, HostError> {
        self.order.lock().expect("order").push("start");
        if let Some(io) = &self.refusal {
            return Err(HostError {
                id: "call_refused".into(),
                io: io.clone(),
            });
        }
        self.calls.send(events).expect("start observed");
        Ok(Started { pgid: u32::MAX })
    }
    fn stop(&self, _: &str) -> bool {
        true
    }
    fn write(&self, _: &str, _: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn adopt(&self, _: &str, _: &JobId, _: &Path, _: Option<&Path>) -> Result<(), HostError> {
        Ok(())
    }
}

/// A hosted run in flight on its own thread: what its emitter and host
/// saw, the host's event sender once it started the run, and the result.
struct Noted {
    order: Order,
    probe: Arc<Probe>,
    started: mpsc::Receiver<mpsc::Sender<HostEvent>>,
    result: mpsc::Receiver<std::io::Result<super::Ran>>,
    _dir: fakes::TempDir,
}

/// Runs `super::execute` on its own thread against a host that notes its
/// start.
fn execute_noting(answer: bool, paced: bool, refusal: Option<&str>) -> Noted {
    let dir = fakes::TempDir::new("fiber-hosted-noting");
    let order: Order = Arc::new(Mutex::new(Vec::new()));
    let (calls, started) = mpsc::channel();
    let host = Arc::new(Noting {
        order: Arc::clone(&order),
        refusal: refusal.map(str::to_owned),
        calls,
    });
    let probe = Arc::new(Probe {
        order: Arc::clone(&order),
        answer,
        paced,
        recorder: Recorder::default(),
    });
    let workdir = dir.path().to_path_buf();
    let emit = Arc::clone(&probe);
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let settings = super::super::sandbox::Settings {
            enabled: false,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home: workdir.clone(),
        };
        let ran = super::execute(
            host,
            Path::new("/bin/sh"),
            "held",
            &workdir,
            None,
            &fakes::clock::SystemClock,
            &CancelToken::default(),
            &*emit,
            super::command::MovePolicy::Stay,
            None,
            None,
            None,
            &settings,
            &workdir,
            None,
        );
        let _sent = done.send(ran);
    });
    Noted {
        order,
        probe,
        started,
        result,
        _dir: dir,
    }
}

#[test]
fn a_hosted_call_asks_to_start_once_before_the_host_starts_it() {
    let Noted {
        order,
        started,
        result,
        ..
    } = execute_noting(true, false, None);
    let wait = Deadline::after(Duration::from_secs(10));
    let events = wait.recv_or_fail(&started, "host start");
    assert_eq!(*order.lock().expect("order"), ["starting", "start"]);
    events.send(HostEvent::Exit(end())).expect("exit");
    drop(events);
    let ran = wait.recv_or_fail(&result, "hosted end");
    assert!(matches!(ran, Ok(super::Ran::Finished(..))));
}

#[test]
fn a_declined_hosted_start_never_reaches_the_host() {
    let Noted {
        order,
        probe,
        started,
        result,
        ..
    } = execute_noting(false, false, None);
    let wait = Deadline::after(Duration::from_secs(10));
    let ran = wait.recv_or_fail(&result, "hosted decline");
    assert!(matches!(ran, Ok(super::Ran::Declined)));
    assert_eq!(*order.lock().expect("order"), ["starting"]);
    assert!(
        started.try_recv().is_err(),
        "the host was never asked to start"
    );
    assert!(probe.recorder.events().is_empty());
}

#[test]
fn a_refused_host_start_after_starting_reports_the_error() {
    let Noted {
        order,
        probe,
        result,
        ..
    } = execute_noting(true, false, Some("the keeper refused to start it"));
    let wait = Deadline::after(Duration::from_secs(10));
    let ran = wait.recv_or_fail(&result, "hosted refusal");
    let Err(error) = ran else {
        panic!("a refused start is an error");
    };
    assert!(
        error.to_string().contains("the keeper refused to start it"),
        "the error holds the host's text: {error}"
    );
    assert_eq!(
        *order.lock().expect("order"),
        ["starting", "start"],
        "asked once, before the one start"
    );
    assert!(
        probe.recorder.events().is_empty(),
        "a refused start streamed nothing"
    );
}

/// A hosted call whose emitter asks for pacing: a held change goes out on
/// the park deadline, with the host quiet.
#[test]
fn a_paced_hosted_call_flushes_a_held_change_on_the_park_deadline() {
    let Noted {
        probe,
        started,
        result,
        ..
    } = execute_noting(true, true, None);
    let wait = Deadline::after(Duration::from_secs(40));
    let events = wait.recv_or_fail(&started, "host start");
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: b"a".to_vec(),
        })
        .expect("first output");
    assert!(
        probe.recorder.wait_for_text("a", wait),
        "a goes out at once"
    );
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: b"b".to_vec(),
        })
        .expect("second output");
    assert!(
        probe.recorder.wait_for_text("ab", wait),
        "the held b goes out on the park deadline: {:?}",
        probe.recorder.text()
    );
    events.send(HostEvent::Exit(end())).expect("exit");
    drop(events);
    let ran = wait.recv_or_fail(&result, "hosted end");
    assert!(matches!(ran, Ok(super::Ran::Finished(..))));
    assert_eq!(probe.recorder.text(), "ab");
}
