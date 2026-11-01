//! Opening a job, reporting its end, and delivering that end once.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{JobCompleted, Outcome};
use contract::inbox::Delivery;
use contract::jobs::{Foreground, JobRecord, Jobs, OpenError, Opening, Stop};
use contract::shapes::{Failure, Process};
use contract::{ErrorCode, JobId};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder, TempDir};

use super::Registry;
use crate::support::{delegate_finished, delegate_started, ended_ok, opening, world};
use support::clock::Parker;

fn is_job_id(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("j_") else {
        return false;
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn failure() -> JobCompleted {
    JobCompleted {
        job_id: JobId("j_other".into()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::Indeterminate,
            message: "The job ended without a result.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    }
}

#[test]
fn extension_delegates_are_unknown_to_message_and_hide_only_the_final_text_in_answers() {
    for extension in [false, true] {
        let (_dir, registry) = world();
        let finish = if extension {
            registry.open_extension_started(
                JobId("j_one".into()),
                "a".into(),
                "scan".into(),
                "/child/events.jsonl".into(),
                Stop(Box::new(|| {})),
                delegate_started("j_one"),
            )
        } else {
            registry
                .open_started(
                    JobId("j_one".into()),
                    "delegate_spawn".into(),
                    "scan".into(),
                    "/child/events.jsonl".into(),
                    Stop(Box::new(|| {})),
                    delegate_started("j_one"),
                )
                .1
        };
        assert_eq!(
            matches!(registry.target("j_one"), super::runs::Target::Unknown),
            extension
        );
        finish.report(
            ended_ok("j_one"),
            Some(delegate_finished("j_one", "private final")),
        );
        assert_eq!(
            matches!(registry.target("j_one"), super::runs::Target::Unknown),
            extension
        );
        let answer = registry.wait("j_one", 0, &CancelToken::new()).unwrap();
        assert_eq!(answer.text.contains("Final message:"), !extension);
        assert_eq!(answer.text.contains("private final"), !extension);
        assert!(answer.text.contains("Output: /child/events.jsonl"));
        let mut called = false;
        let resumed = registry.resume("j_one", None, |_| {
            called = true;
            Err::<(Stop, ()), _>(())
        });
        assert_eq!(
            matches!(resumed, Err(super::runs::Resume::Unknown)),
            extension
        );
        assert_eq!(called, !extension);
    }
}

#[test]
fn delegate_message_refuses_an_extensions_running_and_ended_job_without_contacting_it() {
    use contract::tool::Tool as _;
    let (_dir, registry) = world();
    let finish = registry.open_extension_started(
        JobId("j_ext".into()),
        "a".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started("j_ext"),
    );
    let tool = crate::delegate::DelegateMessage {
        sandbox: Arc::new(|| Ok(Vec::new())),
        finish: crate::support::clean_finish(),
        depth: 0,
        registry,
        parent: contract::SessionId("s_parent".into()),
        clock: FakeClock::new(),
        bound: fakes::MUST_SUCCEED_WITHIN,
        cap: 1024,
        launch: Arc::new(|_| panic!("an extension delegate must not resume")),
        watch: Arc::new(|_, _, _| panic!("an extension delegate must not be watched")),
        steer: Arc::new(|_, _, _, _, _| panic!("an extension delegate must not be steered")),
        worktree_record: Arc::new(|_, _| Ok(None)),
        last_seq: Arc::new(|_, _| panic!("an extension delegate must not be read")),
    };
    let args = serde_json::json!({"id":"j_ext", "message":"hi"});
    let assert_refused = || {
        let error = tool
            .run(
                args.as_object().unwrap(),
                &CancelToken::new(),
                &Recorder::default(),
            )
            .error
            .unwrap();
        assert_eq!(error.code, ErrorCode::InvalidArguments);
        assert_eq!(
            error.message,
            crate::JobsError::UnknownJob {
                id: contract::JobId("j_ext".into()),
            }
            .to_string()
        );
    };
    assert_refused();
    finish.report(ended_ok("j_ext"), None);
    assert_refused();
}

#[test]
fn open_through_the_jobs_trait_mints_an_id_and_a_file() {
    let (_dir, registry) = world();
    let jobs: &dyn contract::jobs::Jobs = registry.as_ref();
    let opened = contract::jobs::Jobs::open(jobs, opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    assert!(is_job_id(&id), "{id}");
    assert_eq!(opened.started.tool.as_deref(), Some("shell"));
    assert_eq!(opened.started.description, "npm test");
    assert_eq!(opened.started.output_path, format!("artifacts/{id}.log"));
    assert!(opened.path.is_file());
    assert!(registry.list_text().contains(&id));
    assert!(registry.list_text().contains("npm test"));
    drop(opened.end);
}

#[test]
fn open_without_the_registry_arc_records_nothing() {
    let dir = TempDir::new("fiber-jobs-detached");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let registry = Registry {
        artifacts: artifacts.clone(),
        clock,
        emit: Arc::new(Recorder::default()),
        inner: Mutex::new(super::Inner {
            jobs: Vec::new(),
            inbox: None,
            usage_tap: None,
            foreground: Vec::new(),
            extension_starts_closed: false,
            stop_all_delegates: false,
        }),
        park: Parker::new(),
        start_gate: Mutex::new(()),
        timing: std::sync::OnceLock::new(),
        me: Weak::new(),
    };
    let Err(error) = registry.open(opening("npm test")) else {
        panic!("open recorded a job without the registry");
    };
    assert!(matches!(error, OpenError { .. }));
    assert!(error.to_string().contains("registry is gone"), "{error}");
    assert!(
        std::fs::read_dir(&artifacts).unwrap().next().is_none(),
        "open created a file without the registry"
    );
    assert_eq!(registry.list_text(), "No jobs.\n");
}

#[test]
fn open_mints_an_id_and_an_empty_output_file() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    assert!(is_job_id(&id), "{id}");
    assert_eq!(opened.started.tool.as_deref(), Some("shell"));
    assert_eq!(opened.started.extension, None);
    assert_eq!(opened.started.description, "npm test");
    assert_eq!(opened.started.output_path, format!("artifacts/{id}.log"));
    assert!(opened.path.is_absolute());
    assert_eq!(
        opened.path.file_name().and_then(|name| name.to_str()),
        Some(format!("{id}.log").as_str())
    );
    assert!(opened.path.exists());
    assert_eq!(std::fs::metadata(&opened.path).unwrap().len(), 0);
    writeln!(&opened.file, "hello").unwrap();
    assert_eq!(std::fs::read_to_string(&opened.path).unwrap(), "hello\n");
    drop(opened.end);
}

#[test]
fn two_opens_mint_different_ids() {
    let (_dir, registry) = world();
    let first = registry.open(opening("one")).unwrap();
    let second = registry.open(opening("two")).unwrap();
    assert_ne!(first.started.job_id, second.started.job_id);
    drop(first.end);
    drop(second.end);
}

#[test]
fn open_into_a_missing_artifacts_directory_records_nothing() {
    let dir = TempDir::new("fiber-jobs-missing");
    let artifacts = dir.path().join("artifacts");
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let registry = Registry::new(artifacts.clone(), clock, Arc::new(Recorder::default()));
    let Err(error) = registry.open(opening("npm test")) else {
        panic!("open recorded a job when the output file could not be created");
    };
    assert!(matches!(error, OpenError { .. }));
    let text = error.to_string();
    assert!(text.contains("could not be created"), "{text}");
    assert!(text.contains(&artifacts.display().to_string()), "{text}");
    assert_eq!(registry.list_text(), "No jobs.\n");
}

#[test]
fn calling_end_records_that_result_and_drop_does_not_record_again() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Completed,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    let records = settled(&answer);
    let [
        contract::jobs::Taken {
            record: JobRecord::Completed(completed),
            ..
        },
    ] = &records[..]
    else {
        panic!("{records:?}");
    };
    assert_eq!(completed.job_id, JobId(id.clone()));
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(completed.error, None);
    let again = registry.wait(&id, 0, &cancel).unwrap();
    assert!(again.text.contains("completed"), "{}", again.text);
    assert!(!again.text.contains("without a result"), "{}", again.text);
    assert!(settled(&again).is_empty());
}

#[test]
fn a_dropped_end_records_its_job_failed_indeterminate() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    drop(opened.end);
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    assert_eq!(
        settled(&answer),
        vec![contract::jobs::Taken {
            record: JobRecord::Completed(JobCompleted {
                job_id: JobId(id.clone()),
                ..failure()
            }),
            action: None,
        }]
    );
    let again = registry.wait(&id, 0, &cancel).unwrap();
    assert!(settled(&again).is_empty());
}

#[test]
fn a_dropped_end_sends_one_indeterminate_notice_for_its_own_job() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    drop(opened);
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "a dropped end reports once");
    assert_eq!(
        sent.completed,
        JobCompleted {
            job_id: JobId(id),
            ..failure()
        }
    );
}

