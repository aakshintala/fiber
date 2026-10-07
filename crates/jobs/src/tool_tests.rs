//! `jobs` list, wait and stop.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use contract::events::{JobCompleted, Outcome};
use contract::jobs::{End, JobRecord, Opening, Stop};
use contract::shapes::{ContentPart, Effect, Failure, Process};
use contract::tool::{Cancel, Output, Tool};
use contract::{ErrorCode, JobId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};

use super::JobsTool;
use crate::Registry;

const DEADLINE: Duration = Duration::from_secs(5);

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn text_of(output: &Output) -> &str {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

fn run(tool: &JobsTool, value: Value, cancel: &dyn Cancel) -> Output {
    tool.run(&args(value), cancel, &Recorder::default())
}

fn setup(clock: Arc<dyn Clock>) -> (TempDir, Arc<Registry>, JobsTool) {
    let dir = TempDir::new("fiber-jobs-tool");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let registry = Registry::new(artifacts, clock, Arc::new(Recorder::default()));
    let tool = JobsTool::new(Arc::clone(&registry));
    (dir, registry, tool)
}

fn open(registry: &Arc<Registry>, description: &str, stop: Stop) -> contract::jobs::Opened {
    registry
        .open(Opening {
            tool: "shell".into(),
            description: description.into(),
            stop,
            lines: false,
            input: None,
        })
        .unwrap()
}

fn idle_stop() -> Stop {
    Stop(Box::new(|| {}))
}

fn completed(id: &str, status: Outcome) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status,
        error: None,
        process: None,
        output_tail: None,
    }
}

struct RecordingClock {
    inner: Arc<FakeClock>,
    untils: Mutex<Vec<Option<Instant>>>,
    cv: Condvar,
    /// When set, [`Clock::now`] returns this instead of the inner clock.
    /// A `now` near the end of `Instant`'s range makes `u64::MAX`
    /// milliseconds overflow `checked_add`.
    at: Mutex<Option<Instant>>,
}

impl RecordingClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: FakeClock::new(),
            untils: Mutex::new(Vec::new()),
            cv: Condvar::new(),
            at: Mutex::new(None),
        })
    }

    /// `now()` stays at `at`. Used so a timeout can overflow the clock.
    fn set_now(&self, at: Instant) {
        *self.at.lock().unwrap() = Some(at);
    }

    /// The `until` values `wait_until` has been called with. Waits until the
    /// first one is recorded.
    fn await_first(&self) -> Vec<Option<Instant>> {
        let guard = self.untils.lock().unwrap();
        let (guard, timeout) = self
            .cv
            .wait_timeout_while(guard, DEADLINE, |untils| untils.is_empty())
            .unwrap();
        assert!(!timeout.timed_out(), "the wait did not reach the clock");
        guard.clone()
    }
}

impl Clock for RecordingClock {
    fn now(&self) -> Instant {
        self.at.lock().unwrap().unwrap_or_else(|| self.inner.now())
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, duration: Duration) {
        self.inner.sleep(duration);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        {
            let mut guard = self.untils.lock().unwrap();
            guard.push(until);
            self.cv.notify_all();
        }
        self.inner.wait_until(until, wait);
    }

    fn subscribe(&self, waker: Weak<dyn Wake>) {
        self.inner.subscribe(waker);
    }
}

#[test]
fn the_definition_is_list_wait_write_and_stop() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, _registry, tool) = setup(clock);
    let definition = tool.definition();
    assert_eq!(definition.name, "jobs");
    assert!(!definition.deferred);
    let schema = definition.input_schema;
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["action"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["list", "wait", "write", "stop"])
    );
    assert_eq!(schema["properties"]["input"]["type"], "string");
    assert_eq!(schema["properties"]["job_id"]["type"], "string");
    assert_eq!(schema["properties"]["timeout_ms"]["type"], "integer");
    assert_eq!(schema["properties"]["timeout_ms"]["minimum"], json!(0));
}

