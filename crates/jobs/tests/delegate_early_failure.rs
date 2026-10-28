//! A delegate whose child fails before its session exists: the parent's
//! completion carries the child's own failure, and `jobs list` shows that
//! error with no log path. Through the jobs crate's public API only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::Arc;
use std::time::Duration;

use contract::events::Outcome;
use contract::inbox::Delivery;
use contract::jobs::{JobRecord, Jobs as _};
use contract::shapes::Failure;
use contract::tool::Tool as _;
use contract::{ErrorCode, PreSessionExit, SessionId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Deadline, Recorder, TempDir, Watchdog};
use jobs::{DelegateSpawn, JobsTool, Registry, Resolve};

fn args(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::Value;
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn text_of(output: &contract::tool::Output) -> &str {
    match output.content.first() {
        Some(contract::shapes::ContentPart::Text { text }) => text,
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn a_delegate_failing_before_its_session_reports_that_failure_and_no_log() {
    let dir = TempDir::new("fiber-jobs-early-failure");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    // The sessions directory exists, but the delegate's own subdirectory
    // never will: the child fails before its session exists.
    let sessions = dir.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let clock = FakeClock::new();
    let registry = Registry::new(
        artifacts,
        Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
        Arc::new(Recorder::default()),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    // Registered before the spawn: the runner's report must have an
    // inbox waiting for it.
    registry.deliver_to(tx);

    // The production startup shape, serialized as the child prints it:
    // one line, no session envelope, no usage, no final message.
    let line = serde_json::to_string(&PreSessionExit::new(
        1,
        Failure {
            code: ErrorCode::InvalidArguments,
            message: "Bad model.".to_owned(),
            retry_after_ms: None,
            provider: None,
        },
    ))
    .unwrap();
    let launch: jobs::Launch = Arc::new(move |_: &jobs::Launched| {
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            &format!(": fiber-jobs-early-failure; printf '%s\\n' '{line}'; exit 1"),
        ]);
        command
    });
    // Refused, as for a child that never bound its socket.
    let watch: jobs::Watch = Arc::new(|_, _, _| Err(std::io::Error::other("refused")));
    let spawn = DelegateSpawn {
        wiring: jobs::DelegateWiring {
            create: Arc::new(
                #[allow(
                    clippy::result_large_err,
                    reason = "the jobs boundary returns its typed error by value"
                )]
                |_, _| panic!("unexpected worktree creation"),
            ),
            finish: Arc::new(|_| panic!("unexpected worktree end")),
            depth: 0,
            registry: Arc::clone(&registry),
            parent: SessionId("s_parent".into()),
            workspace: dir.path().to_path_buf(),
            inherited: Arc::new(Vec::new),
            events_for: {
                let sessions = sessions.clone();
                Arc::new(move |_, id| sessions.join(&id.0).join("events.jsonl"))
            },
            clock: Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
            bound: Duration::from_secs(5),
            cap: 1024,
            launch,
            watch,
        },
        resolve: Resolve {
            lookup: Arc::new(|model: &str| match model {
                "fiber:fake/m" => Ok("fake/m".to_owned()),
                _ => Err(format!("Unknown delegate model {model:?}.")),
            }),
            catalogue: Arc::from("Roles: none\nReferences:\nfiber:fake/m"),
            credential: Arc::new(|_| Ok(None)),
        },
    };

    // Backstop armed before the spawn: the child exits on its own in
    // milliseconds; this only fires if a mutant strands it.
    let watchdog = Watchdog::matching("fiber-jobs-early-failure");
    let cancel = CancelToken::new();
    let output = spawn.run(
        &args(serde_json::json!({
            "description": "scan",
            "prompt": "hi",
            "model": "fiber:fake/m",
        })),
        &cancel,
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    let started = output
        .jobs
        .iter()
        .find_map(|record| match record {
            JobRecord::Started(started) => Some(started.clone()),
            JobRecord::DelegateStarted(_)
            | JobRecord::DelegateFinished(_)
            | JobRecord::Completed(_) => None,
        })
        .expect("the spawn records its start");
    let job_id = started.job_id.0.clone();
    let output_path = started.output_path.clone();

    // The runner's report, under one wall-clock deadline that only guards
    // against a hang: every wake the runner needs comes from the drain.
    let notice = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&rx) {
        Ok(Delivery::Job(notice)) => notice,
        Ok(other) => panic!("expected the runner's report, got {other:?}"),
        Err(_) => panic!("the runner did not report"),
    };
    assert_eq!(notice.completed.job_id.0, job_id);
    assert_eq!(notice.completed.status, Outcome::Failed);
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.code.clone()),
        Some(ErrorCode::InvalidArguments)
    );
    assert_eq!(
        notice
            .completed
            .error
            .as_ref()
            .map(|error| error.message.clone()),
        Some("Bad model.".to_owned())
    );
    assert_eq!(
        notice
            .completed
            .process
            .as_ref()
            .and_then(|process| process.exit_code),
        Some(1)
    );
    assert_eq!(
        notice.delegate.as_ref().map(|finish| finish.text.clone()),
        Some(String::new())
    );

    // The receipt names a log that was never created.
    assert_eq!(
        std::fs::metadata(&output_path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    let tool = JobsTool::new(Arc::clone(&registry));
    let expected = format!("{job_id} failed scan \u{2014} Bad model.\n");
    for _ in 0..2 {
        let listed = tool.run(
            &args(serde_json::json!({"action": "list"})),
            &cancel,
            &Recorder::default(),
        );
        assert!(listed.error.is_none());
        assert_eq!(text_of(&listed), expected);
        // The whole recorded path is gone, not just its filename.
        assert!(
            !text_of(&listed).contains(&output_path),
            "{}",
            text_of(&listed)
        );
    }
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
}