#[test]
fn a_reported_end_sends_one_notice_with_its_own_id_and_no_fallback() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok("j_other"));
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "a reported end reports once");
    assert_eq!(sent.completed, ended_ok(&id));
}

#[test]
fn the_completion_is_claimed_once() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Cancelled,
        error: None,
        process: None,
        output_tail: None,

        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    let cancel = CancelToken::new();
    let first = registry.wait(&id, 0, &cancel).unwrap();
    let second = registry.wait(&id, 0, &cancel).unwrap();
    assert!(matches!(
        &settled(&first)[..],
        [contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        }]
    ));
    assert!(second.text.contains("cancelled"), "{}", second.text);
    assert!(settled(&second).is_empty());
}

#[test]
fn a_wake_after_the_sequence_snapshot_returns_the_wait() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    let waited = id.clone();
    let (tx, rx) = mpsc::channel();
    let waiting = Arc::clone(&registry);
    let end = opened.end;
    let reported = id.clone();
    thread::spawn(move || {
        super::park::BEFORE_PARK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                (end.0)(JobCompleted {
                    job_id: JobId(reported),
                    status: Outcome::Completed,
                    error: None,
                    process: None,
                    output_tail: None,

                    notice: None,
                    changed_by: None,
                    protected_paths: None,
                });
            }));
        });
        let answer = waiting.wait(&id, 60_000, &CancelToken::new()).unwrap();
        let _sent = tx.send(answer);
    });
    let answer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the wait returned after a wake that landed before it parked");
    let records = settled(&answer);
    let [
        contract::jobs::Taken {
            record: JobRecord::Completed(completed),
            ..
        },
    ] = &records[..]
    else {
        panic!("the wait did not deliver the completion");
    };
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(completed.job_id, JobId(waited));
}

/// What the answer's `Settle` takes: the run's news through its end
/// mark, once, in queue order.
fn settled(answer: &super::runs::Answer) -> Vec<contract::jobs::Taken> {
    (answer.settle.clone().expect("an ended answer settles").0)()
}

fn notice(rx: &mpsc::Receiver<Delivery>) -> contract::inbox::JobNotice {
    let delivery = rx.try_recv().expect("the end sent a notice");
    let Delivery::Job(notice) = delivery else {
        panic!("the end sent {delivery:?}");
    };
    notice
}

#[test]
fn an_unclaimed_end_sends_one_notice_whose_claim_holds_once() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "one end sends one notice");
    assert_eq!(sent.completed, ended_ok(&id));
    assert_eq!(
        (sent.claim.0)(),
        Some(Vec::new()),
        "the notice claims the unclaimed end"
    );
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    assert!(answer.text.contains("completed"), "{}", answer.text);
    assert!(settled(&answer).is_empty(), "the notice already claimed it");
}

fn line(id: &str, lines: &str, suppressed: Option<u64>) -> contract::events::JobLine {
    contract::events::JobLine {
        job_id: JobId(id.into()),
        lines: lines.into(),
        suppressed,

        changed_by: None,
    }
}

#[test]
fn lines_are_offered_only_to_a_job_that_asks() {
    let (_dir, registry) = world();
    let plain = registry.open(opening("npm test")).unwrap();
    assert!(plain.lines.is_none());
    let monitor = registry
        .open(Opening {
            lines: true,
            ..opening("tail -f log")
        })
        .unwrap();
    assert!(monitor.lines.is_some());
}