#[test]
fn effects_follow_the_action() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, _registry, tool) = setup(clock);
    let of = |action: &str| tool.effects(&args(json!({"action": action}))).unwrap();
    let list = of("list");
    let wait = of("wait");
    let stop = of("stop");
    let write = of("write");
    assert_eq!(write.declared.effects, vec![Effect::Executes]);
    assert!(!write.declared.reversible);
    assert_eq!(write.declared.paths, None);
    assert_eq!(write.subject, Some(String::new()));
    assert_eq!(list.declared.effects, vec![Effect::Reads]);
    assert!(list.declared.reversible);
    assert_eq!(list.declared.paths, None);
    assert_eq!(wait.declared, list.declared);
    assert!(stop.declared.effects.is_empty());
    assert!(!stop.declared.reversible);
    assert_eq!(stop.declared.paths, None);
    for effects in [&list, &wait, &stop] {
        assert_eq!(effects.subject, Some(String::new()));
        assert_eq!(effects.prefix, None);
    }
    let missing = tool.effects(&Map::new()).unwrap_err();
    assert!(missing.to_string().contains("action"), "{missing}");
}

#[test]
fn list_is_empty_then_shows_jobs_in_start_order() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let cancel = CancelToken::new();
    let empty = run(&tool, json!({"action": "list"}), &cancel);
    assert!(empty.error.is_none());
    assert!(empty.jobs.is_empty());
    assert_eq!(text_of(&empty), "No jobs.\n");

    let running = open(&registry, "npm test", idle_stop());
    let ended = open(&registry, "build", idle_stop());
    let running_id = running.started.job_id.0.clone();
    let ended_id = ended.started.job_id.0.clone();
    let running_path = running.path.clone();
    let ended_path = ended.path.clone();
    (ended.end.0)(completed(&ended_id, Outcome::Completed));
    let listed = run(&tool, json!({"action": "list"}), &cancel);
    assert!(listed.error.is_none());
    assert!(listed.jobs.is_empty(), "list does not deliver a completion");
    assert_eq!(
        text_of(&listed),
        format!(
            "{running_id} running npm test \u{2014} {}\n{ended_id} completed build \u{2014} {}\n",
            running_path.display(),
            ended_path.display()
        )
    );
    // Listing did not claim the ended job.
    let waited = run(
        &tool,
        json!({"action": "wait", "job_id": ended_id, "timeout_ms": 0}),
        &cancel,
    );
    assert!(waited.error.is_none());
    assert!(matches!(waited.jobs.as_slice(), [JobRecord::Completed(_)]));
    drop(running.end);
}

#[test]
fn an_unknown_job_id_is_invalid_arguments_for_every_action() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, _registry, tool) = setup(clock);
    let cancel = CancelToken::new();
    for value in [
        json!({"action": "list", "job_id": "j_missing"}),
        json!({"action": "wait", "job_id": "j_missing", "timeout_ms": 0}),
        json!({"action": "stop", "job_id": "j_missing"}),
    ] {
        let output = run(&tool, value, &cancel);
        assert_eq!(
            output.error.as_ref().map(|error| error.code.clone()),
            Some(ErrorCode::InvalidArguments)
        );
        assert!(
            text_of(&output).contains("j_missing"),
            "{}",
            text_of(&output)
        );
        assert!(output.jobs.is_empty());
    }
}

#[test]
fn list_with_a_known_id_still_lists() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let output = run(
        &tool,
        json!({"action": "list", "job_id": id}),
        &CancelToken::new(),
    );
    assert!(output.error.is_none());
    assert!(text_of(&output).contains(&id), "{}", text_of(&output));
    assert!(text_of(&output).contains("running"), "{}", text_of(&output));
    drop(opened.end);
}

#[test]
fn wait_and_stop_require_the_arguments_they_read() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, _registry, tool) = setup(clock);
    let cancel = CancelToken::new();
    let missing_id = run(&tool, json!({"action": "wait", "timeout_ms": 1}), &cancel);
    assert!(
        text_of(&missing_id).contains("job_id"),
        "{}",
        text_of(&missing_id)
    );
    let missing_stop = run(&tool, json!({"action": "stop"}), &cancel);
    assert!(text_of(&missing_stop).contains("job_id"));
    let missing_timeout = run(&tool, json!({"action": "wait", "job_id": "j_x"}), &cancel);
    assert!(
        text_of(&missing_timeout).contains("timeout_ms"),
        "{}",
        text_of(&missing_timeout)
    );
    assert!(
        text_of(&missing_timeout).contains("Give"),
        "{}",
        text_of(&missing_timeout)
    );
    for bad in [json!(-1), json!(1.5), json!("5")] {
        let output = run(
            &tool,
            json!({"action": "wait", "job_id": "j_x", "timeout_ms": bad}),
            &cancel,
        );
        assert!(text_of(&output).contains("integer"), "{}", text_of(&output));
        assert_eq!(
            output.error.as_ref().map(|error| error.code.clone()),
            Some(ErrorCode::InvalidArguments)
        );
    }
    let missing_action = run(&tool, json!({}), &cancel);
    assert_eq!(
        missing_action
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
}

