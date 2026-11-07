//! `jobs wait` on a finished job carries its tail; a timed-out wait does not.
//!
//! Lives beside `tool_tests.rs` so this lane does not edit that held file:
//! every helper below mirrors the small setup there.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::JobId;
use contract::clock::Clock;
use contract::events::{JobCompleted, Outcome};
use contract::jobs::{End, JobRecord, Opening, Stop, Taken};
use contract::tool::{Cancel, Output, Tool};
use fakes::Deadline;
use fakes::clock::FakeClock;
use fakes::{CancelToken, MUST_SUCCEED_WITHIN, Recorder, TempDir, text_of, tool_args};
use serde_json::{Value, json};

use super::JobsTool;
use crate::Registry;

/// What the output's `Settle` takes: the run's news through its end
/// mark, once, in queue order.
fn settled(output: &Output) -> Vec<Taken> {
    (output.settle.clone().expect("an ended answer settles").0)()
}

fn run(tool: &JobsTool, value: Value, cancel: &dyn Cancel) -> Output {
    tool.run(&tool_args(value), cancel, &Recorder::default())
}

fn setup(clock: Arc<dyn Clock>) -> (TempDir, Arc<Registry>, JobsTool) {
    let (dir, registry) = crate::support::registry_with(clock);
    let tool = JobsTool::new(Arc::clone(&registry));
    (dir, registry, tool)
}

fn open(registry: &Arc<Registry>, description: &str) -> (String, std::path::PathBuf, End) {
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: description.into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: None,
        })
        .unwrap();
    (
        opened.started.job_id.0.clone(),
        opened.path.clone(),
        opened.end,
    )
}

#[test]
fn a_completed_wait_returns_its_stored_tail() {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let (_dir, registry, tool) = setup(clock);
    let (id, path, end) = open(&registry, "printf 'l1\\nl2\\nl3\\n'");
    end.0(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Completed,
        error: None,
        process: None,
        output_tail: Some("l1\nl2\nl3\n".into()),

        notice: None,
        changed_by: None,
        protected_paths: None,
    });
    let output = run(
        &tool,
        json!({"action": "wait", "job_id": id, "timeout_ms": 0}),
        &CancelToken::new(),
    );
    assert!(output.error.is_none());
    let text = text_of(&output);
    assert!(
        text.contains(&format!("Output: {}", path.display())),
        "{text}"
    );
    assert!(text.contains("Last output:\nl1\nl2\nl3\n"), "{text}");
    assert!(
        matches!(&settled(&output)[..], [Taken { record: JobRecord::Completed(done), .. }] if done.output_tail.as_deref() == Some("l1\nl2\nl3\n")),
        "{text}"
    );
}

#[test]
fn a_wait_that_times_out_carries_no_tail() {
    let clock = FakeClock::new();
    let as_clock: Arc<dyn Clock> = clock.clone();
    let (_dir, registry, _tool) = setup(as_clock);
    let (id, _path, end) = open(&registry, "sleep 60");
    let timeout_ms = 5_000u64;
    let deadline = clock
        .now()
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap();
    let tool = JobsTool::new(Arc::clone(&registry));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(run(
            &tool,
            json!({"action": "wait", "job_id": id, "timeout_ms": timeout_ms}),
            &CancelToken::new(),
        ));
    });
    assert!(
        clock.await_parked(deadline, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the wait did not park at its deadline"
    );
    // Past the deadline with the job still running: the wait ends alone.
    clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(timeout_ms),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let output = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("the timeout returned");
    assert!(output.error.is_none());
    assert!(output.settle.is_none());
    let text = text_of(&output);
    assert!(text.contains("still running"), "{text}");
    assert!(!text.contains("Last output"), "{text}");
    drop(end);
}