#[test]
fn a_monitors_lines_reach_the_inbox_in_order_before_its_notice() {
    let (_dir, registry) = world();
    let opened = registry
        .open(Opening {
            lines: true,
            ..opening("tail -f log")
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let lines = opened.lines.unwrap();
    // Before `deliver_to` a batch is dropped.
    (lines.0)(line(&id, "early", None));
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    (lines.0)(line(&id, "one", None));
    (lines.0)(line(&id, "two", Some(3)));
    (opened.end.0)(ended_ok(&id));
    for expected in [line(&id, "one", None), line(&id, "two", Some(3))] {
        let delivery = rx.try_recv().expect("a batch");
        let Delivery::JobLine(sent) = delivery else {
            panic!("expected a line, got {delivery:?}");
        };
        assert_eq!(sent, expected);
    }
    let _end = notice(&rx);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_notice_after_a_wait_claimed_the_end_does_not_hold() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(
        &settled(&answer)[..],
        [contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        }]
    ));
    let sent = notice(&rx);
    assert_eq!((sent.claim.0)(), None, "the wait claimed the end first");
}

#[test]
fn a_second_end_report_sends_no_second_notice() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    registry.finish_run(1, ended_ok(&id), None);
    registry.finish_run(1, ended_ok(&id), None);
    let _first = notice(&rx);
    assert!(rx.try_recv().is_err(), "a job ends once");
    drop(opened.end);
    assert!(rx.try_recv().is_err(), "a job ends once");
}

#[test]
fn a_claim_after_the_registry_is_gone_does_not_hold() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    drop(registry);
    let sent = notice(&rx);
    assert_eq!((sent.claim.0)(), None);
}

#[test]
fn without_an_inbox_an_end_sends_nothing() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(
        &settled(&answer)[..],
        [contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        }]
    ));
}

#[test]
fn an_end_after_the_loop_is_gone_is_still_recorded() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    drop(rx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(
        &settled(&answer)[..],
        [contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        }]
    ));
}

fn counted_stop(
    registry: &Arc<Registry>,
) -> (
    JobId,
    Arc<std::sync::atomic::AtomicUsize>,
    contract::jobs::Opened,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "sleep".into(),
            stop: Stop(Box::new(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            lines: false,
            input: None,
        })
        .unwrap();
    (opened.started.job_id.clone(), calls, opened)
}

#[test]
fn stop_through_the_trait_sends_the_stop_once_and_reports_running() {
    let (_dir, registry) = world();
    let (id, calls, opened) = counted_stop(&registry);
    assert!(Jobs::stop(registry.as_ref(), &id));
    assert!(
        Jobs::stop(registry.as_ref(), &id),
        "a second stop is still running"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(opened);
}

#[test]
fn stop_through_the_trait_on_an_ended_or_unknown_job_is_false() {
    let (_dir, registry) = world();
    let (id, calls, opened) = counted_stop(&registry);
    (opened.end.0)(ended_ok(&id.0));
    assert!(!Jobs::stop(registry.as_ref(), &id));
    assert!(!Jobs::stop(registry.as_ref(), &JobId("j_missing".into())));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let cancel = CancelToken::new();
    assert!(matches!(
        registry.stop("j_missing", &cancel),
        Err(contract::jobs::StopError::NotRunning)
    ));
    assert!(matches!(
        registry.stop(&id.0, &cancel),
        Err(contract::jobs::StopError::Ended(Outcome::Completed))
    ));
}

#[test]
fn background_asks_each_live_call_once_and_counts_the_ones_that_move() {
    let (_dir, registry) = world();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let moves = |moves: bool| {
        let asked = Arc::clone(&asked);
        let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            moves
        });
        registry.foreground(Foreground(Arc::downgrade(&call)));
        call
    };
    let first = moves(true);
    let second = moves(false);
    let dropped = moves(true);
    drop(dropped);
    assert_eq!(registry.background(), 1);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "each live call is asked once; a dropped one is not"
    );
    drop(first);
    assert_eq!(registry.background(), 0);
    drop(second);
    assert_eq!(registry.background(), 0);
}

#[test]
fn background_with_no_calls_is_zero() {
    let (_dir, registry) = world();
    assert_eq!(registry.background(), 0);
}

#[test]
fn registering_forgets_the_calls_that_are_gone() {
    let (_dir, registry) = world();
    for _ in 0..3 {
        let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
        registry.foreground(Foreground(Arc::downgrade(&call)));
    }
    let live: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
    registry.foreground(Foreground(Arc::downgrade(&live)));
    assert_eq!(super::lock(&registry.inner).foreground.len(), 1);
}

#[test]
fn an_opened_job_emits_through_the_registrys_emitter() {
    let dir = TempDir::new("fiber-jobs-emit");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let recorder = Arc::new(Recorder::default());
    let registry = Registry::new(artifacts, clock, Arc::clone(&recorder) as _);
    let opened = registry.open(opening("npm test")).unwrap();
    let event = contract::events::Event::JobDelta(contract::events::JobDelta {
        job_id: opened.started.job_id.clone(),
        progress: contract::events::Progress {
            text: Some("x".to_owned()),
            details: None,
        },
    });
    opened.emit.emit(&event);
    assert_eq!(recorder.events(), vec![event]);
}

// `write`: typing into a job started with `tty` and collecting what arrives.

type Typed = Arc<Mutex<Vec<u8>>>;

fn clocked_world() -> (TempDir, Arc<FakeClock>, Arc<Registry>) {
    let dir = TempDir::new("fiber-jobs-write");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock = FakeClock::new();
    let as_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let registry = Registry::new(artifacts, as_clock, Arc::new(Recorder::default()));
    (dir, clock, registry)
}

/// A `tty` job whose terminal answers every write by appending `reply` to
/// the output file, as the reader does for a program's echo.
fn open_tty(
    registry: &Arc<Registry>,
    reply: &'static [u8],
) -> (String, contract::jobs::Opened, Typed) {
    let typed: Typed = Arc::default();
    let sink: Arc<Mutex<Option<std::fs::File>>> = Arc::default();
    let (record, file) = (Arc::clone(&typed), Arc::clone(&sink));
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "python3".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |bytes, _, _| {
                record.lock().unwrap().extend_from_slice(bytes);
                file.lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .write_all(reply)
                    .map(|()| bytes.len())
            }))),
        })
        .unwrap();
    *sink.lock().unwrap() = Some(opened.file.try_clone().unwrap());
    (opened.started.job_id.0.clone(), opened, typed)
}