#[test]
fn a_job_that_already_ended_is_returned_at_once_and_claimed_once() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let path = opened.path.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::NonzeroExit,
            message: "Exit code 1.".into(),
            retry_after: None,
            provider: None,
        }),
        process: Some(Process {
            exit_code: Some(1),
            signal: Some("SIGKILL".into()),
            timed_out: false,
        }),
        output_tail: Some("1 failing\n".into()),
    });
    let cancel = CancelToken::new();
    let output = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &cancel,
    );
    assert!(output.error.is_none());
    let expected = format!(
        "Job {id} failed.\nExit code 1.\nKilled by SIGKILL.\nExit code 1.\nOutput: {}\nLast output:\n1 failing\n",
        path.display()
    );
    assert_eq!(text_of(&output), expected);
    assert_eq!(output.jobs.len(), 1);
    let again = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &cancel,
    );
    assert!(again.jobs.is_empty());
    assert_eq!(text_of(&again), expected);
    assert!(again.error.is_none());
}

#[test]
fn a_completed_job_names_its_exit_code_and_nothing_else() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let path = opened.path.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Completed,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: true,
        }),
        output_tail: None,
    });
    let output = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &CancelToken::new(),
    );
    assert_eq!(
        text_of(&output),
        format!(
            "Job {id} completed.\nExit code 0.\nOutput: {}\n",
            path.display()
        )
    );
    assert!(!text_of(&output).contains("Last output"));
    assert!(!text_of(&output).contains("Killed"));
}

#[test]
fn final_text_adds_a_newline_only_when_the_piece_has_none() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let path = opened.path.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Cancelled,
        error: Some(Failure {
            code: ErrorCode::ToolError,
            message: "Stopped.\n".into(),
            retry_after: None,
            provider: None,
        }),
        process: Some(Process {
            exit_code: None,
            signal: Some("SIGTERM".into()),
            timed_out: true,
        }),
        output_tail: Some("tail".into()),
    });
    let output = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &CancelToken::new(),
    );
    assert_eq!(
        text_of(&output),
        format!(
            "Job {id} cancelled.\nKilled by SIGTERM.\nStopped.\nOutput: {}\nLast output:\ntail\n",
            path.display()
        )
    );
}

#[test]
fn a_zero_timeout_on_a_running_job_returns_at_once() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
            &CancelToken::new(),
        ));
    });
    let output = rx.recv_timeout(DEADLINE).expect("a zero timeout returned");
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    assert!(
        text_of(&output).contains("still running"),
        "{}",
        text_of(&output)
    );
    let listed = run(
        &JobsTool::new(registry),
        json!({"action": "list"}),
        &CancelToken::new(),
    );
    assert!(text_of(&listed).contains("running"), "{}", text_of(&listed));
    drop(opened.end);
}

#[test]
fn a_timeout_names_the_waited_jobs_output_path_when_another_job_exists() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let other = open(&registry, "one", idle_stop());
    let waited = open(&registry, "two", idle_stop());
    let other_path = other.path.display().to_string();
    let id = waited.started.job_id.0.clone();
    let path = waited.path.display().to_string();
    let output = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &CancelToken::new(),
    );
    assert_eq!(
        text_of(&output),
        format!("Job {id} is still running.\nOutput: {path}\n")
    );
    assert_ne!(path, other_path);
    drop(other.end);
    drop(waited.end);
}

