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
    adopted: Option<mpsc::Sender<Adopted>>,
}
/// What the shell told the host to keep: the job, its output file, its
/// stderr file and whether it delivers monitor lines.
struct Adopted {
    job: JobId,
    path: std::path::PathBuf,
    errors: Option<std::path::PathBuf>,
    lines: bool,
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
    fn adopt(
        &self,
        _: &str,
        job: &JobId,
        path: &Path,
        errors: Option<&Path>,
        lines: bool,
    ) -> Result<(), HostError> {
        if let Some(adopted) = &self.adopted {
            adopted
                .send(Adopted {
                    job: job.clone(),
                    path: path.to_path_buf(),
                    errors: errors.map(Path::to_path_buf),
                    lines,
                })
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
    let Adopted {
        job,
        path,
        errors,
        lines,
    } = Deadline::after(Duration::from_secs(10)).recv_or_fail(&adopts, "adopt");
    assert_eq!((errors, lines), (None, false), "a plain background job");
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
fn an_adopted_end_reads_the_output_tail_a_kept_end_leaves_out() {
    let dir = fakes::TempDir::new("fiber-hosted-adopted-end");
    let output = dir.path().join("job.log");
    std::fs::write(&output, "first\nlast\n").unwrap();
    let exit = Exit {
        code: Some(0),
        signal: None,
        timed_out: false,
        output_cap: false,
    };
    let adopted = super::adopted_end(JobId("j_one".into()), &output, &exit);
    assert_eq!(adopted.status, Outcome::Completed);
    assert_eq!(adopted.output_tail.as_deref(), Some("first\nlast\n"));
    // The same exit, written by the keeper, has no tail to give.
    assert!(kept_end(JobId("j_one".into()), &exit).output_tail.is_none());
    // A file the session cannot read gives no tail and no failure.
    let missing = super::adopted_end(JobId("j_one".into()), &dir.path().join("none"), &exit);
    assert!(missing.output_tail.is_none());
    assert_eq!(missing.status, Outcome::Completed);
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

#[test]
fn monitor_lines_without_stderr_artifact() {
    let dir = fakes::TempDir::new("fiber-hosted-monitor-lines");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    // The fixture's first job writes `j_0000000000000001.log`, so its stderr
    // file would be `j_0000000000000001.stderr.log`: a directory there makes
    // the file's creation fail.
    std::fs::create_dir(dir.path().join("j_0000000000000001.stderr.log")).expect("block stderr");
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
                serde_json::json!({"command":"held", "monitor":true})
                    .as_object()
                    .expect("object"),
                &CancelToken::default(),
                &Recorder::default(),
            ),
        )
        .expect("result");
    });
    let (_, events) = Deadline::after(Duration::from_secs(10)).recv_or_fail(&started, "start");
    let adopt = Deadline::after(Duration::from_secs(10)).recv_or_fail(&adopts, "adopt");
    // docs/tools.md, "The keeper": the keeper carries the monitor intent.
    // The missing stderr file leaves the path empty, not the intent.
    assert!(adopt.lines, "the monitor still delivers lines");
    assert_eq!(adopt.errors, None);
    // The process stays alive through the adoption; now let it end.
    events.send(HostEvent::Exit(end())).expect("exit");
    let output = Deadline::after(Duration::from_secs(10)).recv_or_fail(&result, "receipt");
    assert!(output.error.is_none(), "{:?}", output.error);
}