/// Runs `write` on its own thread and returns the answer's channel.
fn writing(
    registry: &Arc<Registry>,
    id: &str,
    input: &'static str,
    wait_ms: u64,
    cancel: CancelToken,
) -> mpsc::Receiver<Result<super::Answer, contract::jobs::WriteError>> {
    let registry = Arc::clone(registry);
    let id = id.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(registry.write(&id, input, wait_ms, &cancel));
    });
    rx
}

#[test]
fn write_returns_the_output_after_the_write_when_the_wait_passes() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, typed) = open_tty(&registry, b"got:hi\r\n");
    // Output from before the write is not part of the answer.
    writeln!(&opened.file, "earlier").unwrap();
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let rx = writing(&registry, &id, "hi\n", 250, CancelToken::new());
    assert!(
        clock.await_parked(deadline, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
        "the write did not park at its wait"
    );
    assert!(rx.try_recv().is_err(), "the write returned before its wait");
    clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(250),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let answer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the write returned")
        .ok()
        .unwrap();
    assert_eq!(*typed.lock().unwrap(), b"hi\n");
    assert!(answer.text.starts_with("got:hi\r\n"), "{}", answer.text);
    assert!(!answer.text.contains("earlier"), "{}", answer.text);
    assert!(answer.text.contains("still running"), "{}", answer.text);
    assert!(answer.settle.is_none());
    drop(opened.end);
}

#[test]
fn write_returns_at_once_with_the_final_state_when_the_job_ends_in_the_wait() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"got:hi\n");
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let rx = writing(&registry, &id, "hi\n", 250, CancelToken::new());
    assert!(clock.await_parked(deadline, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)));
    (opened.end.0)(ended_ok(&id));
    let answer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the write returned")
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("got:hi\n"), "{}", answer.text);
    assert!(answer.text.contains("completed"), "{}", answer.text);
    assert!(!answer.text.contains("still running"), "{}", answer.text);
    assert!(matches!(
        &settled(&answer)[..],
        [contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        }]
    ));
    // The completion was claimed by the write.
    let again = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(settled(&again).is_empty());
}

#[test]
fn a_cancel_returns_the_write_at_once_with_what_arrived() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"partial\n");
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let cancel = CancelToken::new();
    let rx = writing(&registry, &id, "x", 250, cancel.clone());
    assert!(clock.await_parked(deadline, fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)));
    cancel.cancel();
    let answer = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the cancelled write returned")
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("partial\n"), "{}", answer.text);
    assert!(answer.text.contains("still running"), "{}", answer.text);
    drop(opened.end);
}

#[test]
fn write_does_not_reach_a_job_without_a_terminal_an_unknown_job_or_an_ended_one() {
    let (_dir, _clock, registry) = clocked_world();
    let plain = registry.open(opening("ls")).unwrap();
    let plain_id = plain.started.job_id.0.clone();
    let cancel = CancelToken::new();
    assert!(matches!(
        registry.write(&plain_id, "x", 0, &cancel),
        Err(contract::jobs::WriteError::NotTty)
    ));
    assert!(matches!(
        registry.write("j_missing", "x", 0, &cancel),
        Err(contract::jobs::WriteError::NotRunning)
    ));
    let (id, tty, typed) = open_tty(&registry, b"");
    (tty.end.0)(ended_ok(&id));
    assert!(matches!(
        registry.write(&id, "x", 0, &cancel),
        Err(contract::jobs::WriteError::Ended(Outcome::Completed))
    ));
    assert!(
        typed.lock().unwrap().is_empty(),
        "a write reached an ended job"
    );
    drop(plain.end);
}

#[test]
fn a_failed_write_to_the_terminal_is_an_io_error() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| {
                Err(std::io::Error::other("the terminal is closed"))
            }))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let result = registry.write(&id, "x", 0, &CancelToken::new());
    assert!(
        matches!(&result, Err(contract::jobs::WriteError::Io(err)) if err.to_string().contains("closed"))
    );
    drop(opened.end);
}

#[test]
fn write_returns_all_the_output_with_no_cut() {
    const BIG: &[u8] = &[b'a'; 40 * 1024];
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, BIG);
    let answer = registry
        .write(&id, "x", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert_eq!(answer.text.lines().next().unwrap().len(), 40 * 1024);
    assert!(!answer.text.contains("omitted"));
    drop(opened.end);
}

#[test]
fn a_write_starting_inside_a_character_drops_its_continuation_bytes() {
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"\xA9ok");
    // The file ends inside U+00E9 before the write, so the new bytes begin
    // with its second byte.
    (&opened.file).write_all(b"x\xC3").unwrap();
    let answer = registry
        .write(&id, "x", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("ok\n"), "{:?}", answer.text);
    drop(opened.end);
}

#[test]
fn a_write_that_typed_less_than_it_was_given_says_so() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| Ok(1)))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let answer = registry
        .write(&id, "abc", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(
        answer.text.starts_with("Wrote 1 of 3 bytes.\n"),
        "{}",
        answer.text
    );
    let all = registry
        .write(&id, "", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(!all.text.contains("Wrote"), "{}", all.text);
    drop(opened.end);
}

/// Runs the [`Jobs`] seam's `write` on its own thread and returns the
/// answer's channel.
fn seam_writing(
    registry: &Arc<Registry>,
    id: &str,
    input: String,
) -> mpsc::Receiver<Result<(), contract::jobs::WriteError>> {
    let registry = Arc::clone(registry);
    let id = id.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(seam_write(&registry, &JobId(id), &input));
    });
    rx
}