#[test]
fn a_wait_returns_when_the_job_ends_and_when_the_deadline_passes() {
    let clock = FakeClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, _tool) = setup(as_clock);
    let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&stops);
    let opened = open(
        &registry,
        "npm test",
        Stop(Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
    );
    let id = opened.started.job_id.0.clone();
    let timeout_ms = 5_000u64;
    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap();

    let tool = JobsTool::new(Arc::clone(&registry));
    let wait_id = id.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": wait_id, "timeout_ms": timeout_ms}),
            &CancelToken::new(),
        ));
    });
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the wait did not park at its deadline"
    );
    assert!(
        rx.try_recv().is_err(),
        "the wait returned before its deadline"
    );
    // Exactly the deadline: `now > until` would leave the wait parked.
    clock.advance(Duration::from_millis(timeout_ms));
    let output = rx.recv_timeout(DEADLINE).expect("the timeout returned");
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    assert!(
        text_of(&output).contains("still running"),
        "{}",
        text_of(&output)
    );
    assert_eq!(stops.load(std::sync::atomic::Ordering::SeqCst), 0);

    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap();
    let tool = JobsTool::new(Arc::clone(&registry));
    let wait_id = id.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": wait_id, "timeout_ms": timeout_ms}),
            &CancelToken::new(),
        ));
    });
    assert!(clock.await_parked(deadline, DEADLINE));
    (opened.end.0)(completed(&id, Outcome::Completed));
    let output = rx.recv_timeout(DEADLINE).expect("the wait returned");
    assert!(output.error.is_none());
    assert!(
        matches!(output.jobs.as_slice(), [JobRecord::Completed(done)] if done.status == Outcome::Completed)
    );
    assert!(
        text_of(&output).contains("completed"),
        "{}",
        text_of(&output)
    );
    assert!(
        !text_of(&output).contains("still running"),
        "{}",
        text_of(&output)
    );
    assert_eq!(stops.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// An instant late enough that adding `u64::MAX` milliseconds does not fit.
/// `u64::MAX` milliseconds fits on a fresh [`Instant`], so the overflow is
/// the clock already being near the end of the range.
fn instant_where_max_millis_overflows(base: Instant) -> Instant {
    let mut lo = 0u64;
    let mut hi = u64::MAX / 2;
    while lo < hi {
        let mid = lo + (hi - lo) / 2 + 1;
        if base.checked_add(Duration::from_secs(mid)).is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    base.checked_add(Duration::from_secs(lo)).unwrap()
}

#[test]
fn a_timeout_that_overflows_the_clock_waits_until_the_job_ends() {
    let clock = RecordingClock::new();
    let late = instant_where_max_millis_overflows(clock.now());
    assert!(
        late.checked_add(Duration::from_millis(u64::MAX)).is_none(),
        "the timeout still fits on the clock"
    );
    clock.set_now(late);
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, tool) = setup(as_clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let end_id = id.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": id, "timeout_ms": u64::MAX}),
            &CancelToken::new(),
        ));
    });
    assert_eq!(clock.await_first(), vec![None]);
    assert!(rx.try_recv().is_err(), "the overflow wait returned early");
    (opened.end.0)(completed(&end_id, Outcome::Completed));
    let output = rx.recv_timeout(DEADLINE).expect("the wait returned");
    assert!(output.error.is_none());
    assert!(matches!(
        output.jobs.as_slice(),
        [JobRecord::Completed(done)] if done.status == Outcome::Completed
    ));
    assert!(
        !text_of(&output).contains("still running"),
        "{}",
        text_of(&output)
    );
}

#[test]
fn cancelling_a_wait_leaves_the_job_running() {
    let clock = FakeClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, tool) = setup(as_clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let timeout_ms = 5_000u64;
    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap();
    let cancel = CancelToken::new();
    let in_call = cancel.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": id, "timeout_ms": timeout_ms}),
            &in_call,
        ));
    });
    assert!(clock.await_parked(deadline, DEADLINE));
    cancel.cancel();
    let output = rx
        .recv_timeout(DEADLINE)
        .expect("the cancelled wait returned");
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    assert!(text_of(&output).contains("still running"));
    let listed = run(
        &JobsTool::new(registry),
        json!({"action": "list"}),
        &CancelToken::new(),
    );
    assert!(text_of(&listed).contains("running"), "{}", text_of(&listed));
    assert!(
        !text_of(&listed).contains("cancelled"),
        "{}",
        text_of(&listed)
    );
    drop(opened.end);
}

#[test]
fn two_waits_deliver_the_completion_once() {
    let clock = FakeClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, _tool) = setup(as_clock);
    let opened = open(&registry, "npm test", idle_stop());
    let id = opened.started.job_id.0.clone();
    let timeout_ms = 5_000u64;
    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap();
    let mut handles = Vec::new();
    for _ in 0..2 {
        let tool = JobsTool::new(Arc::clone(&registry));
        let wait_id = id.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _sent = tx.send(run(
                &tool,
                json!({"action": "wait", "job_id": wait_id, "timeout_ms": timeout_ms}),
                &CancelToken::new(),
            ));
        });
        handles.push(rx);
    }
    assert!(
        clock.await_parked_count(deadline, 2, DEADLINE),
        "both waits did not park"
    );
    (opened.end.0)(completed(&id, Outcome::Failed));
    let outputs: Vec<Output> = handles
        .into_iter()
        .map(|rx| rx.recv_timeout(DEADLINE).expect("a wait returned"))
        .collect();
    let records = outputs
        .iter()
        .filter(|output| !output.jobs.is_empty())
        .count();
    assert_eq!(records, 1);
    for output in &outputs {
        assert!(output.error.is_none());
        assert!(text_of(output).contains("failed"), "{}", text_of(output));
    }
}

