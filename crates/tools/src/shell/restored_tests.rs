//! The moved runner through its exported API: byte streams, monitor
//! policy, report ownership and revocation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Event, JobCompleted, JobLine, JobStarted, Outcome};
use contract::jobs::{End, Lines, Opened, Stop};
use contract::keeper::{Exit, HostEvent, Stream};
use contract::{ErrorCode, JobId};
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::{AdoptedRun, AdoptedSink};

fn deadline() -> Deadline {
    Deadline::after(Duration::from_secs(10))
}

/// The fake clock, telling the test each time the code under test reads it:
/// a read is the signal that the runner has its time for the burst in hand.
struct ProbeClock {
    inner: Arc<FakeClock>,
    reads: Mutex<mpsc::Sender<()>>,
}

impl Clock for ProbeClock {
    fn now(&self) -> Instant {
        let now = self.inner.now();
        match support::lock(&self.reads).send(()) {
            Ok(()) | Err(_) => {}
        }
        now
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: std::sync::Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

/// A handed-over job's execution under test: its files, its event sender,
/// and where its reports land.
struct Case {
    _root: fakes::TempDir,
    output: PathBuf,
    events: mpsc::Sender<HostEvent>,
    completions: mpsc::Receiver<JobCompleted>,
    lines: mpsc::Receiver<JobLine>,
    stops: mpsc::Receiver<()>,
    reads: mpsc::Receiver<()>,
    emit: Arc<fakes::Recorder>,
    fake: Arc<FakeClock>,
}

/// The runner for `job`, built through the exported constructor from the
/// registry's `Opened` job with the keeper's bytes already in its file.
fn run_for(
    name: &str,
    job: &str,
    keeper_bytes: &[u8],
    monitor: bool,
    errors: bool,
) -> (AdoptedRun, Case) {
    let root = fakes::TempDir::new(name);
    let output = root.path().join(format!("{job}.log"));
    std::fs::write(&output, keeper_bytes).unwrap();
    let file = OpenOptions::new().append(true).open(&output).unwrap();
    let errors = errors.then(|| {
        let path = root.path().join(format!("{job}.stderr.log"));
        std::fs::write(&path, b"old\n").unwrap();
        OpenOptions::new().append(true).open(&path).unwrap()
    });
    let (events, received) = mpsc::channel();
    let (end_send, completions) = mpsc::channel();
    let (line_send, lines) = mpsc::channel();
    let (stop_send, stops) = mpsc::channel();
    let emit = Arc::new(fakes::Recorder::default());
    let recorder: Arc<dyn Emit> = emit.clone();
    let opened = Opened {
        started: JobStarted {
            job_id: JobId(job.into()),
            tool: Some("shell".into()),
            extension: None,
            description: format!("job {job}"),
            output_path: format!("artifacts/{job}.log"),
        },
        path: output.clone(),
        file,
        end: End(Box::new(move |completed| match end_send.send(completed) {
            Ok(()) | Err(_) => {}
        })),
        emit: recorder,
        lines: monitor.then(|| {
            Lines(Box::new(move |line| match line_send.send(line) {
                Ok(()) | Err(_) => {}
            }))
        }),
    };
    let fake = FakeClock::new();
    let (read_send, reads) = mpsc::channel();
    let clock: Arc<dyn Clock> = Arc::new(ProbeClock {
        inner: Arc::clone(&fake),
        reads: Mutex::new(read_send),
    });
    let stop = Stop(Box::new(move || match stop_send.send(()) {
        Ok(()) | Err(_) => {}
    }));
    let run = AdoptedRun::new(opened, received, errors, stop, clock);
    // The monitor's budget starts now: the constructor read the clock once.
    if monitor {
        deadline().recv_or_fail(&reads, "the feed's first read of the clock");
    }
    let case = Case {
        _root: root,
        output,
        events,
        completions,
        lines,
        stops,
        reads,
        emit,
        fake,
    };
    (run, case)
}

fn out(text: &str) -> HostEvent {
    HostEvent::Output {
        stream: Stream::Out,
        bytes: text.as_bytes().to_vec(),
    }
}

fn err(text: &str) -> HostEvent {
    HostEvent::Output {
        stream: Stream::Err,
        bytes: text.as_bytes().to_vec(),
    }
}

fn exit(code: i32) -> HostEvent {
    HostEvent::Exit(Exit {
        code: Some(code),
        signal: None,
        timed_out: false,
        output_cap: false,
    })
}

/// Takes the event sender out of the case; dropping it closes the stream.
fn take_events(case: &mut Case) -> mpsc::Sender<HostEvent> {
    std::mem::replace(&mut case.events, mpsc::channel().0)
}

/// The progress the runner showed, in order.
fn shown(case: &Case) -> String {
    case.emit
        .events()
        .into_iter()
        .filter_map(|event| {
            if let Event::JobDelta(delta) = event {
                delta.progress.text
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn stdout_stderr_a_split_character_and_exit_append_exactly() {
    let (run, mut case) = run_for("restored-bytes", "j_one", b"from the keeper\n", false, true);
    let events = take_events(&mut case);
    events.send(out("hello\nwor")).unwrap();
    events.send(out("ld\n")).unwrap();
    events.send(err("err\n")).unwrap();
    events.send(out("price: ")).unwrap();
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: vec![0xE2, 0x82],
        })
        .unwrap();
    events
        .send(HostEvent::Output {
            stream: Stream::Out,
            bytes: vec![0xAC, b'\n'],
        })
        .unwrap();
    events.send(exit(0)).unwrap();
    drop(events);
    assert!(run.run().is_none(), "a reported exit returns no report");
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.job_id, JobId("j_one".into()));
    assert_eq!(completed.status, Outcome::Completed);
    // The keeper's bytes stay, the session's follow, byte for byte.
    assert_eq!(
        std::fs::read(&case.output).unwrap(),
        b"from the keeper\nhello\nworld\nprice: \xe2\x82\xac\n"
    );
    assert_eq!(
        completed.output_tail.as_deref(),
        Some("from the keeper\nhello\nworld\nprice: \u{20ac}\n")
    );
    // Progress carries what the session appended, decoded across the split.
    assert_eq!(shown(&case), "hello\nworld\nprice: \u{20ac}\n");
    // Stderr stays in its file.
    assert_eq!(
        std::fs::read(case.output.with_file_name("j_one.stderr.log")).unwrap(),
        b"old\nerr\n"
    );
    assert!(
        case.completions.try_recv().is_err(),
        "the ending is reported once"
    );
}

#[test]
fn stderr_without_its_file_lands_in_the_output() {
    let (run, mut case) = run_for("restored-err-no-file", "j_err", b"", false, false);
    let events = take_events(&mut case);
    events.send(err("err\n")).unwrap();
    events.send(exit(0)).unwrap();
    drop(events);
    assert!(run.run().is_none());
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(std::fs::read(&case.output).unwrap(), b"err\n");
}

#[test]
fn an_exit_queued_before_the_run_is_reported() {
    let (run, mut case) = run_for("restored-early-exit", "j_quick", b"early\n", false, false);
    let events = take_events(&mut case);
    events.send(out("late\n")).unwrap();
    events.send(exit(3)).unwrap();
    drop(events);
    assert!(run.run().is_none());
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.job_id, JobId("j_quick".into()));
    assert_eq!(
        completed.process.and_then(|process| process.exit_code),
        Some(3)
    );
    assert_eq!(std::fs::read(&case.output).unwrap(), b"early\nlate\n");
    assert_eq!(completed.output_tail.as_deref(), Some("early\nlate\n"));
}

#[test]
fn outputs_queued_before_the_run_show_as_one_delta() {
    let (run, mut case) = run_for("restored-burst", "j_burst", b"", false, false);
    let events = take_events(&mut case);
    events.send(out("first\n")).unwrap();
    events.send(out("second\n")).unwrap();
    events.send(exit(0)).unwrap();
    drop(events);
    assert!(run.run().is_none(), "a reported exit returns no report");
    // What was already queued goes out together: one burst shows once.
    let deltas: Vec<String> = case
        .emit
        .events()
        .into_iter()
        .filter_map(|event| {
            if let Event::JobDelta(delta) = event {
                delta.progress.text
            } else {
                None
            }
        })
        .collect();
    assert_eq!(deltas, ["first\nsecond\n"], "one burst shows once");
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.status, Outcome::Completed);
}

#[test]
fn closed_events_return_the_uncalled_end() {
    let (run, mut case) = run_for("restored-lost", "j_lost", b"kept\n", false, false);
    drop(take_events(&mut case));
    let report = run.run();
    let Some(end) = report else {
        panic!("a lost connection returns its uncalled report")
    };
    // Nothing was reported on its own.
    assert!(case.completions.try_recv().is_err());
    // The returned report is uncalled: invoking it reports now.
    (end.0)(super::super::hosted::adopted_end(
        JobId("j_lost".into()),
        &case.output,
        &Exit {
            code: Some(0),
            signal: None,
            timed_out: false,
            output_cap: false,
        },
    ));
    let completed = deadline().recv_or_fail(&case.completions, "the invoked report");
    assert_eq!(completed.job_id, JobId("j_lost".into()));
}

#[test]
fn dropping_before_the_run_reports_nothing() {
    let (run, case) = run_for("restored-unstarted", "j_drop", b"kept\n", false, false);
    drop(run);
    // The channel stays open: the uncalled report was forgotten, never
    // dropped, so the registry's drop guard cannot record it.
    assert!(
        matches!(case.completions.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "an unstarted run reports nothing"
    );
}

#[test]
fn revoking_while_blocked_reports_nothing() {
    let (run, mut case) = run_for("restored-revoke", "j_rev", b"kept\n", false, false);
    let sink: AdoptedSink = run.sink();
    let (send, returned) = mpsc::channel();
    std::thread::spawn(move || match send.send(run.run()) {
        Ok(()) | Err(_) => {}
    });
    // Revoked while the runner waits for its next event: it appends
    // nothing after, and the keeper can take the file back.
    sink.close();
    drop(take_events(&mut case));
    assert!(
        deadline()
            .recv_or_fail(&returned, "the revoked runner to return")
            .is_none(),
        "a revoked run returns no report"
    );
    // The channel stays open: the uncalled report was forgotten, never
    // dropped, so the registry's drop guard cannot record it.
    assert!(
        matches!(case.completions.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a revoked run reports nothing"
    );
    assert_eq!(std::fs::read(&case.output).unwrap(), b"kept\n");
}

#[test]
fn text_decoding_holds_an_incomplete_character_for_the_next_bytes() {
    let mut text = super::Text::default();
    let euro = "€".as_bytes();
    assert_eq!(text.decode(&euro[..2]), "");
    assert_eq!(text.decode(&euro[2..]), "€");
    assert_eq!(text.decode(b"a\xffb"), "a\u{fffd}b");
}

#[test]
fn newest_keeps_the_tail_on_a_character_boundary() {
    let text = format!("a{}", "€".repeat(30_000));
    let kept = super::newest(&text);
    assert!(kept.len() <= 64 << 10);
    assert!(kept.starts_with('€'));
    assert_eq!(super::newest("short"), "short");
}

/// Sends one line as its own burst and waits for the runner to read the
/// clock for it, so the next send or advance cannot reach that burst.
fn burst(case: &mut Case, text: &str) {
    case.events.send(out(text)).unwrap();
    deadline().recv_or_fail(&case.reads, "the runner to read the clock for a burst");
}

/// The lines of the next delivery, which must be a monitor's.
fn delivered(case: &Case) -> JobLine {
    deadline().recv_or_fail(&case.lines, "a monitor delivery")
}

/// Lets `by` pass on the fake clock. The runner only reads it, and the
/// caller has seen its read for the burst before this one.
fn advance(case: &Case, by: Duration) {
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    case.fake.advance(by);
}

#[test]
fn a_restored_monitor_spends_a_budget_of_ten_and_reports_what_it_suppressed() {
    let (run, mut case) = run_for("restored-budget", "j_mon", b"", true, false);
    let (send, returned) = mpsc::channel();
    std::thread::spawn(move || match send.send(run.run()) {
        Ok(()) | Err(_) => {}
    });
    for n in 0..10 {
        burst(&mut case, &format!("line {n}\n"));
        let line = delivered(&case);
        assert_eq!(line.lines, format!("line {n}"));
        assert_eq!(line.suppressed, None);
    }
    // The clock stands still: the next two are dropped, not delivered.
    burst(&mut case, "dropped one\n");
    burst(&mut case, "dropped two\n");
    case.events.send(exit(0)).unwrap();
    drop(take_events(&mut case));
    // The count arrives before the ending, with no lines of its own.
    let count = delivered(&case);
    assert_eq!((count.lines.as_str(), count.suppressed), ("", Some(2)));
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.status, Outcome::Completed);
    // The file keeps what the model was not sent.
    assert!(
        String::from_utf8(std::fs::read(&case.output).unwrap())
            .unwrap()
            .ends_with("dropped one\ndropped two\n")
    );
    assert!(
        deadline()
            .recv_or_fail(&returned, "the runner to return")
            .is_none()
    );
}

#[test]
fn a_restored_monitor_earns_a_delivery_every_two_seconds() {
    let (run, mut case) = run_for("restored-refill", "j_mon", b"", true, false);
    let (send, returned) = mpsc::channel();
    std::thread::spawn(move || match send.send(run.run()) {
        Ok(()) | Err(_) => {}
    });
    for n in 0..10 {
        burst(&mut case, &format!("line {n}\n"));
        delivered(&case);
    }
    advance(&case, Duration::from_secs(2));
    burst(&mut case, "earned\n");
    let line = delivered(&case);
    assert_eq!((line.lines.as_str(), line.suppressed), ("earned", None));
    case.events.send(exit(0)).unwrap();
    drop(take_events(&mut case));
    assert!(
        deadline()
            .recv_or_fail(&returned, "the runner to return")
            .is_none()
    );
}

#[test]
fn a_restored_monitor_that_floods_for_thirty_seconds_is_stopped_and_ends_flooded() {
    let (run, mut case) = run_for("restored-flood", "j_mon", b"", true, false);
    let (send, returned) = mpsc::channel();
    std::thread::spawn(move || match send.send(run.run()) {
        Ok(()) | Err(_) => {}
    });
    for n in 0..10 {
        burst(&mut case, &format!("line {n}\n"));
        delivered(&case);
    }
    // Two bursts every 1.5 s: the refill earns less than one a step, so the
    // second of each is dropped, and the run of suppression reaches 30 s at
    // the 21st step.
    for _ in 0..21 {
        burst(&mut case, "noise\n");
        burst(&mut case, "noise\n");
        advance(&case, Duration::from_millis(1_500));
    }
    deadline().recv_or_fail(&case.stops, "the stop of a flooded monitor");
    case.events.send(exit(0)).unwrap();
    drop(take_events(&mut case));
    let completed = deadline().recv_or_fail(&case.completions, "the ending");
    assert_eq!(completed.status, Outcome::Failed);
    assert_eq!(
        completed.error.map(|error| error.code),
        Some(ErrorCode::Flooded)
    );
    assert!(
        deadline()
            .recv_or_fail(&returned, "the runner to return")
            .is_none()
    );
}

#[test]
fn take_burst_takes_at_most_the_bound_and_stops_after_an_ending() {
    use super::take_burst;
    // A full queue behind a stalled runner: one pass takes the bound and
    // leaves the rest queued, so the pass's copies stay small.
    let (send, received) = mpsc::channel();
    for _ in 0..contract::keeper::HOST_EVENT_QUEUE {
        send.send(out("x\n")).unwrap();
    }
    let burst = take_burst(out("first\n"), &received);
    assert_eq!(burst.len(), contract::keeper::HOST_BURST_FRAMES);
    assert_eq!(
        received.try_iter().count(),
        contract::keeper::HOST_EVENT_QUEUE + 1 - contract::keeper::HOST_BURST_FRAMES,
        "the rest waits for the next pass"
    );
    // An ending stops the burst even with more queued behind it.
    let (send, received) = mpsc::channel();
    for _ in 0..5 {
        send.send(out("x\n")).unwrap();
    }
    send.send(exit(0)).unwrap();
    for _ in 0..20 {
        send.send(out("y\n")).unwrap();
    }
    let burst = take_burst(out("first\n"), &received);
    assert_eq!(burst.len(), 1 + 5 + 1, "the burst ends at the ending");
    assert_eq!(received.try_iter().count(), 20);
}