/// Waits for `cancel` on `clock`, as a terminal whose queue stays full
/// does: parked inside the clock's wait, past the writer's own clock
/// check, woken whenever the clock moves. Every caller moves the clock, so
/// the wait always ends. True when the cancel fired: a hang-guard expiry
/// reports false, so the write fails instead of resolving to the timeout.
fn wait_for_cancel(
    clock: &dyn contract::clock::Clock,
    cancel: &dyn contract::tool::Cancel,
) -> bool {
    struct Nudge(mpsc::Sender<()>);
    impl contract::clock::Wake for Nudge {
        fn wake(&self) {
            let _sent = self.0.send(());
        }
    }
    let (tx, rx) = mpsc::channel();
    let wake: Arc<dyn contract::clock::Wake> = Arc::new(Nudge(tx));
    clock.subscribe(Arc::downgrade(&wake));
    loop {
        // The writer's own clock check: past it before parking.
        if cancel.is_cancelled() {
            return true;
        }
        let mut expired = false;
        clock.wait_until(None, &mut |_| {
            // Bounded in real time: every path below moves the clock first.
            if Deadline::after(Duration::from_secs(10)).recv(&rx).is_err() {
                expired = true;
            }
        });
        if expired {
            return false;
        }
    }
}

fn open_stuck(registry: &Arc<Registry>) -> (String, contract::jobs::Opened, mpsc::Receiver<()>) {
    let (entry_tx, entry_rx) = mpsc::channel();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "sleep infinity".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |_, clock, cancel| {
                let _sent = entry_tx.send(());
                if wait_for_cancel(clock, cancel) {
                    Ok(0)
                } else {
                    Err(std::io::Error::other(
                        "hang guard expired without the clock moving",
                    ))
                }
            }))),
        })
        .unwrap();
    (opened.started.job_id.0.clone(), opened, entry_rx)
}

#[test]
fn seam_write_times_out_once_the_deadline_passes_and_not_before() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, entered) = open_stuck(&registry);
    let rx = seam_writing(&registry, &id, "hi".to_owned());
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entered)
        .expect("the write reached the terminal");
    // Parked inside the clock wait, past the writer's own check.
    let mark = clock.advance_when_parked(
        None,
        None,
        Duration::from_millis(999),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    // The writer processes the pre-deadline advance and parks again
    // before the test asserts it is still waiting.
    assert!(
        clock.await_parked_since(
            &mark,
            None,
            fakes::Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        ),
        "the write did not park again after the pre-deadline advance"
    );
    assert!(
        rx.try_recv().is_err(),
        "the write returned before its deadline"
    );
    // At the deadline it reports that the terminal never took the input.
    clock.advance_when_parked(
        None,
        None,
        Duration::from_millis(1),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let err = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the write returned at its deadline");
    assert!(
        matches!(&err, Err(contract::jobs::WriteError::Io(error))
            if error.kind() == std::io::ErrorKind::TimedOut
                && error.to_string() == "The job's terminal did not take the input."),
        "{err:?}"
    );
    drop(opened.end);
}

#[test]
fn seam_write_behind_another_writer_still_times_out_at_the_deadline() {
    let (_dir, clock, registry) = clocked_world();
    // Another writer holds the terminal while the write arrives.
    let held = Arc::new(Mutex::new(()));
    let guard = held.lock().unwrap();
    let waiting = Arc::clone(&held);
    let (entry_tx, entry_rx) = mpsc::channel();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "sleep infinity".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |_, clock, cancel| {
                let _sent = entry_tx.send(());
                let _other = waiting.lock().unwrap();
                if wait_for_cancel(clock, cancel) {
                    Ok(0)
                } else {
                    Err(std::io::Error::other(
                        "hang guard expired without the clock moving",
                    ))
                }
            }))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let rx = seam_writing(&registry, &id, "hi".to_owned());
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entry_rx)
        .expect("the write reached the terminal");
    // The other writer releases, long after, on the wall clock: the
    // deadline on the session clock still bounds the write.
    drop(guard);
    // Parked inside the clock wait, past the writer's own check, before
    // the clock moves past the deadline.
    clock.advance_when_parked(
        None,
        None,
        Duration::from_millis(1001),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let err = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the write returned at its deadline");
    assert!(
        matches!(&err, Err(contract::jobs::WriteError::Io(error))
            if error.kind() == std::io::ErrorKind::TimedOut),
        "{err:?}"
    );
    drop(opened.end);
}

#[test]
fn seam_write_wakes_through_the_deadline_subscription_alone() {
    let (_dir, clock, registry) = clocked_world();
    let (entry_tx, entry_rx) = mpsc::channel();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "sleep infinity".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |_, clock, cancel| {
                struct Nudge(mpsc::Sender<()>);
                impl contract::clock::Wake for Nudge {
                    fn wake(&self) {
                        let _sent = self.0.send(());
                    }
                }
                let _sent = entry_tx.send(());
                let (tx, rx) = mpsc::channel();
                let wake: Arc<dyn contract::clock::Wake> = Arc::new(Nudge(tx));
                // Only the cancel is subscribed: waking here proves the
                // deadline forwards to the clock.
                cancel.subscribe(Arc::downgrade(&wake));
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let mut expired = false;
                    clock.wait_until(Some(clock.now() + Duration::from_secs(3600)), &mut |_| {
                        // Bounded in real time: expiry fails the
                        // test with another error.
                        if Deadline::after(Duration::from_secs(10)).recv(&rx).is_err() {
                            expired = true;
                        }
                    });
                    if expired {
                        return Err(std::io::Error::other(
                            "hang guard expired without the clock moving",
                        ));
                    }
                }
                Ok(0)
            }))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let rx = seam_writing(&registry, &id, "hi".to_owned());
    Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&entry_rx)
        .expect("the write reached the terminal");
    // Parked inside the clock wait, past the writer's own check, before
    // the clock moves.
    let due = clock.origin() + Duration::from_secs(3600);
    // The only wake is the clock passing the deadline: no second
    // advance, no cancel call.
    clock.advance_when_parked(
        Some(due),
        None,
        Duration::from_secs(2),
        &Deadline::after(fakes::MUST_SUCCEED_WITHIN),
    );
    let err = Deadline::after(fakes::MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the write returned at its deadline");
    assert!(
        matches!(&err, Err(contract::jobs::WriteError::Io(error))
            if error.kind() == std::io::ErrorKind::TimedOut
                && error.to_string() == "The job's terminal did not take the input."),
        "{err:?}"
    );
    drop(opened.end);
}