#[test]
fn stop_on_an_ended_job_names_how_it_ended() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let cancel = CancelToken::new();
    for (status, word) in [
        (Outcome::Completed, "completed"),
        (Outcome::Failed, "failed"),
        (Outcome::Cancelled, "cancelled"),
    ] {
        let opened = open(&registry, "npm test", idle_stop());
        let id = opened.started.job_id.0.clone();
        (opened.end.0)(completed(&id, status));
        let output = run(&tool, json!({"action": "stop", "job_id": id}), &cancel);
        assert_eq!(
            output.error.as_ref().map(|error| error.code.clone()),
            Some(ErrorCode::InvalidArguments)
        );
        assert!(
            text_of(&output).contains(&format!("ended: {word}")),
            "{}",
            text_of(&output)
        );
        assert!(output.jobs.is_empty());
    }
}

#[test]
fn stop_calls_stop_once_and_returns_the_end_it_reports() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let slot: Arc<Mutex<Option<End>>> = Arc::new(Mutex::new(None));
    let give = Arc::clone(&slot);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let opened = open(
        &registry,
        "npm test",
        Stop(Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(end) = give.lock().unwrap().take() {
                (end.0)(JobCompleted {
                    job_id: JobId("j_ignored".into()),
                    status: Outcome::Cancelled,
                    error: None,
                    process: None,
                    output_tail: None,
                });
            }
        })),
    );
    let id = opened.started.job_id.0.clone();
    *slot.lock().unwrap() = Some(opened.end);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "stop", "job_id": id}),
            &CancelToken::new(),
        ));
    });
    let output = rx.recv_timeout(DEADLINE).expect("stop returned");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(output.error.is_none());
    assert!(
        text_of(&output).contains("cancelled"),
        "{}",
        text_of(&output)
    );
    match output.jobs.as_slice() {
        [JobRecord::Completed(done)] => {
            assert_eq!(done.status, Outcome::Cancelled);
            assert_eq!(done.job_id.0, output_id(&output));
        }
        other => panic!("expected one completion, got {other:?}"),
    }
}

fn output_id(output: &Output) -> String {
    text_of(output)
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned()
}

#[test]
fn cancelling_stop_after_the_stop_was_sent_returns_at_once() {
    let clock = RecordingClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, tool) = setup(as_clock);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let opened = open(
        &registry,
        "npm test",
        Stop(Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
    );
    let id = opened.started.job_id.0.clone();
    let cancel = CancelToken::new();
    let in_call = cancel.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "stop", "job_id": id}),
            &in_call,
        ));
    });
    assert_eq!(clock.await_first(), vec![None]);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        rx.try_recv().is_err(),
        "stop returned before it was cancelled"
    );
    cancel.cancel();
    let output = rx
        .recv_timeout(DEADLINE)
        .expect("the cancelled stop returned");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(output.error.is_none());
    assert!(output.jobs.is_empty());
    let listed = run(
        &JobsTool::new(registry),
        json!({"action": "list"}),
        &CancelToken::new(),
    );
    assert!(text_of(&listed).contains("running"), "{}", text_of(&listed));
    drop(opened.end);
}

// `write` through the tool: arguments, the refusals, and the wait.

fn tty_job(registry: &Arc<Registry>) -> (String, contract::jobs::Opened) {
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "python3".into(),
            stop: idle_stop(),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|bytes, _, _| {
                Ok(bytes.len())
            }))),
        })
        .unwrap();
    (opened.started.job_id.0.clone(), opened)
}

fn invalid(output: &Output) -> bool {
    output.error.as_ref().map(|error| error.code.clone()) == Some(ErrorCode::InvalidArguments)
}