#[test]
fn seam_write_reports_a_short_write_as_a_timeout() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| Ok(1)))),
        })
        .unwrap();
    assert!(
        matches!(
            seam_write(&registry, &opened.started.job_id, "abc"),
            Err(contract::jobs::WriteError::Io(error))
                if error.kind() == std::io::ErrorKind::TimedOut
        ),
        "a partial write is still a timeout"
    );
    drop(opened.end);
}

#[test]
fn seam_write_succeeds_when_the_terminal_drains_before_the_deadline() {
    let (_dir, clock, registry) = clocked_world();
    let tick = Arc::clone(&clock);
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |bytes, _, _| {
                #[allow(
                    clippy::disallowed_methods,
                    reason = "no code under test waits on the clock (ruling 6179406086)"
                )]
                tick.advance(Duration::from_millis(500));
                Ok(bytes.len())
            }))),
        })
        .unwrap();
    seam_write(&registry, &opened.started.job_id, "hi").unwrap();
    drop(opened.end);
}
/// Types through the [`Jobs`] seam, not the inherent `write` that waits:
/// the driver command is accepted once written.
fn seam_write(
    registry: &Arc<Registry>,
    job_id: &JobId,
    text: &str,
) -> Result<(), contract::jobs::WriteError> {
    <Registry as Jobs>::write(registry.as_ref(), job_id, text)
}

#[test]
fn seam_write_types_into_a_running_tty_job_at_once() {
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, typed) = open_tty(&registry, b"");
    seam_write(&registry, &JobId(id.clone()), "hi\x1b[A").unwrap();
    assert_eq!(*typed.lock().unwrap(), b"hi\x1b[A");
    drop(opened.end);
}

#[test]
fn seam_write_misses_a_non_tty_an_unknown_or_an_ended_job() {
    let (_dir, _clock, registry) = clocked_world();
    let plain = registry.open(opening("ls")).unwrap();
    assert!(matches!(
        seam_write(&registry, &plain.started.job_id, "x"),
        Err(contract::jobs::WriteError::NotTty)
    ));
    assert!(matches!(
        seam_write(&registry, &JobId("j_missing".into()), "x"),
        Err(contract::jobs::WriteError::NotRunning)
    ));
    let (id, tty, typed) = open_tty(&registry, b"");
    (tty.end.0)(ended_ok(&id));
    assert!(matches!(
        seam_write(&registry, &JobId(id), "x"),
        Err(contract::jobs::WriteError::Ended(Outcome::Completed))
    ));
    assert!(
        typed.lock().unwrap().is_empty(),
        "a write reached an ended job"
    );
    drop(plain.end);
}

#[test]
fn seam_write_reports_a_failed_terminal_write_as_io() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| {
                Err(std::io::Error::other("the terminal is closed"))
            }))),
        })
        .unwrap();
    assert!(matches!(
        seam_write(&registry, &opened.started.job_id, "x"),
        Err(contract::jobs::WriteError::Io(_))
    ));
    drop(opened.end);
}

#[test]
fn an_ended_job_drops_its_input() {
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, typed) = open_tty(&registry, b"");
    assert_eq!(Arc::strong_count(&typed), 2);
    (opened.end.0)(ended_ok(&id));
    assert_eq!(Arc::strong_count(&typed), 1, "the ended job kept its input");
    assert!(matches!(
        registry.write(&id, "x", 0, &CancelToken::new()),
        Err(contract::jobs::WriteError::Ended(Outcome::Completed))
    ));
}

#[test]
fn since_text_ends_in_one_newline_and_is_empty_for_no_bytes() {
    let dir = TempDir::new("fiber-jobs-since");
    let path = dir.path().join("out.log");
    std::fs::write(&path, b"").unwrap();
    assert_eq!(super::since_text(&path, 0), "");
    std::fs::write(&path, b"a").unwrap();
    assert_eq!(super::since_text(&path, 0), "a\n");
    std::fs::write(&path, b"a\n").unwrap();
    assert_eq!(super::since_text(&path, 0), "a\n");
    assert_eq!(super::since_text(&dir.path().join("missing.log"), 0), "");
}

#[test]
fn since_text_from_the_start_keeps_every_byte_and_from_inside_drops_continuations() {
    let dir = TempDir::new("fiber-jobs-since-bytes");
    let path = dir.path().join("out.log");
    // A continuation byte at the very start is invalid, not half of an
    // earlier character, so nothing is dropped.
    std::fs::write(&path, b"\xA9ok").unwrap();
    assert_eq!(super::since_text(&path, 0), "\u{fffd}ok\n");
    // From one byte in, the same byte is the end of a split character.
    assert_eq!(super::since_text(&path, 1), "ok\n");
    std::fs::write(&path, b"ab").unwrap();
    assert_eq!(super::since_text(&path, 1), "b\n");
}

#[test]
fn running_lists_running_jobs_in_start_order_and_drops_ended_ones() {
    let (_dir, registry) = world();
    let seam: &dyn Jobs = registry.as_ref();
    assert!(seam.running().is_empty());
    let first = seam.open(opening("one")).unwrap();
    let second = seam.open(opening("two")).unwrap();
    let third = seam.open(opening("three")).unwrap();
    let ids = [
        first.started.job_id.clone(),
        second.started.job_id.clone(),
        third.started.job_id.clone(),
    ];
    assert_eq!(seam.running(), ids.to_vec());
    (second.end.0)(ended_ok(&ids[1].0));
    assert_eq!(seam.running(), vec![ids[0].clone(), ids[2].clone()]);
    drop(first.end);
    (third.end.0)(ended_ok(&ids[2].0));
    assert!(seam.running().is_empty());
}

#[test]
fn deliver_to_through_the_seam_sends_the_end_before_running_drops_it() {
    let (_dir, registry) = world();
    let seam: &dyn Jobs = registry.as_ref();
    let (tx, rx) = mpsc::channel();
    seam.deliver_to(tx);
    let opened = seam.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.clone();
    assert_eq!(seam.running(), vec![id.clone()]);
    (opened.end.0)(ended_ok(&id.0));
    assert!(seam.running().is_empty());
    let sent = notice(&rx);
    assert_eq!(sent.completed, ended_ok(&id.0));
}

fn stopped() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
}

fn stop_of(flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> Stop {
    let flag = std::sync::Arc::clone(flag);
    Stop(Box::new(move || {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }))
}

#[test]
fn open_started_records_the_job_without_a_file() {
    let (_dir, registry) = world();
    let id = super::mint_job_id();
    let flag = stopped();
    let output = "/sessions/s_x/events.jsonl".to_owned();
    let (started, _finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        output.clone(),
        stop_of(&flag),
        delegate_started(&id.0),
    );
    assert_eq!(started.job_id, id);
    assert_eq!(started.tool.as_deref(), Some("delegate_spawn"));
    assert_eq!(started.description, "scan");
    assert_eq!(started.output_path, output);
    assert!(!std::path::Path::new(&output).exists());
    assert!(registry.list_text().contains(&id.0));
    assert!(registry.list_text().contains("scan"));
}

#[test]
fn minted_job_ids_differ() {
    let first = super::mint_job_id();
    let second = super::mint_job_id();
    assert!(is_job_id(&first.0), "{first:?}");
    assert_ne!(first, second);
}

#[test]
fn a_delegate_wait_answers_the_final_message_and_records_once() {
    let (_dir, registry) = world();
    let id = super::mint_job_id();
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/sessions/s_x/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    finish.report(
        ended_ok(&id.0),
        Some(delegate_finished(&id.0, "Done scanning.")),
    );
    let answer = registry.wait(&id.0, 0, &CancelToken::new()).unwrap();
    assert!(answer.text.contains("Done scanning."), "{}", answer.text);
    // Exactly one newline ends the message: without the guard the text
    // runs into the `Output:` line, with a doubled guard it gains a blank
    // one.
    assert!(
        answer
            .text
            .contains("Final message:\nDone scanning.\nOutput: "),
        "{}",
        answer.text
    );
    let text = answer.text.clone();
    let records = settled(&answer);
    let [
        contract::jobs::Taken {
            record: JobRecord::DelegateFinished(finished),
            ..
        },
        contract::jobs::Taken {
            record: JobRecord::Completed(_),
            ..
        },
    ] = &records[..]
    else {
        panic!("a delegate wait records its finish then its end: {records:?}");
    };
    assert_eq!(finished.text, "Done scanning.");
    let again = registry.wait(&id.0, 0, &CancelToken::new()).unwrap();
    assert_eq!(again.text, text);
    assert!(
        settled(&again).is_empty(),
        "the second wait records nothing"
    );
}

#[test]
fn a_notice_claimed_by_wait_claims_nothing_and_vice_versa() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let id = super::mint_job_id();
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/sessions/s_x/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    finish.report(ended_ok(&id.0), Some(delegate_finished(&id.0, "Done.")));
    let first = notice(&rx);
    assert_eq!((first.claim.0)(), Some(Vec::new()), "the first claim holds");
    let answer = registry.wait(&id.0, 0, &CancelToken::new()).unwrap();
    assert!(settled(&answer).is_empty(), "the notice claimed first");

    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let id = super::mint_job_id();
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/sessions/s_x/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    finish.report(ended_ok(&id.0), Some(delegate_finished(&id.0, "Done.")));
    let answer = registry.wait(&id.0, 0, &CancelToken::new()).unwrap();
    assert_eq!(settled(&answer).len(), 2, "the wait claimed first");
    let second = notice(&rx);
    assert_eq!((second.claim.0)(), None, "the wait already claimed the end");
}

#[test]
fn a_dropped_delegate_end_is_indeterminate_with_an_empty_finish() {
    let (_dir, registry) = world();
    let id = super::mint_job_id();
    let (_started, finish) = registry.open_started(
        id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/sessions/s_x/events.jsonl".into(),
        Stop(Box::new(|| {})),
        delegate_started(&id.0),
    );
    drop(finish);
    let answer = registry.wait(&id.0, 0, &CancelToken::new()).unwrap();
    let records = settled(&answer);
    let [
        contract::jobs::Taken {
            record: JobRecord::DelegateFinished(finished),
            ..
        },
        contract::jobs::Taken {
            record: JobRecord::Completed(completed),
            ..
        },
    ] = &records[..]
    else {
        panic!("a dropped delegate end still records both: {records:?}");
    };
    assert_eq!(finished.text, "");
    // An empty message still ends its line: without the guard the next
    // line starts immediately after the header.
    assert!(
        answer.text.contains("Final message:\n\nOutput: "),
        "{}",
        answer.text
    );
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::Indeterminate)
    );
}

#[test]
fn stop_delegates_stops_only_running_delegates() {
    let (_dir, registry) = world();
    let shell_flag = stopped();
    let mut shell_opening = opening("npm test");
    shell_opening.stop = stop_of(&shell_flag);
    let _shell = registry.open(shell_opening).unwrap();
    let delegate_flag = stopped();
    let delegate_id = super::mint_job_id();
    let (_started, finish) = registry.open_started(
        delegate_id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/sessions/s_x/events.jsonl".into(),
        stop_of(&delegate_flag),
        delegate_started(&delegate_id.0),
    );
    assert_eq!(registry.stop_delegates(), 1);
    assert!(
        delegate_flag.load(std::sync::atomic::Ordering::SeqCst),
        "the delegate got its stop"
    );
    assert!(
        !shell_flag.load(std::sync::atomic::Ordering::SeqCst),
        "an ordinary job keeps running"
    );
    assert_eq!(
        registry.stop_delegates(),
        0,
        "a sent stop is not sent again"
    );
    finish.report(
        ended_ok(&delegate_id.0),
        Some(delegate_finished(&delegate_id.0, "Done.")),
    );
    assert_eq!(
        registry.stop_delegates(),
        0,
        "an ended delegate sends nothing"
    );
}