#[test]
fn write_refuses_what_it_cannot_reach_as_invalid_arguments() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let cancel = CancelToken::new();
    let plain = open(&registry, "ls", idle_stop());
    let plain_id = plain.started.job_id.0.clone();
    let (ended_id, ended) = tty_job(&registry);
    (ended.end.0)(completed(&ended_id, Outcome::Failed));
    let (tty_id, tty) = tty_job(&registry);

    let output = run(
        &tool,
        json!({"action": "write", "job_id": plain_id, "input": "x"}),
        &cancel,
    );
    assert!(invalid(&output));
    assert_eq!(
        text_of(&output),
        format!("Job {plain_id} was not started with `tty`.\n")
    );
    let output = run(
        &tool,
        json!({"action": "write", "job_id": "j_missing", "input": "x"}),
        &cancel,
    );
    assert!(invalid(&output));
    assert!(
        text_of(&output).contains("j_missing"),
        "{}",
        text_of(&output)
    );
    let output = run(
        &tool,
        json!({"action": "write", "job_id": ended_id, "input": "x"}),
        &cancel,
    );
    assert!(invalid(&output));
    assert!(text_of(&output).contains("failed"), "{}", text_of(&output));
    let output = run(
        &tool,
        json!({"action": "write", "job_id": tty_id, "input": "x", "timeout_ms": 30_001}),
        &cancel,
    );
    assert!(invalid(&output));
    assert!(text_of(&output).contains("30000"), "{}", text_of(&output));
    for value in [
        json!({"action": "write", "input": "x"}),
        json!({"action": "write", "job_id": tty_id}),
        json!({"action": "write", "job_id": tty_id, "input": 5}),
        json!({"action": "write", "job_id": 5, "input": "x"}),
        json!({"action": "write", "job_id": tty_id, "input": "x", "timeout_ms": -1}),
        json!({"action": "write", "job_id": tty_id, "input": "x", "timeout_ms": "5"}),
    ] {
        let output = run(&tool, value.clone(), &cancel);
        assert!(invalid(&output), "{value}: {}", text_of(&output));
    }
    drop((plain.end, tty.end));
}

#[test]
fn a_failed_write_to_the_terminal_is_a_tool_error() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: idle_stop(),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| {
                Err(std::io::Error::other("the terminal is closed"))
            }))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let output = run(
        &tool,
        json!({"action": "write", "job_id": id, "input": "x"}),
        &CancelToken::new(),
    );
    assert_eq!(
        output.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::ToolError)
    );
    assert!(text_of(&output).contains("closed"), "{}", text_of(&output));
    drop(opened.end);
}

/// The instant a `write` call with these arguments parks at, and the answer
/// it returns once the clock reaches it.
fn write_parks_at(arguments: Value, wait: Duration) {
    let clock = FakeClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, tool) = setup(as_clock);
    let (id, opened) = tty_job(&registry);
    let mut arguments = arguments;
    arguments["job_id"] = json!(id);
    let deadline = clock.now().checked_add(wait).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(&tool, arguments, &CancelToken::new()));
    });
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the write did not park {wait:?} out: {:?}",
        clock.parked()
    );
    assert!(rx.try_recv().is_err(), "the write returned before its wait");
    // One millisecond short is not the wait.
    clock.advance(wait.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "the write returned a millisecond early"
    );
    clock.advance(Duration::from_millis(1));
    let output = rx.recv_timeout(DEADLINE).expect("the write returned");
    assert!(output.error.is_none(), "{}", text_of(&output));
    drop(opened.end);
}

#[test]
fn write_waits_250_ms_by_default() {
    write_parks_at(
        json!({"action": "write", "input": "x"}),
        Duration::from_millis(250),
    );
}

#[test]
fn write_waits_as_long_as_timeout_ms_says_up_to_30_seconds() {
    write_parks_at(
        json!({"action": "write", "input": "x", "timeout_ms": 1_000}),
        Duration::from_secs(1),
    );
    write_parks_at(
        json!({"action": "write", "input": "x", "timeout_ms": 30_000}),
        Duration::from_secs(30),
    );
}

#[test]
fn a_write_of_no_input_waits_at_least_five_seconds() {
    write_parks_at(
        json!({"action": "write", "input": ""}),
        Duration::from_secs(5),
    );
    write_parks_at(
        json!({"action": "write", "input": "", "timeout_ms": 100}),
        Duration::from_secs(5),
    );
    write_parks_at(
        json!({"action": "write", "input": "", "timeout_ms": 7_000}),
        Duration::from_secs(7),
    );
}