#[test]
fn a_stop_racing_a_gated_start_stops_the_starting_delegate() {
    // The spend trip can fire while a delegate spawn sits inside the
    // admission gate, spawned but not yet registered: the scan misses it,
    // and the one-way trip never fires again. The stop latches, so the
    // starting delegate observes it at commit and stops at once, exactly
    // once. Forced order over channels, no sleeps: the start parks inside
    // the gate; the crossing fires and completes; the start commits after.
    // Without the latch the start escapes unstopped.
    let (_dir, registry) = world();
    let stops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (id_tx, id_rx) = mpsc::channel::<JobId>();
    let deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    std::thread::scope(|scope| {
        // Test-owned: on a panic this sender drops while the closure
        // unwinds, before the scope joins the worker parked on it, so
        // the join cannot hang behind a live sender.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let worker = std::sync::Arc::clone(&registry);
        let worker_stops = std::sync::Arc::clone(&stops);
        scope.spawn(move || {
            let admitted = worker.admit(false, || {
                entered_tx
                    .send(())
                    .expect("the crossing waits for the start");
                release_rx.recv().expect("the crossing fires first");
                let id = super::mint_job_id();
                let stop = Stop(Box::new(move || {
                    worker_stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }));
                let (_started, _finish) = worker.open_started(
                    id.clone(),
                    "delegate_spawn".into(),
                    "scan".into(),
                    "/sessions/s_x/events.jsonl".into(),
                    stop,
                    delegate_started(&id.0),
                );
                id_tx.send(id).expect("the test reads the committed id");
                Ok::<(), String>(())
            });
            admitted.expect("the gated start commits");
        });
        // The crossing fires while the start sits inside the gate: nothing
        // is registered, so the scan stops nothing but latches the stop.
        deadline
            .recv(&entered_rx)
            .expect("the start entered the gate");
        assert_eq!(
            registry.stop_delegates(),
            0,
            "nothing is registered at the crossing"
        );
        release_tx.send(()).expect("the start commits after");
    });
    assert_eq!(
        stops.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the starting delegate stops exactly once"
    );
    let id = deadline.recv(&id_rx).expect("the start committed");
    {
        let inner = super::lock(&registry.inner);
        let job = inner
            .jobs
            .iter()
            .find(|job| job.started.job_id == id)
            .expect("the start committed");
        assert!(job.stop_sent, "the commit observed the latched stop");
    }
    assert_eq!(
        registry.stop_delegates(),
        0,
        "a sent stop is not sent again"
    );
}

#[test]
fn retain_extensions_stops_only_unlisted_extension_jobs_once() {
    let (_dir, registry) = world();
    let flag_a = stopped();
    let id_a = super::mint_job_id();
    let _finish_a = registry.open_extension_started(
        id_a.clone(),
        "a".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        stop_of(&flag_a),
        delegate_started(&id_a.0),
    );
    let flag_b = stopped();
    let id_b = super::mint_job_id();
    let finish_b = registry.open_extension_started(
        id_b.clone(),
        "b".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        stop_of(&flag_b),
        delegate_started(&id_b.0),
    );
    let model_flag = stopped();
    let model_id = super::mint_job_id();
    let (_started, _model_finish) = registry.open_started(
        model_id.clone(),
        "delegate_spawn".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        stop_of(&model_flag),
        delegate_started(&model_id.0),
    );
    let shell_flag = stopped();
    let mut shell_opening = opening("npm test");
    shell_opening.stop = stop_of(&shell_flag);
    let _shell = registry.open(shell_opening).unwrap();
    // Only `b` is unlisted, so only its stop is sent, once.
    assert_eq!(registry.retain_extensions(&["a".into()]), 1);
    assert!(flag_b.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!flag_a.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!model_flag.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!shell_flag.load(std::sync::atomic::Ordering::SeqCst));
    // A sent stop is not sent again.
    assert_eq!(registry.retain_extensions(&["a".into()]), 0);
    // An ended job sends nothing and counts nothing.
    finish_b.report(ended_ok(&id_b.0), Some(delegate_finished(&id_b.0, "Done.")));
    assert_eq!(registry.retain_extensions(&["a".into()]), 0);
    assert!(!flag_a.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn retain_extensions_with_no_loaded_names_stops_every_extension_job() {
    let (_dir, registry) = world();
    let flag_a = stopped();
    let id_a = super::mint_job_id();
    let _finish_a = registry.open_extension_started(
        id_a.clone(),
        "a".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        stop_of(&flag_a),
        delegate_started(&id_a.0),
    );
    let flag_b = stopped();
    let id_b = super::mint_job_id();
    let _finish_b = registry.open_extension_started(
        id_b.clone(),
        "b".into(),
        "scan".into(),
        "/child/events.jsonl".into(),
        stop_of(&flag_b),
        delegate_started(&id_b.0),
    );
    assert_eq!(registry.retain_extensions(&[]), 2);
    assert!(flag_a.load(std::sync::atomic::Ordering::SeqCst));
    assert!(flag_b.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn retain_extensions_waits_for_a_start_holding_the_gate_then_stops_it() {
    let (_dir, registry) = world();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let held = Arc::clone(&registry);
    let (started_tx, started_rx) = mpsc::channel();
    thread::spawn(move || {
        let answer = held.admit(true, || {
            entered_tx.send(()).unwrap();
            let _released = release_rx.recv();
            let id = super::mint_job_id();
            let flag = stopped();
            let finish = held.open_extension_started(
                id.clone(),
                "gone".into(),
                "scan".into(),
                "/child/events.jsonl".into(),
                stop_of(&flag),
                delegate_started(&id.0),
            );
            Ok::<_, ()>((id, finish, flag))
        });
        started_tx.send(answer).unwrap();
    });
    let wait = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    wait.recv(&entered_rx).expect("the start holds the gate");
    // `retain` takes the same gate, so it cannot return yet.
    let (retained_tx, retained_rx) = mpsc::channel();
    let retainer = Arc::clone(&registry);
    thread::spawn(move || {
        retained_tx.send(retainer.retain_extensions(&[])).unwrap();
    });
    assert!(
        Deadline::after(Duration::from_millis(100))
            .recv(&retained_rx)
            .is_err(),
        "retain waits on the start gate"
    );
    release_tx.send(()).unwrap();
    let (id, _finish, flag) = wait
        .recv(&started_rx)
        .expect("the start recorded")
        .expect("the start was admitted");
    assert_eq!(
        wait.recv(&retained_rx).expect("retain returned"),
        1,
        "retain stops the job the closure recorded"
    );
    assert!(
        flag.load(std::sync::atomic::Ordering::SeqCst),
        "the recorded job got its stop"
    );
    assert!(
        Jobs::running(registry.as_ref())
            .iter()
            .any(|running| running == &id),
        "the job is still listed running"
    );
}
